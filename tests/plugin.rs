// On Windows these need the application manifest `build.rs` embeds in test binaries.

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
    App, Assets, Context,
    async_runtime::block_on,
    test::{MockRuntime, mock_builder, mock_context},
    utils::{
        assets::{AssetKey, AssetsIter, CspHash},
        config::{AppDirectoriesOverride, FrontendDist},
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

/// A route whose content starts with this is answered after a second and a half, without it.
const SLOW: &[u8] = b"<slow> ";

/// Serves `routes` by the last path segment and records every requested path, followed by
/// `#` and the value of the `x-channel` header if one was sent, and by `!referer` if the
/// request names where it came from. A route whose content is `-> ` and a URL redirects there.
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
            let mut referer = false;
            while reader.read_line(&mut line).unwrap() > 2 {
                let header = line.to_ascii_lowercase();
                if let Some(value) = header.strip_prefix("x-channel:") {
                    channel = Some(value.trim().to_string());
                }
                referer |= header.starts_with("referer:");
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
                .get(
                    path.split('?')
                        .next()
                        .and_then(|path| path.rsplit('/').next())
                        .unwrap(),
                )
                .cloned();
            let mut request = path;
            if let Some(channel) = channel {
                request.push_str(&format!("#{channel}"));
            }
            if referer {
                request.push_str("!referer");
            }
            requests.lock().unwrap().push(request);
            let body = body.map(|body| match body.strip_prefix(SLOW) {
                Some(rest) => {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    rest.to_vec()
                }
                None => body,
            });
            let (status, body) = match body {
                Some(body) => match body.strip_prefix(b"-> ") {
                    Some(location) => (
                        format!(
                            "302 Found\r\nLocation: {}",
                            String::from_utf8_lossy(location)
                        ),
                        Vec::new(),
                    ),
                    None => ("200 OK".into(), body),
                },
                None => ("404 Not Found".into(), Vec::new()),
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
    try_launch(
        data_dir,
        binary_version,
        pubkey,
        port,
        overrides,
        builder,
        |_| {},
    )
    .expect("failed to build app with plugin")
}

/// [`launch_built`], with `tweak` applied to the app's context, and failing instead of panicking.
fn try_launch(
    data_dir: &Path,
    binary_version: &str,
    pubkey: &str,
    port: u16,
    overrides: serde_json::Value,
    builder: Builder,
    tweak: impl FnOnce(&mut Context<MockRuntime>),
) -> tauri::Result<App<MockRuntime>> {
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
    tweak(&mut context);
    tauri_plugin_ffp_ota_updater::wrap_assets(&mut context);
    mock_builder().plugin(builder.build()).build(context)
}

/// The files in the store, apart from the lock file.
fn stored_files(store: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(store)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".lock")
        .collect();
    files.sort();
    files
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
fn default_compatibility_accepts_the_same_minor_version() {
    // that a bundle is newer than the binary is enforced by the plugin, not the rule
    let v = |s: &str| Version::parse(s).unwrap();
    assert!(default_compatibility(&v("1.2.0"), &v("1.2.1")));
    assert!(default_compatibility(&v("1.2.0"), &v("1.2.0+1")));
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
    // what is absent is left out, as the JavaScript types declare it
    assert!(
        serde_json::to_value(&status)
            .unwrap()
            .get("staged")
            .is_none()
    );

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
    assert!(stored_files(&store).is_empty());

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
    rollbacks_return_to_the_confirmed_bundle();
    unreadable_bundles_are_kept();
    keys_are_required_and_checked();
    development_mode_does_not_serve_bundles();
    build_metadata_is_ignored_unless_asked_for();
    bundles_get_the_csp_processing_of_built_in_frontends();
    headers_stay_on_the_endpoints_origin();
    a_bundle_reinstalled_after_a_reset_goes_on_trial_again();
    failed_deletions_are_reported();
    archive_metadata_counts_towards_the_unpacked_size();
    remote_frontends_do_not_serve_bundles();
    concurrent_installs_never_go_backwards();
    a_version_names_one_bundle();
    an_unreadable_state_leaves_the_store_alone();
    rollbacks_that_are_not_recorded_are_reported();
    a_lost_fallback_is_forgotten();
    bundles_are_read_to_their_end();
    bundles_wait_for_approval();
    approval_is_optional();
    required_files_are_checked();
    frontends_are_bound_to_a_native_contract();
    unknown_configuration_keys_are_refused();
    installing_the_served_version_again_does_nothing();
    a_check_timeout_is_not_a_download_timeout();
}

/// A key pair, and a function staging `own <version>` signed with it in an app.
fn signer() -> (
    String,
    impl Fn(&App<MockRuntime>, &str, &str) -> tauri_plugin_ffp_ota_updater::Result<()>,
) {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let stage = move |app: &App<MockRuntime>, version: &str, index: &str| {
        let archive = archive_of(&[("./index.html", index), ("./assets/app.js", "js")]);
        let signature = sign(&key, &archive, Some(version));
        app.ota_updater()
            .stage(&archive, &signature, &Version::parse(version).unwrap())
    };
    (pubkey, stage)
}

fn v(version: &str) -> Version {
    Version::parse(version).unwrap()
}

/// A rollback goes back to what is known to work, and leaves a newer download for later.
fn rollbacks_return_to_the_confirmed_bundle() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();

    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    updater.install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    updater.notify_ready().unwrap();
    stage(&app, "1.2.2", "own 1.2.2").unwrap();
    updater.install(&v("1.2.2")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.2");
    // downloaded while 1.2.2 is on trial, never shown
    stage(&app, "1.2.3", "own 1.2.3").unwrap();

    updater.rollback().unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    let status = updater.status();
    assert!(status.confirmed);
    assert_eq!(status.staged, Some(v("1.2.3")));
    assert_eq!(status.rejected, vec![v("1.2.2")]);

    // abandoning the confirmed bundle goes back to the built-in frontend
    updater.rollback().unwrap();
    assert_eq!(served_index(&app), "built-in");
    assert_eq!(updater.status().staged, Some(v("1.2.3")));

    // and the bundle that was kept is tried at the next launch
    drop(app);
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    assert_eq!(served_index(&app), "own 1.2.3");
    app.ota_updater().reset().unwrap();
}

/// A bundle that cannot be read for now is skipped, not deleted.
fn unreadable_bundles_are_kept() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    drop(app);
    // reading a directory fails like a file locked by another program does
    std::fs::create_dir(store.join("1.2.2.tar.gz")).unwrap();
    std::fs::write(store.join("1.2.2.tar.gz.sig"), "sig").unwrap();

    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    assert_eq!(served_index(&app), "own 1.2.1");
    assert!(store.join("1.2.2.tar.gz").is_dir());
    assert!(store.join("1.2.2.tar.gz.sig").exists());
    // `reset` would report that it cannot delete it, see `failed_deletions_are_reported`
    std::fs::remove_dir(store.join("1.2.2.tar.gz")).unwrap();
    app.ota_updater().reset().unwrap();
}

fn keys_are_required_and_checked() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    drop(app);

    // without a key nothing can be verified, so nothing is served, and nothing is deleted
    let app = launch(data_dir.path(), "1.2.0", "", 1);
    let updater = app.ota_updater();
    assert_eq!(served_index(&app), "built-in");
    assert_eq!(stored_files(&store), ["1.2.1.tar.gz", "1.2.1.tar.gz.sig"]);
    let error = block_on(updater.check()).err().unwrap();
    assert!(matches!(error, Error::EmptyPubkey));
    assert_eq!(serde_json::to_value(&error).unwrap()["kind"], "emptyPubkey");
    assert!(matches!(stage(&app, "1.2.2", "x"), Err(Error::EmptyPubkey)));
    assert!(matches!(
        updater.install(&v("1.2.1")),
        Err(Error::EmptyPubkey)
    ));
    drop(app);

    // a key that cannot be read stops the app from starting, before it can delete anything
    let unreadable = b64("not a minisign key");
    let launched = try_launch(
        data_dir.path(),
        "1.2.0",
        &unreadable,
        1,
        serde_json::json!({}),
        Builder::new(),
        |_| {},
    );
    assert!(launched.is_err());
    assert_eq!(stored_files(&store), ["1.2.1.tar.gz", "1.2.1.tar.gz.sig"]);

    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    assert_eq!(served_index(&app), "own 1.2.1");
    app.ota_updater().reset().unwrap();
}

/// `tauri dev` shows the dev server, so the plugin does not pretend to serve a bundle.
fn development_mode_does_not_serve_bundles() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    drop(app);

    let app = try_launch(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        1,
        serde_json::json!({}),
        Builder::new(),
        |context| {
            context.config_mut().build.dev_url = Some("http://localhost:1420".parse().unwrap())
        },
    )
    .unwrap();
    let updater = app.ota_updater();
    assert_eq!(updater.current_version(), v("1.2.0"));
    assert!(updater.status().confirmed);
    let error = updater.install(&v("1.2.1")).err().unwrap();
    assert!(matches!(error, Error::CannotServe(_)));
    assert_eq!(serde_json::to_value(&error).unwrap()["kind"], "cannotServe");
    // still staged for a run that can serve it
    assert_eq!(updater.status().staged, Some(v("1.2.1")));
    assert_eq!(stored_files(&store), ["1.2.1.tar.gz", "1.2.1.tar.gz.sig"]);
}

/// By default `1.2.1+2` is the same version as `1.2.1+1`; `compareBuildMetadata` orders them.
fn build_metadata_is_ignored_unless_asked_for() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let routes = Routes::default();
    let port = serve(routes.clone(), Default::default());
    let publish = |version: &str| {
        let bundle = archive(&format!("ota {version}"));
        let manifest = serde_json::json!({
            "version": version,
            "url": format!("http://127.0.0.1:{port}/bundle.tar.gz"),
            "signature": sign(&key, &bundle, Some(version)),
        });
        routes.lock().unwrap().extend([
            ("ota.json", manifest.to_string().into_bytes()),
            ("bundle.tar.gz", bundle),
        ]);
    };
    let offered = |app: &App<MockRuntime>| {
        block_on(app.ota_updater().check())
            .unwrap()
            .map(|update| update.version.to_string())
    };
    let install = |app: &App<MockRuntime>| {
        let update = block_on(app.ota_updater().check()).unwrap().unwrap();
        block_on(update.download_and_install(|_, _| {}, || {})).unwrap();
    };

    let data_dir = tempfile::tempdir().unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
    publish("1.2.0+1");
    assert_eq!(offered(&app), None);
    assert!(matches!(
        app.ota_updater().stage(
            &archive("x"),
            &sign(&key, &archive("x"), Some("1.2.0+1")),
            &v("1.2.0+1")
        ),
        Err(Error::NotNewer(_))
    ));
    publish("1.2.1+1");
    install(&app);
    assert_eq!(served_index(&app), "ota 1.2.1+1");
    publish("1.2.1+2");
    assert_eq!(offered(&app), None);
    app.ota_updater().reset().unwrap();
    drop(app);

    let data_dir = tempfile::tempdir().unwrap();
    let overrides = serde_json::json!({ "compareBuildMetadata": true });
    let app = launch_with(data_dir.path(), "1.2.0", &pubkey, port, overrides.clone());
    publish("1.2.0+1");
    assert_eq!(offered(&app).as_deref(), Some("1.2.0+1"));
    publish("1.2.1+9");
    install(&app);
    // compared as numbers, as semantic versioning does for pre-releases
    publish("1.2.1+10");
    assert_eq!(offered(&app).as_deref(), Some("1.2.1+10"));
    install(&app);
    assert_eq!(served_index(&app), "ota 1.2.1+10");
    drop(app);
    let app = launch_with(data_dir.path(), "1.2.0", &pubkey, port, overrides);
    assert_eq!(served_index(&app), "ota 1.2.1+10");
    app.ota_updater().reset().unwrap();
}

fn sha256_source(script: &str) -> String {
    use sha2::Digest;
    format!("'sha256-{}'", b64(sha2::Sha256::digest(script.as_bytes())))
}

/// With a CSP, a bundle gets the nonces and hashes `tauri build` gives the built-in frontend.
fn bundles_get_the_csp_processing_of_built_in_frontends() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let page = "<html><head><style>body { margin: 0 }</style>\
        <script src=\"https://cdn.example.com/lib.js\"></script>\
        <script>window.inline = 1</script>\
        <script type=\"module\" src=\"/assets/app.js\"></script></head><body></body></html>";
    let bundle = archive_of(&[
        ("./index.html", page),
        ("./assets/app.js", "window.app = 1"),
    ]);
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    app.ota_updater()
        .stage(&bundle, &sign(&key, &bundle, Some("1.2.1")), &v("1.2.1"))
        .unwrap();
    drop(app);

    let app = try_launch(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        1,
        serde_json::json!({}),
        Builder::new(),
        |context| {
            context.config_mut().app.security.csp = Some(tauri::utils::config::Csp::Policy(
                "default-src 'self'".into(),
            ))
        },
    )
    .unwrap();
    let asset = app.asset_resolver().get("index.html".into()).unwrap();
    let html = String::from_utf8(asset.bytes().to_vec()).unwrap();
    let csp = asset.csp_header().unwrap().to_string();
    let directive = |name: &str| {
        csp.split(';')
            .map(str::trim)
            .find(|directive| directive.starts_with(name))
            .unwrap_or_else(|| panic!("no {name} in {csp}"))
            .to_string()
    };
    let scripts = directive("script-src");
    assert!(
        scripts.contains(&sha256_source("window.inline = 1")),
        "{csp}"
    );
    assert!(scripts.contains(&sha256_source("window.app = 1")), "{csp}");
    // the style element and the script from another origin carry a fresh nonce
    let nonces = html.matches("nonce=\"").count();
    assert_eq!(nonces, 2, "{html}");
    assert!(!html.contains("__TAURI_"), "{html}");
    assert!(directive("style-src").contains("'nonce-"), "{csp}");
    assert!(scripts.contains("'nonce-"), "{csp}");
    app.ota_updater().reset().unwrap();
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
    assert!(stored_files(&store).is_empty());
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

    // A deadline belongs to one trial: after a reset, a later trial of the same version gets
    // its own, and is not ended by what is left of the first.
    let (app, _) = published(
        &[("./index.html", "again")],
        "1.2.1",
        serde_json::json!({ "readyTimeout": 2000 }),
    );
    let updater = app.ota_updater();
    let install = || {
        let update = block_on(updater.check()).unwrap().expect("no update");
        block_on(update.download_and_install(|_, _| {}, || {})).unwrap();
        assert_eq!(served_index(&app), "again");
    };
    install();
    wait();
    updater.reset().unwrap();
    install();
    // the first deadline has passed, the second has not
    wait();
    assert_eq!(served_index(&app), "again");
    wait();
    assert_eq!(served_index(&app), "built-in");
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
    assert!(stored_files(&data_dir.path().join("ffp-ota-updater")).is_empty());
}

/// Headers are for the endpoint the app was configured with. Neither the manifest, which is not
/// signed, nor a redirect can send them to a host of its choosing.
fn headers_stay_on_the_endpoints_origin() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let bundle = archive("elsewhere");
    let signature = sign(&key, &bundle, Some("1.2.1"));
    let own_routes = Routes::default();
    let own_requests = Arc::new(Mutex::new(Vec::new()));
    let own_port = serve(own_routes.clone(), own_requests.clone());
    let other_routes = Routes::default();
    let other_requests = Arc::new(Mutex::new(Vec::new()));
    // another port is another origin
    let other_port = serve(other_routes.clone(), other_requests.clone());
    other_routes
        .lock()
        .unwrap()
        .insert("bundle.tar.gz", bundle.clone());
    let manifest = |bundle_port: u16| {
        serde_json::json!({
            "version": "1.2.1",
            "url": format!("http://127.0.0.1:{bundle_port}/bundle.tar.gz?token=1"),
            "signature": signature,
        })
        .to_string()
        .into_bytes()
    };
    let requests =
        |requests: &Arc<Mutex<Vec<String>>>| std::mem::take(&mut *requests.lock().unwrap());

    let data_dir = tempfile::tempdir().unwrap();
    let builder = Builder::new().header("x-channel", "secret").unwrap();
    let app = launch_built(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        own_port,
        serde_json::json!({}),
        builder,
    );
    let updater = app.ota_updater();
    let download = || {
        let update = block_on(updater.check()).unwrap().expect("no update");
        block_on(update.download(|_, _| {}, || {})).unwrap();
        assert_eq!(served_index(&app), "built-in");
        updater.reset().unwrap();
    };

    // a manifest that names a bundle elsewhere
    own_routes
        .lock()
        .unwrap()
        .insert("ota.json", manifest(other_port));
    download();
    assert!(requests(&own_requests)[0].ends_with("/ota.json#secret"));
    assert_eq!(requests(&other_requests), ["/bundle.tar.gz?token=1"]);

    // a bundle on the endpoint's origin gets the headers, but a redirect does not take them
    // along, nor does it tell the other host which URL, token included, it came from
    own_routes.lock().unwrap().extend([
        ("ota.json", manifest(own_port)),
        (
            "bundle.tar.gz",
            format!("-> http://127.0.0.1:{other_port}/bundle.tar.gz").into_bytes(),
        ),
    ]);
    download();
    let own = requests(&own_requests);
    assert!(own[0].ends_with("/ota.json#secret"), "{own:?}");
    assert_eq!(own[1], "/bundle.tar.gz?token=1#secret");
    assert_eq!(requests(&other_requests), ["/bundle.tar.gz"]);

    // the same goes for an endpoint that redirects to a manifest elsewhere, and for the bundle
    // that manifest names on the endpoint's origin, which gets them again
    other_routes
        .lock()
        .unwrap()
        .insert("moved.json", manifest(own_port));
    own_routes.lock().unwrap().extend([
        (
            "ota.json",
            // relative to the URL that redirects
            b"-> moved.json".to_vec(),
        ),
        (
            "moved.json",
            format!("-> http://127.0.0.1:{other_port}/moved.json").into_bytes(),
        ),
        ("bundle.tar.gz", bundle.clone()),
    ]);
    download();
    let own = requests(&own_requests);
    assert!(own[0].ends_with("/ota.json#secret"), "{own:?}");
    assert!(own[1].ends_with("/moved.json#secret"), "{own:?}");
    assert_eq!(own[2], "/bundle.tar.gz?token=1#secret");
    assert_eq!(requests(&other_requests), ["/moved.json"]);

    // a redirect that never ends is given up on
    own_routes
        .lock()
        .unwrap()
        .insert("ota.json", b"-> ota.json".to_vec());
    let lost = block_on(updater.check()).err().unwrap();
    assert!(matches!(lost, Error::ReleaseNotFound(_)), "{lost}");
    assert_eq!(requests(&own_requests).len(), 11);
    own_routes.lock().unwrap().extend([
        ("ota.json", manifest(own_port)),
        ("bundle.tar.gz", b"-> bundle.tar.gz".to_vec()),
    ]);
    let update = block_on(updater.check()).unwrap().expect("no update");
    let lost = block_on(update.download(|_, _| {}, || {})).err().unwrap();
    assert!(matches!(lost, Error::TooManyRedirects(_)), "{lost}");
    assert_eq!(serde_json::to_value(&lost).unwrap()["kind"], "network");
    updater.reset().unwrap();
}

/// A reset ends the trial of the bundle being shown; the same version installed again in the
/// same run starts a new one, which `notifyReady` can end.
fn a_bundle_reinstalled_after_a_reset_goes_on_trial_again() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();

    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    updater.install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    updater.reset().unwrap();
    assert_eq!(served_index(&app), "built-in");

    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    updater.install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    assert!(!updater.status().confirmed);
    updater.notify_ready().unwrap();
    assert!(updater.status().confirmed);
    updater.reset().unwrap();
}

/// `discard` and `reset` must not report a bundle as gone that would be served again.
fn failed_deletions_are_reported() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    updater.install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    // a directory cannot be deleted as a file, much like a file another program holds open
    std::fs::create_dir(store.join("1.2.2.tar.gz")).unwrap();
    std::fs::write(store.join("1.2.2.tar.gz").join("x"), "x").unwrap();

    assert!(matches!(updater.discard(&v("1.2.2")), Err(Error::Io(_))));
    // the rest is reset, and this run goes back to the built-in frontend all the same
    assert!(matches!(updater.reset(), Err(Error::Io(_))));
    assert_eq!(served_index(&app), "built-in");
    assert!(!store.join("1.2.1.tar.gz").exists());
    std::fs::remove_dir_all(store.join("1.2.2.tar.gz")).unwrap();
    updater.reset().unwrap();
}

/// PAX headers and GNU long names are read into memory before the entry they describe, so they
/// count towards what an archive may decompress to.
fn archive_metadata_counts_towards_the_unpacked_size() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    let mut pax = tar::Header::new_ustar();
    pax.set_entry_type(tar::EntryType::XHeader);
    // more than the 20,000 entries a bundle may have could need for their headers
    let metadata = vec![b'0'; 48 << 20];
    pax.set_size(metadata.len() as u64);
    pax.set_cksum();
    tar.append(&pax, metadata.as_slice()).unwrap();
    let mut index = tar::Header::new_ustar();
    index.set_size(5);
    index.set_cksum();
    tar.append_data(&mut index, "index.html", b"index".as_slice())
        .unwrap();
    let archive = tar.into_inner().unwrap().finish().unwrap();
    let signature = sign(&key, &archive, Some("1.2.1"));

    let app = launch_with(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        1,
        serde_json::json!({ "maxUnpackedSize": 1 << 20 }),
    );
    assert!(matches!(
        app.ota_updater().stage(&archive, &signature, &v("1.2.1")),
        Err(Error::TooLarge("unpacked bundle", _))
    ));
}

/// With a URL as `frontendDist` the webview loads that site, never the bundle.
fn remote_frontends_do_not_serve_bundles() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    drop(app);

    let app = try_launch(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        1,
        serde_json::json!({}),
        Builder::new(),
        |context| {
            context.config_mut().build.frontend_dist = Some(FrontendDist::Url(
                "https://app.example.com".parse().unwrap(),
            ))
        },
    )
    .unwrap();
    let updater = app.ota_updater();
    assert_eq!(updater.current_version(), v("1.2.0"));
    assert!(matches!(
        updater.install(&v("1.2.1")),
        Err(Error::CannotServe(_))
    ));
    assert_eq!(updater.current_version(), v("1.2.0"));
    assert_eq!(updater.status().staged, Some(v("1.2.1")));
    drop(app);
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    app.ota_updater().reset().unwrap();
}

/// Whatever order two installs finish in, the newer frontend is the one served.
fn concurrent_installs_never_go_backwards() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let app = try_launch(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        1,
        serde_json::json!({}),
        Builder::new(),
        |context| {
            context.config_mut().app.security.csp = Some(tauri::utils::config::Csp::Policy(
                "default-src 'self'".into(),
            ))
        },
    )
    .unwrap();
    let updater = app.ota_updater();
    let stage = |version: &str, entries: &[(String, String)]| {
        let entries: Vec<(&str, &str)> = entries
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_str()))
            .collect();
        let archive = archive_of(&entries);
        updater
            .stage(&archive, &sign(&key, &archive, Some(version)), &v(version))
            .unwrap();
    };
    // With a CSP, every page is parsed before a bundle is served: many pages make that step
    // slow for the older bundle, so the install of the newer one starts and ends meanwhile.
    let page = format!(
        "<html><body>{}</body></html>",
        "<div><script>window.x = 1</script></div>".repeat(200)
    );
    let mut older: Vec<(String, String)> = (0..100)
        .map(|i| (format!("./page{i}.html"), page.clone()))
        .collect();
    older.push(("./index.html".into(), "own 1.2.1".into()));
    stage("1.2.1", &older);
    stage("1.2.2", &[("./index.html".into(), "own 1.2.2".into())]);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let result = updater.activate(&v("1.2.1"));
            // should the newer one go first after all, the older one is refused
            assert!(
                matches!(result, Ok(()) | Err(Error::NotNewer(_))),
                "{result:?}"
            );
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        scope.spawn(|| updater.activate(&v("1.2.2")).unwrap());
    });
    assert_eq!(updater.current_version(), v("1.2.2"));
    assert!(served_index(&app).contains("own 1.2.2"));
    updater.reset().unwrap();
}

/// What is on record for a version is about the bundle that was shown under it.
fn a_version_names_one_bundle() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();

    let store = data_dir.path().join("ffp-ota-updater");
    let other = |app: &App<MockRuntime>| {
        // the same contents again are fine, others are not
        stage(app, "1.2.1", "own 1.2.1").unwrap();
        assert!(matches!(
            stage(app, "1.2.1", "other 1.2.1"),
            Err(Error::InUse(_))
        ));
    };

    // To replace a bundle that was never shown, it has to be discarded first: left in place
    // it could be in memory already, and be confirmed in the place of its replacement.
    stage(&app, "1.2.1", "first 1.2.1").unwrap();
    assert!(matches!(
        stage(&app, "1.2.1", "own 1.2.1"),
        Err(Error::InUse(_))
    ));
    updater.discard(&v("1.2.1")).unwrap();
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    other(&app);
    // activated, but no page was loaded from it yet
    updater.activate(&v("1.2.1")).unwrap();
    other(&app);
    // on trial, then confirmed
    assert_eq!(served_index(&app), "own 1.2.1");
    other(&app);
    updater.notify_ready().unwrap();
    other(&app);
    drop(app);
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    assert_eq!(served_index(&app), "own 1.2.1");
    assert!(app.ota_updater().status().confirmed);

    // two versions that would be one file where the file system ignores case
    stage(&app, "1.2.2-RC", "own 1.2.2-RC").unwrap();
    assert!(matches!(
        stage(&app, "1.2.2-rc", "own 1.2.2-rc"),
        Err(Error::InUse(_))
    ));
    // an archive that is no bundle does not hold on to its version
    std::fs::write(store.join("1.2.3.tar.gz"), "damaged").unwrap();
    stage(&app, "1.2.3", "own 1.2.3").unwrap();
    app.ota_updater().install(&v("1.2.3")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.3");
    app.ota_updater().reset().unwrap();
}

/// A state that cannot be read is not an empty one: nothing is served, deleted or offered on
/// the strength of a guess, and all is as it was once it can be read again.
fn an_unreadable_state_leaves_the_store_alone() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    updater.install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    updater.notify_ready().unwrap();
    // a newer one that fails its only attempt, and one staged after it
    stage(&app, "1.2.2", "own 1.2.2").unwrap();
    updater.install(&v("1.2.2")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.2");
    updater.rollback().unwrap();
    stage(&app, "1.2.3", "own 1.2.3").unwrap();
    drop(app);
    let files = stored_files(&store);
    assert_eq!(
        files,
        [
            "1.2.1.tar.gz",
            "1.2.1.tar.gz.sig",
            "1.2.3.tar.gz",
            "1.2.3.tar.gz.sig",
            "state.json"
        ]
    );

    // reading a directory fails like a file another program has locked does
    std::fs::rename(store.join("state.json"), store.join("state.kept")).unwrap();
    std::fs::create_dir(store.join("state.json")).unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();
    assert_eq!(served_index(&app), "built-in");
    // with an empty state 1.2.3 would have been served and the fallback 1.2.1 deleted
    assert!(store.join("1.2.1.tar.gz").exists() && store.join("1.2.3.tar.gz").exists());
    // and the rejected 1.2.2 accepted again
    assert!(matches!(
        stage(&app, "1.2.2", "own 1.2.2"),
        Err(Error::Io(_))
    ));
    assert!(matches!(updater.install(&v("1.2.3")), Err(Error::Io(_))));
    assert_eq!(served_index(&app), "built-in");
    drop(app);

    std::fs::remove_dir(store.join("state.json")).unwrap();
    std::fs::rename(store.join("state.kept"), store.join("state.json")).unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    assert_eq!(served_index(&app), "own 1.2.3");
    assert_eq!(app.ota_updater().status().rejected, [v("1.2.2")]);
    assert!(store.join("1.2.1.tar.gz").exists());
    drop(app);

    // A state that is read but makes no sense is lost for good. The bundles are still verified
    // and the newest goes on trial; the file is written anew.
    std::fs::write(store.join("state.json"), "{ not json").unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    assert_eq!(served_index(&app), "own 1.2.3");
    assert!(!app.ota_updater().status().confirmed);
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["trial"], "1.2.3");
    app.ota_updater().reset().unwrap();
}

/// A rejection that could not be recorded is reported, and nothing is deleted for it.
fn rollbacks_that_are_not_recorded_are_reported() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    updater.install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");

    // the state is written to this file first
    std::fs::create_dir(store.join("state.json.part")).unwrap();
    assert!(matches!(updater.rollback(), Err(Error::Io(_))));
    // this run has gone back all the same
    assert_eq!(served_index(&app), "built-in");
    assert_eq!(updater.current_version(), v("1.2.0"));
    assert!(store.join("1.2.1.tar.gz").exists());
    assert!(updater.status().rejected.is_empty());

    // the record is the old one, and what the next launch goes by
    std::fs::remove_dir(store.join("state.json.part")).unwrap();
    drop(app);
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();
    assert_eq!(served_index(&app), "own 1.2.1");
    updater.rollback().unwrap();
    assert_eq!(updater.status().rejected, [v("1.2.1")]);
    assert!(!store.join("1.2.1.tar.gz").exists());

    // A bundle whose trial cannot be put on record is not served, to any of the pages that are
    // requested at once.
    stage(&app, "1.2.2", "own 1.2.2").unwrap();
    updater.activate(&v("1.2.2")).unwrap();
    std::fs::create_dir(store.join("state.json.part")).unwrap();
    let handle = app.handle();
    std::thread::scope(|scope| {
        let pages: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| handle.asset_resolver().get("index.html".into())))
            .collect();
        for page in pages {
            assert_eq!(page.join().unwrap().unwrap().bytes(), b"built-in");
        }
    });
    assert_eq!(updater.current_version(), v("1.2.0"));
    std::fs::remove_dir(store.join("state.json.part")).unwrap();
    updater.reset().unwrap();
}

/// A confirmed bundle whose archive is gone is no fallback, and does not keep its version from
/// being stored again.
fn a_lost_fallback_is_forgotten() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    app.ota_updater().install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    app.ota_updater().notify_ready().unwrap();
    drop(app);

    std::fs::write(store.join("1.2.1.tar.gz"), "damaged").unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    assert_eq!(served_index(&app), "built-in");
    assert!(!store.join("1.2.1.tar.gz").exists());
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    app.ota_updater().install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");
    // on trial like any bundle that was not confirmed
    assert!(!app.ota_updater().status().confirmed);
    app.ota_updater().reset().unwrap();
}

/// A bundle is a whole gzip stream: one that is cut short or damaged after the last file is
/// refused, although every file could be unpacked.
fn bundles_are_read_to_their_end() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let stage = |archive: &[u8]| {
        app.ota_updater()
            .stage(archive, &sign(&key, archive, Some("1.2.1")), &v("1.2.1"))
    };

    let whole = archive("own 1.2.1");
    // the checksum the stream ends with
    let mut damaged = whole.clone();
    let checksum = damaged.len() - 8;
    damaged[checksum] ^= 0xff;
    assert!(matches!(stage(&damaged), Err(Error::InvalidBundle(_))));
    assert!(matches!(
        stage(&whole[..whole.len() - 4]),
        Err(Error::InvalidBundle(_))
    ));
    stage(&whole).unwrap();
    app.ota_updater().reset().unwrap();
}

/// With `requireApproval` a bundle that was downloaded is not served, however often the app is
/// started, until the app says so.
fn bundles_wait_for_approval() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let overrides = serde_json::json!({ "requireApproval": true });
    let launch = |binary: &str| launch_with(data_dir.path(), binary, &pubkey, 1, overrides.clone());
    let state = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(store.join("state.json")).unwrap()).unwrap()
    };

    let app = launch("1.2.0");
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    let status = app.ota_updater().status();
    assert_eq!(status.staged, Some(v("1.2.1")));
    assert_eq!(status.approved, None);
    // what is absent is left out, as the JavaScript types declare it
    assert!(
        serde_json::to_value(&status)
            .unwrap()
            .get("approved")
            .is_none()
    );
    drop(app);

    // it stays where it is across restarts
    for _ in 0..2 {
        let app = launch("1.2.0");
        assert_eq!(served_index(&app), "built-in");
        assert_eq!(app.ota_updater().current_version(), v("1.2.0"));
        assert_eq!(app.ota_updater().status().staged, Some(v("1.2.1")));
        assert_eq!(stored_files(&store), ["1.2.1.tar.gz", "1.2.1.tar.gz.sig"]);
    }

    // approving it changes nothing in this run, and can be taken back
    let app = launch("1.2.0");
    let updater = app.ota_updater();
    assert!(matches!(
        updater.approve(&v("1.2.2")),
        Err(Error::NotDownloaded(_))
    ));
    updater.approve(&v("1.2.1")).unwrap();
    assert_eq!(updater.status().approved, Some(v("1.2.1")));
    assert_eq!(
        serde_json::to_value(updater.status()).unwrap()["approved"],
        "1.2.1"
    );
    assert_eq!(served_index(&app), "built-in");
    updater.unapprove(&v("1.2.1")).unwrap();
    assert_eq!(updater.status().approved, None);
    drop(app);
    let app = launch("1.2.0");
    assert_eq!(served_index(&app), "built-in");
    // another version's approval is not this one's to take back
    app.ota_updater().approve(&v("1.2.1")).unwrap();
    app.ota_updater().unapprove(&v("1.2.2")).unwrap();
    assert_eq!(app.ota_updater().status().approved, Some(v("1.2.1")));
    drop(app);

    // the next launch serves it, on trial like any bundle
    let app = launch("1.2.0");
    let updater = app.ota_updater();
    assert_eq!(updater.current_version(), v("1.2.1"));
    assert_eq!(served_index(&app), "own 1.2.1");
    assert!(!updater.status().confirmed);
    updater.notify_ready().unwrap();
    assert!(updater.status().confirmed);
    assert!(state()["approved"].is_null());

    // of the bundles that wait, a launch keeps the newest, which is the one `staged` names
    stage(&app, "1.2.2", "own 1.2.2").unwrap();
    stage(&app, "1.2.3", "own 1.2.3").unwrap();
    // only what is ahead of the frontend being served can be approved
    assert!(matches!(
        updater.approve(&v("1.2.0")),
        Err(Error::NotNewer(_))
    ));
    drop(app);
    let app = launch("1.2.0");
    let updater = app.ota_updater();
    assert_eq!(served_index(&app), "own 1.2.1");
    assert_eq!(updater.status().staged, Some(v("1.2.3")));
    assert_eq!(
        stored_files(&store),
        [
            "1.2.1.tar.gz",
            "1.2.1.tar.gz.sig",
            "1.2.3.tar.gz",
            "1.2.3.tar.gz.sig",
            "state.json"
        ]
    );

    // Switching while the app runs is the app's choice too. It is remembered even if the app
    // is closed before a page was loaded from the bundle, when its trial has not begun.
    updater.activate(&v("1.2.3")).unwrap();
    assert_eq!(state()["approved"], "1.2.3");
    assert!(state()["trial"].is_null());
    drop(app);
    let app = launch("1.2.0");
    assert_eq!(app.ota_updater().current_version(), v("1.2.3"));
    // ...and the trial is what it always is: two runs that show it without a confirmation
    assert_eq!(served_index(&app), "own 1.2.3");
    drop(app);
    assert_eq!(served_index(&launch("1.2.0")), "own 1.2.3");
    let app = launch("1.2.0");
    let updater = app.ota_updater();
    assert_eq!(served_index(&app), "own 1.2.1");
    assert_eq!(updater.status().rejected, [v("1.2.3")]);
    assert!(state()["approved"].is_null());
    assert!(matches!(
        updater.approve(&v("1.2.3")),
        Err(Error::Rejected(_))
    ));

    // a rollback leaves a bundle that waits where it is, and discarding one withdraws its
    // approval
    stage(&app, "1.2.4", "own 1.2.4").unwrap();
    updater.approve(&v("1.2.4")).unwrap();
    updater.rollback().unwrap();
    assert_eq!(served_index(&app), "built-in");
    assert_eq!(updater.status().approved, Some(v("1.2.4")));
    updater.discard(&v("1.2.4")).unwrap();
    assert_eq!(updater.status().approved, None);
    assert!(state()["approved"].is_null());

    // A record that is lost takes the approvals with it: the bundles wait again, instead of
    // the newest going on trial as it does without `requireApproval`.
    stage(&app, "1.2.5", "own 1.2.5").unwrap();
    updater.approve(&v("1.2.5")).unwrap();
    drop(app);
    std::fs::write(store.join("state.json"), "{ not json").unwrap();
    let app = launch("1.2.0");
    assert_eq!(served_index(&app), "built-in");
    let status = app.ota_updater().status();
    assert_eq!(status.staged, Some(v("1.2.5")));
    assert_eq!(status.approved, None);
    drop(app);

    // a bundle that waits does not outlive the binary it was made for
    let app = launch("1.3.0");
    assert_eq!(served_index(&app), "built-in");
    assert_eq!(stored_files(&store), ["state.json"]);
    drop(app);

    // a download is held back like a staged bundle, and `install` needs no approval
    let (app, relaunch) = published(&[("./index.html", "held")], "1.2.1", overrides.clone());
    let update = block_on(app.ota_updater().check())
        .unwrap()
        .expect("no update");
    block_on(update.download(|_, _| {}, || {})).unwrap();
    drop(app);
    let app = relaunch();
    assert_eq!(served_index(&app), "built-in");
    let update = block_on(app.ota_updater().check())
        .unwrap()
        .expect("no update");
    assert!(update.downloaded);
    update.install().unwrap();
    assert_eq!(served_index(&app), "held");
    app.ota_updater().reset().unwrap();
}

/// Without `requireApproval` the next launch takes the newest stored bundle, approved or not.
fn approval_is_optional() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    app.ota_updater().approve(&v("1.2.1")).unwrap();
    assert_eq!(app.ota_updater().status().approved, Some(v("1.2.1")));
    stage(&app, "1.2.2", "own 1.2.2").unwrap();
    drop(app);

    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    assert_eq!(served_index(&app), "own 1.2.2");
    assert_eq!(app.ota_updater().status().approved, None);
    app.ota_updater().reset().unwrap();
}

/// `requiredFiles` names what a bundle must contain besides `index.html`: Tauri would answer a
/// request for a page that is missing with the index page.
fn required_files_are_checked() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let stage = |app: &App<MockRuntime>, version: &str, entries: &[(&str, &str)]| {
        let archive = archive_of(entries);
        app.ota_updater()
            .stage(&archive, &sign(&key, &archive, Some(version)), &v(version))
    };
    // with and without a leading slash
    let required = serde_json::json!({ "requiredFiles": ["settings.html", "/editor/index.html"] });

    let app = launch_with(data_dir.path(), "1.2.0", &pubkey, 1, required);
    let error = stage(
        &app,
        "1.2.1",
        &[("./index.html", "home"), ("./settings.html", "settings")],
    )
    .unwrap_err();
    assert!(matches!(error, Error::InvalidBundle(_)), "{error}");
    assert!(error.to_string().contains("editor/index.html"), "{error}");
    assert!(stored_files(&store).is_empty());
    let pages = [
        ("./index.html", "home"),
        ("./settings.html", "settings"),
        ("./editor/index.html", "editor"),
    ];
    stage(&app, "1.2.1", &pages).unwrap();
    app.ota_updater().install(&v("1.2.1")).unwrap();
    assert_eq!(served(&app, "editor"), "editor");
    app.ota_updater().notify_ready().unwrap();
    drop(app);

    // a binary that needs another page does not serve a bundle from before that
    let more = serde_json::json!({ "requiredFiles": ["settings.html", "about.html"] });
    let app = launch_with(data_dir.path(), "1.2.0", &pubkey, 1, more);
    assert_eq!(served_index(&app), "built-in");
    assert!(!store.join("1.2.1.tar.gz").exists());
    drop(app);

    // a path that cannot be in a bundle is a mistake in the configuration
    let launched = try_launch(
        data_dir.path(),
        "1.2.0",
        &pubkey,
        1,
        serde_json::json!({ "requiredFiles": ["../settings.html"] }),
        Builder::new(),
        |_| {},
    );
    assert!(launched.is_err());
}

/// A key the plugin does not know stops the app from starting: a misspelled setting would
/// otherwise be ignored, leaving its default in force. The documented aliases are known keys.
fn unknown_configuration_keys_are_refused() {
    let (pubkey, _) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let launch = |overrides: serde_json::Value| {
        try_launch(
            data_dir.path(),
            "1.2.0",
            &pubkey,
            1,
            overrides,
            Builder::new(),
            |_| {},
        )
    };
    let Err(error) = launch(serde_json::json!({ "requireAproval": true })) else {
        panic!("a misspelled key was accepted");
    };
    assert!(error.to_string().contains("requireAproval"), "{error}");
    let app = launch(serde_json::json!({
        "require-approval": true,
        "trial-attempts": 3,
        "max-bundle-size": 1 << 20,
    }))
    .unwrap();
    assert!(app.ota_updater().status().rejected.is_empty());
}

/// The recipe of the integration guide: a tag in the version's build metadata says which native
/// build or IPC contract a frontend was made for, and `Builder::compatibility` holds bundles to
/// the one compiled into the binary. Two binaries of the same version are then told apart.
fn frontends_are_bound_to_a_native_contract() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let routes = Routes::default();
    let port = serve(routes.clone(), Default::default());
    let publish = |version: &str| {
        let bundle = archive(&format!("ota {version}"));
        let manifest = serde_json::json!({
            "version": version,
            "url": format!("http://127.0.0.1:{port}/bundle.tar.gz"),
            "signature": sign(&key, &bundle, Some(version)),
        });
        routes.lock().unwrap().extend([
            ("ota.json", manifest.to_string().into_bytes()),
            ("bundle.tar.gz", bundle),
        ]);
    };
    let stage = |app: &App<MockRuntime>, version: &str| {
        let archive = archive(&format!("own {version}"));
        app.ota_updater()
            .stage(&archive, &sign(&key, &archive, Some(version)), &v(version))
    };
    let launch = |contract: &'static str| {
        let builder = Builder::new().compatibility(move |binary, bundle| {
            default_compatibility(binary, bundle) && bundle.build.as_str() == contract
        });
        let none = serde_json::json!({});
        launch_built(data_dir.path(), "1.2.0", &pubkey, port, none, builder)
    };

    let app = launch("ipc3");
    let updater = app.ota_updater();
    // a frontend made for another contract, or for none, is neither offered nor accepted
    publish("1.2.1+ipc4");
    assert!(block_on(updater.check()).unwrap().is_none());
    for other in ["1.2.1+ipc4", "1.2.1"] {
        assert!(matches!(stage(&app, other), Err(Error::Incompatible(_))));
    }
    // the one made for this binary is, with the tag covered by the signature
    publish("1.2.1+ipc3");
    let update = block_on(updater.check()).unwrap().expect("no update");
    block_on(update.download_and_install(|_, _| {}, || {})).unwrap();
    assert_eq!(served_index(&app), "ota 1.2.1+ipc3");
    updater.notify_ready().unwrap();
    assert_eq!(
        stored_files(&store),
        ["1.2.1+ipc3.tar.gz", "1.2.1+ipc3.tar.gz.sig", "state.json"]
    );
    drop(app);

    // A binary of the same version with another contract drops it, although it was confirmed,
    // and takes the frontend made for itself, which is the same version as far as order goes.
    let app = launch("ipc4");
    assert_eq!(served_index(&app), "built-in");
    assert_eq!(stored_files(&store), ["state.json"]);
    publish("1.2.1+ipc4");
    let update = block_on(app.ota_updater().check())
        .unwrap()
        .expect("no update");
    block_on(update.download_and_install(|_, _| {}, || {})).unwrap();
    assert_eq!(served_index(&app), "ota 1.2.1+ipc4");
    app.ota_updater().reset().unwrap();
}

/// Two windows, or a button pressed twice, that install the same update switch and reload once:
/// the second install finds that version served already, and does not even read its archive.
fn installing_the_served_version_again_does_nothing() {
    let (pubkey, stage) = signer();
    let data_dir = tempfile::tempdir().unwrap();
    let store = data_dir.path().join("ffp-ota-updater");
    let app = launch(data_dir.path(), "1.2.0", &pubkey, 1);
    let updater = app.ota_updater();
    stage(&app, "1.2.1", "own 1.2.1").unwrap();
    updater.install(&v("1.2.1")).unwrap();
    assert_eq!(served_index(&app), "own 1.2.1");

    std::fs::write(store.join("1.2.1.tar.gz"), "damaged").unwrap();
    updater.install(&v("1.2.1")).unwrap();
    updater.activate(&v("1.2.1")).unwrap();
    assert_eq!(updater.current_version(), v("1.2.1"));
    assert_eq!(served_index(&app), "own 1.2.1");
    updater.reset().unwrap();
}

/// As in the updater plugin, the timeout of a check is for the manifest: a bundle that takes
/// longer to arrive than the check was given is still downloaded.
fn a_check_timeout_is_not_a_download_timeout() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pubkey = b64(key.pk.to_box().unwrap().to_string());
    let data_dir = tempfile::tempdir().unwrap();
    let bundle = archive("slow 1.2.1");
    let routes = Routes::default();
    let port = serve(routes.clone(), Default::default());
    let manifest = serde_json::json!({
        "version": "1.2.1",
        "url": format!("http://127.0.0.1:{port}/bundle.tar.gz"),
        "signature": sign(&key, &bundle, Some("1.2.1")),
    });
    routes.lock().unwrap().extend([
        ("ota.json", manifest.to_string().into_bytes()),
        ("bundle.tar.gz", [SLOW, &bundle].concat()),
    ]);
    let app = launch(data_dir.path(), "1.2.0", &pubkey, port);
    let options = RequestOptions::new().timeout(std::time::Duration::from_secs(1));
    let update = block_on(app.ota_updater().check_with(options))
        .unwrap()
        .expect("no update");
    block_on(update.download(|_, _| {}, || {})).unwrap();
    assert_eq!(app.ota_updater().status().staged, Some(v("1.2.1")));
}
