const COMMANDS: &[&str] = &[
    "check",
    "download",
    "install",
    "download_and_install",
    "current_version",
    "reset",
    "notify_ready",
    "status",
    "rollback",
    "discard",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
