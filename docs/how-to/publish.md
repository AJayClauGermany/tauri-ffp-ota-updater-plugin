# Publish a frontend update

How to turn a frontend build into an update that installed apps pick up. This
page covers the steps that are the same everywhere and hosting on a server of
your own. For the two most common hosts there are complete recipes:

- [GitHub Releases](publish-github-releases.md)
- [CrabNebula Cloud](publish-crabnebula.md)

The plugin does not hook into `tauri build`. An update is three things you
produce yourself:

| | What | Made by |
| --- | --- | --- |
| Bundle | `frontend-<version>.tar.gz`, your whole frontend build | `tar` |
| Signature | `frontend-<version>.tar.gz.sig` | `tauri signer sign` |
| Manifest | a small JSON file at the URL in `endpoints` | you (`jq` below) |

## Before you start

- **Pick the version.** It must be a newer *patch* of the app version that is
  installed: users on `1.2.0` (or `1.2.x`) receive `1.2.1`, `1.2.2`, …
  Anything else is ignored by default
  ([why](../explanation/how-it-works.md#versions)). You do not have to change
  `version` in `tauri.conf.json`: the bundle's version is the one you sign.
- **The frontend calls `notifyReady()`** on start. A bundle that does not is
  rolled back on every device.
- **It only uses what the installed binary has**: no new Rust commands,
  plugins, permissions or CSP entries
  ([details](../explanation/frontend-only-updates.md)).
- **It contains every page the app requires**, if the app sets
  `requiredFiles`.
- **Tauri CLI 2.12 or newer** (`npm run tauri -- --version`), for
  `--app-version`.

## 1. Build the frontend

Run your normal frontend build, the command in `build.beforeBuildCommand`
(for example `npm run build`), with the same bundler `base` and environment
variables as for the app. The output is the folder `build.frontendDist` points
to, called `dist` below.

## 2. Package it

```bash
tar -czf frontend-1.2.1.tar.gz -C dist .
```

`index.html` has to be at the root of the archive, which is what `-C dist .`
does. Put the version in the file name; step 5 explains why.

## 3. Sign it

```bash
npm run tauri signer sign -- --app-version 1.2.1 frontend-1.2.1.tar.gz
```

This writes `frontend-1.2.1.tar.gz.sig`. The key is the one your `pubkey`
belongs to: the pair the updater plugin uses, or
[one of its own](../explanation/how-it-works.md#security). The signer finds it
in environment variables:

| Variable | Value |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | the private key itself (the content of the key file) |
| `TAURI_SIGNING_PRIVATE_KEY_PATH` | *or* the path of the key file |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | its password; set it to an empty string if there is none |

> `tauri build` accepts a path in `TAURI_SIGNING_PRIVATE_KEY`; `tauri signer
> sign` does not, and fails with `failed to decode base64 secret key`. For a
> key file, use `TAURI_SIGNING_PRIVATE_KEY_PATH`.

`--app-version` records the version inside the signature. Without it the app
refuses the bundle (`missingSignedVersion`).

## 4. Write the manifest

```bash
jq -n --arg v "1.2.1" --arg u "https://releases.example.com/frontend-1.2.1.tar.gz" \
  --rawfile s frontend-1.2.1.tar.gz.sig \
  '{version: $v, url: $u, signature: $s, notes: "New colors"}' > ota.json
```

`signature` is the *content* of the `.sig` file, not its path or URL. All
fields: [manifest reference](../reference/formats.md#manifest).

## 5. Upload: bundle first, manifest last

Upload the tarball, check that its URL downloads, then replace the manifest.
The moment the manifest changes, apps start downloading.

Because the tarball's name contains its version, an app that reads the
manifest while you are publishing either gets the previous release in full or
the new one in full. With a single `frontend.tar.gz` that is overwritten, a
device could combine the old manifest with the new file; the signature check
would refuse it, but the update would fail for that device until its next
check.

Both files must be served over `https`.

## 6. Check it

```bash
curl -sL https://releases.example.com/ota.json | jq '{version, url}'
```

```bash
curl -sIL "$(curl -sL https://releases.example.com/ota.json | jq -r .url)" | grep -i '^HTTP'
```

Then start an installed release build of the app (not `tauri dev`) on the
previous version and let it update.

## The whole thing as a script

`VERSION` is the new patch version, `BASE_URL` where the files will be served
from:

```bash
grep -rq notify_ready dist   # heuristic: fail if the build lost notifyReady()
tar -czf "frontend-$VERSION.tar.gz" -C dist .
npx tauri signer sign --app-version "$VERSION" "frontend-$VERSION.tar.gz"
jq -n --arg v "$VERSION" --arg u "$BASE_URL/frontend-$VERSION.tar.gz" \
  --rawfile s "frontend-$VERSION.tar.gz.sig" \
  '{version: $v, url: $u, signature: $s}' > ota.json
```

## Choosing where the manifest lives

Any of these work; the plugin only needs a URL that answers with the manifest.

**A static file** (object storage, a CDN, GitHub Releases, your web server).
One `ota.json` that you overwrite with each release:

```json
{ "endpoints": ["https://releases.example.com/ota.json"] }
```

A static file describes one release, so it serves one minor version line at a
time. If you still publish patches for `1.2` after `1.3` is out, keep one
manifest per line, for example `…/1.2/ota.json` and `…/1.3/ota.json`, and
select it from Rust with [`Builder::endpoints`](../reference/configuration.md#builder).

**A server that decides per request.** The endpoint can contain variables,
which the app fills in:

```json
{ "endpoints": ["https://releases.example.com/ota/{{binary_version}}/{{current_version}}"] }
```

Answer `204 No Content` when there is nothing newer, or `200` with the
manifest. `{{binary_version}}` is the installed app, `{{current_version}}` the
frontend it is showing; a server needs the first to know which line the device
is on. Headers (an auth token, a channel) can be added from Rust.
[Endpoint variables](../reference/configuration.md#endpoint-variables).

**Several endpoints** are tried in order until one answers with a manifest or
with `204`.

## Per-platform bundles

One tarball serves every platform. Only if your frontend build differs between
platforms, publish one manifest per platform, each pointing at its own
tarball:

```json
{ "endpoints": ["https://releases.example.com/{{target}}-{{arch}}/ota.json"] }
```

Sign each tarball with the same `--app-version`.

Nothing in a signature says which platform a bundle is for, so the app cannot
tell if an endpoint hands it another platform's: it accepts any bundle signed
for that version. This layout is for builds that differ in detail, not for
ones that break on the wrong platform. A device also keeps the first bundle
it stored under a version.

For builds that do break on the wrong platform, put the platform into the
version (`1.2.1+windows`), which is signed, and let the app refuse the others:
[Bind a frontend to a native build or IPC contract](integrate.md#bind-a-frontend-to-a-native-build-or-ipc-contract).

## Taking a release back

- **Not yet widely installed:** put the previous manifest back. Devices that
  already have the new bundle keep it.
- **Installed:** publish the previous build again under a new, higher version
  (`1.2.2` with the content of `1.2.0`). Versions only move forward.
- A bundle that fails to start is reverted on each device by itself; see
  [rollback](../explanation/how-it-works.md#trial-and-rollback).
