# Consent example

An app with two windows that downloads frontend updates ahead of time and
switches only when the user agrees. It shows the pieces an app like that
needs:

| | Where |
| --- | --- |
| A downloaded frontend waits, across restarts, until it is approved | `requireApproval` in [`src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json) |
| The user's answer: now, at the next start, not after all, skip | the buttons in [`src/main.js`](src/main.js) |
| Checking and downloading happen in Rust, not in a webview | `fetch_update` in [`src-tauri/src/lib.rs`](src-tauri/src/lib.rs) |
| Only the main window may confirm a frontend, not the bootstrap window | [`src-tauri/capabilities`](src-tauri/capabilities) |
| A frontend without the bootstrap window's page is refused | `requiredFiles` in `tauri.conf.json` |
| A Content Security Policy, which downloaded frontends pass too | `app.security.csp` in `tauri.conf.json` |

The recipes behind it are in
[Integrate updates into your app](../../docs/how-to/integrate.md).

## Try it by hand

Follow the [tutorial](../../docs/tutorial.md) with this folder in place of
`examples/tauri-app`: create a key, put the public key into `tauri.conf.json`,
build, and publish a changed `src` folder to `http://localhost:8000`. The app
downloads it when it starts and then waits for you.

## Run it end to end

```bash
npm install
```

```bash
npm run e2e
```

On Linux without a display, `xvfb-run -a npm run e2e`.

[`e2e.mjs`](e2e.mjs) builds the app with a key of its own, publishes
frontends to a server on localhost and clicks the app's buttons, in the
webview of the platform it runs on. It checks that

1. an update that was downloaded changes nothing, also after a restart;
2. approving it and switching to it work without a connection;
3. the main page confirms the new frontend, and the bootstrap window is not
   allowed to;
4. a frontend whose main page is broken is taken back, offline, in every
   window;
5. a frontend that lacks the bootstrap page is refused;
6. inline scripts of a downloaded frontend run under the app's Content
   Security Policy.

CI runs this on Linux, macOS and Windows.
