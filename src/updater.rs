use std::{sync::Mutex, time::Duration};

use futures_util::StreamExt;
use http::{HeaderMap, HeaderValue, StatusCode, header::ACCEPT};
use percent_encoding::{AsciiSet, CONTROLS};
use semver::Version;
use serde::{Deserialize, Deserializer};
use tauri::{AppHandle, Manager, Runtime};
use url::Url;

use crate::{
    Config, Error, OtaUpdaterExt, Result, assets,
    bundle::{Store, VersionComparator},
};

const USER_AGENT: &str = concat!("tauri-plugin-ffp-ota-updater/", env!("CARGO_PKG_VERSION"));

/// The release manifest an endpoint responds with.
///
/// Same shape as the updater plugin's dynamic response. A frontend bundle is platform
/// independent, so there is no `platforms` map.
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteRelease {
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

/// Per-request options for [`OtaUpdater::check_with`].
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    /// Extra headers sent with the manifest request and the bundle download.
    pub headers: HeaderMap,
    /// Timeout applied to each request.
    pub timeout: Option<Duration>,
}

/// Access to the frontend OTA updater. Get it with [`OtaUpdaterExt::ota_updater`].
pub struct OtaUpdater<R: Runtime> {
    pub(crate) app: AppHandle<R>,
    pub(crate) config: Config,
    pub(crate) store: Store,
    pub(crate) binary_version: Version,
    pub(crate) comparator: VersionComparator,
    /// The release the last successful `check` from the webview returned.
    pub(crate) pending: Mutex<Option<Update<R>>>,
}

impl<R: Runtime> OtaUpdater<R> {
    /// Version of the app binary, i.e. of the built-in frontend.
    pub fn binary_version(&self) -> &Version {
        &self.binary_version
    }

    /// Version of the frontend being served: the active bundle's, or else the binary's.
    pub fn current_version(&self) -> Version {
        assets::active()
            .map(|bundle| bundle.version.clone())
            .unwrap_or_else(|| self.binary_version.clone())
    }

    /// Checks the endpoints for a frontend update.
    ///
    /// Returns `None` when there is none, or when the release is not frontend-only according to
    /// the version comparator (by default: anything but a newer patch release of the binary).
    /// Those releases are the regular updater plugin's job.
    pub async fn check(&self) -> Result<Option<Update<R>>> {
        self.check_with(RequestOptions::default()).await
    }

    /// [`Self::check`] with extra headers and/or a timeout.
    pub async fn check_with(&self, options: RequestOptions) -> Result<Option<Update<R>>> {
        if self.config.endpoints.is_empty() {
            return Err(Error::EmptyEndpoints);
        }

        let current_version = self.current_version();
        let mut headers = options.headers.clone();
        headers
            .entry(ACCEPT)
            .or_insert(HeaderValue::from_static("application/json"));
        let client = client(options.timeout)?;

        let mut last_error = String::from("no endpoint responded successfully");
        let mut found = None;
        for endpoint in &self.config.endpoints {
            let url = self.resolve_endpoint(endpoint, &current_version)?;
            log::debug!("checking for frontend updates at {url}");
            let response = match client.get(url).headers(headers.clone()).send().await {
                Ok(response) => response,
                Err(error) => {
                    last_error = error.to_string();
                    continue;
                }
            };
            if response.status() == StatusCode::NO_CONTENT {
                return Ok(None);
            }
            if !response.status().is_success() {
                last_error = format!("endpoint responded with {}", response.status());
                continue;
            }
            let raw_json: serde_json::Value = match response.json().await {
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
            return Err(Error::ReleaseNotFound(last_error));
        };

        if release.version <= current_version
            || !(self.comparator)(&self.binary_version, &release.version)
        {
            return Ok(None);
        }

        Ok(Some(Update {
            app: self.app.clone(),
            current_version,
            version: release.version,
            date: release.pub_date,
            body: release.notes,
            raw_json,
            download_url: release.url,
            signature: release.signature,
            options,
        }))
    }

    /// Deletes every downloaded bundle and goes back to the built-in frontend, reloading the
    /// webviews if a bundle was being served.
    pub fn reset(&self) -> Result<()> {
        self.store.clear();
        if assets::active().is_some() {
            assets::set_active(None);
            self.reload()?;
        }
        Ok(())
    }

    fn reload(&self) -> Result<()> {
        for window in self.app.webview_windows().values() {
            window.reload()?;
        }
        Ok(())
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

fn client(timeout: Option<Duration>) -> Result<reqwest::Client> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // can only fail if another thread installed one in the meantime, which is just as good
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let mut builder = reqwest::ClientBuilder::new().user_agent(USER_AGENT);
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    Ok(builder.build()?)
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
    pub async fn download<C: FnMut(usize, Option<u64>), D: FnOnce()>(
        &self,
        mut on_chunk: C,
        on_download_finish: D,
    ) -> Result<()> {
        let updater = self.app.ota_updater();
        if self.download_url.scheme() != "https"
            && !updater.config.dangerous_insecure_transport_protocol
        {
            log::warn!(
                "the frontend bundle URL {} does not use https; only its signature protects it",
                self.download_url
            );
        }

        let mut headers = self.options.headers.clone();
        headers
            .entry(ACCEPT)
            .or_insert(HeaderValue::from_static("application/octet-stream"));
        let response = client(self.options.timeout)?
            .get(self.download_url.clone())
            .headers(headers)
            .send()
            .await?
            .error_for_status()?;

        let content_length = response.content_length();
        let mut archive = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            on_chunk(chunk.len(), content_length);
            archive.extend_from_slice(&chunk);
        }
        on_download_finish();

        updater
            .store
            .save(&archive, &self.signature, &self.version)?;
        Ok(())
    }

    /// Switches to the downloaded bundle and reloads every webview so it takes effect.
    ///
    /// The bundle is verified again as it is loaded from disk.
    pub fn install(&self) -> Result<()> {
        let updater = self.app.ota_updater();
        let bundle = updater.store.load(&self.version)?;
        assets::set_active(Some(bundle));
        updater.store.prune_older_than(&self.version);
        updater.reload()
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
