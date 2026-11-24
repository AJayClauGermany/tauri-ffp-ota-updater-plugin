# What a frontend update can change

A frontend bundle is new web code running inside an old binary. This page
explains where the line between the two runs, so that you can tell whether a
change can ship as a frontend update or needs a full release.

## One bundle, many binaries

A bundle runs on every binary of its version line that is still installed,
including the oldest. A bundle `1.2.5` may be loaded by an app that was
installed as `1.2.0` and never fully updated since. It may therefore only rely
on what **all** of those binaries have.

If you keep patch releases frontend-only (see
[Decide which release goes where](../how-to/use-with-updater-plugin.md#decide-which-release-goes-where)),
every binary of a line is identical in this respect and the question
disappears. If you do not, the oldest binary sets the limit.

If binaries of one version can differ in what they offer, the version is not
enough to go by. A frontend can then name the native build or IPC contract it
was made for, and the binary holds bundles to it:
[Bind a frontend to a native build or IPC contract](../how-to/integrate.md#bind-a-frontend-to-a-native-build-or-ipc-contract).

## Fixed in the binary

These are compiled in or read from `tauri.conf.json` at build time. A frontend
update cannot change them, and a frontend that assumes otherwise fails at
runtime, typically with a rejected `invoke`.

| | Consequence |
| --- | --- |
| **Rust commands** | A new or changed command needs a full release. |
| **Plugins** | Keep the `@tauri-apps/plugin-*` npm packages at the versions the binary was built with. The JavaScript half of a plugin talks to the Rust half; a newer JS package may call commands the installed plugin does not have. |
| **Capabilities and permissions** | Calling a command that was not permitted at build time is refused, however new the frontend is. |
| **Windows** | Window labels and the page each window opens. Name those pages in `requiredFiles`, so that a bundle without one is refused. |
| **Content Security Policy** | A new API host, font or image origin has to be in the CSP of the binary. |
| **Asset and filesystem scopes** | |
| **The app version** | `getVersion()` from `@tauri-apps/api/app` keeps returning the binary's version. Use this plugin's `currentVersion()` for what the user is actually running. |

## Free to change

Everything the bundler emits: markup, styles, scripts, images, fonts shipped
as files, routes of a single-page app, translations, and any logic that only
uses commands the binary already exposes.

## Carried over between frontends

The new frontend runs at the same origin as the old one, so it inherits its
data:

- `localStorage`, `sessionStorage` (until the window closes), IndexedDB and
  cookies. Version anything you persist, or be ready to read the previous
  shape. With TanStack Query's persisted cache, set its `buster` to the
  frontend version.
- State held on the Rust side. `install()` reloads the webviews, not the
  process: managed state, open connections and background tasks continue, and
  the new frontend meets them mid-flight.

In-memory state of the page itself is lost on the reload, as with any reload.

## Build it like the app's frontend

A bundle is a drop-in replacement for the embedded files, so it has to be
built the same way: the same bundler `base`, the same environment variables,
the same output layout. The simplest guarantee is to produce it with the same
command that `build.beforeBuildCommand` runs.

Keep page paths stable in a multi-page app. A request for a page the bundle no
longer contains is answered with `index.html`, not with an error.

## When in doubt

Ask: "would this frontend work if I dropped it into the oldest binary of this
line?" If the answer needs a look at Rust code, a capability file or
`tauri.conf.json`, ship a full release.
