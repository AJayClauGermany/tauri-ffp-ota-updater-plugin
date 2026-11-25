//! The plugin next to `tauri-plugin-updater` in one app, set up as
//! `docs/how-to/use-with-updater-plugin.md` shows ("Set up both" and the version comparator). Keep
//! the two in step: this is what keeps those snippets compiling.
//!
//! A test binary of its own, because the plugin's asset state is per process.

use std::borrow::Cow;

use semver::Version;
use tauri::{
    Assets,
    test::{MockRuntime, mock_builder, mock_context},
    utils::{
        assets::{AssetKey, AssetsIter, CspHash},
        config::AppDirectoriesOverride,
    },
};
use tauri_plugin_ffp_ota_updater::{OtaUpdaterExt, default_compatibility};
use tauri_plugin_updater::UpdaterExt;

struct BuiltIn;

impl Assets<MockRuntime> for BuiltIn {
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        (key.as_ref() == "/index.html").then_some(Cow::Borrowed(b"built-in"))
    }
    fn iter(&self) -> Box<AssetsIter<'_>> {
        Box::new(std::iter::empty())
    }
    fn csp_hashes(&self, _: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

/// The comparator of "Every release is also a full release": the updater leaves the releases
/// this plugin takes to it.
fn full_update_wanted(current: Version, release: &Version) -> bool {
    *release > current && !default_compatibility(&current, release)
}

#[test]
fn both_plugins_share_an_app_and_a_key() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        key.pk.to_box().unwrap().to_string(),
    );
    let data_dir = tempfile::tempdir().unwrap();

    let mut context = mock_context(BuiltIn);
    context.package_info_mut().version = "1.2.0".parse().unwrap();
    context.config_mut().app.app_directories_override =
        Some(AppDirectoriesOverride::Root(data_dir.path().into()));
    let plugins = &mut context.config_mut().plugins.0;
    plugins.insert(
        "updater".into(),
        serde_json::json!({
            "pubkey": pubkey,
            "endpoints": ["https://releases.example.com/latest.json"],
        }),
    );
    plugins.insert(
        "ffp-ota-updater".into(),
        serde_json::json!({
            "pubkey": pubkey,
            "endpoints": ["https://releases.example.com/ota.json"],
        }),
    );
    tauri_plugin_ffp_ota_updater::wrap_assets(&mut context);

    let app = mock_builder()
        .plugin(tauri_plugin_ffp_ota_updater::init())
        .plugin(
            tauri_plugin_updater::Builder::new()
                .default_version_comparator(|current, release| {
                    full_update_wanted(current, &release.version)
                })
                .build(),
        )
        .build(context)
        .expect("the two plugins could not be set up together");

    assert_eq!(app.ota_updater().current_version(), Version::new(1, 2, 0));
    app.updater().expect("the updater plugin is not usable");

    let v = |s: &str| Version::parse(s).unwrap();
    assert!(!full_update_wanted(v("1.2.0"), &v("1.2.1")));
    assert!(full_update_wanted(v("1.2.0"), &v("1.3.0")));
    assert!(!full_update_wanted(v("1.3.0"), &v("1.2.9")));
}
