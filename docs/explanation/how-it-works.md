# How it works

Background on the design: what happens between publishing a bundle and a user
seeing it, and why the plugin behaves the way it does. For steps, see the
[how-to guides](../../README.md#documentation); for exact values, the
[reference](../reference/configuration.md).

- [Serving a different frontend](#serving-a-different-frontend)
- [The life of a bundle](#the-life-of-a-bundle)
- [Versions](#versions)
- [Trial and rollback](#trial-and-rollback)
- [Security](#security)
- [Limitations](#limitations)

## Serving a different frontend

A Tauri app does not load its frontend from disk. `tauri build` embeds the
files in the binary, and the webview's requests to `tauri://localhost` (or
`http://tauri.localhost` on Windows) are answered from that embedded set.

The plugin puts itself in front of that set. `wrap_assets(&mut context)` swaps the
context's assets for a wrapper that answers from a downloaded bundle when one
is active and from the embedded files otherwise. This is why the plugin needs
a line of setup that other plugins do not, and why it has to happen before the
app runs: Tauri only offers this swap on the context.

Three consequences follow from doing it at this level:

- **The page keeps its origin.** IPC, capabilities, `localStorage` and
  IndexedDB carry over, because as far as the webview can tell nothing moved.
- **A bundle replaces the frontend as a whole.** A file the bundle lacks is
  not looked up in the embedded assets; mixing two builds would be worse than
  a missing file.
- **`tauri dev` is unaffected.** In development the webview loads from the dev
  server, not from the asset set, so a bundle is never displayed there.

Bundles are kept on disk as the signed archive and unpacked into memory, never
extracted. What is served is therefore always exactly what was signed, and a
file tampered with on disk can at worst cause the bundle to be refused. The
price is memory: the unpacked frontend is held in RAM, which is what
`maxUnpackedSize` bounds.

## The life of a bundle

```text
            check()                download()               install() or next launch
 endpoint ───────────▶ manifest ───────────────▶ stored ───────────────────────────▶ on trial
                                                   │                                    │
                                              discard()                    notifyReady()│  no confirmation
                                                   ▼                                    ▼  in trialAttempts runs,
                                                deleted                            confirmed  readyTimeout, rollback()
                                                                                                    │
                                                                                                    ▼
                                                                                                 rejected
```

- **`check()`** asks the endpoints for a manifest and decides whether the
  release it describes is for this device (see [Versions](#versions)).
- **`download()`** fetches the tarball, verifies it and stores it. Nothing
  else happens: the app keeps showing what it showed.
- **`install()`** makes the stored bundle the active one and reloads the
  webviews. Without it, the next launch does the same thing.
- **At every launch** the plugin picks the newest stored bundle that is still
  compatible with the binary, verifies it again and serves it. If none is
  usable, the built-in frontend is served. Selection never fails.

Downloading and applying are separate on purpose. A bundle that is stored
needs no network to be applied, so "download now, switch when convenient" and
"switch at the next start" are the same mechanism.

## Versions

There are two version numbers in play, and most confusion comes from mixing
them up:

- the **binary version**: `version` in `tauri.conf.json`, fixed when the app
  was built. `getVersion()` returns it.
- the **frontend version**: the version of the bundle being served, or the
  binary version when there is none. `currentVersion()` returns it.

A release is offered when both of these hold:

1. it is **newer than the frontend being served**, and
2. it is **compatible with the binary**: newer than the binary, and allowed by
   the compatibility rule. The default rule allows the binary's own major and
   minor version, which makes these its later patches: binary `1.2.0` accepts
   `1.2.1` and `1.2.7`, not `1.3.0` or `2.0.0`.

The second rule encodes an assumption: a minor or major bump probably needs
new Rust code, and a frontend that calls commands the binary does not have is
broken in ways no rollback can detect in time. Those releases are left to a
full update. The plugin cannot inspect your Rust code, so the version number
is the contract: publishing a patch is your statement that the binary did not
need to change.

`Builder::compatibility` replaces the rule in 2 for projects with another
convention. It is deliberately not a "version comparator" like the updater
plugin's: it answers "can this binary run that frontend?", never "is this an
update?". Being newer, than the frontend being served and than the binary,
always applies, so versions only move forward. That keeps the store simple
(the newest usable bundle wins) and makes a replayed old manifest harmless.

"Newer" follows semantic versioning, which ignores build metadata: `1.2.1+2`
is the same version as `1.2.1+1`, so publishing it is not an update, and
`1.2.0+1` is not newer than the binary `1.2.0`. Build numbers are not as
dependable an order as the patch number (they may be dates, hashes or reset by
a CI change), so ordering by them is opt-in: with `compareBuildMetadata`,
versions that differ only in build metadata are ordered by it, numerically
where it is a number (`+10` after `+9`).

After a full update, bundles made for the previous binary no longer pass rule
2 and are deleted at the first launch, together with the record of rejected
versions from that line.

## Trial and rollback

A full update that fails to start is obvious to the user and fixable with a
reinstall. A frontend that fails to start is worse: the code that would fetch
the fix is the code that is broken. So a new bundle has to prove itself.

A bundle is **on trial** from the first page loaded from it until the frontend
calls `notifyReady()`. A run of the app that shows the bundle and ends without
that call is a failed attempt.

- One failed attempt may just be a user who closed the app while it was
  starting, so the bundle gets another. After `trialAttempts` of them (2 by
  default) it is taken to be broken.
- A broken bundle's version is **rejected for good** on that device: it is
  deleted, never served and never offered by `check()` again. The app serves
  the last confirmed bundle, or else the built-in frontend. The fix has to be
  published as a new version. A newer bundle that was downloaded but never
  shown is kept, and is tried at the next launch.
- The confirmed bundle is kept as the fallback until its successor is
  confirmed.

Details that matter in practice:

- **Runs that never showed the bundle do not count.** An app that starts
  hidden in the tray, or is closed before its window appears, has learned
  nothing about the bundle. The attempt is recorded on disk just before the
  first page is served, so a crash during startup is counted.
- **Without `readyTimeout`, rollback happens at a launch.** A broken bundle
  stays on screen until the user restarts the app, twice with the default
  settings. `readyTimeout` adds a deadline that reverts it while the app is
  running; `rollback()` lets the frontend or the Rust side give up at once.
- **Only a page of the bundle can confirm it.** Any window can, as long as its
  page was loaded from the bundle on trial. While `install()` reloads the
  windows one after the other, a window still showing the previous frontend
  cannot vouch for the new one. `notify_ready()` in Rust is not subject to
  this: from Rust it confirms whatever is being served.

  The plugin knows where a page is from by what was being served when its
  navigation started; Tauri does not say which window an asset is requested
  by. A switch made with `activate()` at the very moment a window navigates
  can therefore be attributed to the wrong side of it. `install()` reloads
  every window right after switching, which settles it.
- **The trial covers what happens from the first page on.** Before that, the
  bundle is verified, unpacked and, with a CSP, parsed. A bundle that is
  validly signed and within the size limits but takes the process down in
  that step would do so at every launch, uncounted. `maxBundleSize` and
  `maxUnpackedSize` are the guard against that; set them to what your frontend
  needs rather than leaving them at the defaults.
- **A good bundle can be rejected** if the app is closed between showing it
  and its `notifyReady()` call in `trialAttempts` runs in a row. The device
  then stays on the previous frontend until the next version is published.

This is why every published frontend must call `notifyReady()`. `"rollback":
false` turns the mechanism off; bundles are then served without a trial.

To take back a frontend that was confirmed but turned out to be wrong, publish
the previous build again under a new, higher version.

### A version is one bundle

Everything the plugin records (confirmed, on trial, rejected, stored, pending
download) goes by version, so a version has to name exactly one archive,
across every channel, endpoint and platform. Publish a rebuild, even an
identical one, under a new version.

The plugin holds a device to this from the moment a bundle is stored: other
contents under that version are refused with `inUse`. That includes a bundle
no page was loaded from yet. It may already be in memory, about to be shown
and confirmed, and whatever replaced it on disk would start the next launch
as confirmed without ever having run. To replace a bundle that was never
shown, `discard()` it first.

The same goes for what a webview holds after `check()`: the `Update` object
stands for a version, not for one particular answer of the endpoint. If two
checks return the same version, the later one decides how it is downloaded.

## Security

The trust anchor is the public key compiled into the app. A bundle is served
only if it carries a valid signature of that key, checked when it is
downloaded and again every time it is loaded from disk.

**The manifest itself is not signed**, and it does not need to be for the
bundle's integrity: a tampered manifest cannot make the app run code that was
not signed. What it could do is lie about the version: present an old, signed
bundle (one with a known bug, say) under a new version number so that apps
"update" to it. `requireSignedVersion`, on by default, closes that: the
version is recorded inside the signature (`--app-version`) and has to match
what the manifest announces.

Other measures:

- `https` is required for endpoints, bundle URLs and redirects.
- Headers given for the manifest request (an `Authorization` token, say) are
  only ever sent to the origin of the endpoint: with the download when it is
  there, and not along a redirect that leaves it. Otherwise whoever can edit
  the unsigned manifest could point the download at their own server and
  collect the token before the signature check fails. For the same reason
  there is no option that sends headers wherever the manifest points, and a
  redirect does not tell the next host which URL it came from.
- The webview cannot choose endpoints or keys; only Rust can.
- Archives are held to an unambiguous subset of tar: no links, no paths that
  climb out, no duplicate entries, and limits on size and entry count. What
  they decompress to is capped too, metadata entries included.
- `rollback()` and `reset()`, which discard a frontend, are not in the default
  permission set.

What this does not cover:

- Whoever holds the private key can ship any frontend to every user. Protect
  it as you protect the updater's key.
- The signature says who signed which bytes for which version. It does not
  say for which app, channel or platform. Every app that has the public key
  accepts the bundle, from any endpoint. Use one key pair per app, and do not
  rely on the endpoint to keep a bundle for one platform or channel away from
  another: if that would do harm, give them different versions, or build one
  frontend that runs everywhere.
- For the same reason, an app that shares its key pair with the updater plugin
  has two plugins that accept whatever that key signed. Reusing the pair is
  the simple setup and what the guides here assume. A pair of its own for
  frontend updates keeps the two apart: the pipeline that publishes frontends
  then never holds the key that signs installers, and a frontend bundle is of
  no use to anyone who can tamper with the updater's manifest.
- The files of the store are not secret and not protected from the user or
  other programs on the device. Tampering with a bundle is caught by its
  signature; deleting or damaging `state.json` loses the record of what was
  confirmed and rejected.

## Limitations

- **Dev mode**: a downloaded bundle is never displayed under `tauri dev`, so
  the plugin does not switch to one there: `install()` fails with
  `cannotServe`, and `currentVersion()` is the app's version. The same goes
  for an app whose `frontendDist` is a remote URL.
- **CSP**: with a CSP, `tauri build` adds nonces and hashes for the embedded
  HTML. The plugin does the same for a bundle when it is served, so inline
  scripts and styles work as they do in the built-in frontend. Anything else
  the policy has to allow, such as a new host, needs a full update.
- **Rollback timing** and **false rejections**: see
  [Trial and rollback](#trial-and-rollback).
- **Several instances**: processes of the same app take turns on the store
  (it is locked while changed), but each counts its own run as an attempt of a
  trial. If more instances than `trialAttempts` are started before the first
  of them has called `notifyReady()`, a working bundle is rejected, and an
  instance that already runs it keeps doing so until it exits. The device is
  then on the previous frontend until the next version. Apps whose users start
  them several times at once should use
  [`tauri-plugin-single-instance`](https://v2.tauri.app/plugin/single-instance/)
  or raise `trialAttempts`.
- **Windows with several webviews** (Tauri's `unstable` feature): `install()`,
  `rollback()` and `reset()` reload each window's own webview, not webviews
  added to a window. Reload those from Rust.
- **Overlapping calls**: `discard()` and `reset()` do not cancel a download
  that is still running; when it completes, its bundle is stored. Wait for the
  download before discarding.
- **Memory**: the active bundle is held unpacked in memory.
- **Desktop only.**
