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
| `index.html` at the root | Required. No other file is checked for. |
| Paths | Relative. `./assets/app.js`, `assets/app.js` and `assets//app.js` all become `/assets/app.js`. Absolute paths, `..`, backslashes, control characters and non-UTF-8 names are refused. |
| Entry types | Regular files and directories. Links and special files are refused. A leading global pax header (`git archive`) is skipped. |
| Duplicates | The same file twice is refused. |
| Size | Archive at most `maxBundleSize`; files in total at most `maxUnpackedSize`; at most 20 000 entries. |

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

## Store

Directory `ffp-ota-updater/` in the app's local data directory
(`app_local_data_dir`).

| File | |
| --- | --- |
| `<version>.tar.gz` | A bundle exactly as downloaded. |
| `<version>.tar.gz.sig` | The `signature` it was announced with. |
| `state.json` | `confirmed` (version or null), `trial` (version or null), `attempts` (number), `rejected` (versions). |
| `*.part` | A write in progress. Removed at the next launch if left behind. |

Bundles are never extracted to disk. A bundle is verified against its
signature and unpacked into memory each time it is loaded: at every launch and
on `activate` / `install`.

At launch the newest stored bundle that is compatible with the binary, not
rejected and valid is served. Bundles that are incompatible, rejected, invalid
or older than the one served (except the confirmed fallback) are deleted.

One running instance per user is assumed. The store is not locked against a
second process.

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
