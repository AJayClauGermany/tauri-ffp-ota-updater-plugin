# Tauri Plugin ffp-ota-updater

Over-the-air updates for the **frontend** of a [Tauri v2](https://v2.tauri.app)
app. Where [`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/)
downloads and installs a whole new application, this plugin downloads a signed
tarball of your web assets and serves it in place of the ones embedded in the
binary. Design tweaks and small fixes ship without a binary download or an app
restart.

It is modelled on the updater plugin and meant to sit next to it: same signing
keys and `tauri signer` tooling, same manifest fields, same
`check()` / `download()` / `install()` API.

| | `tauri-plugin-updater` | this plugin |
| --- | --- | --- |
| Replaces | the application | the frontend assets |
| Artifact | installer / app bundle per platform, signed by `tauri build` | one `frontend.tar.gz` for all platforms, signed by you |
| Applies | after restart | on webview reload, or on next launch; optionally only once the user agreed |
| Default scope | any newer version | newer **patch** versions of the installed binary |
| Safety net | none | a frontend that does not call `notifyReady()` is rolled back |

Desktop only (Windows, macOS, Linux), like the updater plugin.

## Quick start

Five steps: four in the app, one to publish. The
[tutorial](docs/tutorial.md) walks through the same thing with the example app
and a local server, start to finish.

### 1. Install

```toml
# src-tauri/Cargo.toml
[target.'cfg(not(any(target_os = "android", target_os = "ios")))'.dependencies]
tauri-plugin-ffp-ota-updater = { git = "https://github.com/AJayClauGermany/tauri-ffp-ota-updater-plugin", tag = "v1.0.0" }
```

```bash
npm install https://github.com/AJayClauGermany/tauri-ffp-ota-updater-plugin/releases/download/v1.0.0/tauri-plugin-ffp-ota-updater-api-1.0.0.tgz
```

> The crate and the npm package (`tauri-plugin-ffp-ota-updater-api`) are not
> on crates.io / npm yet. The crate is installed from the release's git tag,
> the JS package from the file attached to the
> [release](https://github.com/AJayClauGermany/tauri-ffp-ota-updater-plugin/releases),
> which is built already. Give both the same version: the two halves talk to
> each other and have to match.
> To install the JS package from git instead, use
> `npm install "github:AJayClauGermany/tauri-ffp-ota-updater-plugin#v1.0.0"`;
> it is then built on your machine while it installs, which needs a package
> manager that runs the `prepare` script of a git dependency.
> `tauri add` does not know this plugin. Without a bundler, call the commands
> through `invoke` as [`examples/tauri-app`](examples/tauri-app/src/main.js)
> does.
>
> `serde_json` has to be a dependency of your app as well. Tauri's templates
> add it; with a `plugins` block in `tauri.conf.json`, the code that
> `generate_context!` produces refers to it.

### 2. Register

Unlike most plugins this one needs an extra line, `wrap_assets(&mut context)`,
which is what allows it to replace the embedded assets. That means the context
has to be a variable instead of being created inside `.run(...)`:

```rust
// src-tauri/src/lib.rs
pub fn run() {
    let mut context = tauri::generate_context!();
    tauri_plugin_ffp_ota_updater::wrap_assets(&mut context);

    tauri::Builder::default()
        .plugin(tauri_plugin_ffp_ota_updater::init())
        .run(context)
        .expect("error while running tauri application");
}
```

Register it on the `Builder` as shown, not from inside `.setup()` the way the
updater's documentation registers that plugin: this plugin has to choose the
frontend before the first window is created.

The plugin is desktop-only, like the updater. An app that also builds for
Android or iOS puts both lines under `#[cfg(desktop)]`:
[Apps that also build for mobile](docs/how-to/integrate.md#apps-that-also-build-for-mobile).

### 3. Configure

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

The key pair can be the updater's: reuse the `pubkey` you already have, or
create one with `npm run tauri signer generate -- -w ~/.tauri/myapp.key`.
[When a pair of its own is worth it](docs/explanation/how-it-works.md#security).

Grant the permission in `src-tauri/capabilities/default.json`:

```json
{ "permissions": ["core:default", "ffp-ota-updater:default"] }
```

### 4. Call it from the frontend

```js
import { check, notifyReady } from "tauri-plugin-ffp-ota-updater-api";

// On every start, once the app works. A downloaded frontend that never
// gets here is rolled back.
await notifyReady();

try {
  const update = await check();
  if (update) await update.downloadAndInstall(); // reloads into the new frontend
} catch (error) {
  console.warn("no frontend update:", error); // offline, for instance
}
```

**Every frontend you publish must call `notifyReady()`.** One that does not is
taken to be broken and reverted on the third launch.

### 5. Publish a frontend update

Build the frontend as usual, then package, sign and describe it. The version
is a new **patch** of the app that is installed (`1.2.0` → `1.2.1`):

```bash
tar -czf frontend-1.2.1.tar.gz -C dist .
```

```bash
npm run tauri signer sign -- --app-version 1.2.1 frontend-1.2.1.tar.gz
```

```json
{
  "version": "1.2.1",
  "url": "https://releases.example.com/frontend-1.2.1.tar.gz",
  "signature": "CONTENT OF frontend-1.2.1.tar.gz.sig"
}
```

Upload the tarball, then the manifest to the URL in `endpoints`. Step-by-step
guides: [GitHub Releases](docs/how-to/publish-github-releases.md),
[CrabNebula Cloud](docs/how-to/publish-crabnebula.md),
[any other host](docs/how-to/publish.md).

`tauri dev` always shows the dev server's frontend, so try an update with a
release build (`tauri build`).

## Documentation

| I want to… | Read |
| --- | --- |
| **learn** it by shipping one update locally | [Tutorial: your first frontend update](docs/tutorial.md) |
| **publish** updates | [Anywhere](docs/how-to/publish.md) · [GitHub Releases](docs/how-to/publish-github-releases.md) · [CrabNebula Cloud](docs/how-to/publish-crabnebula.md) |
| **use it next to `tauri-plugin-updater`** | [Use with the updater plugin](docs/how-to/use-with-updater-plugin.md), including what differs if you know that plugin |
| **build the update experience** (asking the user first, staging, channels, several pages or windows, no-reload switching, tying a frontend to a native build) | [Integrate updates into your app](docs/how-to/integrate.md) |
| **see it** in an app that asks first and has two windows | [`examples/consent-app`](examples/consent-app), which CI runs in a real webview on Linux, macOS and Windows |
| **fix** something | [Troubleshooting](docs/how-to/troubleshoot.md) |
| **look up** a key, function, error or format | [Configuration](docs/reference/configuration.md) · [API](docs/reference/api.md) · [Manifest, bundle and store](docs/reference/formats.md) |
| **understand** the design | [How it works](docs/explanation/how-it-works.md) · [What a frontend update can change](docs/explanation/frontend-only-updates.md) |

Working on the plugin itself: [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[Boost Software License 1.0](LICENSE).
