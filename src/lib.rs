//! Over-the-air updates for the frontend of a Tauri app.
//!
//! Where `tauri-plugin-updater` replaces the whole application, this plugin only replaces the
//! web assets: it downloads a signed tarball of the frontend build and serves it in place of
//! the assets embedded in the binary. Its configuration, manifest, signing and API follow the
//! updater plugin's.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use http::{HeaderMap, HeaderName, HeaderValue};
use semver::Version;
use tauri::{
    Manager, Runtime,
    plugin::{Builder as PluginBuilder, TauriPlugin},
};
use url::Url;

mod assets;
mod bundle;
mod commands;
mod config;
mod error;
mod updater;

pub use assets::install;
pub use bundle::{Compatibility, default_compatibility};
pub use config::Config;
pub use error::{Error, Result};
/// The HTTP client, for [`Builder::configure_client`].
pub use reqwest;
pub use updater::{OtaUpdater, RequestOptions, Status, Update};

/// Extension to [`tauri::App`], [`tauri::AppHandle`] and windows to access the OTA updater.
pub trait OtaUpdaterExt<R: Runtime> {
    fn ota_updater(&self) -> &OtaUpdater<R>;
}

impl<R: Runtime, T: Manager<R>> OtaUpdaterExt<R> for T {
    fn ota_updater(&self) -> &OtaUpdater<R> {
        self.state::<OtaUpdater<R>>().inner()
    }
}

/// Initializes the plugin with the configuration from `tauri.conf.json`.
pub fn init<R: Runtime>() -> TauriPlugin<R, Option<Config>> {
    Builder::new().build()
}

/// Builds the plugin, overriding parts of the configuration from Rust.
#[derive(Default)]
pub struct Builder {
    pubkey: Option<String>,
    endpoints: Option<Vec<Url>>,
    compatibility: Option<Compatibility>,
    client: updater::ClientOptions,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the `pubkey` from the configuration file.
    pub fn pubkey(mut self, pubkey: impl Into<String>) -> Self {
        self.pubkey = Some(pubkey.into());
        self
    }

    /// Overrides the `endpoints` from the configuration file, for example to pick a release
    /// channel at startup. To change them while the app is running, pass
    /// [`RequestOptions::endpoints`] to [`OtaUpdater::check_with`].
    pub fn endpoints(mut self, endpoints: Vec<Url>) -> Self {
        self.endpoints = Some(endpoints);
        self
    }

    /// Adds a header sent with every manifest request and bundle download. A header of the
    /// same name given to a single check replaces it.
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
        self.client.headers.insert(key?, value?);
        Ok(self)
    }

    /// Replaces the headers sent with every manifest request and bundle download.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.client.headers = headers;
        self
    }

    /// Deadline for each request, instead of 30 seconds for the manifest and 30 minutes for
    /// the bundle.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.client.timeout = Some(timeout);
        self
    }

    /// Sends every request through this proxy instead of the system's.
    pub fn proxy(mut self, proxy: Url) -> Self {
        self.client.proxy = Some(proxy);
        self
    }

    /// Ignores the system's proxy settings.
    pub fn no_proxy(mut self) -> Self {
        self.client.no_proxy = true;
        self
    }

    /// Adjusts the HTTP client used for the manifest and the bundle, for example to trust a
    /// private certificate authority. Runs last, so it can override what the plugin and its
    /// configuration set up, including the `https`-only rule.
    pub fn configure_client(
        mut self,
        configure: impl Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder + Send + Sync + 'static,
    ) -> Self {
        self.client.configure = Some(Arc::new(configure));
        self
    }

    /// Replaces the rule deciding which bundle versions are frontend-only updates for a given
    /// binary. Receives `(binary_version, bundle_version)`.
    ///
    /// The rule is applied to releases found by `check` and whenever a bundle is stored or
    /// loaded. At startup a bundle that fails it (typically after a full update) is deleted.
    /// Defaults to [`default_compatibility`].
    ///
    /// This is not the updater plugin's `version_comparator`: it says what a binary can run,
    /// not what counts as an update. A bundle is only ever offered if it is also newer than
    /// the frontend being served.
    pub fn compatibility(
        mut self,
        compatibility: impl Fn(&Version, &Version) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.compatibility = Some(Arc::new(compatibility));
        self
    }

    pub fn build<R: Runtime>(self) -> TauriPlugin<R, Option<Config>> {
        PluginBuilder::<R, Option<Config>>::new("ffp-ota-updater")
            .invoke_handler(tauri::generate_handler![
                commands::check,
                commands::download,
                commands::install,
                commands::download_and_install,
                commands::current_version,
                commands::reset,
                commands::notify_ready,
                commands::status,
                commands::rollback,
            ])
            .setup(move |app, api| {
                let mut config = api.config().clone().unwrap_or_default();
                if let Some(pubkey) = self.pubkey {
                    config.pubkey = pubkey;
                }
                if let Some(endpoints) = self.endpoints {
                    config.endpoints = endpoints;
                }
                config.validate()?;

                let binary_version = app.package_info().version.clone();
                let store = bundle::Store {
                    dir: app.path().app_local_data_dir()?.join("ffp-ota-updater"),
                    pubkey: config.pubkey.clone(),
                    require_signed_version: config.require_signed_version,
                    limits: bundle::Limits {
                        archive: config.max_bundle_size,
                        unpacked: config.max_unpacked_size,
                    },
                    rollback: config.rollback,
                    trial_attempts: config.trial_attempts,
                    shown: Mutex::new(None),
                    binary: binary_version.clone(),
                    compatibility: self
                        .compatibility
                        .unwrap_or_else(|| Arc::new(default_compatibility)),
                    lock: Mutex::new(()),
                };

                // Plugins are set up before any window is created, so the bundle chosen here is
                // what the first page load gets.
                if !assets::is_installed() {
                    log::warn!(
                        "tauri_plugin_ffp_ota_updater::install(&mut context) was not called; downloaded frontends will not be served"
                    );
                }
                let updater = OtaUpdater {
                    app: app.clone(),
                    config,
                    client: self.client,
                    store,
                    binary_version,
                    pending: Mutex::new(None),
                    downloading: Default::default(),
                };
                updater.serve(updater.store.select(None));
                app.manage(updater);
                Ok(())
            })
            .build()
    }
}
