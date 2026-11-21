// Windows test binaries that link the Tauri runtime crash at startup with
// STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) unless an application manifest is
// embedded, so these runtime-backed tests only run on Linux and macOS.
#![cfg(not(windows))]

use std::{
    borrow::Cow,
    collections::HashMap,
    io::{BufRead, BufReader, Cursor, Write},
    net::TcpListener,
    path::Path,
    sync::{Arc, Mutex},
};

use base64::Engine;
use semver::Version;
use tauri::{
    App, Assets,
    async_runtime::block_on,
    test::{MockRuntime, mock_builder, mock_context},
    utils::{
        assets::{AssetKey, AssetsIter, CspHash},
        config::AppDirectoriesOverride,
    },
};
use tauri_plugin_ffp_ota_updater::{
    Builder, Error, OtaUpdaterExt, RequestOptions, default_compatibility,
};

/// Stands in for the frontend embedded in the binary.
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

fn b64(value: impl AsRef<[u8]>) -> String {
    base64::engine::general_purpose::STANDARD.encode(value)
}

/// A gzipped tarball with an `index.html` containing `index`, laid out like `tar -C dist .`.
fn archive(index: &str) -> Vec<u8> {
    archive_of(&[("./index.html", index), ("./assets/app.js", "js")])
}

fn archive_of(entries: &[(&str, &str)]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for &(path, contents) in entries {
        // written verbatim: `append_data` would normalize the leading `./` away, but the
        // `tar` command line tool keeps it
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, contents.as_bytes()).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

/// Signs like `tauri signer sign --app-version <version>` does.
fn sign(key: &minisign::KeyPair, data: &[u8], version: Option<&str>) -> String {
    let mut comment = String::from("timestamp:0\tfile:frontend.tar.gz");
    if let Some(version) = version {
        comment.push_str(&format!("\tversion:{version}"));
    }
    let signature = minisign::sign(
        Some(&key.pk),
        &key.sk,
        Cursor::new(data),
        Some(&comment),
        Some("signature from tauri secret key"),
    )
    .unwrap();
    b64(signature.to_string())
}

type Routes = Arc<Mutex<HashMap<&'static str, Vec<u8>>>>;

/// Serves `routes` by the last path segment and records every requested path, followed by
/// `#` and the value of the `x-channel` header if one was sent.
fn serve(routes: Routes, requests: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut line = String::new();
            let mut channel = None;
            while reader.read_line(&mut line).unwrap() > 2 {
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("x-channel:") {
                    channel = Some(value.trim().to_string());
                }
                line.clear();
            }
            let path = request_line
                .split(' ')
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let body = routes
                .lock()
                .unwrap()
                .get(path.rsplit('/').next().unwrap())
                .cloned();
            requests.lock().unwrap().push(match channel {
                Some(channel) => format!("{path}#{channel}"),
                None => path,
            });
            let (status, body) = match body {
                Some(body) => ("200 OK", body),
                None => ("404 Not Found", Vec::new()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    port
}

fn launch(data_dir: &Path, binary_version: &str, pubkey: &str, port: u16) -> App<MockRuntime> {
    launch_with(
        data_dir,
        binary_version,
        pubkey,
        port,
        serde_json::json!({}),
    )
}

/// `overrides` are extra keys for the plugin configuration.
fn launch_with(
    data_dir: &Path,
    binary_version: &str,
    pubkey: &str,
    port: u16,
    overrides: serde_json::Value,
) -> App<MockRuntime> {
    launch_built(
        data_dir,
        binary_version,
        pubkey,
        port,
        overrides,
        Builder::new(),
    )
}

fn launch_built(
    data_dir: &Path,
    binary_version: &str,
    pubkey: &str,
    port: u16,
    overrides: serde_json::Value,
    builder: Builder,
) -> App<MockRuntime> {
    let mut context = mock_context(BuiltIn);
    context.package_info_mut().version = binary_version.parse().unwrap();
    context.config_mut().app.app_directories_override =
        Some(AppDirectoriesOverride::Root(data_dir.into()));
    let mut config = serde_json::json!({
        "endpoints": [format!(
            "http://127.0.0.1:{port}/{{{{target}}}}/{{{{binary_version}}}}/{{{{current_version}}}}/ota.json"
        )],
        "pubkey": pubkey,
        "dangerousInsecureTransportProtocol": true,
    });
    for (key, value) in overrides.as_object().unwrap() {
        config[key] = value.clone();
    }
    context
        .config_mut()
        .plugins
        .0
        .insert("ffp-ota-updater".into(), config);
    tauri_plugin_ffp_ota_updater::install(&mut context);
    mock_builder()
        .plugin(builder.build())
        .build(context)
        .expect("failed to build app with plugin")
}

fn served_index(app: &App<MockRuntime>) -> String {
    served(app, "index.html")
}

/// What the webview gets for `path`, going through Tauri's own lookup rules.
fn served(app: &App<MockRuntime>, path: &str) -> String {
    let asset = app
        .asset_resolver()
        .get(path.into())
        .unwrap_or_else(|| panic!("{path} is not served"));
    String::from_utf8(asset.bytes().to_vec()).unwrap()
}

#[test]
fn default_compatibility_only_accepts_newer_patches() {
    let v = |s: &str| Version::parse(s).unwrap();
    assert!(default_compatibility(&v("1.2.0"), &v("1.2.1")));
    assert!(!default_compatibility(&v("1.2.1"), &v("1.2.1")));
    assert!(!default_compatibility(&v("1.2.1"), &v("1.2.0")));
    assert!(!default_compatibility(&v("1.2.0"), &v("1.3.0")));
    assert!(!default_compatibility(&v("1.2.0"), &v("2.2.1")));
}

// One scenario rather than several tests: the active bundle is process-wide state.
#[test]
fn frontend_update_lifecycle() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");

    let routes = Routes::default();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let port = serve(routes.clone(), requests.clone());
    let publish = |version: &str, file: &'static str, signature: &str| {
        let manifest = serde_json::json!({
            "version": version,
            "notes": "notes",
            "pub_date": "2026-01-01T00:00:00Z",
            "url": format!("http://127.0.0.1:{port}/{file}"),
            "signature": signature,
        });
        routes
            .lock()
            .unwrap()
            .insert("ota.json", manifest.to_string().into_bytes());
    };

    let v121 = archive("ota 1.2.1");
    let v121_signature = sign(&key, &v121, Some("1.2.1"));
    routes.lock().unwrap().insert("1.2.1.tar.gz", v121.clone());
    let unversioned = archive("ota 1.2.2");
    let unversioned_signature = sign(&key, &unversioned, None);
    routes
        .lock()
        .unwrap()
        .insert("unversioned.tar.gz", unversioned);
    routes
        .lock()
        .unwrap()
        .insert("corrupt.tar.gz", archive("tampered"));

    // --- first launch: nothing downloaded, the built-in frontend is served
    let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
    let updater = app.ota_updater();
    assert_eq!(served_index(&app), "built-in");
    assert_eq!(updater.current_version().to_string(), "1.2.0");
    let status = updater.status();
    assert_eq!(status.binary_version.to_string(), "1.2.0");
    assert!(status.confirmed && status.staged.is_none() && status.rejected.is_empty());

    // an endpoint that cannot be reached is told apart from one that has no usable manifest
    let check_at = |url: String| {
        block_on(updater.check_with(RequestOptions::new().endpoints(vec![url.parse().unwrap()])))
    };
    let unreachable = check_at("http://127.0.0.1:1/ota.json".into())
        .err()
        .unwrap();
    assert!(matches!(unreachable, Error::Reqwest(_)), "{unreachable}");
    assert_eq!(
        serde_json::to_value(&unreachable).unwrap()["kind"],
        "network"
    );
    let missing = check_at(format!("http://127.0.0.1:{port}/nothing.json"))
        .err()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&missing).unwrap(),
        serde_json::json!({ "kind": "releaseNotFound", "message": missing.to_string() })
    );

    // a minor bump is a full update, not ours
    publish("1.3.0", "1.2.1.tar.gz", &v121_signature);
    assert!(block_on(updater.check()).unwrap().is_none());

    // bundles that fail verification are rejected and never stored
    let download = |version: &str, file: &'static str, signature: &str| {
        publish(version, file, signature);
        let update = block_on(updater.check()).unwrap().expect("no update");
        block_on(update.download(|_, _| {}, || {}))
    };
    assert!(matches!(
        download("1.2.2", "1.2.1.tar.gz", &v121_signature),
        Err(Error::SignedVersionMismatch { .. })
    ));
    assert!(matches!(
        download("1.2.2", "unversioned.tar.gz", &unversioned_signature),
        Err(Error::MissingSignedVersion)
    ));
    assert!(matches!(
        download("1.2.1", "corrupt.tar.gz", &v121_signature),
        Err(Error::InvalidSignature(_))
    ));
    // ...as are archives that are ambiguous about what they contain
    for (file, entries) in [
        (
            "twice.tar.gz",
            &[("./index.html", "a"), ("index.html", "b")][..],
        ),
        ("climbs.tar.gz", &[("./index.html", "a"), ("../x", "b")]),
        ("absolute.tar.gz", &[("./index.html", "a"), ("/x", "b")]),
        ("no-index.tar.gz", &[("./app.js", "a")]),
    ] {
        let bad = archive_of(entries);
        let signature = sign(&key, &bad, Some("1.2.1"));
        routes.lock().unwrap().insert(file, bad);
        assert!(
            matches!(
                download("1.2.1", file, &signature),
                Err(Error::InvalidBundle(_))
            ),
            "{file} was accepted"
        );
    }
    assert_eq!(std::fs::read_dir(&store).map(|d| d.count()).unwrap_or(0), 0);

    // a patch bump is a frontend update
    publish("v1.2.1", "1.2.1.tar.gz", &v121_signature);
    let update = block_on(updater.check()).unwrap().expect("no update");
    assert_eq!(update.version.to_string(), "1.2.1");
    assert_eq!(update.current_version.to_string(), "1.2.0");
    assert_eq!(update.body.as_deref(), Some("notes"));
    assert!(
        requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .ends_with("/1.2.0/1.2.0/ota.json")
    );

    // installing before downloading fails
    assert!(matches!(update.install(), Err(Error::NotDownloaded(_))));

    // download only stages the bundle...
    let mut downloaded = 0;
    let mut finished = false;
    block_on(update.download(|chunk, _| downloaded += chunk, || finished = true)).unwrap();
    assert_eq!(downloaded, v121.len());
    assert!(finished);
    assert!(!update.downloaded);
    assert_eq!(served_index(&app), "built-in");
    // which shows without asking the server
    let status = updater.status();
    assert_eq!(status.staged.unwrap().to_string(), "1.2.1");
    assert_eq!(status.current_version.to_string(), "1.2.0");

    // ...and a staged bundle is not fetched a second time, as happens when every page of a
    // multi-page app checks for updates
    let fetches = |file: &str| {
        let requests = requests.lock().unwrap();
        requests.iter().filter(|path| path.ends_with(file)).count()
    };
    let before = fetches("1.2.1.tar.gz");
    let again = block_on(updater.check()).unwrap().expect("no update");
    assert!(again.downloaded);
    let (mut chunks, mut finished) = (0, false);
    block_on(again.download(|_, _| chunks += 1, || finished = true)).unwrap();
    assert!(finished && chunks == 0);
    assert_eq!(fetches("1.2.1.tar.gz"), before);
    // unless what is stored is not what the release announces
    std::fs::write(store.join("1.2.1.tar.gz"), archive("damaged on disk")).unwrap();
    block_on(again.download(|_, _| {}, || {})).unwrap();
    assert_eq!(fetches("1.2.1.tar.gz"), before + 1);
    assert_eq!(std::fs::read(store.join("1.2.1.tar.gz")).unwrap(), v121);

    // ...activate swaps it in, without reloading anything
    update.activate().unwrap();
    assert_eq!(served_index(&app), "ota 1.2.1");
    assert!(app.asset_resolver().get("assets/app.js".into()).is_some());
    assert_eq!(updater.current_version().to_string(), "1.2.1");
    assert!(block_on(updater.check()).unwrap().is_none());
    assert!(
        requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .ends_with("/1.2.0/1.2.1/ota.json")
    );
    let status = updater.status();
    assert!(!status.confirmed && status.staged.is_none());
    // the frontend reports that it works, which ends the trial
    updater.notify_ready().unwrap();
    assert!(updater.status().confirmed);
    drop(app);

    // --- next launch: the newest valid bundle is picked up; a newer but tampered one and an
    // older one are discarded
    std::fs::write(store.join("1.2.9.tar.gz"), archive("evil")).unwrap();
    std::fs::write(store.join("1.2.9.tar.gz.sig"), &v121_signature).unwrap();
    std::fs::copy(store.join("1.2.1.tar.gz"), store.join("1.2.0-rc.1.tar.gz")).unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
    assert_eq!(served_index(&app), "ota 1.2.1");
    assert!(!store.join("1.2.9.tar.gz").exists());
    assert!(!store.join("1.2.9.tar.gz.sig").exists());
    assert!(!store.join("1.2.0-rc.1.tar.gz").exists());
    assert!(store.join("1.2.1.tar.gz").exists());

    // --- a bundle that is shown but never confirmed is rolled back once its attempts are used
    // up
    let stage = |app: &App<MockRuntime>, version: &'static str, file: &'static str| {
        let bundle = archive(&format!("ota {version}"));
        let signature = sign(&key, &bundle, Some(version));
        routes.lock().unwrap().insert(file, bundle);
        publish(version, file, &signature);
        let update = block_on(app.ota_updater().check())
            .unwrap()
            .expect("no update");
        block_on(update.download(|_, _| {}, || {})).unwrap();
        update
    };
    stage(&app, "1.2.2", "1.2.2.tar.gz");
    std::fs::write(store.join("1.2.5.tar.gz.part"), "interrupted").unwrap();
    std::fs::write(store.join("1.2.6.tar.gz.sig"), "archive never written").unwrap();
    drop(app);

    // launches that end before a page was loaded from it are not attempts, however many
    let state = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(store.join("state.json")).unwrap()).unwrap()
    };
    for _ in 0..3 {
        let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
        assert_eq!(app.ota_updater().current_version().to_string(), "1.2.2");
        // and a frontend that was not shown cannot be vouched for
        app.ota_updater().notify_ready().unwrap();
        assert_eq!(state()["confirmed"], "1.2.1");
        assert!(state()["trial"].is_null());
    }

    let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
    assert_eq!(served_index(&app), "ota 1.2.2");
    // loading more pages in the same run is the same attempt
    assert_eq!(served_index(&app), "ota 1.2.2");
    assert_eq!(state()["trial"], "1.2.2");
    assert_eq!(state()["attempts"], 1);
    // the confirmed bundle is kept to fall back on
    assert!(store.join("1.2.1.tar.gz").exists());
    assert!(!store.join("1.2.5.tar.gz.part").exists());
    assert!(!store.join("1.2.6.tar.gz.sig").exists());
    assert!(store.join("1.2.2.tar.gz.sig").exists());
    drop(app); // without notify_ready

    // once could be an app that was closed while starting: it gets a second attempt
    let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
    assert_eq!(served_index(&app), "ota 1.2.2");
    assert_eq!(state()["attempts"], 2);
    drop(app);

    let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
    let updater = app.ota_updater();
    assert_eq!(served_index(&app), "ota 1.2.1");
    assert_eq!(state()["rejected"], serde_json::json!(["1.2.2"]));
    assert!(!store.join("1.2.2.tar.gz").exists());
    assert_eq!(updater.status().rejected, ["1.2.2".parse().unwrap()]);
    // the rejected version is not offered again, the next one is
    assert!(block_on(updater.check()).unwrap().is_none());

    // --- a confirmed trial replaces the fallback
    stage(&app, "1.2.3", "1.2.3.tar.gz").install().unwrap();
    assert_eq!(served_index(&app), "ota 1.2.3");
    assert!(store.join("1.2.1.tar.gz").exists());
    updater.notify_ready().unwrap();
    assert!(!store.join("1.2.1.tar.gz").exists());

    // --- rollback from Rust abandons the served bundle, confirmed or not
    stage(&app, "1.2.4", "1.2.4.tar.gz").install().unwrap();
    assert_eq!(served_index(&app), "ota 1.2.4");
    updater.rollback().unwrap();
    assert_eq!(served_index(&app), "ota 1.2.3");
    assert!(block_on(updater.check()).unwrap().is_none());
    updater.rollback().unwrap();
    assert_eq!(served_index(&app), "built-in");
    drop(app);

    // rejections survive a restart, and a stored copy of a rejected version is not used
    std::fs::write(
        store.join("1.2.4.tar.gz"),
        routes.lock().unwrap()["1.2.4.tar.gz"].clone(),
    )
    .unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
    assert_eq!(served_index(&app), "built-in");
    assert!(!store.join("1.2.4.tar.gz").exists());
    assert!(block_on(app.ota_updater().check()).unwrap().is_none());
    drop(app);

    // --- after a full update the old bundle no longer applies and is removed
    let app = launch(data_dir.path(), "1.3.0", &pubkey, port);
    assert_eq!(served_index(&app), "built-in");

    // --- reset goes back to the built-in frontend
    let v131 = archive("ota 1.3.1");
    routes.lock().unwrap().insert("1.3.1.tar.gz", v131.clone());
    publish("1.3.1", "1.3.1.tar.gz", &sign(&key, &v131, Some("1.3.1")));
    let updater = app.ota_updater();
    let update = block_on(updater.check()).unwrap().expect("no update");
    block_on(update.download_and_install(|_, _| {}, || {})).unwrap();
    assert_eq!(served_index(&app), "ota 1.3.1");
    updater.reset().unwrap();
    assert_eq!(served_index(&app), "built-in");
    assert!(!store.join("1.3.1.tar.gz").exists());

    drop(app);
    oversized_bundles_are_refused();
    multi_page_bundles_are_served_page_by_page();
    unconfirmed_bundles_time_out();
    trial_attempts_are_configurable();
    builder_overrides_the_configuration();
    bundles_are_staged_applied_and_discarded_without_a_check();
}

/// The store on its own: the app brings the bundles, and no endpoint is ever asked.
fn bundles_are_staged_applied_and_discarded_without_a_check() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let v = |version: &str| Version::parse(version).unwrap();
    let bundle = |version: &str| {
        let archive = archive(&format!("own {version}"));
        let signature = sign(&key, &archive, Some(version));
        (archive, signature)
    };
    // port 1: nothing listens there
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();

    // staging verifies like a download does
    let (v121, v121_signature) = bundle("1.2.1");
    assert!(matches!(
        updater.stage(&v121, &v121_signature, &v("1.2.2")),
        Err(Error::SignedVersionMismatch { .. })
    ));
    assert!(matches!(
        updater.stage(&archive("tampered"), &v121_signature, &v("1.2.1")),
        Err(Error::InvalidSignature(_))
    ));
    let (v130, v130_signature) = bundle("1.3.0");
    assert!(matches!(
        updater.stage(&v130, &v130_signature, &v("1.3.0")),
        Err(Error::Incompatible(_))
    ));
    assert!(!store.exists());
    assert!(matches!(
        updater.install(&v("1.2.1")),
        Err(Error::NotDownloaded(_))
    ));

    updater.stage(&v121, &v121_signature, &v("1.2.1")).unwrap();
    assert_eq!(updater.status().staged, Some(v("1.2.1")));
    assert_eq!(served_index(&app), "built-in");

    // a staged bundle can be dropped again, which is not a rejection
    updater.discard(&v("1.2.1")).unwrap();
    assert_eq!(updater.status().staged, None);
    assert!(!store.join("1.2.1.tar.gz").exists());
    assert!(!store.join("1.2.1.tar.gz.sig").exists());
    updater.discard(&v("1.2.1")).unwrap();
    drop(app);
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();
    assert_eq!(served_index(&app), "built-in");

    // ...and is applied by its version alone
    updater.stage(&v121, &v121_signature, &v("1.2.1")).unwrap();
    updater.install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    updater.notify_ready().unwrap();

    // what is in use stays: the bundle being served and the confirmed one behind a trial
    assert!(matches!(updater.discard(&v("1.2.1")), Err(Error::InUse(_))));
    let (v122, v122_signature) = bundle("1.2.2");
    updater.stage(&v122, &v122_signature, &v("1.2.2")).unwrap();
    updater.activate(&v("1.2.2")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.2");
    for version in ["1.2.1", "1.2.2"] {
        assert!(matches!(updater.discard(&v(version)), Err(Error::InUse(_))));
    }
    // versions only move forward
    assert!(matches!(
        updater.stage(&v121, &v121_signature, &v("1.2.1")),
        Err(Error::NotNewer(_))
    ));
    assert!(matches!(
        updater.activate(&v("1.2.1")),
        Err(Error::NotNewer(_))
    ));
    assert_eq!(served_index(&app), "own 1.2.2");
    updater.reset().unwrap();
}

fn builder_overrides_the_configuration() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let bundle = archive("beta");
    let routes = Routes::default();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let port = serve(routes.clone(), requests.clone());
    let manifest = serde_json::json!({
        "version": "1.4.0",
        "url": format!("http://127.0.0.1:{port}/bundle.tar.gz"),
        "signature": sign(&key, &bundle, Some("1.4.0")),
    });
    routes.lock().unwrap().extend([
        ("beta.json", manifest.to_string().into_bytes()),
        ("bundle.tar.gz", bundle),
    ]);

    let builder = Builder::new()
        .endpoints(vec![
            format!("http://127.0.0.1:{port}/beta.json")
                .parse()
                .unwrap(),
        ])
        .header("x-channel", "beta")
        .unwrap()
        .header("authorization", "Bearer default")
        .unwrap()
        .timeout(std::time::Duration::from_secs(5))
        .no_proxy()
        .configure_client(|client| client.user_agent("configured"))
        // any newer version of the same major
        .compatibility(|binary, bundle| bundle.major == binary.major && bundle > binary);
    let app = launch_built(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        port,
        serde_json::json!({}),
        builder,
    );
    let updater = app.ota_updater();

    // the builder's endpoint, headers and rule are used, for the download as well
    let update = block_on(updater.check()).unwrap().expect("no update");
    assert_eq!(update.version.to_string(), "1.4.0");
    block_on(update.download_and_install(|_, _| {}, || {})).unwrap();
    assert_eq!(served_index(&app), "beta");
    assert_eq!(
        *requests.lock().unwrap(),
        ["/beta.json#beta", "/bundle.tar.gz#beta"]
    );

    // a header given to one check replaces the builder's of that name
    let options = RequestOptions::new()
        .header("x-channel", "nightly")
        .unwrap();
    assert!(block_on(updater.check_with(options)).unwrap().is_none());
    assert_eq!(
        requests.lock().unwrap().last().unwrap(),
        "/beta.json#nightly"
    );
    updater.reset().unwrap();
}

fn trial_attempts_are_configurable() {
    let overrides = serde_json::json!({ "trialAttempts": 1 });
    let (app, relaunch) = published(&[("./index.html", "strict")], "1.2.1", overrides);
    let update = block_on(app.ota_updater().check())
        .unwrap()
        .expect("no update");
    block_on(update.download(|_, _| {}, || {})).unwrap();
    drop(app);
    assert_eq!(served_index(&relaunch()), "strict");
    assert_eq!(served_index(&relaunch()), "built-in");

    // a trial that goes well ends the counting: later runs do not have to confirm again
    let (app, relaunch) = published(&[("./index.html", "fine")], "1.2.1", serde_json::json!({}));
    let update = block_on(app.ota_updater().check())
        .unwrap()
        .expect("no update");
    block_on(update.download_and_install(|_, _| {}, || {})).unwrap();
    assert_eq!(served_index(&app), "fine");
    app.ota_updater().notify_ready().unwrap();
    drop(app);
    for _ in 0..3 {
        assert_eq!(served_index(&relaunch()), "fine");
    }
}

/// A test server with one published release, and an app on binary 1.2.0 that sees it. The
/// closure launches that app again.
fn published(
    entries: &[(&str, &str)],
    version: &str,
    overrides: serde_json::Value,
) -> (App<MockRuntime>, impl Fn() -> App<MockRuntime>) {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let bundle = archive_of(entries);
    let routes = Routes::default();
    let port = serve(routes.clone(), Default::default());
    let manifest = serde_json::json!({
        "version": version,
        "url": format!("http://127.0.0.1:{port}/bundle.tar.gz"),
        "signature": sign(&key, &bundle, Some(version)),
    });
    routes.lock().unwrap().extend([
        ("ota.json", manifest.to_string().into_bytes()),
        ("bundle.tar.gz", bundle),
    ]);
    let launch = move || launch_with(data_dir.path(), "1.2.0", &pubkey, port, overrides.clone());
    (launch(), launch)
}

/// The layout Vite writes for several HTML entry points.
fn multi_page_bundles_are_served_page_by_page() {
    let (app, _) = published(
        &[
            ("./index.html", "home"),
            ("./about.html", "about"),
            ("./settings/index.html", "settings"),
            ("./assets/settings-C0ffee.js", "settings js"),
        ],
        "1.2.1",
        serde_json::json!({}),
    );
    let update = block_on(app.ota_updater().check())
        .unwrap()
        .expect("no update");
    block_on(update.download_and_install(|_, _| {}, || {})).unwrap();

    for (path, content) in [
        ("index.html", "home"),
        ("about.html", "about"),
        ("about", "about"),
        ("settings/index.html", "settings"),
        ("settings/", "settings"),
        ("settings", "settings"),
        ("assets/settings-C0ffee.js", "settings js"),
        // Tauri answers a path it cannot find with the index page, also for a bundle: a page
        // or chunk missing from it is not an error the webview sees
        ("removed.html", "home"),
        ("assets/settings-0ldHash.js", "home"),
    ] {
        assert_eq!(served(&app, path), content, "{path}");
    }
    let js = app
        .asset_resolver()
        .get("assets/settings-C0ffee.js".into())
        .unwrap();
    assert_eq!(js.mime_type(), "text/javascript");
    app.ota_updater().reset().unwrap();
}

fn unconfirmed_bundles_time_out() {
    let wait = || std::thread::sleep(std::time::Duration::from_millis(1500));
    let overrides = serde_json::json!({ "readyTimeout": 300 });

    // confirmed in time: kept
    let (app, _) = published(&[("./index.html", "good")], "1.2.1", overrides.clone());
    let updater = app.ota_updater();
    let update = block_on(updater.check()).unwrap().expect("no update");
    block_on(update.download_and_install(|_, _| {}, || {})).unwrap();
    assert_eq!(served_index(&app), "good");
    updater.notify_ready().unwrap();
    wait();
    assert_eq!(served_index(&app), "good");
    updater.reset().unwrap();
    drop(app);

    // never confirmed: rolled back while the app is running, and not offered again
    let (app, _) = published(&[("./index.html", "broken")], "1.2.1", overrides.clone());
    let updater = app.ota_updater();
    let update = block_on(updater.check()).unwrap().expect("no update");
    block_on(update.download(|_, _| {}, || {})).unwrap();
    update.activate().unwrap();
    // the clock starts with the first page loaded from the bundle, not before
    wait();
    assert_eq!(updater.current_version().to_string(), "1.2.1");
    assert_eq!(served_index(&app), "broken");
    wait();
    assert_eq!(served_index(&app), "built-in");
    assert_eq!(updater.current_version().to_string(), "1.2.0");
    assert!(block_on(updater.check()).unwrap().is_none());
    drop(app);

    // the same goes for a bundle picked up at launch
    let (app, relaunch) = published(&[("./index.html", "broken")], "1.2.2", overrides);
    let update = block_on(app.ota_updater().check())
        .unwrap()
        .expect("no update");
    block_on(update.download(|_, _| {}, || {})).unwrap();
    drop(app);
    let app = relaunch();
    assert_eq!(served_index(&app), "broken");
    wait();
    assert_eq!(served_index(&app), "built-in");
    drop(app);
    // ...which stays rejected
    assert_eq!(served_index(&relaunch()), "built-in");
}

fn oversized_bundles_are_refused() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let bundle = archive("ota 1.2.1");
    let manifest = serde_json::json!({
        "version": "1.2.1",
        "url": "http://127.0.0.1:1/unused",
        "signature": sign(&key, &bundle, Some("1.2.1")),
    });
    let routes = Routes::default();
    let port = serve(routes.clone(), Default::default());
    let manifest = manifest
        .to_string()
        .replace(":1/unused", &format!(":{port}/bundle.tar.gz"));
    routes.lock().unwrap().extend([
        ("ota.json", manifest.into_bytes()),
        ("bundle.tar.gz", bundle.clone()),
    ]);

    let app = launch_with(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        port,
        serde_json::json!({ "maxBundleSize": bundle.len() as u64 - 1 }),
    );
    let update = block_on(app.ota_updater().check())
        .unwrap()
        .expect("no update");
    assert!(matches!(
        block_on(update.download(|_, _| {}, || {})),
        Err(Error::TooLarge("bundle", _))
    ));
    assert!(!data_dir.path().join("ffp-ota-updater").exists());
}
