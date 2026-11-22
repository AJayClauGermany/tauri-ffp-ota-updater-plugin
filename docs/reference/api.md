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
| `notifyReady()` | `Promise<void>` | Confirms the frontend being served. Does nothing when there is no trial, or when the calling page was not loaded from the bundle on trial (a window that still shows the previous frontend). |
| `status()` | `Promise<Status>` | State of the updater. Makes no request. |
| `currentVersion()` | `Promise<string>` | Version of the frontend being served: the active bundle's, or else the app's. |
| `install(version)` | `Promise<void>` | Switches to the stored bundle `version` and reloads every webview. Makes no request. Rejects with `cannotServe` under `tauri dev`, without `wrap_assets`, or with a URL as `frontendDist`. |
| `discard(version)` | `Promise<void>` | Deletes the stored bundle `version`. Does nothing if it is not stored; the version stays available to `check()`. Rejects with `inUse` for the bundle being served and for the confirmed fallback. |
| `rollback()` | `Promise<void>` | Rejects the bundle being served and reloads into the last confirmed bundle or the built-in frontend. A newer stored bundle that was never shown is kept for the next launch. Does nothing when the built-in frontend is served. Needs `allow-rollback`. |
| `reset()` | `Promise<void>` | Deletes every stored bundle and returns to the built-in frontend, reloading if a bundle was served. Rejected versions stay rejected. Needs `allow-reset`. |

### `CheckOptions`

| Field | Type | |
| --- | --- | --- |
| `headers` | `HeadersInit` | Sent with the manifest request, and with the bundle download if it is on the manifest's origin. Replace builder headers of the same name. |
| `downloadHeaders` | `HeadersInit` | Sent with the bundle download only, wherever it is. Replace builder download headers of the same name. |
| `timeout` | `number` (ms) | Deadline for each request. |
| `proxy` | `string` | Proxy URL for both requests. |

Endpoints cannot be set from the webview.

### `Update`

| Member | Type | |
| --- | --- | --- |
| `kind` | `"frontend"` | Distinguishes it from the updater plugin's `Update`. |
| `version` | `string` | Version of the update. |
| `currentVersion` | `string` | Version of the frontend being served when the check was made. |
| `downloaded` | `boolean` | The bundle is already stored on this device. |
| `date` | `string?` | `pub_date` of the manifest, as sent. |
| `body` | `string?` | `notes` of the manifest. |
| `rawJson` | `Record<string, unknown>` | The manifest as received, including fields of your own. |
| `download(onEvent?)` | `Promise<void>` | Downloads, verifies and stores the bundle. Used from the next launch. For a bundle that is already stored nothing is fetched and only `Finished` is sent. |
| `install()` | `Promise<void>` | `install(this.version)`. |
| `downloadAndInstall(onEvent?)` | `Promise<void>` | Both. |

### `DownloadEvent`

| `event` | `data` |
| --- | --- |
| `"Started"` | `{ contentLength?: number }` |
| `"Progress"` | `{ chunkLength: number }` |
| `"Finished"` | none |

### `Status`

| Field | Type | |
| --- | --- | --- |
| `binaryVersion` | `string` | Version of the app, i.e. of the built-in frontend. |
| `currentVersion` | `string` | Version of the frontend being served. |
| `confirmed` | `boolean` | `false` while the frontend being served is a bundle on trial. |
| `staged` | `string?` | Newest stored bundle that is newer than the one being served: what the next launch switches to. Not yet verified. |
| `rejected` | `string[]` | Versions that failed their trial or were rolled back, oldest first. Cleared of versions a full update made irrelevant. |

## Commands

For `invoke("plugin:ffp-ota-updater|<command>", args)`. Errors are
`{ kind, message }` objects.

| Command | Arguments | Result |
| --- | --- | --- |
| `check` | `headers?: [string, string][]`, `downloadHeaders?: [string, string][]`, `timeout?: number`, `proxy?: string` | update metadata (`currentVersion`, `version`, `downloaded`, `date`, `body`, `rawJson`) or `null` |
| `download` | `version: string`, `onEvent: Channel<DownloadEvent>` | |
| `download_and_install` | `version: string`, `onEvent: Channel<DownloadEvent>` | |
| `install` | `version: string` | |
| `discard` | `version: string` | |
| `current_version` | | `string` |
| `status` | | `Status` |
| `notify_ready` | | |
| `rollback` | | |
| `reset` | | |

`download` and `download_and_install` accept only a version that a `check` in
the same run of the app returned (`noPendingUpdate` otherwise).

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
| `stage(archive: &[u8], signature: &str, version: &Version)` | `Result<()>` | Verifies and stores a bundle obtained by other means. Same checks as a download. `NotNewer` for a version that is not newer than the one being served (other than that very version). |
| `activate(&Version)` | `Result<()>` | Serves the stored bundle from now on, without touching the webviews. `CannotServe` under `tauri dev` or without `wrap_assets`. |
| `install(&Version)` | `Result<()>` | `activate`, then `reload`. |
| `discard(&Version)` | `Result<()>` | |
| `notify_ready()` | `Result<()>` | Confirms whatever is being served, unlike the JavaScript `notifyReady()`, which only counts from a page of the bundle on trial. |
| `rollback()` | `Result<()>` | As the JavaScript `rollback()`. |
| `reset()` | `Result<()>` | |
| `reload()` | `Result<()>` | Reloads every webview window. |

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
| `network` | `Reqwest` | No endpoint or download host could be reached, a timeout, a non-success status for the bundle. |
| `releaseNotFound` | `ReleaseNotFound` | An endpoint answered, but none with a usable manifest. |
| `emptyEndpoints` | `EmptyEndpoints` | No endpoints are configured. |
| `emptyPubkey` | `EmptyPubkey` | No public key is configured. |
| `cannotServe` | `CannotServe` | `install`/`activate` while bundles cannot be served: under `tauri dev`, without `wrap_assets`, or with a URL as `frontendDist`. |
| `insecureTransportProtocol` | `InsecureTransportProtocol` | An endpoint or bundle URL is not `https`. |
| `signature` | `InvalidSignature` | The public key or signature is unreadable, or the signature is not this key's for this bundle. |
| `missingSignedVersion` | `MissingSignedVersion` | `requireSignedVersion` is on and the signature records no version. |
| `signedVersionMismatch` | `SignedVersionMismatch` | Signed for another version than the one announced. |
| `invalidBundle` | `InvalidBundle` | The archive is not a usable frontend bundle. |
| `tooLarge` | `TooLarge` | A manifest or bundle exceeds its size limit. |
| `incompatible` | `Incompatible` | The version is not a frontend update for this binary. |
| `rejected` | `Rejected` | The version failed a trial or was rolled back before. |
| `noPendingUpdate` | `NoPendingUpdate` | `download` for a version `check` did not return. |
| `notDownloaded` | `NotDownloaded` | `install` for a version that is not stored. |
| `notNewer` | `NotNewer` | The version is not newer than the frontend being served. |
| `inUse` | `InUse` | `discard` of the bundle being served or of the confirmed fallback, or `stage` of different contents under the version that is confirmed or on trial. |
| `invalidHeader` | `InvalidHeaderName`, `InvalidHeaderValue`, `Http` | |
| `invalidUrl` | `UrlParse` | |
| `invalidVersion` | `Semver` | |
| `io` | `Io` | |
| `json` | `Json` | |
| `tauri` | `Tauri` | |
| `unknown` | none | JavaScript only: the rejection did not come from the plugin (a denied permission, a plugin that is not registered). |

New kinds may be added in minor releases.
