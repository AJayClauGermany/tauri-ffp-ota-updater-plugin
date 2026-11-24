# Manifest, bundle and store reference

- [Manifest](#manifest)
- [Bundle](#bundle)
- [Signature](#signature)
- [Store](#store)
- [Stability](#stability)

## Manifest

The JSON document an endpoint answers with. At most 1 MiB.

| Field | Required | Type | |
| --- | --- | --- | --- |
| `version` | yes | string | Version of the bundle, semantic versioning. A leading `v` is ignored. |
| `url` | yes | string | Where the bundle is downloaded from. `https`. |
| `signature` | yes | string | The content of the bundle's `.sig` file. |
| `notes` | no | string | Release notes; `Update.body`. |
| `pub_date` | no | string | Release date, passed through as sent; `Update.date`. RFC 3339 by convention. |

Unknown fields are ignored and available through `Update.rawJson`.

```json
{
  "version": "1.2.1",
  "notes": "New colors",
  "pub_date": "2026-01-01T00:00:00Z",
  "url": "https://releases.example.com/frontend-1.2.1.tar.gz",
  "signature": "dW50cnVzdGVkIGNvbW1lbnQ6…"
}
```

This is the shape of the updater plugin's *dynamic* response. The updater's
*static* file with a `platforms` map (`latest.json`) is not accepted: `url`
and `signature` have to be at the top level.

The manifest is not signed. See [Security](../explanation/how-it-works.md#security).

## Bundle

A gzip-compressed tar archive of the frontend build output
(`build.frontendDist`).

| Rule | |
| --- | --- |
| `index.html` at the root | Required. |
| Files named in [`requiredFiles`](configuration.md#tauriconfjson) | Required. No other file is checked for. |
| Paths | Relative. `./assets/app.js`, `assets/app.js` and `assets//app.js` all become `/assets/app.js`. Absolute paths, `..`, backslashes, control characters and non-UTF-8 names are refused. |
| Entry types | Regular files and directories. Links and special files are refused. A leading global pax header (`git archive`) is skipped. |
| Duplicates | The same file twice is refused. |
| Size | Archive at most `maxBundleSize`; files in total at most `maxUnpackedSize`; at most 20 000 entries. |
| End | The gzip stream is read to its end and has to be sound, checksum included. What follows the end of the tar archive in it (padding) is ignored. |

A bundle that breaks a rule is refused as a whole (`invalidBundle` or
`tooLarge`).

A bundle replaces the built-in frontend entirely. Files it does not contain
are not taken from the built-in assets.

## Signature

A [minisign](https://jedisct1.github.io/minisign/) signature over the archive
bytes, base64-encoded, as written by `tauri signer sign` to `<file>.sig`.

The trusted comment, which the signature covers, has tab-separated fields.
The plugin reads `version:`:

```text
timestamp:1795239006	file:frontend-1.2.1.tar.gz	version:1.2.1
```

| `version:` field | `requireSignedVersion: true` | `false` |
| --- | --- | --- |
| equals the manifest's `version` | accepted | accepted |
| differs | `signedVersionMismatch` | `signedVersionMismatch` |
| absent | `missingSignedVersion` | accepted |

The field is written by `tauri signer sign --app-version <version>` (Tauri CLI
2.12 or newer). The file name in the comment is not checked.

The signature says who signed which bytes for which version, and nothing
else: not for which app, channel or platform. Every app with that public key
accepts the bundle wherever it comes from. See
[Security](../explanation/how-it-works.md#security).

## Store

Directory `ffp-ota-updater/` in the app's local data directory
(`app_local_data_dir`).

| File | |
| --- | --- |
| `<version>.tar.gz` | A bundle exactly as downloaded. Never replaced by other contents; see below. |
| `<version>.tar.gz.sig` | The `signature` it was announced with. |
| `state.json` | `confirmed` (version or null), `trial` (version or null), `attempts` (number), `rejected` (versions), `approved` (version or null). Fields a version of the plugin does not know are ignored. |
| `*.part` | A write in progress. Removed at the next launch if left behind. |
| `.lock` | Locked by the process that is changing the store. |

Bundles are never extracted to disk. A bundle is verified against its
signature and unpacked into memory each time it is loaded: at every launch and
on `activate` / `install`.

At launch the newest stored bundle that is compatible with the binary, not
rejected and valid is served. Bundles that are incompatible, rejected, invalid
or not newer than the one served (except the confirmed fallback) are deleted.
A bundle that cannot be read for now (an I/O error) is skipped and kept.

With `requireApproval`, a launch chooses only among the bundles on record as
`approved`, `trial` or `confirmed`. The other stored bundles wait and are not
verified at launch. Of those, the newest is kept; older ones and ones the
binary cannot run are deleted.

A version is stored once. Other contents for a stored version are refused
(`inUse`) until it is discarded or deleted at a launch, with one exception: a
stored archive that fails verification is replaced. Versions that differ only
in letter case (`1.2.1-RC`, `1.2.1-rc`) would be one file on Windows and
macOS, so the second is refused on every platform.

`state.json` is what the plugin goes by:

| `state.json` | At launch | Afterwards |
| --- | --- | --- |
| missing | A new store: nothing confirmed, on trial or rejected. | |
| cannot be read (permissions, an I/O error) | The built-in frontend is served. Nothing is deleted or written. | Whatever needs the record (accepting, installing or discarding a bundle, a `check()` that finds a release) fails with `io` until it can be read. |
| not valid JSON | Its content is lost. It is written anew; the stored bundles are kept, verified as always, and the newest goes on trial. With `requireApproval` the built-in frontend is served instead and the newest stored bundle waits for approval again. | |

A version is recorded as rejected before its bundle is deleted. If that
cannot be written, nothing is deleted, and `rollback()` fails after having
switched back.

Newest is by semantic versioning precedence, which ignores build metadata,
unless `compareBuildMetadata` is set. A version that is rejected rejects every
version that counts as the same.

Processes of the same app take turns: each locks `.lock` while it changes the
store. Where the file system cannot lock files, the store is used unlocked.
The lock is held for one operation, not for a trial: see
[Limitations](../explanation/how-it-works.md#limitations).

## Stability

From 1.0 on these are covered by semantic versioning, in addition to the Rust
and JavaScript APIs:

- the configuration keys and the manifest fields, the bundle format and the
  endpoint variables;
- the command names and their permissions, and which of them `default`
  grants;
- the `kind` strings of errors. New kinds may appear in minor releases;
- the store: a newer plugin version reads what an older one wrote.

`reqwest` and `http` types are part of the Rust API (`configure_client`,
headers, `Error`), so a new major version of either is a new major version of
this crate.
