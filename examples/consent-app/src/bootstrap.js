// Stands in for a splash screen, a tray popup or a hidden window that sets
// things up: a page that loads fine even when the main one is broken.
//
// It therefore does not vouch for a downloaded frontend. It never calls
// `notify_ready`, and its window is not permitted to: the capability that
// allows it, `capabilities/main.json`, is for the main window only.
const { invoke } = window.__TAURI__.core;

const status = await invoke("plugin:ffp-ota-updater|status");
document.getElementById("version").textContent = status.currentVersion;
