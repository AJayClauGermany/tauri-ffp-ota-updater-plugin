use tauri_plugin_ffp_ota_updater::OtaUpdaterExt;

#[cfg(feature = "e2e")]
mod e2e;

/// Checks for a frontend update and downloads it. Returns its version, or `None` when there is
/// nothing new.
///
/// Nothing is switched here, now or at the next launch: the app is configured with
/// `requireApproval`, so the bundle waits until the user agrees (`approve` or `install`, which
/// the main window calls). Doing this in Rust keeps the request out of the webview's hands. An
/// app that looks for updates on a timer calls the same two lines from a task of its own.
#[tauri::command]
async fn fetch_update(
    app: tauri::AppHandle,
) -> tauri_plugin_ffp_ota_updater::Result<Option<String>> {
    let Some(update) = app.ota_updater().check().await? else {
        return Ok(None);
    };
    update.download(|_, _| {}, || {}).await?;
    Ok(Some(update.version.to_string()))
}

pub fn run() {
    let mut context = tauri::generate_context!();
    // lets the plugin serve a downloaded frontend instead of the embedded one
    tauri_plugin_ffp_ota_updater::wrap_assets(&mut context);

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_ffp_ota_updater::init())
        .invoke_handler(tauri::generate_handler![fetch_update]);
    #[cfg(feature = "e2e")]
    let builder = builder.plugin(e2e::init());
    builder
        .run(context)
        .expect("error while running tauri application");
}
