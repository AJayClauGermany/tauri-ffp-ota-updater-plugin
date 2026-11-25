use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use futures_util::StreamExt;
use http::{
    HeaderMap, HeaderName, HeaderValue, StatusCode,
    header::{ACCEPT, LOCATION},
};
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
/// Redirects followed for one request before it is given up on.
const MAX_REDIRECTS: usize = 10;

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
///
/// ```
/// # use tauri_plugin_ffp_ota_updater::RequestOptions;
/// let options = RequestOptions::new()
///     .endpoints(vec!["https://releases.example.com/beta/ota.json".parse().unwrap()])
///     .timeout(std::time::Duration::from_secs(20));
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RequestOptions {
    /// Manifest URLs to query instead of the configured ones, in order. This is how an app
    /// switches release channels while it is running.
    pub endpoints: Option<Vec<Url>>,
    /// Extra headers for the manifest request and the bundle download. They are only ever sent
    /// to the origin (scheme, host and port) of the endpoint that is being asked: not to a
    /// bundle on another origin, and not along a redirect that leaves it.
    ///
    /// The manifest is not signed, so whoever can change it could otherwise point the download
    /// at their own server and collect an `Authorization` header before the signature check
    /// rejects what they serve. There is deliberately no way to send headers to wherever the
    /// manifest points: an app whose bundles need credentials on another origin fetches them
    /// itself and hands them to [`OtaUpdater::stage`].
    pub headers: HeaderMap,
    /// Timeout applied to each request, and to each request a redirect leads to. Defaults to
    /// 30 seconds for the manifest and 30 minutes for the bundle; a connection that stalls for
    /// 30 seconds is given up on either way.
    pub timeout: Option<Duration>,
    /// Proxy for the manifest request and the bundle download.
    pub proxy: Option<Url>,
}

impl RequestOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets [`Self::endpoints`].
    pub fn endpoints(mut self, endpoints: Vec<Url>) -> Self {
        self.endpoints = Some(endpoints);
        self
    }

    /// Adds a header to [`Self::headers`], replacing one of the same name.
    pub fn header<K, V>(mut self, key: K, value: V) -> Result<Self>
    where
        HeaderName: TryFrom<K>,
        <HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        HeaderValue: TryFrom<V>,
        <HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        let key: std::result::Result<HeaderName, http::Error> = key.try_into().map_err(Into::into);
        let value: std::result::Result<HeaderValue, http::Error> =
            value.try_into().map_err(Into::into);
        self.headers.insert(key?, value?);
        Ok(self)
    }

    /// Sets [`Self::timeout`].
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets [`Self::proxy`].
    pub fn proxy(mut self, proxy: Url) -> Self {
        self.proxy = Some(proxy);
        self
    }
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
    /// A stored bundle newer than the one being served, which [`OtaUpdater::install`] switches
    /// to and [`OtaUpdater::discard`] removes. The next launch switches to it by itself, unless
    /// `requireApproval` is set: then it waits for [`OtaUpdater::approve`]. It is verified only
    /// when it is loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged: Option<Version>,
    /// The stored bundle that was approved with [`OtaUpdater::approve`] and is newer than the
    /// one being served: with `requireApproval`, what the next launch switches to. It can be
    /// older than [`Self::staged`], when a newer bundle was downloaded since.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved: Option<Version>,
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
    /// The releases `check` returned to the webview, which holds on to them by version only.
    pub(crate) pending: Mutex<HashMap<Version, Update<R>>>,
    /// Held for the length of a download, so that concurrent requests for the same bundle
    /// (two windows, or two pages of a multi-page app in a row) fetch it once.
    pub(crate) downloading: tauri::async_runtime::Mutex<()>,
    /// Why no bundle can be served in this run of the app, if that is the case.
    pub(crate) cannot_serve: Option<&'static str>,
    /// For each webview, by label, the bundle its current page was loaded from (`None` for the
    /// built-in frontend or another site). Only a page of the bundle on trial may confirm it.
    pub(crate) documents: Mutex<HashMap<String, Option<Version>>>,
    /// Held while deciding on and switching the frontend being served, see [`Self::switching`].
    pub(crate) switching: Mutex<()>,
}

/// Whether a page whose recorded origin is `shown` (see [`OtaUpdater::documents`]; `None` when
/// nothing was recorded) may confirm the bundle `serving`.
///
/// A webview no page load was seen for cannot: nothing says its page is the new frontend. Tauri
/// reports every navigation of every webview to the plugin, so this only leaves out pages
/// that were loaded before the plugin was there to pick a bundle.
pub(crate) fn may_confirm(shown: Option<&Option<Version>>, serving: &Version) -> bool {
    shown.is_some_and(|shown| shown.as_ref() == Some(serving))
}

/// Whether `url` is a page of the app's own frontend: `tauri://localhost` or, on Windows,
/// `http(s)://tauri.localhost`.
pub(crate) fn is_app_url(url: &Url) -> bool {
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => url.host_str() == Some("tauri.localhost"),
        _ => false,
    }
}

impl<R: Runtime> OtaUpdater<R> {
    /// Serializes switches of the frontend being served. Each decides what to serve from the
    /// store and from what is being served, and has to publish the result before another
    /// switch looks: otherwise two installs racing each other, or an install racing an expired
    /// ready timeout, could leave an older frontend served over a newer one, possibly one whose
    /// files the other switch already deleted.
    ///
    /// Taken before [`Store::guard`], never while holding it. Not held while reloading.
    fn switching(&self) -> MutexGuard<'_, ()> {
        self.switching
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Version of the frontend being served: the active bundle's, or else the binary's.
    pub fn current_version(&self) -> Version {
        assets::active()
            .map(|bundle| bundle.version.clone())
            .unwrap_or_else(|| self.binary_version.clone())
    }

    /// Where the updater stands: what is being served and whether it is confirmed, what is
    /// staged and what is approved for the next launch, and which versions were rolled back.
    pub fn status(&self) -> Status {
        let current_version = self.current_version();
        Status {
            binary_version: self.binary_version.clone(),
            confirmed: assets::active().is_none() || !self.store.unproven(&current_version),
            staged: self.store.staged(&current_version),
            approved: self.store.approved(&current_version),
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
        self.ensure_pubkey()?;

        // what the download of the release found here is made with, too
        options.headers = merge_headers(&self.client.headers, std::mem::take(&mut options.headers));
        options.timeout = options.timeout.or(self.client.timeout);
        options.proxy = options.proxy.or_else(|| self.client.proxy.clone());

        let current_version = self.current_version();
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
            let response =
                match get(&client, &url, &url, &options.headers, "application/json").await {
                    Ok(response) => response,
                    Err(Error::Reqwest(error)) => {
                        last_error = error.to_string();
                        unreachable = Some(error);
                        continue;
                    }
                    Err(error) => {
                        // a redirect that leads nowhere: the endpoint did answer
                        responded = true;
                        last_error = error.to_string();
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
                    found = Some((release, raw_json, url));
                    break;
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        let Some((release, raw_json, manifest_url)) = found else {
            return Err(match unreachable {
                // offline, most likely: a different matter than a broken manifest
                Some(error) if !responded => Error::Reqwest(error),
                _ => Error::ReleaseNotFound(last_error),
            });
        };

        if !self.store.order.newer(&release.version, &current_version)
            || !self.store.compatible(&release.version)
            || self.store.is_rejected(&release.version)?
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
            manifest_url,
            options,
        }))
    }

    /// Verifies and stores a bundle that the app obtained by its own means: another transport
    /// or manifest format, a file shipped with the installer, a patch applied to the previous
    /// archive. The checks are those of [`Update::download`]: `signature` (the content of the
    /// `.sig` file) has to be this app's key signing exactly `archive`, for `version`, and the
    /// version has to be compatible with the binary and not rejected.
    ///
    /// The bundle is then staged like a downloaded one: served from the next launch on (with
    /// `requireApproval`, once it is [approved](Self::approve)), or at once with
    /// [`Self::install`].
    ///
    /// A version names one bundle: staging different contents under a version that is already
    /// stored fails with [`Error::InUse`], as does a version that differs from a stored one
    /// only in letter case. Staging the same contents again does nothing. To replace a bundle
    /// that was never shown, [`Self::discard`] it first.
    pub fn stage(&self, archive: &[u8], signature: &str, version: &Version) -> Result<()> {
        self.ensure_pubkey()?;
        self.ensure_not_older(version)?;
        self.store.save(archive, signature, version)
    }

    fn ensure_pubkey(&self) -> Result<()> {
        if self.config.pubkey.trim().is_empty() {
            return Err(Error::EmptyPubkey);
        }
        Ok(())
    }

    /// Fails with [`Error::NotNewer`] unless `version` is newer than the frontend being served or
    /// is exactly that one.
    fn ensure_not_older(&self, version: &Version) -> Result<()> {
        let current = self.current_version();
        if *version != current && !self.store.order.newer(version, &current) {
            return Err(Error::NotNewer(version.to_string()));
        }
        Ok(())
    }

    /// Switches to the stored bundle `version` without touching the webviews: documents that
    /// are already loaded keep running the old frontend, and whatever they or new windows
    /// request from now on is served from the new one. No request is made, so a bundle staged
    /// earlier (see [`Status::staged`]) can be applied offline.
    ///
    /// For apps that cannot be reloaded at an arbitrary moment. Finish or block the work that
    /// must not be interrupted, call this, then reload or recreate each window (see
    /// [`Self::reload`]) before the old documents load more of their assets.
    ///
    /// The bundle is verified again as it is loaded from disk, and is on trial from the first
    /// page loaded from it until [`Self::notify_ready`]. With `requireApproval`, switching to a
    /// bundle approves it as well.
    ///
    /// Fails with [`Error::CannotServe`] when the assets were not wrapped with
    /// [`wrap_assets`](crate::wrap_assets), the app runs from `devUrl` in development mode, or
    /// its `frontendDist` is a URL.
    ///
    /// Does nothing when `version` is already the frontend being served.
    pub fn activate(&self, version: &Version) -> Result<()> {
        self.switch_to(version).map(|_| ())
    }

    /// [`Self::activate`], then reloads every webview so the bundle takes effect.
    ///
    /// Does nothing, and reloads nothing, when `version` is already the frontend being served:
    /// two windows, or a button pressed twice, that install the same update reload the app once.
    ///
    /// An [`Error::Tauri`] is a webview that could not be reloaded: the switch has happened
    /// ([`Self::current_version`] is `version`), and that webview shows the previous frontend
    /// until it is reloaded or recreated with [`Self::reload`]; installing again does not reload
    /// it. Any other error means nothing was switched.
    pub fn install(&self, version: &Version) -> Result<()> {
        if self.switch_to(version)? {
            self.reload()
        } else {
            Ok(())
        }
    }

    /// [`Self::activate`], returning whether anything was switched.
    fn switch_to(&self, version: &Version) -> Result<bool> {
        if let Some(reason) = self.cannot_serve {
            return Err(Error::CannotServe(reason));
        }
        self.ensure_pubkey()?;
        let _switching = self.switching();
        if assets::active().is_some_and(|active| active.version == *version) {
            return Ok(false);
        }
        self.ensure_not_older(version)?;
        self.serve(Some(self.store.activate(version)?));
        Ok(true)
    }

    /// Approves the stored bundle `version` for the next launch, and changes nothing in this
    /// run of the app. This is the consent that `requireApproval` makes a launch wait for:
    /// with it, a bundle that was downloaded stays stored, across restarts, until it is approved
    /// here or switched to with [`Self::activate`] or [`Self::install`]. Without
    /// `requireApproval` the next launch takes the newest stored bundle either way, and this
    /// only records the choice.
    ///
    /// The bundle is verified as it is when loaded, so that what is approved can be served.
    /// One version is approved at a time: approving another replaces it. No request is made.
    ///
    /// Fails with [`Error::NotDownloaded`] for a version that is not stored, [`Error::NotNewer`]
    /// for one older than the frontend being served and [`Error::Rejected`] for one that was
    /// rolled back.
    pub fn approve(&self, version: &Version) -> Result<()> {
        self.ensure_pubkey()?;
        // what is being served must not change between looking and recording
        let _switching = self.switching();
        self.ensure_not_older(version)?;
        self.store.approve(version)
    }

    /// Takes back the approval of `version`, for a user who changed their mind. The bundle
    /// stays stored and can be approved again. Does nothing if that version is not the approved
    /// one, and nothing about a bundle that was served before: that one is abandoned with
    /// [`Self::rollback`].
    pub fn unapprove(&self, version: &Version) -> Result<()> {
        self.store.unapprove(version)
    }

    /// Deletes the stored bundle `version` so that it is not served at the next launch, for an
    /// update the user declined or the app decided to skip. The version is not rejected and
    /// can be downloaded again. Fails with [`Error::InUse`] for the bundle being served and
    /// for the confirmed one kept as the fallback; does nothing if the version is not stored.
    ///
    /// A download of that version that is still running is not cancelled, and stores the
    /// bundle again when it completes: wait for it first. The same goes for [`Self::reset`].
    pub fn discard(&self, version: &Version) -> Result<()> {
        // what is being served must not change between looking and deleting
        let _switching = self.switching();
        let serving = assets::active().map(|bundle| bundle.version.clone());
        self.store.discard(version, serving.as_ref())
    }

    /// Tells the updater that the frontend being served works.
    ///
    /// A bundle is on trial from the first page loaded from it until this is called. A run of
    /// the app that ends with the trial still open is a failed attempt; after `trialAttempts`
    /// of them (2 by default) that version is rejected for good and the last confirmed bundle,
    /// or else the built-in frontend, is served instead. Does nothing when there is no trial,
    /// which includes a bundle no page was loaded from yet, so call it on every start, once
    /// the frontend got far enough that it could update itself again.
    ///
    /// This vouches for the bundle without asking which page works: call it from Rust only once
    /// something has shown that the new frontend runs. The `notifyReady()` of the JavaScript API
    /// is only accepted from a page that was loaded from the bundle on trial.
    pub fn notify_ready(&self) -> Result<()> {
        match assets::active() {
            Some(bundle) => self.store.confirm(&bundle.version),
            None => Ok(()),
        }
    }

    /// [`Self::notify_ready`] on behalf of the page in the webview `label`. Ignored when that
    /// page was not loaded from the bundle being served, for instance a window that still shows
    /// the previous frontend while the others reload into a new one.
    pub(crate) fn notify_ready_from(&self, label: &str) -> Result<()> {
        let Some(bundle) = assets::active() else {
            return Ok(());
        };
        if !may_confirm(self.documents.lock().unwrap().get(label), &bundle.version) {
            log::debug!(
                "ignoring notifyReady from `{label}`: its page is not from frontend {}",
                bundle.version
            );
            return Ok(());
        }
        self.store.confirm(&bundle.version)
    }

    /// Records which frontend a page that starts loading in the webview `label` comes from.
    ///
    /// This is the frontend being served when the navigation is reported, which is the one the
    /// page is loaded from unless a switch falls between that report and the request for the
    /// page. Tauri does not say which webview an asset is requested by, so the two cannot be
    /// tied together more closely. The switch would have to be an [`OtaUpdater::activate`]
    /// landing in the middle of a navigation, and for the wrong page to confirm anything, the
    /// trial has to have begun in another window as well. `install` reloads every webview
    /// right after switching, which records them all anew.
    pub(crate) fn record_document(&self, label: &str, url: &Url) {
        let version = is_app_url(url)
            .then(assets::active)
            .flatten()
            .map(|bundle| bundle.version.clone());
        self.documents
            .lock()
            .unwrap()
            .insert(label.to_string(), version);
    }

    /// Abandons the bundle being served, whether on trial or confirmed, without help from the
    /// frontend: its version is rejected, the app switches to the last confirmed bundle or the
    /// built-in frontend and the webviews are reloaded. A newer bundle that is stored but was
    /// never shown is kept, and is tried at the next launch.
    ///
    /// Fails if the rejection could not be recorded, in which case the version would be served
    /// or offered again after a restart, or if a webview could not be reloaded. This run of the
    /// app has gone back all the same.
    pub fn rollback(&self) -> Result<()> {
        let recorded = {
            let _switching = self.switching();
            assets::active().map(|bundle| {
                let (fallback, recorded) = self.store.select(Some(&bundle.version));
                self.serve(fallback);
                recorded
            })
        };
        match recorded {
            Some(recorded) => recorded.and(self.reload()),
            None => Ok(()),
        }
    }

    /// Makes `bundle` (or the built-in frontend) what is served from now on. Called with
    /// [`Self::switching`] held, except at setup.
    ///
    /// The trial of an unconfirmed bundle starts when the first page is loaded from it, not
    /// here: an app that is closed before it showed the bundle has learned nothing about it.
    /// With `readyTimeout` set, the bundle is rolled back if it is still unconfirmed that long
    /// after that page load.
    ///
    /// What happens before that page load is therefore not covered by the trial: verifying and
    /// unpacking the bundle, and preparing it for the CSP below. A bundle that is validly
    /// signed and within `maxBundleSize` and `maxUnpackedSize`, yet takes the process down
    /// there, would do so at every launch. Those limits are the guard against that. Counting
    /// an interrupted preparation as an attempt is possible later without changing the store's
    /// format (`state.json` ignores fields it does not know).
    pub(crate) fn serve(&self, bundle: Option<Bundle>) {
        let bundle = bundle
            .filter(|_| self.cannot_serve.is_none())
            .map(|mut bundle| {
                crate::csp::prepare(&mut bundle, &self.app.config().app.security);
                bundle
            });
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
                let trial = {
                    let _switching = updater.switching();
                    if assets::active().is_none_or(|active| active.version != version) {
                        // switched away from in the meantime
                        return;
                    }
                    match updater.store.begin_trial(&version) {
                        Ok(trial) => trial,
                        Err(error) => {
                            // a trial that is not on record could not be rolled back
                            log::warn!("not serving frontend bundle {version}: {error}");
                            updater.serve(updater.store.fallback());
                            return;
                        }
                    }
                };
                let (Some(trial), Some(timeout)) = (trial, timeout) else {
                    return;
                };
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(timeout));
                    let Some(updater) = app.try_state::<OtaUpdater<R>>() else {
                        return;
                    };
                    let rolled_back = {
                        let _switching = updater.switching();
                        // a trial only ever runs for the bundle being served; anything else
                        // means it was switched away from
                        assets::active()
                            .is_some_and(|active| active.version == version)
                            // not a later trial of the same version, after a reset
                            .then(|| updater.store.fail_trial(&version, trial))
                            .flatten()
                            .map(|(fallback, recorded)| {
                                updater.serve(fallback);
                                if let Err(error) = recorded {
                                    log::error!(
                                        "frontend bundle {version} may be served again after a restart: {error}"
                                    );
                                }
                            })
                            .is_some()
                    };
                    if rolled_back {
                        log::warn!(
                            "frontend bundle {version} was not confirmed within {timeout} ms; rolled it back"
                        );
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
    ///
    /// Fails if a bundle could not be deleted, which would be served again at the next launch;
    /// this run of the app goes back to the built-in frontend all the same.
    pub fn reset(&self) -> Result<()> {
        let (cleared, was_active) = {
            let _switching = self.switching();
            let cleared = self.store.clear();
            let was_active = assets::active().is_some();
            if was_active {
                self.serve(None);
            }
            (cleared, was_active)
        };
        if was_active {
            self.reload()?;
        }
        cleared
    }

    /// Reloads every webview window, which makes them load the frontend that is active now.
    /// Each of them is tried; the first error is returned.
    ///
    /// Webviews that share a window (Tauri's `unstable` multi-webview windows) are not listed
    /// by Tauri without that feature, and are not reloaded: an app that has them reloads them
    /// itself with `Webview::reload` after [`Self::activate`] or [`Self::install`].
    pub fn reload(&self) -> Result<()> {
        let mut result = Ok(());
        for window in self.app.webview_windows().values() {
            if let Err(error) = window.reload() {
                result = result.and(Err(error.into()));
            }
        }
        result
    }

    /// The HTTP client for one check or download. It follows no redirects: [`get`] does, to
    /// decide for each one which headers go along.
    fn client(&self, timeout: Duration, proxy: Option<&Url>) -> Result<reqwest::Client> {
        #[cfg(feature = "rustls-tls")]
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
        // After `configure`, which may change everything else: were reqwest to follow redirects,
        // it would take every header but a few standard ones (`Authorization`, `Cookie`) to
        // another origin, and name the URL it came from, query included, in a `Referer`.
        //
        // Nor does it decompress anything. The plugin does not enable those features, but
        // another crate of the app may (they are shared). A host that sends the `.tar.gz` with
        // `Content-Encoding: gzip`, as some object stores do, would then hand over the tar
        // inside it, whose bytes are not the ones that were signed.
        builder = builder
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd();
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

/// `overrides` on top of `base`: a name in `overrides` replaces all of its values in `base`.
fn merge_headers(base: &HeaderMap, overrides: HeaderMap) -> HeaderMap {
    let mut headers = base.clone();
    let mut replaced = None;
    for (name, value) in overrides {
        // `None` is another value for the name before it
        if let Some(name) = name {
            headers.remove(&name);
            replaced = Some(name);
        }
        if let Some(name) = &replaced {
            headers.append(name, value);
        }
    }
    headers
}

/// Requests `url` and follows redirects, up to [`MAX_REDIRECTS`] of them.
///
/// `headers` are the caller's, and whoever receives them can keep them: they are sent only to
/// the origin of `trusted`, the endpoint the app was configured to ask. Neither the unsigned
/// manifest, which names `url` for a download, nor a redirect can take them elsewhere. A request
/// to another origin carries nothing but `accept` and the client's defaults.
async fn get(
    client: &reqwest::Client,
    url: &Url,
    trusted: &Url,
    headers: &HeaderMap,
    accept: &'static str,
) -> Result<reqwest::Response> {
    let mut url = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let mut request = if url.origin() == trusted.origin() {
            headers.clone()
        } else {
            if !headers.is_empty() {
                log::debug!("not sending the request's headers to {url}, which is another origin");
            }
            HeaderMap::new()
        };
        request
            .entry(ACCEPT)
            .or_insert(HeaderValue::from_static(accept));
        let response = client.get(url.clone()).headers(request).send().await?;
        let location = matches!(
            response.status(),
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        )
        .then(|| response.headers().get(LOCATION))
        .flatten()
        .and_then(|location| location.to_str().ok());
        match location {
            // relative to the URL that redirected; `https` is enforced by the client
            Some(location) => url = url.join(location)?,
            None => return Ok(response),
        }
    }
    Err(Error::TooManyRedirects(url.to_string()))
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
    /// [`Self::install`], the next launch or, with `requireApproval`, [`OtaUpdater::approve`].
    /// [`Self::download`] then does not fetch it again.
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
    /// The endpoint the manifest was asked from, which is where [`RequestOptions::headers`]
    /// may go.
    manifest_url: Url,
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
            manifest_url: self.manifest_url.clone(),
            options: self.options.clone(),
        }
    }
}

impl<R: Runtime> Update<R> {
    /// This update with other headers and a timeout for its download, as the updater plugin's
    /// `DownloadOptions` give them. Headers given here replace those of the check, on top of the
    /// [`Builder`](crate::Builder)'s, and still only go to the endpoint's origin.
    pub(crate) fn with_download_options(
        mut self,
        headers: Option<HeaderMap>,
        timeout: Option<Duration>,
    ) -> Self {
        if let Some(headers) = headers {
            let base = &self.app.ota_updater().client.headers;
            self.options.headers = merge_headers(base, headers);
        }
        if let Some(timeout) = timeout {
            self.options.timeout = Some(timeout);
        }
        self
    }

    /// Downloads the bundle, verifies it and stores it.
    ///
    /// A stored bundle is picked up automatically on the next launch, or with `requireApproval`
    /// once it is [approved](OtaUpdater::approve); call [`Self::install`] to switch to it right
    /// away. `on_chunk` receives the chunk length and the total content length, if known.
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

        let client = updater.client(
            self.options.timeout.unwrap_or(DOWNLOAD_TIMEOUT),
            self.options.proxy.as_ref(),
        )?;
        let response = get(
            &client,
            &self.download_url,
            &self.manifest_url,
            &self.options.headers,
            "application/octet-stream",
        )
        .await?
        .error_for_status()?;
        if !response.status().is_success() {
            // a redirect without a destination, say: its body is not the bundle
            return Err(Error::TooManyRedirects(response.url().to_string()));
        }

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

    /// [`OtaUpdater::activate`] for this update, once it is downloaded.
    pub fn activate(&self) -> Result<()> {
        self.app.ota_updater().activate(&self.version)
    }

    /// [`OtaUpdater::install`] for this update, once it is downloaded.
    pub fn install(&self) -> Result<()> {
        self.app.ota_updater().install(&self.version)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pages_of_the_bundle_on_trial_confirm_it() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(may_confirm(Some(&Some(v("1.2.1"))), &v("1.2.1")));
        // nothing recorded (no page load seen for that webview): nothing to go by
        assert!(!may_confirm(None, &v("1.2.1")));
        // a page still showing the previous bundle or the built-in frontend
        assert!(!may_confirm(Some(&Some(v("1.2.0"))), &v("1.2.1")));
        assert!(!may_confirm(Some(&None), &v("1.2.1")));
    }

    #[test]
    fn app_urls_are_recognized() {
        for url in [
            "tauri://localhost/",
            "tauri://localhost/settings/index.html",
            "http://tauri.localhost/",
            "https://tauri.localhost/about.html",
        ] {
            assert!(is_app_url(&url.parse().unwrap()), "{url}");
        }
        for url in [
            "https://example.com/",
            "http://localhost:1420/",
            "tauri://example.com/",
            "about:blank",
        ] {
            assert!(!is_app_url(&url.parse().unwrap()), "{url}");
        }
    }
}
