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

Desktop only (Windows, macOS, Linux) for now.

## How it works

- `check()` asks your endpoint for a release manifest. By default a release is
  a frontend update only if it is a newer patch of the installed binary
  (binary `1.2.0` accepts `1.2.1`, `1.2.7`, but not `1.3.0` or `2.0.0`): minor
  and major bumps are assumed to need new Rust code and are left to the
  updater plugin. Override this with `Builder::version_comparator`.
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
| `requireSignedVersion` | `true` | Reject signatures that do not record the version they were made for. Needs a Tauri CLI with `tauri signer sign --app-version`. |

| `rollback` | `true` | Revert a bundle the frontend does not confirm with `notifyReady()`. See [Rollback](#rollback). |
| `maxBundleSize` | 64 MiB | Largest `.tar.gz` accepted, in bytes. |
| `maxUnpackedSize` | 256 MiB | Largest total size of the files in a bundle, in bytes. They are held in memory. |

Grant the permission in a capability file:

```json
{ "permissions": ["core:default", "ffp-ota-updater:default"] }
```

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

Also available: `currentVersion()` (version of the frontend being served) and
`reset()` (delete downloaded bundles and go back to the built-in frontend).
Requests give up after 30 seconds without progress, and after 30 seconds
(manifest) or 30 minutes (bundle) in total unless `timeout` is set.
Without a bundler, call the commands through `window.__TAURI__.core.invoke`
as `examples/tauri-app` does.

From Rust:

```rust
use tauri_plugin_ffp_ota_updater::OtaUpdaterExt;

if let Some(update) = app.ota_updater().check().await? {
    update.download_and_install(|_chunk, _total| {}, || {}).await?;
}
```

### Rollback

A downloaded bundle is on trial from the first time it is served until the
frontend calls `notifyReady()`. If the app starts again and the trial is still
open, the bundle is taken to be broken: its version is rejected for good
(`check()` no longer offers it; publish the fix as a new version) and the last
confirmed bundle, or else the built-in frontend, is served. The trial is
recorded on disk before the bundle is served, and the confirmed bundle is kept
until its successor is confirmed.

**Every frontend you publish must call `notifyReady()`**, or it is reverted on
the second launch. Call it once the app got far enough that it could update
itself again. Set `"rollback": false` to serve bundles without a trial.

From Rust, `app.ota_updater().rollback()` abandons the bundle being served
without help from the frontend (for example from a menu item, or your own
watchdog timer), and `notify_ready()` confirms it.

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

## Limitations

- **Dev mode**: `tauri dev` loads the frontend from the dev server, so a
  downloaded bundle is never displayed there. Test with a release build.
- **Strict CSP**: Tauri computes nonces and hashes for inline scripts and
  styles at build time, for the embedded HTML only. In a downloaded bundle,
  load scripts and styles from files (what bundlers emit) rather than inline.
- **Rollback happens at the next launch**, not while the broken frontend is
  on screen, and `notifyReady()` is not tied to a particular page: any window
  can confirm the trial. Add a timer around `rollback()` in Rust if you need
  more.
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
3. Add `allow-<command>` to `permissions/default.toml`.
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
