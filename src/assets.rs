//! The [`Assets`] provider that serves the active OTA bundle instead of the embedded frontend.

use std::{
    borrow::Cow,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use tauri::{
    App, Assets, Context, Runtime,
    utils::assets::{AssetKey, AssetsIter, CspHash},
};

use crate::bundle::Bundle;

// The asset provider has to be installed on the `Context` before the app (and so the plugin)
// exists, so the two halves share the active bundle through process-wide state.
static ACTIVE: RwLock<Option<Arc<Bundle>>> = RwLock::new(None);
static INSTALLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn active() -> Option<Arc<Bundle>> {
    ACTIVE.read().unwrap().clone()
}

pub(crate) fn set_active(bundle: Option<Bundle>) {
    *ACTIVE.write().unwrap() = bundle.map(Arc::new);
}

pub(crate) fn is_installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

struct OtaAssets<R: Runtime> {
    embedded: Box<dyn Assets<R>>,
}

impl<R: Runtime> Assets<R> for OtaAssets<R> {
    fn setup(&self, app: &App<R>) {
        self.embedded.setup(app);
    }

    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        match active() {
            // A bundle replaces the frontend as a whole: files it lacks are not looked up in
            // the embedded assets, which would mix two versions.
            Some(bundle) => bundle.files.get(key.as_ref()).cloned().map(Cow::Owned),
            None => self.embedded.get(key),
        }
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
        match active() {
            Some(bundle) => Box::new(
                bundle
                    .files
                    .iter()
                    .map(|(k, v)| (Cow::Owned(k.clone()), Cow::Owned(v.clone())))
                    .collect::<Vec<_>>()
                    .into_iter(),
            ),
            None => self.embedded.iter(),
        }
    }

    fn csp_hashes(&self, html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        match active() {
            // The embedded hashes were computed at build time for the embedded HTML and do not
            // describe the bundle's inline scripts.
            Some(_) => Box::new(std::iter::empty()),
            None => self.embedded.csp_hashes(html_path),
        }
    }
}

/// Lets the plugin replace the embedded frontend with a downloaded bundle.
///
/// Call this on the context before passing it to [`tauri::Builder::run`]:
///
/// ```ignore
/// let mut context = tauri::generate_context!();
/// tauri_plugin_ffp_ota_updater::install(&mut context);
/// tauri::Builder::default()
///     .plugin(tauri_plugin_ffp_ota_updater::init())
///     .run(context)
///     .expect("error while running tauri application");
/// ```
pub fn install<R: Runtime>(context: &mut Context<R>) {
    // `set_assets` needs the replacement up front and hands back the original, so park a
    // placeholder for the instant in between.
    struct Placeholder;
    impl<R: Runtime> Assets<R> for Placeholder {
        fn get(&self, _: &AssetKey) -> Option<Cow<'_, [u8]>> {
            None
        }
        fn iter(&self) -> Box<AssetsIter<'_>> {
            Box::new(std::iter::empty())
        }
        fn csp_hashes(&self, _: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
            Box::new(std::iter::empty())
        }
    }

    let embedded = context.set_assets(Box::new(Placeholder));
    context.set_assets(Box::new(OtaAssets { embedded }));
    INSTALLED.store(true, Ordering::Relaxed);
}
