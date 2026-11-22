#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut context = tauri::generate_context!();
    // lets the plugin serve a downloaded frontend instead of the embedded one
    tauri_plugin_ffp_ota_updater::wrap_assets(&mut context);

    tauri::Builder::default()
        .plugin(tauri_plugin_ffp_ota_updater::init())
        .run(context)
        .expect("error while running tauri application");
}
