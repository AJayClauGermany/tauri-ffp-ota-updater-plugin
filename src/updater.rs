use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use http::{HeaderMap, HeaderValue, StatusCode, header::ACCEPT};
use percent_encoding::{AsciiSet, CONTROLS};
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize};
use tauri::{AppHandle, Manager, Runtime};
use url::Url;

use crate::{
    Config, Error, OtaUpdaterExt, Result, assets,
    bundle::{Bundle, Store},
};

const USER_AGENT: &str = concat!("tauri-plugin-ffp-ota-updater/", env!("CARGO_PKG_VERSION"));
/// A manifest is a few hundred bytes.
const MAX_MANIFEST_SIZE: u64 = 1 << 20;
/// Deadlines for a whole request when [`RequestOptions::timeout`] is not set.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Longest wait for a connection, and for more data once connected.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The release manifest an endpoint responds with.
///
/// Same shape as the updater plugin's dynamic response. A frontend bundle is platform
/// independent, so there is no `platforms` map.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RemoteRelease {
    /// Version of the frontend bundle. A leading `v` is tolerated.
    #[serde(deserialize_with = "parse_version")]
    pub version: Version,
    /// Release notes.
    pub notes: Option<String>,
    /// Release date, passed through as-is (RFC 3339 by convention).
    pub pub_date: Option<String>,
    /// Where to download the `.tar.gz` bundle from.
    pub url: Url,
    /// Content of the bundle's `.sig` file.
    pub signature: String,
}

fn parse_version<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Version, D::Error> {
    let value = String::deserialize(deserializer)?;
    Version::parse(value.trim_start_matches('v')).map_err(serde::de::Error::custom)
}

/// Per-request options for [`OtaUpdater::check_with`]. What is left unset falls back to the
/// [`Builder`](crate::Builder)'s setting.
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    /// Manifest URLs to query instead of the configured ones, in order. This is how an app
    /// switches release channels while it is running.
    pub endpoints: Option<Vec<Url>>,
    /// Extra headers sent with the manifest request and the bundle download.
    pub headers: HeaderMap,
    /// Timeout applied to each request. Defaults to 30 seconds for the manifest and 30 minutes
    /// for the bundle; a connection that stalls for 30 seconds is given up on either way.
    pub timeout: Option<Duration>,
    /// Proxy for the manifest request and the bundle download.
    pub proxy: Option<Url>,
}

/// What the [`Builder`](crate::Builder) sets for every request.
#[derive(Default)]
pub(crate) struct ClientOptions {
    pub headers: HeaderMap,
    pub timeout: Option<Duration>,
    pub proxy: Option<Url>,
    pub no_proxy: bool,
    #[allow(clippy::type_complexity)]
    pub configure:
        Option<Arc<dyn Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder + Send + Sync>>,
}

/// Where the updater stands, from [`OtaUpdater::status`]. Read from disk and memory; no request
/// is made.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Status {
    /// Version of the app binary, i.e. of the built-in frontend.
    pub binary_version: Version,
    /// Version of the frontend being served: the active bundle's, or else the binary's.
    pub current_version: Version,
    /// `false` while the frontend being served is a downloaded bundle that still has to be
    /// confirmed with `notify_ready`, and would be rolled back otherwise.
    pub confirmed: bool,
    /// A downloaded bundle newer than the one being served, which the next launch (or
    /// [`Update::install`]) switches to. It is verified only when it is loaded.
    pub staged: Option<Version>,
    /// Versions that failed their trial or were rolled back, oldest first. They are not
    /// offered or served again. The record is dropped once a full update makes them
    /// irrelevant.
    pub rejected: Vec<Version>,
}

/// Access to the frontend OTA updater. Get it with [`OtaUpdaterExt::ota_updater`].
pub struct OtaUpdater<R: Runtime> {
    pub(crate) app: AppHandle<R>,
    pub(crate) config: Config,
    pub(crate) client: ClientOptions,
    pub(crate) store: Store,
    pub(crate) binary_version: Version,
    /// The release the last successful `check` from the webview returned.
    pub(crate) pending: Mutex<Option<Update<R>>>,
    /// Held for the length of a download, so that concurrent requests for the same bundle
    /// (two windows, or two pages of a multi-page app in a row) fetch it once.
    pub(crate) downloading: tauri::async_runtime::Mutex<()>,
}

impl<R: Runtime> OtaUpdater<R> {
    /// Version of the frontend being served: the active bundle's, or else the binary's.
    pub fn current_version(&self) -> Version {
        assets::active()
            .map(|bundle| bundle.version.clone())
            .unwrap_or_else(|| self.binary_version.clone())
    }

    /// Where the updater stands: what is being served and whether it is confirmed, what is
    /// staged for the next launch and which versions were rolled back.
    pub fn status(&self) -> Status {
        let current_version = self.current_version();
        Status {
            binary_version: self.binary_version.clone(),
            confirmed: assets::active().is_none() || !self.store.unproven(&current_version),
            staged: self.store.staged(&current_version),
            rejected: self.store.rejected(),
            current_version,
        }
    }

    /// Checks the endpoints for a frontend update.
    ///
    /// Returns `None` when there is none, when the release is not frontend-only according to
    /// the compatibility rule (by default: anything but a newer patch release of the binary;
    /// those releases are the regular updater plugin's job), or when that version was rejected
    /// by a rollback before.
    pub async fn check(&self) -> Result<Option<Update<R>>> {
        self.check_with(RequestOptions::default()).await
    }

    /// [`Self::check`] with other endpoints, extra headers, a timeout and/or a proxy.
    pub async fn check_with(&self, mut options: RequestOptions) -> Result<Option<Update<R>>> {
        let endpoints = options.endpoints.take();
        let endpoints = endpoints.as_ref().unwrap_or(&self.config.endpoints);
        if endpoints.is_empty() {
            return Err(Error::EmptyEndpoints);
        }
        if !self.config.dangerous_insecure_transport_protocol
            && let Some(url) = endpoints.iter().find(|url| url.scheme() != "https")
        {
            return Err(Error::InsecureTransportProtocol(url.to_string()));
        }

        // what the download of the release found here is made with, too
        let mut headers = self.client.headers.clone();
        let mut replaced = None;
        for (name, value) in std::mem::take(&mut options.headers) {
            // `None` is another value for the name before it
            if let Some(name) = name {
                headers.remove(&name);
                replaced = Some(name);
            }
            if let Some(name) = &replaced {
                headers.append(name, value);
            }
        }
        options.headers = headers;
        options.timeout = options.timeout.or(self.client.timeout);
        options.proxy = options.proxy.or_else(|| self.client.proxy.clone());

        let current_version = self.current_version();
        let mut headers = options.headers.clone();
        headers
            .entry(ACCEPT)
            .or_insert(HeaderValue::from_static("application/json"));
        let client = self.client(
            options.timeout.unwrap_or(CHECK_TIMEOUT),
            options.proxy.as_ref(),
        )?;

        let mut last_error = String::from("no endpoint responded successfully");
        // set while no endpoint has answered at all
        let mut unreachable = None;
        let mut responded = false;
        let mut found = None;
        for endpoint in endpoints {
            let url = self.resolve_endpoint(endpoint, &current_version)?;
            log::debug!("checking for frontend updates at {url}");
            let response = match client.get(url).headers(headers.clone()).send().await {
                Ok(response) => response,
                Err(error) => {
                    last_error = error.to_string();
                    unreachable = Some(error);
                    continue;
                }
            };
            responded = true;
            if response.status() == StatusCode::NO_CONTENT {
                return Ok(None);
            }
            if !response.status().is_success() {
                last_error = format!("endpoint responded with {}", response.status());
                continue;
            }
            let body = read_limited(response, "manifest", MAX_MANIFEST_SIZE, |_| {}).await;
            let raw_json: serde_json::Value =
                match body.and_then(|body| Ok(serde_json::from_slice(&body)?)) {
                    Ok(json) => json,
                    Err(error) => {
                        last_error = error.to_string();
                        continue;
                    }
                };
            match serde_json::from_value::<RemoteRelease>(raw_json.clone()) {
                Ok(release) => {
                    found = Some((release, raw_json));
                    break;
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        let Some((release, raw_json)) = found else {
            return Err(match unreachable {
                // offline, most likely: a different matter than a broken manifest
                Some(error) if !responded => Error::Reqwest(error),
                _ => Error::ReleaseNotFound(last_error),
            });
        };

        if release.version <= current_version
            || !self.store.compatible(&release.version)
            || self.store.is_rejected(&release.version)
        {
            return Ok(None);
        }

        Ok(Some(Update {
            app: self.app.clone(),
            current_version,
            downloaded: self
                .store
                .is_stored(&release.version, &release.signature, false),
            version: release.version,
            date: release.pub_date,
            body: release.notes,
            raw_json,
            download_url: release.url,
            signature: release.signature,
            options,
        }))
    }

    /// Tells the updater that the frontend being served works.
    ///
    /// A bundle is on trial from the first page loaded from it until this is called. A run of
    /// the app that ends with the trial still open is a failed attempt; after `trialAttempts`
    /// of them (2 by default) that version is rejected for good and the last confirmed bundle,
    /// or else the built-in frontend, is served instead. Does nothing when there is no trial,
    /// which includes a bundle no page was loaded from yet, so call it on every start, once
    /// the frontend got far enough that it could update itself again.
    pub fn notify_ready(&self) -> Result<()> {
        match assets::active() {
            Some(bundle) => self.store.confirm(&bundle.version),
            None => Ok(()),
        }
    }

    /// Abandons the bundle being served, whether on trial or confirmed, without help from the
    /// frontend: its version is rejected, the app switches to the last confirmed bundle or the
    /// built-in frontend and the webviews are reloaded.
    pub fn rollback(&self) -> Result<()> {
        if let Some(bundle) = assets::active() {
            self.serve(self.store.select(Some(&bundle.version)));
            self.reload()?;
        }
        Ok(())
    }

    /// Makes `bundle` (or the built-in frontend) what is served from now on.
    ///
    /// The trial of an unconfirmed bundle starts when the first page is loaded from it, not
    /// here: an app that is closed before it showed the bundle has learned nothing about it.
    /// With `readyTimeout` set, the bundle is rolled back if it is still unconfirmed that long
    /// after that page load.
    pub(crate) fn serve(&self, bundle: Option<Bundle>) {
        let timeout = self.config.ready_timeout.filter(|ms| *ms > 0);
        let unproven = bundle
            .as_ref()
            .map(|bundle| bundle.version.clone())
            .filter(|version| self.store.unproven(version));
        let on_document = unproven.map(|version| {
            let app = self.app.clone();
            Box::new(move || {
                let Some(updater) = app.try_state::<OtaUpdater<R>>() else {
                    return;
                };
                if let Err(error) = updater.store.begin_trial(&version) {
                    // a trial that is not on record could not be rolled back
                    log::warn!("not serving frontend bundle {version}: {error}");
                    updater.serve(updater.store.fallback());
                    return;
                }
                let Some(timeout) = timeout else {
                    return;
                };
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(timeout));
                    let Some(updater) = app.try_state::<OtaUpdater<R>>() else {
                        return;
                    };
                    if let Some(fallback) = updater.store.fail_trial(&version) {
                        log::warn!(
                            "frontend bundle {version} was not confirmed within {timeout} ms; rolling it back"
                        );
                        updater.serve(fallback);
                        if let Err(error) = updater.reload() {
                            log::error!("could not reload after the rollback: {error}");
                        }
                    }
                });
            }) as assets::DocumentHook
        });
        assets::set_active(bundle, on_document);
    }

    /// Deletes every downloaded bundle and goes back to the built-in frontend, reloading the
    /// webviews if a bundle was being served.
    pub fn reset(&self) -> Result<()> {
        self.store.clear()?;
        if assets::active().is_some() {
            self.serve(None);
            self.reload()?;
        }
        Ok(())
    }

    /// Reloads every webview, which makes them load the frontend that is active now.
    pub fn reload(&self) -> Result<()> {
        for window in self.app.webview_windows().values() {
            window.reload()?;
        }
        Ok(())
    }

    fn client(&self, timeout: Duration, proxy: Option<&Url>) -> Result<reqwest::Client> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            // can only fail if another thread installed one in the meantime, which is just as
            // good
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let mut builder = reqwest::ClientBuilder::new()
            .user_agent(USER_AGENT)
            // covers redirects too
            .https_only(!self.config.dangerous_insecure_transport_protocol)
            .connect_timeout(STALL_TIMEOUT)
            .read_timeout(STALL_TIMEOUT)
            .timeout(timeout);
        if let Some(proxy) = proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy.as_str())?);
        } else if self.client.no_proxy {
            builder = builder.no_proxy();
        }
        if let Some(configure) = &self.client.configure {
            builder = configure(builder);
        }
        Ok(builder.build()?)
    }

    fn resolve_endpoint(&self, endpoint: &Url, current_version: &Version) -> Result<Url> {
        const ENCODE: &AsciiSet = &CONTROLS.add(b'+');
        let encode =
            |v: &Version| percent_encoding::utf8_percent_encode(&v.to_string(), ENCODE).to_string();
        let mut url = endpoint.to_string();
        for (name, value) in [
            ("current_version", encode(current_version)),
            ("binary_version", encode(&self.binary_version)),
            ("target", target().to_string()),
            ("arch", arch().to_string()),
        ] {
            // `Url` percent-encodes braces in the path but not in the query
            url = url
                .replace(&format!("%7B%7B{name}%7D%7D"), &value)
                .replace(&format!("{{{{{name}}}}}"), &value);
        }
        Ok(url.parse()?)
    }
}

/// Reads a response body of at most `limit` bytes.
async fn read_limited(
    response: reqwest::Response,
    what: &'static str,
    limit: u64,
    mut on_chunk: impl FnMut(usize),
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(Error::TooLarge(what, limit));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if (body.len() + chunk.len()) as u64 > limit {
            return Err(Error::TooLarge(what, limit));
        }
        on_chunk(chunk.len());
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn target() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        os => os,
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "i686",
        "arm" => "armv7",
        arch => arch,
    }
}

/// An available frontend update, returned by [`OtaUpdater::check`].
pub struct Update<R: Runtime> {
    app: AppHandle<R>,
    /// Version of the frontend currently being served.
    pub current_version: Version,
    /// Version of the update.
    pub version: Version,
    /// Whether this bundle was already downloaded when the check was made, and is waiting for
    /// [`Self::install`] or the next launch. [`Self::download`] then does not fetch it again.
    pub downloaded: bool,
    /// Release date from the manifest.
    pub date: Option<String>,
    /// Release notes from the manifest.
    pub body: Option<String>,
    /// The manifest as the endpoint returned it.
    pub raw_json: serde_json::Value,
    /// Where the bundle is downloaded from.
    pub download_url: Url,
    /// The bundle's signature.
    pub signature: String,
    options: RequestOptions,
}

impl<R: Runtime> Clone for Update<R> {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            current_version: self.current_version.clone(),
            version: self.version.clone(),
            downloaded: self.downloaded,
            date: self.date.clone(),
            body: self.body.clone(),
            raw_json: self.raw_json.clone(),
            download_url: self.download_url.clone(),
            signature: self.signature.clone(),
            options: self.options.clone(),
        }
    }
}

impl<R: Runtime> Update<R> {
    /// Downloads the bundle, verifies it and stores it.
    ///
    /// A stored bundle is picked up automatically on the next launch; call [`Self::install`] to
    /// switch to it right away. `on_chunk` receives the chunk length and the total content
    /// length, if known.
    ///
    /// A bundle that is already stored, with the signature this release announces, is not
    /// fetched again: only `on_download_finish` is called. Calling this on every start, or on
    /// every page of a multi-page app, is therefore cheap.
    pub async fn download<C: FnMut(usize, Option<u64>), D: FnOnce()>(
        &self,
        mut on_chunk: C,
        on_download_finish: D,
    ) -> Result<()> {
        let updater = self.app.ota_updater();
        if self.download_url.scheme() != "https"
            && !updater.config.dangerous_insecure_transport_protocol
        {
            return Err(Error::InsecureTransportProtocol(
                self.download_url.to_string(),
            ));
        }

        let _downloading = updater.downloading.lock().await;
        if updater
            .store
            .is_stored(&self.version, &self.signature, true)
        {
            on_download_finish();
            return Ok(());
        }

        let mut headers = self.options.headers.clone();
        headers
            .entry(ACCEPT)
            .or_insert(HeaderValue::from_static("application/octet-stream"));
        let response = updater
            .client(
                self.options.timeout.unwrap_or(DOWNLOAD_TIMEOUT),
                self.options.proxy.as_ref(),
            )?
            .get(self.download_url.clone())
            .headers(headers)
            .send()
            .await?
            .error_for_status()?;

        let content_length = response.content_length();
        let limit = updater.config.max_bundle_size;
        let archive = read_limited(response, "bundle", limit, |chunk| {
            on_chunk(chunk, content_length)
        })
        .await?;
        on_download_finish();

        updater
            .store
            .save(&archive, &self.signature, &self.version)?;
        Ok(())
    }

    /// Switches to the downloaded bundle without touching the webviews: documents that are
    /// already loaded keep running the old frontend, and whatever they or new windows request
    /// from now on is served from the new one.
    ///
    /// For apps that cannot be reloaded at an arbitrary moment. Finish or block the work that
    /// must not be interrupted, call this, then reload or recreate each window (see
    /// [`OtaUpdater::reload`]) before the old documents load more of their assets.
    ///
    /// The bundle is verified again as it is loaded from disk, and is on trial from the first
    /// page loaded from it until [`OtaUpdater::notify_ready`].
    pub fn activate(&self) -> Result<()> {
        let updater = self.app.ota_updater();
        updater.serve(Some(updater.store.activate(&self.version)?));
        Ok(())
    }

    /// [`Self::activate`], then reloads every webview so the bundle takes effect.
    pub fn install(&self) -> Result<()> {
        self.activate()?;
        self.app.ota_updater().reload()
    }

    /// [`Self::download`] followed by [`Self::install`].
    pub async fn download_and_install<C: FnMut(usize, Option<u64>), D: FnOnce()>(
        &self,
        on_chunk: C,
        on_download_finish: D,
    ) -> Result<()> {
        self.download(on_chunk, on_download_finish).await?;
        self.install()
    }
}
