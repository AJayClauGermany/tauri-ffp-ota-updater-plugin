# Integrate updates into your app

Recipes for the update experience, once the plugin is
[set up](../../README.md#quick-start). Each section stands alone.

- [Confirm that the frontend works](#confirm-that-the-frontend-works)
- [Update silently at the next launch](#update-silently-at-the-next-launch)
- [Ask the user before updating](#ask-the-user-before-updating)
- [Show download progress](#show-download-progress)
- [Handle errors and being offline](#handle-errors-and-being-offline)
- [Check from Rust](#check-from-rust)
- [Recover from a broken frontend sooner](#recover-from-a-broken-frontend-sooner)
- [Release channels](#release-channels)
- [Send headers, use a proxy or a private CA](#send-headers-use-a-proxy-or-a-private-ca)
- [Switch without reloading at a bad moment](#switch-without-reloading-at-a-bad-moment)
- [Multi-page apps](#multi-page-apps)
- [Apps with several windows](#apps-with-several-windows)
- [Bind a frontend to a native build or IPC contract](#bind-a-frontend-to-a-native-build-or-ipc-contract)
- [Bring your own bundles](#bring-your-own-bundles)
- [Apps that also build for mobile](#apps-that-also-build-for-mobile)
- [Use it without a bundler](#use-it-without-a-bundler)

## Confirm that the frontend works

Call `notifyReady()` on every start, at the point where the app has got far
enough that it could update itself again:

```js
import { notifyReady } from "tauri-plugin-ffp-ota-updater-api";

await notifyReady();
```

- Put it after the first successful render, not at the top of the entry
  module. With React, call it in an effect of the root component, so that a
  build that fails to render is not confirmed.
- Do not make it wait for the user (a login screen, say).
- It is a no-op when there is nothing to confirm, so no condition is needed.

To catch a build that lost it, check the output in CI:

```bash
grep -rq notify_ready dist
```

The command name survives minification, and bundlers drop `notifyReady` from
the output when nothing imports it. This is a heuristic, not a proof: it
passes for a build that imports `notifyReady()` but never reaches the call.
Only starting the release build (see [publish](publish.md#6-check-it))
shows that the confirmation actually happens.

## Update silently at the next launch

Download in the background and let the next start pick the bundle up. Nothing
changes under the user:

```js
import { check } from "tauri-plugin-ffp-ota-updater-api";

check()
  .then((update) => update?.download())
  .catch(() => {}); // offline: try again at the next start
```

`download()` does nothing for a bundle that is already stored, so running
this on every start is cheap.

## Ask the user before updating

Set `requireApproval`. A bundle that was downloaded then changes nothing until
the user has agreed: not in this run, and not at the next launch either.

```json
{ "plugins": { "ffp-ota-updater": { "requireApproval": true } } }
```

Download whenever it suits the app, for instance
[from Rust](#check-from-rust) at startup. `status()` makes no request, so it
also knows about a bundle that was downloaded in an earlier run, and the
user's answer can be acted on while offline:

```js
import { status, approve, install, discard } from "tauri-plugin-ffp-ota-updater-api";

const { staged, approved } = await status();
if (staged && staged !== approved) {
  // `ask` is yours: a dialog with these four answers
  const answer = await ask(`Version ${staged} is ready.`, [
    "Reload now",
    "At the next start",
    "Later",
    "Skip",
  ]);
  if (answer === "Reload now") {
    await install(staged);   // reloads every window
  } else if (answer === "At the next start") {
    await approve(staged);   // nothing changes until the app is started again
  } else if (answer === "Skip") {
    await discard(staged);   // deletes the download
  }
  // "Later" needs no call: the bundle stays stored, and `staged` reports it
  // at every start until the user decides
}
```

- `approve()` can be taken back with `unapprove(version)` until the app is
  restarted. Show `approved` as "will be used at the next start".
- One version is approved at a time. If a newer bundle arrives before the
  restart, the next launch still switches to the approved one, and the newer
  one is reported as `staged` afterwards.
- Of several bundles that wait, a launch keeps the newest.
- To ask in a native dialog, call `app.ota_updater().approve(&version)` or
  `install(&version)` from Rust, and leave `allow-approve`, `allow-install`
  and `allow-download-and-install` out of the capabilities: the frontend can
  then fetch updates but not switch to one
  ([Permissions](../reference/configuration.md#permissions)).
- Discarding is not a rejection: `check()` offers the version again, so
  remember the user's answer if it should not be asked again. Discard only
  once `download()` has settled: a download that is still running stores the
  bundle again.

[`examples/consent-app`](../../examples/consent-app) is a small app built this
way, with two windows.

### Without `requireApproval`

A downloaded bundle is then used at the next launch whatever the user
answered, unless it is discarded. "Later" and "at the next start" are the
same thing, and declining means deleting the download:

```js
import { check, status, install, discard } from "tauri-plugin-ffp-ota-updater-api";

try {
  const update = await check();
  await update?.download();
} catch (error) {
  // offline, say: there may still be a bundle from an earlier run
}

const { staged } = await status();
if (staged) {
  const answer = await ask(`Version ${staged} is ready.`, ["Reload now", "Later", "Skip"]);
  if (answer === "Reload now") {
    await install(staged);   // reloads every window
  } else if (answer === "Skip") {
    await discard(staged);   // otherwise it is applied at the next launch
  }
}
```

## Show download progress

```js
let total = 0, received = 0;
await update.downloadAndInstall((event) => {
  if (event.event === "Started") total = event.data.contentLength ?? 0;
  if (event.event === "Progress") {
    received += event.data.chunkLength;
    setProgress(total ? received / total : undefined);
  }
});
```

`Finished` ends the progress bar, not the update: the bundle is verified and
stored after it. Whether that worked is what the promise says.

## Handle errors and being offline

`check()` rejects when the device is offline or the endpoint is down. Catch
it, and never make rendering wait for it.

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

All kinds are in the [API reference](../reference/api.md#errors). Keep a
fallback branch: new kinds can appear in minor releases.

To find out afterwards that a version was rolled back on this device:

```js
const { rejected } = await status();
if (rejected.length) report(`rolled back: ${rejected.join(", ")}`);
```

## Check from Rust

Independent of which page is open, and of whether a window exists at all:

```rust
use tauri_plugin_ffp_ota_updater::OtaUpdaterExt;

.setup(|app| {
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        // staged for the next launch, or with `requireApproval` until the
        // user agrees; errors (offline) are not fatal
        if let Ok(Some(update)) = handle.ota_updater().check().await {
            let _ = update.download(|_, _| {}, || {}).await;
        }
    });
    Ok(())
})
```

`update.download_and_install(…)` applies it at once instead.

## Recover from a broken frontend sooner

By default a broken bundle stays on screen until the user has restarted the
app twice. Two ways to shorten that:

**A deadline.** With `readyTimeout`, a bundle that has not called
`notifyReady()` that many milliseconds after its first page was loaded is
rejected and replaced right away by reloading into the previous frontend:

```json
{ "plugins": { "ffp-ota-updater": { "readyTimeout": 20000 } } }
```

Choose a value well above the slowest start you expect.

**From the frontend itself.** A frontend that notices it is broken can give up
at once, for example from an error boundary. Grant
`ffp-ota-updater:allow-rollback`, then:

```js
import { rollback } from "tauri-plugin-ffp-ota-updater-api";

await rollback(); // rejects this version and reloads into the last confirmed one
```

If `rollback()` rejects with `io`, the app has gone back for this run but the
rejection could not be written down (a full disk, say), and the version is
served or offered again after a restart.

From Rust, `app.ota_updater().rollback()` does the same without help from the
frontend, for example from a menu item.

## Release channels

Give one check its own endpoints. The webview cannot choose endpoints, so this
is done in Rust, typically in a command of your own:

```rust
use tauri_plugin_ffp_ota_updater::{OtaUpdaterExt, RequestOptions};

let update = app
    .ota_updater()
    .check_with(RequestOptions::new().endpoints(vec![beta_url]))
    .await?;
```

The download of the release that check finds uses the same options. To fix
the channel for the whole run instead, use `Builder::endpoints` at startup.

Leaving a channel does not remove what it installed, and versions only move
forward. If the stable channel is behind the beta frontend a device has, call
`reset()` (permission `allow-reset`) to return to the built-in frontend first.

## Send headers, use a proxy or a private CA

For every request, on the builder:

```rust
tauri_plugin_ffp_ota_updater::Builder::new()
    .header("Authorization", format!("Bearer {token}"))?
    .timeout(std::time::Duration::from_secs(20))
    .proxy("http://proxy.example.com:8080".parse()?)   // or .no_proxy()
    .configure_client(|client| client.add_root_certificate(company_ca.clone()))
    .build()
```

For one check, from the webview:

```js
await check({ headers: { "X-Channel": "beta" }, timeout: 20000 });
```

These headers go to the manifest endpoint, and to the bundle download only
when the manifest points to the same origin (scheme, host and port). They never
leave that origin, not along a redirect either. The manifest is not signed, so
whoever can change it could otherwise send the download, and an
`Authorization` header with it, to a server of their choosing; the signature
check only rejects the bundle after that request.

There is no setting that sends headers to wherever the manifest points, for the
same reason. If the bundles are on another origin and need credentials:

- serve the manifest and the bundles from one origin, or
- have the manifest name a URL that carries its own, short-lived authorization
  (a pre-signed URL), or
- fetch the bundle in Rust with a client of your own and hand it to
  [`stage`](#bring-your-own-bundles).

Options given to `check()` in the webview replace the builder's for that one
check; they are defaults, not a policy the webview is held to. The webview
cannot read a header set in Rust, and a proxy it names sees no more of an
`https` request than the host it goes to. An app that must not let its
frontend choose them leaves `ffp-ota-updater:allow-check` out of its
capabilities and [checks from Rust](#check-from-rust).

## Switch without reloading at a bad moment

`install()` reloads every webview window. An app that cannot be reloaded at
an arbitrary moment can do the two steps itself from Rust:

```rust
// after finishing or blocking whatever must not be interrupted
update.activate()?;              // new requests are served from the new bundle
app.ota_updater().reload()?;     // or reload / recreate windows one by one
```

Between the two calls, documents that are already loaded still run the old
frontend but would get lazily loaded assets from the new one, so keep the gap
short.

## Multi-page apps

Every page is a fresh document, which has three consequences:

- The start-up code runs on every navigation. Call `notifyReady()` on each
  page (it is a no-op once confirmed), from a module all entry points share.
- Check for updates once per launch, not once per page:
  [from Rust](#check-from-rust), or guarded by a `sessionStorage` flag, which
  survives navigation.
- `install()` reloads the page the window is on. Pages a bundle no longer
  contains, and requests for files of the previous build, are answered with
  `index.html` (Tauri's fallback), not with an error: keep page paths stable.

Only the presence of `index.html` is checked when a bundle is accepted, unless
you name the other pages in `requiredFiles`: a bundle that lacks one of them
is then refused instead of answering for it with the index page.

```json
{ "plugins": { "ffp-ota-updater": { "requiredFiles": ["about.html", "settings/index.html"] } } }
```

## Apps with several windows

**Require the page of every window.** Which page a window opens is fixed in
the binary. A bundle that lacks it would not fail: Tauri answers with
`index.html`, and the window shows the wrong page. List the pages in
`requiredFiles` and such a bundle is refused when it is downloaded:

```json
{ "plugins": { "ffp-ota-updater": { "requiredFiles": ["editor.html", "splash.html"] } } }
```

**Let only a real page confirm.** A bundle is confirmed by the first page of
it that calls `notifyReady()`, in any window. A splash screen, a tray popup or
a hidden bootstrap window loads fine from a bundle whose main page is broken,
so it must not be the one that vouches for it. Do not call `notifyReady()`
there, and take the permission away so that a later change cannot add it
back. Capabilities are per window; instead of `ffp-ota-updater:default` for
all of them, give every window what it needs by name:

```json
{
  "identifier": "every-window",
  "windows": ["*"],
  "permissions": ["core:default", "ffp-ota-updater:allow-status"]
}
```

```json
{
  "identifier": "main-window",
  "windows": ["main"],
  "permissions": [
    "ffp-ota-updater:allow-notify-ready",
    "ffp-ota-updater:allow-approve",
    "ffp-ota-updater:allow-unapprove",
    "ffp-ota-updater:allow-install",
    "ffp-ota-updater:allow-discard"
  ]
}
```

A window whose page was loaded puts the bundle on trial all the same: with
`readyTimeout`, the clock starts when the splash screen is served.

**Check once, in Rust.** Each window is a document of its own and would run
its own check. [Check from Rust](#check-from-rust) and let the windows read
`status()`.

**Switch all windows together.** `install()` reloads every window, from
whichever one it is called. For windows that hold unsaved work, see
[Switch without reloading at a bad moment](#switch-without-reloading-at-a-bad-moment),
or let the user [approve](#ask-the-user-before-updating) the bundle for the
next start, when no document is open.

## Bind a frontend to a native build or IPC contract

By default the version says which binaries a frontend runs on: the later
patches of their line. That is not enough when two binaries with the same
version differ in what the frontend can call: a rebuild that changed a
command, an installer feed that counts builds separately from the version
users see, a platform with commands of its own.

Give the binary a name for what it offers, and let each frontend say in its
version which one it was built for. Build metadata, the part after `+`, is
the place: it is part of the version, so the signature covers it, and it
plays no role in deciding which version is newer.

```rust
/// Changes whenever a frontend built for the previous value could not run on
/// this binary: a command or its arguments changed, a permission is gone.
const IPC_CONTRACT: &str = "ipc3";

tauri_plugin_ffp_ota_updater::Builder::new()
    .compatibility(|binary, bundle| {
        tauri_plugin_ffp_ota_updater::default_compatibility(binary, bundle)
            && bundle.build.as_str() == IPC_CONTRACT
    })
    .build()
```

Publish the frontend under that version, wherever a version is given:

```bash
npm run tauri signer sign -- --app-version 1.2.1+ipc3 frontend-1.2.1-ipc3.tar.gz
```

```json
{
  "version": "1.2.1+ipc3",
  "url": "https://releases.example.com/frontend-1.2.1-ipc3.tar.gz",
  "signature": "CONTENT OF frontend-1.2.1-ipc3.tar.gz.sig"
}
```

Keep the `+` out of the file name: some hosts read it as a space in a URL.

What this gives you:

- A frontend with another tag, or with none, is not offered by `check()`
  (`null`) and is refused by `download()` and `stage()` with `incompatible`.
  The tag is inside the signature, so a changed manifest cannot relabel a
  bundle.
- The rule is applied again at every launch. When the binary is replaced by
  one with another contract, **even one with the same version**, the bundles
  stored for the old one are deleted at its first launch, the confirmed one
  included, and the built-in frontend is served.
- `1.2.1+ipc3` and `1.2.1+ipc4` are one release built twice: they have the
  same place in the order of versions, and a device accepts only the one made
  for it.

Things to decide:

- **What the tag stands for.** A contract number that you raise by hand when
  the IPC changes incompatibly lets several native builds share a frontend. A
  build id from CI (`env!("APP_BUILD_ID")`, a commit hash) binds each
  frontend to exactly one native build, at the price of publishing one for
  every build that is in use. A binary that still serves the previous
  contract can accept both:
  `["ipc3", "ipc4"].contains(&bundle.build.as_str())`.
- **Which manifest a device asks.** One URL describes one release, so each
  contract needs its own. Put the tag into the endpoint from Rust,
  `.endpoints(vec![format!("https://releases.example.com/ota/{IPC_CONTRACT}/ota.json").parse()?])`,
  or send it as a header (`.header("X-Ipc-Contract", IPC_CONTRACT)?`) to a
  server that decides. A device that is handed another contract's manifest
  loses nothing: its `check()` resolves to `null`.
- **Leave `compareBuildMetadata` off.** With it, build metadata also orders
  versions, and a tag is not a counter.

The same works for frontends that differ by platform: `1.2.1+windows`, and
`cfg!(windows)` in the rule.

As a second line of defence, the frontend can ask the binary what it offers,
through a command of your own, before it calls `notifyReady()`, and call
[`rollback()`](#recover-from-a-broken-frontend-sooner) when that is not what
it was built for.

## Bring your own bundles

`check()` and `download()` are one way to fill the bundle store, not the only
one. From Rust, hand the plugin an archive and its signature that you got by
other means: your own manifest format or transport, a release API, a file
shipped next to the installer, a patch applied to the previous archive.

```rust
let updater = app.ota_updater();
updater.stage(&archive, &signature, &version)?;  // verified, then stored
updater.install(&version)?;                      // or leave it for the next launch
                                                 // (`approve` it, with `requireApproval`)
```

`stage` applies every check a download gets (signature, signed version,
compatibility, size limits, archive contents, required files).
`activate(&version)`, `install(&version)`, `approve(&version)` and
`discard(&version)` work on any stored bundle.

A version is stored once: staging other contents under a version that is
already stored fails with `InUse`. Staging the same archive again does
nothing, so it is safe to repeat.

## Apps that also build for mobile

The plugin is desktop-only. Keep it out of mobile builds:

```toml
# src-tauri/Cargo.toml
[target.'cfg(not(any(target_os = "android", target_os = "ios")))'.dependencies]
tauri-plugin-ffp-ota-updater = "…"
```

```rust
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[allow(unused_mut)]
    let mut context = tauri::generate_context!();
    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        tauri_plugin_ffp_ota_updater::wrap_assets(&mut context);
        builder = builder.plugin(tauri_plugin_ffp_ota_updater::init());
    }

    builder
        .run(context)
        .expect("error while running tauri application");
}
```

Put `ffp-ota-updater:default` in a capability file that is limited to desktop
(`"platforms": ["linux", "macOS", "windows"]`), and guard the JavaScript
calls, which reject on mobile.

## Use it without a bundler

With `app.withGlobalTauri` enabled, call the commands directly, as
[`examples/tauri-app`](../../examples/tauri-app/src/main.js) does:

```js
const { invoke } = window.__TAURI__.core;

await invoke("plugin:ffp-ota-updater|notify_ready");
const update = await invoke("plugin:ffp-ota-updater|check");
```

Errors are then plain `{ kind, message }` objects. Command names and
arguments are in the [API reference](../reference/api.md#commands).
