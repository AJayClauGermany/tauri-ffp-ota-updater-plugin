//! The [`Assets`] provider that serves the active OTA bundle instead of the embedded frontend.

use std::{
    borrow::Cow,
    sync::{
        Arc, Mutex, RwLock,
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
static WRAPPED: AtomicBool = AtomicBool::new(false);
/// Runs once, when the active bundle serves its next HTML document.
static ON_DOCUMENT: Mutex<Option<DocumentHook>> = Mutex::new(None);

pub(crate) type DocumentHook = Box<dyn FnOnce() + Send>;

pub(crate) fn active() -> Option<Arc<Bundle>> {
    ACTIVE.read().unwrap().clone()
}

/// Switches the frontend being served. `on_document` is called once, right before the first
/// page is loaded from `bundle`, and replaces a hook that has not run yet. It may switch again.
pub(crate) fn set_active(bundle: Option<Bundle>, on_document: Option<DocumentHook>) {
    let mut active = ACTIVE.write().unwrap();
    *ON_DOCUMENT.lock().unwrap() = on_document;
    *active = bundle.map(Arc::new);
}

/// Whether [`wrap_assets`] was called, without which no bundle can be served.
pub(crate) fn is_wrapped() -> bool {
    WRAPPED.load(Ordering::Relaxed)
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
            Some(bundle) => {
                let file = bundle.files.get(key.as_ref())?;
                if key.as_ref().ends_with(".html") {
                    // taken out first: the hook must not run under the lock
                    let hook = ON_DOCUMENT.lock().unwrap().take();
                    if let Some(hook) = hook {
                        hook();
                        // the hook may have decided against serving this bundle
                        return self.get(key);
                    }
                }
                Some(Cow::Owned(file.clone()))
            }
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
            // The embedded script hashes describe the embedded files, so the bundle brings its
            // own (see `csp::prepare`). The embedded style hashes are kept: they are not of the
            // frontend's files but of what Tauri itself adds, the isolation pattern's iframe.
            Some(bundle) => {
                let own = bundle
                    .csp
                    .scripts
                    .iter()
                    .chain(
                        bundle
                            .csp
                            .inline_scripts
                            .get(html_path.as_ref())
                            .into_iter()
                            .flatten(),
                    )
                    .map(|hash| CspHash::Script(hash));
                Box::new(
                    self.embedded
                        .csp_hashes(html_path)
                        .filter(|hash| matches!(hash, CspHash::Style(_)))
                        .chain(own)
                        .collect::<Vec<_>>()
                        .into_iter(),
                )
            }
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
/// tauri_plugin_ffp_ota_updater::wrap_assets(&mut context);
/// tauri::Builder::default()
///     .plugin(tauri_plugin_ffp_ota_updater::init())
///     .run(context)
///     .expect("error while running tauri application");
/// ```
pub fn wrap_assets<R: Runtime>(context: &mut Context<R>) {
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
    WRAPPED.store(true, Ordering::Relaxed);
}
