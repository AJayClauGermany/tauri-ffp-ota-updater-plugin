# Configuration reference

- [Registration](#registration)
- [`tauri.conf.json`](#tauriconfjson)
- [Endpoint variables](#endpoint-variables)
- [Builder](#builder)
- [Permissions](#permissions)
- [Fixed limits](#fixed-limits)

## Registration

| Item | |
| --- | --- |
| Crate | `tauri-plugin-ffp-ota-updater` |
| npm package | `tauri-plugin-ffp-ota-updater-api` |
| Plugin name (config block, permission prefix, command prefix) | `ffp-ota-updater` |
| Platforms | Windows, macOS, Linux |
| Minimum Rust | see `rust-version` in `Cargo.toml` |

| Function | |
| --- | --- |
| `wrap_assets(&mut context)` | Wraps the context's embedded assets so that a bundle can be served in their place. Call once, before `tauri::Builder::run(context)`. Without it, checking for and downloading bundles works, but none is ever served: `install()` fails with `cannotServe`, `currentVersion()` is the app's version, and a warning is logged at startup. |
| `init()` | The plugin with the configuration from `tauri.conf.json`. Same as `Builder::new().build()`. |
| `Builder::new()…build()` | The plugin with settings from Rust; see [Builder](#builder). |

The plugin selects the frontend while it is being set up, so it must be added
with `tauri::Builder::plugin`, before any window exists.

## `tauri.conf.json`

Under `plugins.ffp-ota-updater`. All keys are optional in the file;
`endpoints` and `pubkey` are needed for `check()` and for accepting a bundle
and can be given on the [Builder](#builder) instead.

| Key | Type | Default | |
| --- | --- | --- | --- |
| `endpoints` | string[] | `[]` | Manifest URLs, tried in order. May contain [variables](#endpoint-variables). Must be `https`. |
| `pubkey` | string | `""` | Minisign public key: the content of the `.pub` file written by `tauri signer generate`. A key that cannot be read is a setup error. Without a key, `check()` fails with `emptyPubkey` and stored bundles are neither served nor deleted. |
| `dangerousInsecureTransportProtocol` | boolean | `false` | Allow non-`https` endpoints, bundle URLs and redirects. |
| `requireSignedVersion` | boolean | `true` | Reject signatures that do not record the version they were made for. Needs bundles signed with `tauri signer sign --app-version` (Tauri CLI 2.12 or newer). |
| `rollback` | boolean | `true` | Revert a bundle the frontend does not confirm with `notifyReady()`. With `false`, bundles are served without a trial. |
| `trialAttempts` | integer | `2` | Runs of the app in which a new bundle may be shown without confirming itself before it is rejected. Values below 1 count as 1. |
| `readyTimeout` | integer (ms) | off | Time a bundle on trial gets, from the first page loaded from it, to call `notifyReady()` before it is rejected and replaced while the app is running. `0` is off. |
| `compareBuildMetadata` | boolean | `false` | Order versions that differ only in build metadata by it (`1.2.1+2` after `1.2.1+1`, `1.2.0+1` after the binary `1.2.0`). By default build metadata is ignored when deciding what is newer. See [Versions](../explanation/how-it-works.md#versions). |
| `requireApproval` | boolean | `false` | Serve a stored bundle only once the app has chosen it: with `approve()` for the next launch, or with `install()` / `activate()` at once. Until then it stays stored, across restarts, and is reported as `staged`. With `false`, the next launch switches to the newest stored bundle by itself. See [Approval](../explanation/how-it-works.md#approval). |
| `requiredFiles` | string[] | `[]` | Files every bundle has to contain besides `index.html`, as paths from the root of the frontend build (`settings.html`, `editor/index.html`). A bundle that lacks one is refused with `invalidBundle`, when it is downloaded or staged and whenever it is loaded. An entry that cannot be a path in a bundle (`..`, a backslash) is a setup error. |
| `maxBundleSize` | integer (bytes) | `67108864` (64 MiB) | Largest `.tar.gz` accepted. |
| `maxUnpackedSize` | integer (bytes) | `268435456` (256 MiB) | Largest total size of the files in a bundle. They are held in memory. |

A non-`https` entry in `endpoints` without
`dangerousInsecureTransportProtocol` is a setup error: the app does not start.
The same goes for a `pubkey` that cannot be read and an entry of
`requiredFiles` that cannot be a path.

```json
{
  "plugins": {
    "ffp-ota-updater": {
      "endpoints": ["https://releases.example.com/ota.json"],
      "pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6…",
      "readyTimeout": 20000
    }
  }
}
```

## Endpoint variables

Replaced in each endpoint URL before the request.

| Variable | Value | Example |
| --- | --- | --- |
| `{{current_version}}` | Version of the frontend being served: the active bundle's, or else the app's. | `1.2.1` |
| `{{binary_version}}` | Version of the app (`version` in `tauri.conf.json`). | `1.2.0` |
| `{{target}}` | `linux`, `windows` or `darwin`. | `darwin` |
| `{{arch}}` | `x86_64`, `aarch64`, `i686` or `armv7`. | `aarch64` |

Versions are percent-encoded (`+` becomes `%2B`).

Responses:

| Status | Meaning |
| --- | --- |
| `204` | No update. No further endpoint is tried. |
| other `2xx` with a valid [manifest](formats.md#manifest) | The release to consider. |
| anything else, an unreachable host, an invalid body | The next endpoint is tried. |

Requests carry `Accept: application/json` (manifest) or
`Accept: application/octet-stream` (bundle) unless a header overrides it, and
`User-Agent: tauri-plugin-ffp-ota-updater/<version>`.

Redirects (`301`, `302`, `303`, `307`, `308`) are followed, at most 10 for a
request. A request that a redirect sends to another origin than the
endpoint's carries none of the configured headers, and no `Referer`.

## Builder

`tauri_plugin_ffp_ota_updater::Builder`. Values set here take precedence over
`tauri.conf.json`.

| Method | |
| --- | --- |
| `pubkey(impl Into<String>)` | Overrides `pubkey`. |
| `endpoints(Vec<Url>)` | Overrides `endpoints`. |
| `header(key, value) -> Result<Self>` | Adds a header sent with every manifest request, and with bundle downloads on the endpoint's origin (scheme, host and port). Never sent to another origin, including by a redirect. |
| `headers(HeaderMap)` | Replaces those headers. |
| `timeout(Duration)` | Deadline for each request, instead of 30 s (manifest) and 30 min (bundle). A redirect starts a new request. |
| `proxy(Url)` | Use this proxy instead of the system's. |
| `no_proxy()` | Ignore the system's proxy settings. |
| `configure_client(Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder)` | Runs last on the HTTP client, so it can override everything above, including the `https`-only rule. Not the handling of redirects: the plugin follows them itself. `reqwest` is re-exported as `tauri_plugin_ffp_ota_updater::reqwest`. |
| `compatibility(Fn(&Version, &Version) -> bool)` | Receives `(binary_version, bundle_version)` and says whether that binary can run that bundle. Default: [`default_compatibility`](api.md#functions). Applied to releases found by `check`, and whenever a bundle is stored or loaded; at startup a stored bundle that fails it is deleted. Independently of the rule, a bundle has to be newer than the binary, and is only offered if it is newer than the frontend being served. |

### TLS

Requests use [rustls](https://docs.rs/rustls). If no process-wide rustls
crypto provider is installed when the first request is made, the plugin
installs *ring*'s, as `tauri-plugin-updater` does. An app that wants another
provider installs it before then, for example at the start of `main`.
| `build()` | The plugin. |

Per-check settings: [`RequestOptions`](api.md#requestoptions).

## Permissions

| Permission | Allows |
| --- | --- |
| `ffp-ota-updater:default` | `check`, `download`, `install`, `download-and-install`, `current-version`, `notify-ready`, `status`, `discard`, `approve`, `unapprove` |
| `ffp-ota-updater:allow-rollback` | `rollback()`. Not in `default`. |
| `ffp-ota-updater:allow-reset` | `reset()`. Not in `default`. |

Every command has an `allow-<command>` and a `deny-<command>` permission; the
full list is in
[`permissions/autogenerated/reference.md`](../../permissions/autogenerated/reference.md).
Permissions apply to calls from the webview. The Rust API is not subject to
them.

Each permission is for one command, not for what it does: `download-and-install`
installs without `install` being allowed, so denying `install` alone does not
keep a webview from switching frontends. A webview that may fetch updates but
not apply them gets the commands by name instead of `default`:

```json
{
  "permissions": [
    "core:default",
    "ffp-ota-updater:allow-check",
    "ffp-ota-updater:allow-download",
    "ffp-ota-updater:allow-status",
    "ffp-ota-updater:allow-current-version",
    "ffp-ota-updater:allow-notify-ready"
  ]
}
```

What it downloads is still applied at the next launch; only `discard` or a
decision in Rust prevents that. With `requireApproval` it is not: nothing is
applied until `approve`, `install` or `download-and-install` is called, so a
webview without those three cannot switch frontends, and the decision can be
left to Rust.

Capabilities are per window. In an app with several windows, a window that
should not be able to confirm a bundle (a splash screen, say) gets a
capability without `allow-notify-ready`: see
[Apps with several windows](../how-to/integrate.md#apps-with-several-windows).

The options a webview passes to `check` (`headers`, `timeout`, `proxy`)
replace the builder's for that check. To keep them out of the webview's hands,
leave out `allow-check` and check from Rust.

## Fixed limits

| | |
| --- | --- |
| Manifest size | 1 MiB |
| Redirects followed for a request | 10 |
| Files and directories in a bundle | 20 000 |
| Time without progress before a request is given up | 30 s |
