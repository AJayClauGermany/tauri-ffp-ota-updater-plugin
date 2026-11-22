# Troubleshooting

Find the symptom, apply the fix. Error names are the `kind` of an
`OtaUpdateError` (JavaScript) or the result of `Error::kind()` (Rust).

## First, look at what the plugin knows

```js
import { status } from "tauri-plugin-ffp-ota-updater-api";
console.log(await status());
// { binaryVersion, currentVersion, confirmed, staged, rejected }
```

The plugin logs through the `log` crate. Add
[`tauri-plugin-log`](https://v2.tauri.app/plugin/logging/) (or any logger) to
see its warnings, which name the bundle and the reason whenever one is
discarded or rolled back.

Downloaded bundles live in `ffp-ota-updater/` inside the app's local data
directory. Deleting that folder while the app is closed returns it to the
built-in frontend.

| OS | Local data directory |
| --- | --- |
| Linux | `~/.local/share/<identifier>/` |
| macOS | `~/Library/Application Support/<identifier>/` |
| Windows | `%LOCALAPPDATA%\<identifier>\` |

## The update downloads, but the app looks the same

| Cause | Fix |
| --- | --- |
| Running under `tauri dev`. The dev server's frontend is always shown, and `install()` fails with `cannotServe`. | Test with `tauri build` (add `-- --no-bundle` for a quick binary). |
| `wrap_assets(&mut context)` is missing. `install()` fails with `cannotServe`, and the log says `wrap_assets(&mut context)` was not called. | Add it before `.run(context)`; see the [quick start](../../README.md#2-register). |
| No `pubkey` is configured. `check()` fails with `emptyPubkey`. | Add it; see [Configuration](../reference/configuration.md#tauriconfjson). |
| The plugin is registered inside `.setup()`. | Register it with `.plugin(…)` on `tauri::Builder`. |
| Only `download()` was called. | That is how it works: the bundle is used from the next launch. Call `install()` or `downloadAndInstall()` to switch now. |

## `check()` resolves to `null`, but I published an update

`null` means "nothing for this device", which covers more than "no release":

| Cause | How to tell | Fix |
| --- | --- | --- |
| The version is not a newer **patch** of the installed binary (`1.3.0` for an app on `1.2.x`). | Compare the manifest's `version` with `binaryVersion`. | Publish it as a patch, ship it as a full update, or change the rule with [`Builder::compatibility`](../reference/configuration.md#builder). |
| The version is not greater than the frontend already being served. | `currentVersion` in `status()`. | Use a higher version. Build metadata does not count: `1.2.1+2` is not greater than `1.2.1+1` unless `compareBuildMetadata` is set. |
| The version was rolled back on this device. | It is in `rejected`. | Publish the fix under a new version. A rejected version is never offered again. |
| The app still sees the old manifest. | `curl -sL <endpoint>` | Wait for or purge the CDN cache. |
| The endpoint answered `204`. | `curl -si <endpoint>` | Fix the server's condition. Remember that `{{current_version}}` is the frontend's version, `{{binary_version}}` the app's. |

## The app shows the update, then goes back to the old frontend

The bundle was on trial and never confirmed, so it was rolled back; its
version is now in `status().rejected`.

- The frontend does not call `notifyReady()`, or fails before reaching it.
  Minifiers do not remove it, but a refactoring can: check the build output
  with `grep -rq notify_ready dist`.
- `notifyReady()` waits for something that does not always happen (a login, a
  network request).
- `readyTimeout` is shorter than a slow start.
- The permission for it is missing; see ["not allowed"](#unknown--command-not-allowed).
- More instances of the app than `trialAttempts` were started before the
  first one confirmed; see
  [Limitations](../explanation/how-it-works.md#limitations).

Publish the corrected frontend under a **new** version.
[How the trial works](../explanation/how-it-works.md#trial-and-rollback).

## The app does not start after adding the plugin

`error while running tauri application: … does not use https`: an entry of
`endpoints` is `http://`. Use `https`, or for local testing only set
`"dangerousInsecureTransportProtocol": true`.

## Errors from `check()`

### `releaseNotFound`

An endpoint answered, but not with a usable manifest. The message carries the
last reason.

- **`endpoint responded with 404`**: the URL is wrong, or it is a GitHub
  `releases/latest/download/…` URL and the latest release no longer contains
  the file. [Use a fixed tag](publish-github-releases.md).
- **`missing field …`**: the JSON is not this plugin's manifest. The usual
  cause is pointing at the updater's `latest.json`, which keeps `url` and
  `signature` inside `platforms`. [Manifest format](../reference/formats.md#manifest).
- **A version error**: `version` is not semantic versioning (`1.2`, `1.2.1.0`).

### `network`

No endpoint could be reached: offline, DNS, TLS, a proxy, a timeout, or a
redirect to `http`. Expected on a device without a connection; retry later.

From `download()` it is also a status other than success, or more than 10
redirects. A `401` or `403` from a download host on another origin than the
endpoint is by design: [headers stay on the endpoint's origin](integrate.md#send-headers-use-a-proxy-or-a-private-ca).

### `emptyEndpoints` / `insecureTransportProtocol`

No `endpoints` under `plugins.ffp-ota-updater` (check the spelling of the
block: it is not `plugins.updater`), or one of them is not `https`.

### `emptyPubkey`

No `pubkey` under `plugins.ffp-ota-updater` and none given to
`Builder::pubkey`. Without it no bundle can be verified, so none is served
either; bundles already stored are kept until a key is configured.

## Errors from `download()` and `install()`

### `cannotServe`

`install()` was called in a run of the app that cannot show a downloaded
frontend: under `tauri dev`, or without `wrap_assets(&mut context)`. The
bundle stays stored. See [the update downloads, but the app looks the
same](#the-update-downloads-but-the-app-looks-the-same).

### `signature`

The bundle is not signed by the key in `pubkey`.

- `pubkey` is a file path. It has to be the content of the `.pub` file.
- `signature` in the manifest is a path or URL. It has to be the content of
  the `.sig` file.
- The bundle was signed with a different key than the app was built with.
- The tarball was rebuilt or modified after signing, or the manifest points at
  another file than the one that was signed.

### `missingSignedVersion`

The signature does not record a version. Sign with
`tauri signer sign --app-version <version>` (Tauri CLI 2.12 or newer). Bundles
signed by other means need `"requireSignedVersion": false`, which
[weakens the protection](../explanation/how-it-works.md#security) against a
tampered manifest.

### `signedVersionMismatch`

`--app-version` and the manifest's `version` differ. They must be the same
version.

### `invalidBundle`

The message says what is wrong with the archive.

- `no index.html at the root of the archive`: the folder itself was packed.
  Use `tar -czf frontend.tar.gz -C dist .`, not `tar -czf frontend.tar.gz dist`.
- A link or special file, a path with `..`, or the same file twice.
- Not a gzipped tarball (a `.zip`, or an HTML error page saved as the file),
  or one that is cut short.

### `tooLarge`

The tarball exceeds `maxBundleSize` (64 MiB) or its files exceed
`maxUnpackedSize` (256 MiB). Raise the limits in the configuration of the
*next full release*; installed apps keep the limits they were built with.

### `insecureTransportProtocol`

The manifest's `url` is not `https`.

### `noPendingUpdate`

`download()` was called for a version that no `check()` returned in this run
of the app. Call `check()` first. To apply a bundle that is already stored,
use `install(version)`.

### `notDownloaded`, `notNewer`, `inUse`, `rejected`, `incompatible`

| Kind | Meaning |
| --- | --- |
| `notDownloaded` | `install()` for a version that is not stored. Download it first. |
| `notNewer` | The version is not newer than the frontend being served. Versions only move forward. |
| `inUse` | `discard()` of the bundle being served, or of the confirmed one kept as the fallback. From `download()`: this device already stores other contents under that version. A version was published twice with different files; publish the new build under a new version. |
| `rejected` | The version was rolled back on this device before. |
| `incompatible` | The version is not a frontend update for this binary (see the first row of the `null` table above). |

### `unknown` / "command not allowed"

The call did not reach the plugin.

- The capability lacks `ffp-ota-updater:default`. `rollback()` and `reset()`
  additionally need `ffp-ota-updater:allow-rollback` / `allow-reset`.
- The plugin is not registered (for example on mobile).

### `io`

A file in the [store](../reference/formats.md#store) could not be read or
written: permissions, a full disk, a file held by another program. While
`state.json` cannot be read, the app serves the built-in frontend and changes
nothing, and goes back to normal at the first launch that can read it.

## After the update, styles or scripts are blocked

- **`dangerousDisableAssetCspModification`** is set. Then neither Tauri nor
  the plugin adds nonces or hashes, for the built-in frontend or a bundle, and
  inline code needs `'unsafe-inline'` or a hash in the policy itself.
- **A new host** (API, fonts, images) is not in the CSP. The CSP is part of
  the binary; adding a host needs a full release.

## After the update, a command fails or is "not allowed"

The frontend calls something the installed binary does not have or permit.
See [What a frontend update can change](../explanation/frontend-only-updates.md).

## Signing fails

| Message | Cause | Fix |
| --- | --- | --- |
| `failed to decode base64 secret key` | `TAURI_SIGNING_PRIVATE_KEY` holds a file path. `tauri build` accepts that, `tauri signer sign` does not. | Put the path in `TAURI_SIGNING_PRIVATE_KEY_PATH`, or the key's content in `TAURI_SIGNING_PRIVATE_KEY`. |
| It waits for a password | No password was given and the terminal is interactive. | `export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""` for a key without one. |
| `unexpected argument '--app-version'` | Tauri CLI older than 2.12. | Update `@tauri-apps/cli`. |

## Full updates stopped working after I started publishing frontend updates

On GitHub, a frontend-only release became the repository's "latest" release,
so `releases/latest/download/latest.json` returns 404. Mark that release as a
pre-release (or delete it) and
[publish frontend bundles under a fixed tag](publish-github-releases.md).
