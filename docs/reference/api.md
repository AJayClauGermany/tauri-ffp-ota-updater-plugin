# API reference

- [JavaScript](#javascript)
- [Commands](#commands)
- [Rust](#rust)
- [Errors](#errors)

## JavaScript

Package `tauri-plugin-ffp-ota-updater-api`. Every function rejects with an
[`OtaUpdateError`](#errors).

### Functions

| Function | Returns | |
| --- | --- | --- |
| `check(options?)` | `Promise<Update \| null>` | Asks the endpoints for a release. `null` when there is none, when it is not newer than the frontend being served, when it is not compatible with the binary, or when its version was rejected on this device. |
| `notifyReady()` | `Promise<void>` | Confirms the frontend being served. Does nothing when there is no trial, or when the calling page was not loaded from the bundle on trial (a window that still shows the previous frontend, or a webview the plugin saw no page load for). |
| `status()` | `Promise<Status>` | State of the updater. Makes no request. |
| `currentVersion()` | `Promise<string>` | Version of the frontend being served: the active bundle's, or else the app's. |
| `install(version)` | `Promise<void>` | Switches to the stored bundle `version` and reloads every webview window. Makes no request. Rejects with `cannotServe` under `tauri dev`, without `wrap_assets`, or with a URL as `frontendDist`. Does nothing, and reloads nothing, when `version` is already being served. A rejection of kind `tauri` means the switch happened but a window could not be reloaded (reload it yourself; installing again does not); with any other kind nothing was switched. |
| `approve(version)` | `Promise<void>` | Approves the stored bundle `version` for the next launch, which with `requireApproval` is what makes a launch switch to it. Changes nothing in this run and makes no request. The bundle is verified first. One version is approved at a time. Rejects with `notDownloaded`, `notNewer` (older than the frontend being served) or `rejected`. Without `requireApproval` it only records the choice. |
| `unapprove(version)` | `Promise<void>` | Takes back the approval of `version`. The bundle stays stored. Does nothing if another version, or none, is approved, and nothing about a bundle that was shown before. |
| `discard(version)` | `Promise<void>` | Deletes the stored bundle `version`, and its approval with it. Does nothing if it is not stored; the version stays available to `check()`. Rejects with `inUse` for the bundle being served and for the confirmed fallback. Does not cancel a `download()` of that version that is still running, which stores it again. |
| `rollback()` | `Promise<void>` | Rejects the bundle being served and reloads into the last confirmed bundle or the built-in frontend. A newer stored bundle that was never shown is kept for the next launch. Does nothing when the built-in frontend is served. Rejects with `io` if the rejection could not be recorded: the app has gone back for this run, but the version is served or offered again after a restart. Needs `allow-rollback`. |
| `reset()` | `Promise<void>` | Deletes every stored bundle and returns to the built-in frontend, reloading if a bundle was served. Rejected versions stay rejected. Needs `allow-reset`. |

### `CheckOptions`

| Field | Type | |
| --- | --- | --- |
| `headers` | `HeadersInit` | Sent with the manifest request, and with the bundle download if it is on the endpoint's origin; never to another origin. Replace builder headers of the same name. |
| `timeout` | `number` (ms) | Deadline for each request: the manifest, and the bundle download unless `download()` sets its own. |
| `proxy` | `string` | Proxy URL for both requests. `invalidUrl` if it is not a URL. |

These replace the builder's settings for one check. Endpoints cannot be set
from the webview.

### `DownloadOptions`

The updater plugin's `DownloadOptions`, for one `download()` or
`downloadAndInstall()`. What is left out is what the check used.

| Field | Type | |
| --- | --- | --- |
| `headers` | `HeadersInit` | Instead of the check's headers (builder headers of other names still apply). Like those, only sent if the bundle is on the endpoint's origin. |
| `timeout` | `number` (ms) | Deadline for the download, instead of the check's `timeout`, or else 30 minutes. |

### `Update`

| Member | Type | |
| --- | --- | --- |
| `kind` | `"frontend"` | Distinguishes it from the updater plugin's `Update`, which has no `kind`. On a union of the two in TypeScript, test `"kind" in update` (or `instanceof` this `Update`): reading `update.kind` does not compile there. |
| `version` | `string` | Version of the update. |
| `currentVersion` | `string` | Version of the frontend being served when the check was made. |
| `downloaded` | `boolean` | The bundle is already stored on this device. |
| `date` | `string?` | `pub_date` of the manifest, as sent. |
| `body` | `string?` | `notes` of the manifest. |
| `rawJson` | `Record<string, unknown>` | The manifest as received, including fields of your own. |
| `download(onEvent?, options?)` | `Promise<void>` | Downloads, verifies and stores the bundle. Used from the next launch, or with `requireApproval` once it is approved. For a bundle that is already stored nothing is fetched and only `Finished` is sent. |
| `install()` | `Promise<void>` | `install(this.version)`. |
| `downloadAndInstall(onEvent?, options?)` | `Promise<void>` | Both. |
| `close()` | `Promise<void>` | Does nothing: this `Update` holds no resource. There so that code written for both plugins can call it. |

### `DownloadEvent`

| `event` | `data` |
| --- | --- |
| `"Started"` | `{ contentLength?: number }` |
| `"Progress"` | `{ chunkLength: number }` |
| `"Finished"` | none |

`Finished` means the last byte arrived. The bundle is verified and stored
after that: it is ready when the promise of `download()` resolves, and was
refused when it rejects.

### `Status`

| Field | Type | |
| --- | --- | --- |
| `binaryVersion` | `string` | Version of the app, i.e. of the built-in frontend. |
| `currentVersion` | `string` | Version of the frontend being served. |
| `confirmed` | `boolean` | `false` while the frontend being served is a bundle on trial. |
| `staged` | `string?` | Newest stored bundle that is newer than the one being served: what the next launch switches to, unless `requireApproval` is set. Not yet verified. |
| `approved` | `string?` | The stored bundle that was approved and is newer than the one being served: with `requireApproval`, what the next launch switches to. Can be older than `staged`. |
| `rejected` | `string[]` | Versions that failed their trial or were rolled back, oldest first. Cleared of versions a full update made irrelevant. |

## Commands

For `invoke("plugin:ffp-ota-updater|<command>", args)`. Errors are
`{ kind, message }` objects.

| Command | Arguments | Result |
| --- | --- | --- |
| `check` | `headers?: [string, string][]`, `timeout?: number`, `proxy?: string` | update metadata (`currentVersion`, `version`, `downloaded`, `date`, `body`, `rawJson`) or `null` |
| `download` | `version: string`, `onEvent: Channel<DownloadEvent>`, `headers?: [string, string][]`, `timeout?: number` | |
| `download_and_install` | `version: string`, `onEvent: Channel<DownloadEvent>`, `headers?: [string, string][]`, `timeout?: number` | |
| `install` | `version: string` | |
| `discard` | `version: string` | |
| `approve` | `version: string` | |
| `unapprove` | `version: string` | |
| `current_version` | | `string` |
| `status` | | `Status` |
| `notify_ready` | | |
| `rollback` | | |
| `reset` | | |

`download` and `download_and_install` accept only a version that a `check` in
the same run of the app returned (`noPendingUpdate` otherwise). The offers of
the 8 newest versions are kept. An offer is kept by version, since
[a version is one bundle](../explanation/how-it-works.md#a-version-is-one-bundle):
if several checks return the same version, for instance from two windows, the
latest of them decides the options (`headers`, `timeout`, `proxy`) and the
URL that version is downloaded with. `headers` and `timeout` given to
`download` itself apply to that download only.

`download_and_install` installs as well, whether or not `install` is allowed:
see [Permissions](configuration.md#permissions).

## Rust

### Functions

| Item | |
| --- | --- |
| `wrap_assets(&mut Context<R>)` | See [Registration](configuration.md#registration). |
| `init() -> TauriPlugin<R, Option<Config>>` | |
| `default_compatibility(binary: &Version, bundle: &Version) -> bool` | `true` when `bundle` has the major and minor version of `binary`. That a bundle is newer than the binary is checked by the plugin, whatever the rule. |
| `OtaUpdaterExt::ota_updater(&self) -> &OtaUpdater<R>` | On `App`, `AppHandle` and windows. Panics if the plugin is not registered. |

### `OtaUpdater`

| Method | Returns | |
| --- | --- | --- |
| `check()` | `Result<Option<Update<R>>>` (async) | As the JavaScript `check()`. |
| `check_with(RequestOptions)` | `Result<Option<Update<R>>>` (async) | With other endpoints, headers, timeout or proxy. |
| `current_version()` | `Version` | |
| `status()` | `Status` | Fields as in JavaScript, in snake case, as `Version`. |
| `stage(archive: &[u8], signature: &str, version: &Version)` | `Result<()>` | Verifies and stores a bundle obtained by other means. Same checks as a download. `NotNewer` for a version that is not newer than the one being served (other than that very version). `InUse` for other contents than those stored under that version. |
| `activate(&Version)` | `Result<()>` | Serves the stored bundle from now on, without touching the webviews. Does nothing when that version is already being served. `CannotServe` under `tauri dev` or without `wrap_assets`. With `requireApproval` this approves the bundle as well. |
| `install(&Version)` | `Result<()>` | `activate`, then `reload`; does nothing when that version is already being served. `Error::Tauri` is a failed reload after the switch. |
| `approve(&Version)` | `Result<()>` | As the JavaScript `approve()`. |
| `unapprove(&Version)` | `Result<()>` | As the JavaScript `unapprove()`. |
| `discard(&Version)` | `Result<()>` | |
| `notify_ready()` | `Result<()>` | Confirms whatever is being served, unlike the JavaScript `notifyReady()`, which only counts from a page of the bundle on trial. |
| `rollback()` | `Result<()>` | As the JavaScript `rollback()`. |
| `reset()` | `Result<()>` | |
| `reload()` | `Result<()>` | Reloads every webview window. All are tried; the first error is returned. Webviews that share a window (Tauri's `unstable` feature) are not included: reload those with `Webview::reload`. |

### `Update`

Fields: `version`, `current_version` (`Version`), `downloaded` (`bool`),
`date`, `body` (`Option<String>`), `raw_json` (`serde_json::Value`),
`download_url` (`Url`), `signature` (`String`).

| Method | |
| --- | --- |
| `download(on_chunk: FnMut(usize, Option<u64>), on_download_finish: FnOnce())` (async) | `on_chunk` receives the chunk length and the content length, if known. |
| `activate()` | `OtaUpdater::activate` for this version. |
| `install()` | `OtaUpdater::install` for this version. |
| `download_and_install(on_chunk, on_download_finish)` (async) | |

### `RequestOptions`

What is left unset falls back to the builder's setting.

| Method | |
| --- | --- |
| `new()` | |
| `endpoints(Vec<Url>)` | Manifest URLs for this check, instead of the configured ones. |
| `header(key, value) -> Result<Self>` | Replaces a builder header of the same name. |
| `timeout(Duration)` | |
| `proxy(Url)` | |

The download of the release a check finds uses that check's options.

### Other exports

`Builder` ([reference](configuration.md#builder)), `Config`, `Compatibility`,
`Status`, `Error`, `Result`, `reqwest`.

## Errors

`OtaUpdateError` (JavaScript) has `kind` and `message`. In Rust, `Error` is a
non-exhaustive enum and `Error::kind()` returns the same string.

| `kind` | Rust variant | When |
| --- | --- | --- |
| `network` | `Reqwest`, `TooManyRedirects` | No endpoint or download host could be reached, a timeout, a non-success status for the bundle, more than 10 redirects. |
| `releaseNotFound` | `ReleaseNotFound` | An endpoint answered, but none with a usable manifest. That includes a manifest over 1 MiB; the message has the reason. |
| `emptyEndpoints` | `EmptyEndpoints` | No endpoints are configured. |
| `emptyPubkey` | `EmptyPubkey` | No public key is configured. |
| `invalidConfig` | `InvalidConfig` | A value of the configuration cannot be used (an entry of `requiredFiles`). Raised at setup, where it keeps the app from starting; the webview does not see it. |
| `cannotServe` | `CannotServe` | `install`/`activate` while bundles cannot be served: under `tauri dev`, without `wrap_assets`, or with a URL as `frontendDist`. |
| `insecureTransportProtocol` | `InsecureTransportProtocol` | An endpoint or bundle URL is not `https`. |
| `invalidSignature` | `InvalidSignature` | The public key or signature is unreadable, or the signature is not this key's for this bundle. |
| `missingSignedVersion` | `MissingSignedVersion` | `requireSignedVersion` is on and the signature records no version. |
| `signedVersionMismatch` | `SignedVersionMismatch` | Signed for another version than the one announced. |
| `invalidBundle` | `InvalidBundle` | The archive is not a usable frontend bundle, or lacks a file named in `requiredFiles`. |
| `tooLarge` | `TooLarge` | A bundle, or what it unpacks to, exceeds its size limit. |
| `incompatible` | `Incompatible` | The version is not a frontend update for this binary. |
| `rejected` | `Rejected` | The version failed a trial or was rolled back before. |
| `noPendingUpdate` | `NoPendingUpdate` | `download` for a version `check` did not return. |
| `notDownloaded` | `NotDownloaded` | `install` or `approve` for a version that is not stored. |
| `notNewer` | `NotNewer` | The version is not newer than the frontend being served. |
| `inUse` | `InUse` | `discard` of the bundle being served or of the confirmed fallback; `download` or `stage` of other contents than those stored under that version, or of a version that differs from a stored one only in letter case. |
| `invalidHeader` | `InvalidHeaderName`, `InvalidHeaderValue`, `Http` | |
| `invalidUrl` | `UrlParse` | |
| `invalidVersion` | `Semver` | |
| `io` | `Io` | A file of the store could not be read or written. From `rollback()`: the rejection was not recorded. While the store's state cannot be read, the built-in frontend is served and bundles are neither accepted nor installed. |
| `json` | `Json` | |
| `tauri` | `Tauri` | From `install()`, `rollback()` and `reset()`: a window could not be reloaded, after the switch. Reload or recreate it; installing the same version again does nothing. |
| `unknown` | none | JavaScript only: the rejection did not come from the plugin (a denied permission, a plugin that is not registered). |

New kinds may be added in minor releases.
