# Use with the updater plugin

How to run this plugin next to
[`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/): the updater
for releases that change the binary, this plugin for releases that only change
the frontend.

The two plugins are independent. Each has its own configuration block, its own
manifest and its own permission. They can share the signing key, which is
what this page assumes, or [each have one](../explanation/how-it-works.md#security).

- [If you know the updater plugin](#if-you-know-the-updater-plugin): what looks
  the same but is not
- [Set up both](#set-up-both)
- [Decide which release goes where](#decide-which-release-goes-where)
- [Check for both in the app](#check-for-both-in-the-app)
- [Publish without the two colliding](#publish-without-the-two-colliding)

## If you know the updater plugin

The API was made to feel familiar, which makes the differences easy to miss.
These are the ones that cost people time.

### Setup

| | Updater plugin | This plugin |
| --- | --- | --- |
| Install | `tauri add updater` | add the crate and `tauri-plugin-ffp-ota-updater-api` by hand |
| Register | usually inside `.setup()` with `app.handle().plugin(…)` | on `tauri::Builder` with `.plugin(…)`, **plus** `wrap_assets(&mut context)` before `.run(context)`. Registered from `.setup()` it is too late: the window already exists. |
| Config block | `plugins.updater` | `plugins.ffp-ota-updater` (its own `endpoints`; the `pubkey` can be the same) |
| Permission | `updater:default` | `ffp-ota-updater:default`; `rollback()` and `reset()` need `allow-rollback` / `allow-reset` |
| Extra duty | none | every frontend must call `notifyReady()` |
| `tauri dev` | checks work | checks work, but a downloaded frontend is never shown; test with a release build |

Forgetting `wrap_assets(&mut context)` does not stop the app: checks and
downloads succeed, but nothing ever changes on screen and `install()` fails
with `cannotServe`. The log has a warning saying that
`wrap_assets(&mut context)` was not called.

### Building and signing

| | Updater plugin | This plugin |
| --- | --- | --- |
| Artifacts | made and signed by `tauri build` when `bundle.createUpdaterArtifacts` is set | you run `tar` and `tauri signer sign`; `createUpdaterArtifacts` has no effect on it |
| Key in the environment | `TAURI_SIGNING_PRIVATE_KEY` may be the key **or a path** | for `tauri signer sign` it must be the **key itself**; a path goes in `TAURI_SIGNING_PRIVATE_KEY_PATH` |
| Version in the signature | optional (`requireSignedVersion` is off by default) | **required** by default: sign with `--app-version`, Tauri CLI 2.12+ |
| What is published | one artifact per platform | one tarball for all platforms |

### Manifest and endpoints

| | Updater plugin | This plugin |
| --- | --- | --- |
| Manifest shapes | static (`platforms` map) or dynamic (`url` + `signature`) | **only** top-level `url` + `signature` |
| `latest.json` from `tauri-action` | yes | no. Pointing `endpoints` at it fails with `releaseNotFound`. |
| `{{current_version}}` | version of the app | version of the **frontend being served** (the app's until the first bundle is applied) |
| Version of the app | `{{current_version}}` | `{{binary_version}}` |
| `{{bundle_type}}` | yes | no |
| `http://` endpoints | allowed in debug builds | only with `dangerousInsecureTransportProtocol`, in any build |
| Download URL and redirects | `https` not enforced | `https` enforced |
| Timeouts | none unless set | 30 s for the manifest, 30 min for the bundle, 30 s without progress |

### Versions

| | Updater plugin | This plugin |
| --- | --- | --- |
| What is an update | any version greater than the app's | a version greater than the frontend being served **and** a newer patch of the binary (`1.2.0` takes `1.2.x` only) |
| Changing the rule | `version_comparator` | `Builder::compatibility`, which answers a different question: "can this binary run that frontend?" A bundle still has to be newer. |
| Downgrades | `allowDowngrades` / comparator | never. Go back with a rollback, or republish old content under a higher version. |
| `getVersion()` afterwards | the new version | still the binary's version. Use `currentVersion()` from this plugin. |

A manifest announcing `1.3.0` to an app on `1.2.0` is not an error: `check()`
resolves to `null`, exactly as if there were no update.

### Applying an update

| | Updater plugin | This plugin |
| --- | --- | --- |
| Takes effect | after the app restarts (`relaunch()` from the process plugin; Windows exits by itself during install) | `install()` reloads the webviews; without it, at the next launch. No restart, no process plugin. |
| State | lost with the process | in-memory page state is lost with the reload; `localStorage`, IndexedDB and the Rust side keep running |
| A bad update | stays installed | reverted if it never calls `notifyReady()` |
| Downloaded but not installed | gone when the app quits | kept on disk and applied at the next launch, unless you `discard()` it. With `requireApproval` it is kept and **not** applied until you `approve()` or `install()` it. |
| Asking the user first | your code decides before `downloadAndInstall()` | the same, or `requireApproval`: download ahead of time, [ask whenever it suits](integrate.md#ask-the-user-before-updating) |

### API

| | Updater plugin | This plugin |
| --- | --- | --- |
| JS `Update` | a resource: `close()` it; `install()` only on the object that downloaded | plain object; `install(version)` also works in a later run, offline |
| JS `check()` options | `headers`, `timeout`, `proxy`, `target` | `headers`, `timeout`, `proxy` |
| Headers on the download | all of them, wherever the manifest points | only to the endpoint's origin, redirects included |
| JS errors | strings | `OtaUpdateError` with a `kind` to branch on |
| Rust entry point | `app.updater()?`, `app.updater_builder()` | `app.ota_updater()`; per-check options with `check_with(RequestOptions)` |
| Rust download | `download()` returns the bytes, `install(bytes)` | `download()` stores the bundle, `install()` takes no bytes |

## Set up both

```rust
// src-tauri/src/lib.rs
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[allow(unused_mut)]
    let mut context = tauri::generate_context!();
    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        tauri_plugin_ffp_ota_updater::wrap_assets(&mut context);
        builder = builder
            .plugin(tauri_plugin_ffp_ota_updater::init())
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init());
    }

    builder
        .run(context)
        .expect("error while running tauri application");
}
```

Both plugins are desktop-only; the `#[cfg(desktop)]` block keeps an app that
also targets Android or iOS compiling. Declare the dependencies for desktop
targets only, too.

```json
{
  "bundle": { "createUpdaterArtifacts": true },
  "plugins": {
    "updater": {
      "pubkey": "CONTENT OF YOUR .key.pub FILE",
      "endpoints": ["https://releases.example.com/latest.json"]
    },
    "ffp-ota-updater": {
      "pubkey": "CONTENT OF YOUR .key.pub FILE",
      "endpoints": ["https://releases.example.com/ota.json"]
    }
  }
}
```

```json
{
  "permissions": [
    "core:default",
    "updater:default",
    "process:default",
    "ffp-ota-updater:default"
  ]
}
```

## Decide which release goes where

A scheme that needs no extra code:

| The release changes… | Bump | Publish through |
| --- | --- | --- |
| only the frontend | patch (`1.2.0` → `1.2.1`) | this plugin |
| Rust code, plugins, capabilities, `tauri.conf.json`, Tauri itself | minor or major (`1.2.x` → `1.3.0`) | the updater |

What counts as "only the frontend" is listed in
[What a frontend update can change](../explanation/frontend-only-updates.md).

After a full update the app drops the bundles of the previous line by itself:
on `1.3.0`, a stored `1.2.4` is deleted at the first launch.

This scheme relies on the version changing whenever the binary does. If your
installer feed can replace the binary without that, for instance because it
goes by a build number of its own, a bundle made for the old binary would stay
in use on the new one. Tie frontends to the native build instead of to the
version alone:
[Bind a frontend to a native build or IPC contract](integrate.md#bind-a-frontend-to-a-native-build-or-ipc-contract).

### Every release is also a full release

You may want every release built as a full application as well, so that new
users download the latest frontend. The updater would then offer `1.2.1` as a
full download even to users who already run it as a frontend bundle, because
it compares against the binary's version. Tell it to leave such releases to
this plugin:

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

With this in place a patch release must never contain Rust changes: existing
users will only ever receive its frontend.

## Check for both in the app

Check for a full update first and fall back to a frontend one. A full update
replaces the frontend anyway.

```js
import { check as checkApp } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { check as checkFrontend, notifyReady } from "tauri-plugin-ffp-ota-updater-api";

await notifyReady();

async function update() {
  const appUpdate = await checkApp();
  if (appUpdate) {
    await appUpdate.downloadAndInstall(); // Windows exits here
    await relaunch();
    return;
  }
  const frontendUpdate = await checkFrontend();
  await frontendUpdate?.downloadAndInstall(); // reloads the page
}

update().catch((error) => console.warn("update check failed:", error));
```

If one function handles both kinds, `update.kind === "frontend"` identifies
this plugin's object.

Show the user this plugin's `currentVersion()`, not `getVersion()`, or the
"About" screen will be a frontend release behind.

## Publish without the two colliding

Keep two manifests at two URLs. They describe different files and usually
different versions.

**GitHub Releases.** The updater's usual endpoint is
`releases/latest/download/latest.json`. A release that holds only a frontend
tarball must never become the repository's "latest" release, or that URL
returns 404 and full-update checks fail. Keep frontend bundles in one
pre-release with a fixed tag:
[Publish with GitHub Releases](publish-github-releases.md).

**CrabNebula Cloud.** The `…/update/ORG/APP/{{target}}-{{arch}}/{{current_version}}`
endpoint belongs to the updater. Give frontend releases their own channel and
fetch the manifest through a download link:
[Publish with CrabNebula Cloud](publish-crabnebula.md).

**Your own server.** Two routes. Remember that `{{current_version}}` means
something different in each plugin's endpoint (see the table above).

**In CI**, the secret that holds the key content works for both `tauri build`
(or `tauri-action`) and `tauri signer sign`. A secret that holds a *path*
only works for the build.
