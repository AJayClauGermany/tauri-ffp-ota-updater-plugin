# Tutorial: your first frontend update

In this tutorial you build the example app, publish a new frontend for it from
your own machine, and watch the running app switch to it without being
rebuilt. Then you publish a broken frontend and watch the plugin take it back.

It takes about 15 minutes and needs nothing but this repository: the "release
server" is a folder served on `localhost`.

You need the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/),
Node.js, `tar`, [`jq`](https://jqlang.org) and Python 3 (only to serve a
folder; any static file server will do).

## 1. Get the example

```bash
git clone https://github.com/AJayClauGermany/tauri-ffp-ota-updater-plugin
```

```bash
cd tauri-ffp-ota-updater-plugin/examples/tauri-app
```

```bash
npm install
```

Everything below is run from `examples/tauri-app`.

The example is already set up for the plugin. Its
`src-tauri/tauri.conf.json` asks `http://localhost:8000/ota.json` for updates,
and its version is `0.1.0`.

## 2. Create a signing key

Frontend bundles are signed, and the app only accepts bundles signed with your
key.

```bash
npm run tauri signer generate -- -w ~/.tauri/ota-tutorial.key
```

Press Enter twice when asked for a password. Two files are written: the
private key `ota-tutorial.key`, and the public key `ota-tutorial.key.pub`.

Open `src-tauri/tauri.conf.json` and set `pubkey`, which is empty, to the
content of the `.pub` file (the content, not its path):

```bash
cat ~/.tauri/ota-tutorial.key.pub
```

## 3. Build and start the app

A downloaded frontend is never shown under `tauri dev`, so make a release
build:

```bash
npm run tauri build -- --no-bundle
```

Start it:

```bash
./src-tauri/target/release/tauri-app
```

(On Windows: `src-tauri\target\release\tauri-app.exe`.)

The window shows **Frontend version: 0.1.0**. Click **Check for frontend
update**. The output says `Error: [network] …`: nothing is listening on port
8000 yet. Leave the app running.

## 4. Change the frontend

In `src/index.html`, change the heading:

```html
<h1>Updated over the air</h1>
```

The running app does not change: it serves the frontend that was compiled
into it.

## 5. Package and sign the new frontend

In a second terminal, in `examples/tauri-app`, put the frontend into a tarball
in a new `ota` folder. The version is `0.1.1`: a newer patch of the app you
built.

```bash
mkdir -p ota && tar -czf ota/frontend-0.1.1.tar.gz -C src .
```

Sign it for that version:

```bash
TAURI_SIGNING_PRIVATE_KEY_PATH=~/.tauri/ota-tutorial.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
  npm run tauri signer sign -- --app-version 0.1.1 ota/frontend-0.1.1.tar.gz
```

This writes `ota/frontend-0.1.1.tar.gz.sig`.

## 6. Write the manifest

The manifest is what the app downloads first. It says which version is
available, where the tarball is, and carries the signature:

```bash
jq -n --rawfile s ota/frontend-0.1.1.tar.gz.sig \
  '{version: "0.1.1", notes: "A new heading", url: "http://localhost:8000/frontend-0.1.1.tar.gz", signature: $s}' \
  > ota/ota.json
```

## 7. Serve it

```bash
python3 -m http.server 8000 --directory ota
```

Your release server is up. Leave it running.

## 8. Update the app

Back in the app window:

1. Click **Check for frontend update**. The output shows
   `Frontend update 0.1.1 available (current 0.1.0): A new heading`.
2. Click **Download and install now**.

The page reloads. The heading reads **Updated over the air** and the version
is **0.1.1**. No rebuild, no restart.

Close the app and start it again: it still shows 0.1.1. The bundle is stored
on disk and picked at every launch.

Click **Check for frontend update** once more: `No frontend update available`.

## 9. Publish a broken frontend

A frontend tells the plugin that it works by calling `notifyReady()`. The
example does that at the bottom of `src/main.js`. Simulate a frontend that
crashes before it gets there: delete this line from `src/main.js`.

```js
  await plugin("notify_ready");
```

Publish it as `0.1.2` with the same three commands as before, in a third
terminal. The server keeps running; it serves whatever is in `ota`.

```bash
tar -czf ota/frontend-0.1.2.tar.gz -C src .
```

```bash
TAURI_SIGNING_PRIVATE_KEY_PATH=~/.tauri/ota-tutorial.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
  npm run tauri signer sign -- --app-version 0.1.2 ota/frontend-0.1.2.tar.gz
```

```bash
jq -n --rawfile s ota/frontend-0.1.2.tar.gz.sig \
  '{version: "0.1.2", url: "http://localhost:8000/frontend-0.1.2.tar.gz", signature: $s}' \
  > ota/ota.json
```

In the app, **Check for frontend update**, then **Download and install now**.
It shows **0.1.2**, which is now *on trial*.

Close the app and start it again. Still 0.1.2: one run without a confirmation
could have been a user closing the app early, so it gets a second attempt.

Close and start it a third time. It shows **0.1.1** again, and the output says
`Rolled back earlier: 0.1.2`. That version will never be used on this machine
again; **Check for frontend update** no longer offers it.

## 10. Clean up

Click **Reset to built-in** to delete the downloaded bundles; the app is back
on 0.1.0 with its original heading. Put the `notify_ready` line back in
`src/main.js`, and stop the server with Ctrl+C.

## What you did

- Registered nothing new and rebuilt nothing after step 3: both updates
  reached a binary that was already running.
- Published an update as three files: a tarball, its signature (inside the
  manifest), and the manifest.
- Saw the two ways an update is applied (at once with a reload, and at every
  later launch) and the safety net that reverts a frontend which does not
  confirm itself.

## Where next

- Put the plugin in your own app: the [quick start](../README.md#quick-start).
- Publish for real: [GitHub Releases](how-to/publish-github-releases.md),
  [CrabNebula Cloud](how-to/publish-crabnebula.md) or
  [any host](how-to/publish.md).
- You also ship full app updates: [Use with the updater plugin](how-to/use-with-updater-plugin.md).
- Why it behaves this way: [How it works](explanation/how-it-works.md).
