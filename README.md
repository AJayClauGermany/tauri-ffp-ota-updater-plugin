# Tauri Plugin ffp-ota-updater

Over-the-air updates for the **frontend** of a [Tauri v2](https://v2.tauri.app)
app. Where [`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/)
downloads and installs a whole new application, this plugin downloads a signed
tarball of your web assets and serves it in place of the ones embedded in the
binary. Design tweaks and small fixes ship without a binary download or an app
restart.

It is modelled on the updater plugin and meant to sit next to it: same config
keys, same signing keys and `tauri signer` tooling, same manifest fields, same
`check()` / `download()` / `install()` API.

| | `tauri-plugin-updater` | this plugin |
| --- | --- | --- |
| Replaces | the application | the frontend assets |
| Artifact | installer / app bundle per platform | one `frontend.tar.gz` for all platforms |
| Applies | after restart | on webview reload, or on next launch |
| Default scope | any newer version | newer **patch** versions of the installed binary |

Desktop only (Windows, macOS, Linux), like the updater plugin: register it
under `#[cfg(desktop)]` in an app that also builds for Android or iOS.

## How it works

- `check()` asks your endpoint for a release manifest. By default a release is
  a frontend update only if it is a newer patch of the installed binary
  (binary `1.2.0` accepts `1.2.1`, `1.2.7`, but not `1.3.0` or `2.0.0`): minor
  and major bumps are assumed to need new Rust code and are left to the
  updater plugin. Override this with `Builder::compatibility`.
- `download()` fetches the tarball, verifies its minisign signature (and that
  it was signed for the announced version) and stores it in the app's local
  data directory. Nothing else happens until the next launch.
- `install()` switches to the downloaded bundle and reloads the webviews.
  `downloadAndInstall()` does both.
- On every launch the plugin picks the **newest** stored bundle that is still
  compatible with the binary, verifies it again and serves it. Bundles that
  fail verification, are older, or no longer apply (for instance after a full
  update to `1.3.0`) are deleted. If none is usable the built-in frontend is
  served.

Bundles are kept as the signed archive and unpacked in memory, never extracted
to disk, so what is served is always exactly what was signed. The page keeps
its origin (`tauri://localhost` / `http://tauri.localhost`), so IPC,
capabilities, `localStorage` and IndexedDB carry over.

## Setup

Add the crate and, if you use a bundler, the JS package
(`tauri-plugin-ffp-ota-updater-api`). Then register the plugin. Unlike most
plugins it needs one extra line, `install(&mut context)`, which is what allows
it to replace the embedded assets:

```rust
pub fn run() {
    let mut context = tauri::generate_context!();
    tauri_plugin_ffp_ota_updater::install(&mut context);

    tauri::Builder::default()
        .plugin(tauri_plugin_ffp_ota_updater::init())
        .run(context)
        .expect("error while running tauri application");
}
```

Configure it in `tauri.conf.json`. If you already use the updater plugin,
reuse its `pubkey`:

```json
{
  "plugins": {
    "ffp-ota-updater": {
      "endpoints": ["https://releases.example.com/ota.json"],
      "pubkey": "CONTENT OF YOUR .key.pub FILE"
    }
  }
}
```

| Key | Default | |
| --- | --- | --- |
| `endpoints` | `[]` | Manifest URLs, tried in order. Variables: `{{current_version}}` (frontend being served), `{{binary_version}}`, `{{target}}`, `{{arch}}`. |
| `pubkey` | | Minisign public key from `tauri signer generate`. |
| `dangerousInsecureTransportProtocol` | `false` | Allow non-`https` endpoints, bundle URLs and redirects. |
| `requireSignedVersion` | `true` | Reject signatures that do not record the version they were made for. Needs Tauri CLI 2.12 or newer (`tauri signer sign --app-version`). |
| `rollback` | `true` | Revert a bundle the frontend does not confirm with `notifyReady()`. See [Rollback](#rollback). |
| `trialAttempts` | `2` | Runs of the app in which a new bundle may be shown without confirming itself before it is reverted. See [Rollback](#rollback). |
| `readyTimeout` | off | Milliseconds a new bundle gets to call `notifyReady()` before it is reverted while the app is running. See [Rollback](#rollback). |
| `maxBundleSize` | 64 MiB | Largest `.tar.gz` accepted, in bytes. |
| `maxUnpackedSize` | 256 MiB | Largest total size of the files in a bundle, in bytes. They are held in memory. |

Grant the permission in a capability file:

```json
{ "permissions": ["core:default", "ffp-ota-updater:default"] }
```

`default` covers `check`, `download`, `install`, `downloadAndInstall`,
`discard`, `currentVersion`, `notifyReady` and `status`. The two calls that discard a
frontend, `rollback()` and `reset()`, are opt-in: add
`ffp-ota-updater:allow-rollback` and/or `ffp-ota-updater:allow-reset`.

### Configuring from Rust

What depends on the build, the user or the environment is set on the builder
instead of in `tauri.conf.json`:

```rust
tauri_plugin_ffp_ota_updater::Builder::new()
    .endpoints(vec!["https://releases.example.com/beta/ota.json".parse()?])
    .header("Authorization", format!("Bearer {token}"))?
    .timeout(std::time::Duration::from_secs(20))
    .proxy("http://proxy.example.com:8080".parse()?)   // or .no_proxy()
    .configure_client(|client| client.add_root_certificate(company_ca.clone()))
    .build()
```

| Method | |
| --- | --- |
| `pubkey`, `endpoints` | Override the configuration file. |
| `header`, `headers` | Sent with every manifest request and bundle download. |
| `timeout` | Deadline for each request. |
| `proxy`, `no_proxy` | Use this proxy, or none, instead of the system's. |
| `configure_client` | Last word on the `reqwest` client (re-exported as `tauri_plugin_ffp_ota_updater::reqwest`). |
| `compatibility` | Which bundle versions a binary can run; see [How it works](#how-it-works). |

To switch release channels while the app is running, give one check its own
endpoints. The headers, timeout and proxy can be set the same way, and the
download of the release that check finds uses them too:

```rust
use tauri_plugin_ffp_ota_updater::{OtaUpdaterExt, RequestOptions};

let update = app
    .ota_updater()
    .check_with(RequestOptions::new().endpoints(vec![beta_url]))
    .await?;
```

The webview can pass `headers`, `timeout` and `proxy` to `check()`, but not
endpoints.

## Publishing a frontend update

The plugin does not hook into the build. Package the output of your normal
frontend build, i.e. what `build.beforeBuildCommand` writes to
`build.frontendDist` (`dist` here), with the frontend version bumped to a new
patch release:

```bash
tar -czf frontend.tar.gz -C dist .
```

```bash
npx tauri signer sign --app-version 1.2.1 frontend.tar.gz
```

The signer reads the same `TAURI_SIGNING_PRIVATE_KEY` /
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` variables as the updater's build, and
writes `frontend.tar.gz.sig`. Upload the tarball and publish the manifest, whose
`signature` is the *content* of the `.sig` file:

```json
{
  "version": "1.2.1",
  "notes": "New colors",
  "pub_date": "2026-01-01T00:00:00Z",
  "url": "https://releases.example.com/1.2.1/frontend.tar.gz",
  "signature": "dW50cnVzdGVkIGNvbW1lbnQ6…"
}
```

As with the updater, an endpoint may answer `204 No Content` for "no update".
The archive must contain the whole frontend with `index.html` at its root; it
replaces the built-in assets entirely. Only plain files and directories are
accepted: an archive with links, absolute or `..` paths, or the same file
twice is refused as a whole.

In a multi-page app every page is served, as long as it is in the archive:
only the presence of `index.html` is checked, so a bundle that lacks another
page is accepted and that page is then missing.

The manifest and the tarball can live on any host that serves them over
`https`.

In CI, after the frontend build, that comes down to (`VERSION` being the new
patch version, `BASE_URL` where the files will be served from):

```bash
grep -rq 'ffp-ota-updater|notify_ready' dist   # fail if the build lost notifyReady()
tar -czf "frontend-$VERSION.tar.gz" -C dist .
npx tauri signer sign --app-version "$VERSION" "frontend-$VERSION.tar.gz"
jq -n --arg v "$VERSION" --arg u "$BASE_URL/frontend-$VERSION.tar.gz" \
  --rawfile s "frontend-$VERSION.tar.gz.sig" \
  '{version: $v, url: $u, signature: $s}' > ota.json
```

Give the tarball a name that contains its version. A manifest and a tarball
that are both "the latest" can be fetched from two different releases while
one is being published; with versioned names that download fails cleanly
instead, and the signature check catches the mix-up in either case.

### Per-platform bundles

One tarball serves every platform by default. If the frontend build differs
between platforms, publish one manifest per platform, each pointing at its own
tarball, and select it with the endpoint variables:

```json
{ "endpoints": ["https://releases.example.com/{{target}}-{{arch}}/ota.json"] }
```

`{{target}}` is `linux`, `windows` or `darwin`; `{{arch}}` is `x86_64`,
`aarch64`, `i686` or `armv7`. Sign each tarball with the same
`--app-version`.

## Using it

```js
import { check, notifyReady } from "tauri-plugin-ffp-ota-updater-api";

// on every start, once the app works: keeps a downloaded frontend (see Rollback)
await notifyReady();

const update = await check();
if (update) {
  // store it now, use it from the next launch on
  await update.download();
  // ...or switch immediately (reloads the page)
  await update.downloadAndInstall((event) => {
    if (event.event === "Progress") console.log(event.data.chunkLength);
  });
}
```

`check()` rejects when the device is offline or the endpoint is down, so
catch its errors and never make rendering wait for it. `download()` does
nothing for a bundle that is already stored (`update.downloaded`), so running
this on every start is cheap.

Every function rejects with an `OtaUpdateError`, whose `kind` says what
happened without parsing the message:

```js
import { check, OtaUpdateError } from "tauri-plugin-ffp-ota-updater-api";

try {
  await (await check())?.download();
} catch (error) {
  if (error instanceof OtaUpdateError && error.kind === "network") {
    // offline or the host is unreachable: try again later
  } else {
    report(error); // "signature", "invalidBundle", "releaseNotFound", ...
  }
}
```

`status()` describes the updater without a request, so it also works offline:

```js
const { currentVersion, confirmed, staged, rejected } = await status();
if (staged) showBanner(`Restart to update to ${staged}`);
if (rejected.length) report(`rolled back: ${rejected.join(", ")}`);
```

`staged` is a downloaded bundle waiting for the next launch, `confirmed` is
`false` while the frontend on screen is still on trial, and `rejected` lists
the versions that were rolled back. In Rust the same is
`app.ota_updater().status()`.

A staged bundle is addressed by its version, without another `check()`:

```js
import { install, discard } from "tauri-plugin-ffp-ota-updater-api";

await install(staged);   // switch to it now (reloads), also when offline
await discard(staged);   // or delete it: the user declined this update
```

Without `discard()` a downloaded bundle is used at the next launch whatever
happened since. Discarding is not a rejection: `check()` offers the version
again. The bundle being served and the confirmed one behind it cannot be
discarded (`inUse`).

Also available: `currentVersion()` (version of the frontend being served),
`rollback()` (see [Rollback](#rollback)) and `reset()` (delete downloaded
bundles and go back to the built-in frontend).
Requests give up after 30 seconds without progress, and after 30 seconds
(manifest) or 30 minutes (bundle) in total unless `timeout` is set.
Without a bundler, call the commands through `window.__TAURI__.core.invoke`
as `examples/tauri-app` does; errors are then plain `{ kind, message }`
objects.

From Rust:

```rust
use tauri_plugin_ffp_ota_updater::OtaUpdaterExt;

if let Some(update) = app.ota_updater().check().await? {
    update.download_and_install(|_chunk, _total| {}, || {}).await?;
}
```

### Bringing your own bundles

`check()` and `download()` are one way to fill the bundle store, not the only
one. From Rust, hand the plugin an archive and its signature that you got by
other means: your own manifest format or transport, a release API, a file
shipped next to the installer, a patch applied to the previous archive.

```rust
let updater = app.ota_updater();
updater.stage(&archive, &signature, &version)?;  // verified, then stored
updater.install(&version)?;                      // or leave it for the next launch
```

`stage` applies every check a download gets (signature, signed version,
compatibility, size limits, archive contents) and nothing else about the
bundle is trusted. `activate(&version)`, `install(&version)` and
`discard(&version)` work on any stored bundle. Versions only move forward: a
version older than the frontend being served is refused (`NotNewer`).

### Rollback

A downloaded bundle is on trial from the first page loaded from it until the
frontend calls `notifyReady()`. A run of the app that shows the bundle and
ends without that call is a failed attempt. One such run may be an app that
was closed while it was starting, so the bundle is tried again; after the
second (`trialAttempts`) it is taken to be broken: its version is rejected for
good (`check()` no longer offers it; publish the fix as a new version) and the
last confirmed bundle, or else the built-in frontend, is served. The confirmed
bundle is kept until its successor is confirmed.

Runs in which no page was loaded from the bundle, such as an app that starts
hidden in the tray or is closed before its window appears, are not attempts
and leave the bundle as it was. The attempt is recorded on disk before the
page is served.

**Every frontend you publish must call `notifyReady()`**, or it is reverted on
the third launch. Call it once the app got far enough that it could update
itself again. Set `"rollback": false` to serve bundles without a trial.

Without `readyTimeout` a broken bundle stays on screen until the user restarts
the app, twice with the default `trialAttempts`. Set it, for example `"readyTimeout": 20000`, and a bundle that has not
confirmed itself that many milliseconds after the first page was loaded from
it is rejected and replaced right away, whatever attempts it has left, by
reloading into the previous frontend. Choose a value well above the slowest start you expect, and do not
make `notifyReady()` wait for the user (a login, say).

`rollback()` abandons the bundle being served at once, on trial or confirmed:
its version is rejected and the app reloads into the last confirmed bundle or
the built-in frontend. A frontend that notices it is broken can call it from
an error boundary instead of waiting for the timeout (it needs the
`allow-rollback` permission). From Rust, `app.ota_updater().rollback()` does
the same without help from the frontend (for example from a menu item), and
`notify_ready()` confirms the bundle once a page was loaded from it.

To take back a frontend that was confirmed but turned out to be wrong, publish
the previous build again under a new, higher version.

### Switching without reloading

`install()` reloads every webview. An app that cannot be reloaded at an
arbitrary moment can do the two steps itself from Rust:

```rust
// after finishing or blocking whatever must not be interrupted
update.activate()?;              // new requests are served from the new bundle
app.ota_updater().reload()?;     // or reload / recreate windows one by one
```

Between the two calls, documents that are already loaded still run the old
frontend but would get lazily loaded assets from the new one, so keep the gap
short.

### What a frontend update can change

A bundle runs on every binary of its minor version line, including the oldest
one still installed, so it may only rely on what all of them have. Fixed in
the binary, and therefore out of reach of a frontend update:

- Rust commands and plugins, and the capabilities that permit them. Keep the
  `@tauri-apps/plugin-*` npm packages at the versions the binary was built
  with.
- `tauri.conf.json`: window labels and the page each window opens, the CSP
  (a new API host, font or image origin needs a binary release), asset and
  filesystem scopes.
- `getVersion()` from `@tauri-apps/api/app` keeps returning the binary's
  version. Use `currentVersion()` for what the user is actually running.

Build the bundle exactly like the frontend of the app: same bundler `base`,
same environment variables. Data the old frontend left in `localStorage` or
IndexedDB is read by the new one, so version anything you persist (with
TanStack Query's persisted cache, set its `buster` to the frontend version).

### Multi-page apps

Every page is a fresh document, which has three consequences:

- The start-up code runs on every navigation. Call `notifyReady()` on each
  page (it is a no-op once confirmed), from a module all entry points share.
  With React, call it in an effect of the root component rather than at the
  top of the module, so that a build that fails to render is not confirmed.
- Check for updates once per launch, not once per page: from Rust (below), or
  guarded by a `sessionStorage` flag, which survives navigation.
- `install()` reloads the page the window is on. Pages a bundle no longer
  contains, and requests for files of the previous build, are answered with
  `index.html` (Tauri's fallback), not with an error: keep page paths stable.

Checking from Rust is independent of which page is open:

```rust
.setup(|app| {
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        // staged for the next launch; errors (offline) are not fatal
        if let Ok(Some(update)) = handle.ota_updater().check().await {
            let _ = update.download(|_, _| {}, || {}).await;
        }
    });
    Ok(())
})
```

### Together with the updater plugin

The two plugins are independent, each with its own manifest. Check for a full
update first and fall back to a frontend one; `update.kind === "frontend"`
tells them apart if one function handles both:

```js
import { check as checkApp } from "@tauri-apps/plugin-updater";
import { check as checkFrontend } from "tauri-plugin-ffp-ota-updater-api";

const appUpdate = await checkApp();
if (appUpdate) {
  await appUpdate.downloadAndInstall(); // then relaunch
} else {
  await (await checkFrontend())?.downloadAndInstall();
}
```

A workable versioning scheme: bump the patch version for frontend-only
releases and publish them through this plugin; bump minor or major when the
Rust side changes and publish those through the updater.

If every release is also built and published as a full application (so that
new users install the latest frontend), the updater would offer a patch
release as a full download even to users who already have it as a frontend
bundle, because it compares against the binary's version. Tell it to leave
those releases to this plugin:

```rust
tauri_plugin_updater::Builder::new()
    .default_version_comparator(|current, release| {
        release.version > current
            && !tauri_plugin_ffp_ota_updater::default_compatibility(
                &current,
                &release.version,
            )
    })
    .build()
```

A patch release must then never contain Rust changes.

## Stability

From 1.0 on these are covered by semantic versioning, in addition to the Rust
and JavaScript APIs:

- the configuration keys and the manifest fields (`version`, `notes`,
  `pub_date`, `url`, `signature`), the bundle format (a `.tar.gz` of the
  frontend, signed with `tauri signer`) and the endpoint variables;
- the command names and their permissions, and which of them `default`
  grants;
- the `kind` strings of errors. New kinds may appear in minor releases, so
  keep a fallback branch;
- the store in the app's local data directory
  (`ffp-ota-updater/<version>.tar.gz`, `.tar.gz.sig` and `state.json`): a
  newer plugin version reads what an older one wrote.

`reqwest` and `http` types are part of the Rust API (`configure_client`,
headers, `Error`), so a new major version of either is a new major version of
this crate.

## Limitations

- **Dev mode**: `tauri dev` loads the frontend from the dev server, so a
  downloaded bundle is never displayed there. Test with a release build.
- **Strict CSP**: Tauri computes nonces and hashes for inline scripts and
  styles at build time, for the embedded HTML only. In a downloaded bundle,
  load scripts and styles from files (what bundlers emit) rather than inline.
- **Rollback happens at the next launch** unless `readyTimeout` is set, and
  `notifyReady()` is not tied to a particular page: any window can confirm
  the trial.
- **A good bundle can still be rejected** if the app is closed between
  showing it and its `notifyReady()` call in `trialAttempts` runs in a row.
  The device then stays on the previous frontend until the next version is
  published.
- One running instance per user is assumed; the bundle store is not locked
  against a second process.
- The manifest itself is not signed, the bundle is. `requireSignedVersion`
  is what prevents a tampered manifest from presenting an old bundle as new.

## Layout

| Path                 | Purpose                                                    |
| -------------------- | ---------------------------------------------------------- |
| `src/`               | Rust plugin crate (`tauri-plugin-ffp-ota-updater`)         |
| `build.rs`           | Declares plugin commands (generates their permissions)     |
| `permissions/`       | Permission definitions (`default.toml` + autogenerated)    |
| `guest-js/`          | TypeScript API wrapper, built to `dist-js/` (tsc + rollup) |
| `examples/tauri-app` | Plain HTML/JS app for trying out the plugin                |

## Prerequisites

- Rust (see `rust-version` in `Cargo.toml`) and Node.js
- [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/)
  (on Debian/Ubuntu: `libwebkit2gtk-4.1-dev`, `build-essential`, `libxdo-dev`,
  `libssl-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`)

## Development

Build the JS API and check the Rust crate:

```bash
npm install
npm run check   # fast type-check (TypeScript 7 native compiler)
npm run build   # tsc emits ESM + types, rollup derives the CJS build
cargo check
```

The JS API uses TypeScript 7 (the native compiler). It has no JS compiler API
yet, so `@rollup/plugin-typescript` can't be used; `tsc` does the emit instead.

Run the example app:

```bash
cd examples/tauri-app
npm install
npm run dev
```

## Adding a command

1. Add the function in `src/commands.rs` and register it in `src/lib.rs`
   (`generate_handler!`).
2. Add its name to `COMMANDS` in `build.rs`.
3. Add `allow-<command>` to `permissions/default.toml`, unless it should be
   opt-in.
4. Expose it in `guest-js/index.ts`.

## Trying the example

The example points at `http://localhost:8000/ota.json`. Generate a key, put
the public key in `examples/tauri-app/src-tauri/tauri.conf.json`, build a
release binary, then publish a modified copy of `examples/tauri-app/src` as
version `0.1.1` following [Publishing a frontend update](#publishing-a-frontend-update)
and serve the manifest and tarball with any static file server on port 8000.

## Testing

```bash
cargo test                                    # Rust tests (mock Tauri runtime; not on Windows)
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
npm run check                                 # TypeScript type-check
```

## License

[Boost Software License 1.0](LICENSE).
