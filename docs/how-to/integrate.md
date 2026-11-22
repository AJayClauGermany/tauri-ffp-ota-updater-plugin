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

`status()` makes no request, so it also tells you about a bundle downloaded in
an earlier run. Ask it whether or not the check worked, so that the bundle can
be applied while offline:

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
  // `ask` is yours: a dialog with these three answers
  const answer = await ask(`Version ${staged} is ready.`, ["Reload now", "Later", "Skip"]);
  if (answer === "Reload now") {
    await install(staged);   // reloads every window
  } else if (answer === "Skip") {
    await discard(staged);   // otherwise it is applied at the next launch
  }
}
```

Without `discard()` a downloaded bundle is used at the next launch whatever
the user answered. Discarding is not a rejection: `check()` offers the version
again, so remember the user's answer if it should not be asked again.
Discard only once `download()` has settled: a download that is still running
stores the bundle again.

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
        // staged for the next launch; errors (offline) are not fatal
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

Only the presence of `index.html` is checked when a bundle is accepted. A
bundle that lacks another page is accepted and that page is then missing.

## Bring your own bundles

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
compatibility, size limits, archive contents). `activate(&version)`,
`install(&version)` and `discard(&version)` work on any stored bundle.

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
