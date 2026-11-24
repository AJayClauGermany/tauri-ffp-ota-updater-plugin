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
    "approve",
    "unapprove",
];

/// The application manifest `tauri-build` embeds in apps, declaring Common Controls 6.
const WINDOWS_MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();

    // A test binary that links the Tauri runtime imports from Common Controls 6, which Windows
    // only loads for executables that ask for it in a manifest; without one it exits at once
    // with STATUS_ENTRYPOINT_NOT_FOUND. Tauri works around this the same way for its own tests.
    // `-tests` limits it to this crate's integration tests: apps get their manifest from
    // `tauri-build`, and nothing here reaches them.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("tests.manifest");
        std::fs::write(&manifest, WINDOWS_MANIFEST).expect("failed to write the test manifest");
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
