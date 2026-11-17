//! Over-the-air updates for the frontend of a Tauri app.
//!
//! Where `tauri-plugin-updater` replaces the whole application, this plugin only replaces the
//! web assets: it downloads a signed tarball of the frontend build and serves it in place of
//! the assets embedded in the binary. Its configuration, manifest, signing and API follow the
//! updater plugin's.

use std::sync::{Arc, Mutex};

use semver::Version;
use tauri::{
    Manager, Runtime,
    plugin::{Builder as PluginBuilder, TauriPlugin},
};

mod assets;
mod bundle;
mod commands;
mod config;
mod error;
mod updater;

pub use assets::install;
pub use bundle::{VersionComparator, default_version_comparator};
pub use commands::DownloadEvent;
pub use config::Config;
pub use error::{Error, Result};
pub use updater::{OtaUpdater, RemoteRelease, RequestOptions, Update};

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
    comparator: Option<VersionComparator>,
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

    /// Replaces the rule deciding which bundle versions are frontend-only updates for a given
    /// binary. Receives `(binary_version, bundle_version)`.
    ///
    /// The rule is applied to releases found by `check` and to the downloaded bundles at
    /// startup, where a bundle that fails it (typically after a full update) is deleted.
    /// Defaults to [`default_version_comparator`].
    pub fn version_comparator(
        mut self,
        comparator: impl Fn(&Version, &Version) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.comparator = Some(Arc::new(comparator));
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
            ])
            .setup(move |app, api| {
                let mut config = api.config().clone().unwrap_or_default();
                if let Some(pubkey) = self.pubkey {
                    config.pubkey = pubkey;
                }
                config.validate()?;

                let store = bundle::Store {
                    dir: app.path().app_local_data_dir()?.join("ffp-ota-updater"),
                    pubkey: config.pubkey.clone(),
                    require_signed_version: config.require_signed_version,
                };
                let binary_version = app.package_info().version.clone();
                let comparator = self
                    .comparator
                    .unwrap_or_else(|| Arc::new(default_version_comparator));

                // Plugins are set up before any window is created, so the bundle chosen here is
                // what the first page load gets.
                if !assets::is_installed() {
                    log::warn!(
                        "tauri_plugin_ffp_ota_updater::install(&mut context) was not called; downloaded frontends will not be served"
                    );
                }
                assets::set_active(store.load_newest(&binary_version, &comparator));

                app.manage(OtaUpdater {
                    app: app.clone(),
                    config,
                    store,
                    binary_version,
                    comparator,
                    pending: Mutex::new(None),
                });
                Ok(())
            })
            .build()
    }
}
