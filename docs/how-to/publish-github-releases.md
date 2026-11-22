# Publish frontend updates with GitHub Releases

A recipe for hosting frontend updates in the same public GitHub repository as
your app, without disturbing the full updates that
[`tauri-action`](https://github.com/tauri-apps/tauri-action) publishes there.
The general steps it automates are in [Publish a frontend update](publish.md).

## The layout

Frontend updates go into **one long-lived release** with the tag `ota`. It
holds the manifest, which is overwritten each time, and every tarball:

```text
Release "ota" (pre-release)
├── ota.json                  ← replaced on every publish
├── frontend-1.2.1.tar.gz
└── frontend-1.2.2.tar.gz
```

The app asks for the manifest at a URL that never changes:

```json
{
  "plugins": {
    "ffp-ota-updater": {
      "endpoints": ["https://github.com/OWNER/REPO/releases/download/ota/ota.json"],
      "pubkey": "…"
    }
  }
}
```

### Why not a release per frontend version

If you also use the updater plugin, its endpoint is almost certainly
`https://github.com/OWNER/REPO/releases/latest/download/latest.json`. GitHub
resolves `latest` to the newest release that is neither a draft nor a
pre-release. A new release `v1.2.1` that only contains a frontend tarball
would become "latest", it has no `latest.json`, and **every full-update check
would start failing with a 404**. The same happens in reverse to an
`ota.json` fetched through `releases/latest/download/`: it disappears with the
next full release.

Keeping frontend updates in one release marked as a pre-release avoids both:
it is never "latest", and its URL does not depend on what is.

## One-time setup

1. Create the release (the tag is created with it):

   ```bash
   gh release create ota --prerelease --title "Frontend updates" \
     --notes "Over-the-air frontend bundles. Not an application download."
   ```

2. Make the signing key available to Actions as repository secrets. If you
   already build with `tauri-action` and the updater, you have them:

   | Secret | Value |
   | --- | --- |
   | `TAURI_SIGNING_PRIVATE_KEY` | the **content** of the private key file, not a path |
   | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | its password; omit if there is none |

   To keep the key that signs installers out of this workflow, give frontend
   updates [a key pair of their own](../explanation/how-it-works.md#security)
   and store it under other secret names.

3. Set the `endpoints` URL above in `tauri.conf.json` and ship that in a full
   release. Only apps that contain the plugin and the endpoint can receive
   frontend updates.

## The workflow

Save as `.github/workflows/frontend-update.yml`. Adjust the build commands
and `dist` to your project.

```yaml
name: Publish frontend update

on:
  workflow_dispatch:
    inputs:
      version:
        description: "Frontend version: a new patch of the released app, e.g. 1.2.1"
        required: true

# One publish at a time, in the order they were started.
concurrency:
  group: frontend-update
  cancel-in-progress: false

permissions:
  contents: read

jobs:
  # Runs the project's own code and that of its dependencies: no secrets here.
  build:
    runs-on: ubuntu-latest
    env:
      VERSION: ${{ inputs.version }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
      - run: npm ci
      - run: npm run build
      - name: Package
        run: |
          grep -rq notify_ready dist
          tar -czf "frontend-$VERSION.tar.gz" -C dist .
      - uses: actions/upload-artifact@v4
        with:
          name: frontend
          path: frontend-*.tar.gz
          if-no-files-found: error

  # Holds the signing key and may write to the release. Runs nothing from the
  # repository: it only signs and uploads what the build produced.
  publish:
    needs: build
    runs-on: ubuntu-latest
    permissions:
      contents: write
    env:
      VERSION: ${{ inputs.version }}
      BASE_URL: ${{ github.server_url }}/${{ github.repository }}/releases/download/ota
      GH_REPO: ${{ github.repository }}
      GH_TOKEN: ${{ github.token }}
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: frontend

      - name: Refuse a version that is not newer than the published one
        run: |
          PUBLISHED=$(curl -fsSL "$BASE_URL/ota.json" | jq -r .version || true)
          if [ -n "$PUBLISHED" ]; then
            npx --yes semver@7 --range ">$PUBLISHED" "$VERSION"
          fi

      - name: Sign
        env:
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
        run: |
          npx --yes @tauri-apps/cli@2 signer sign --app-version "$VERSION" "frontend-$VERSION.tar.gz"
          jq -n --arg v "$VERSION" --arg u "$BASE_URL/frontend-$VERSION.tar.gz" \
            --rawfile s "frontend-$VERSION.tar.gz.sig" \
            '{version: $v, url: $u, signature: $s}' > ota.json

      - name: Upload the bundle, then the manifest
        run: |
          gh release upload ota "frontend-$VERSION.tar.gz"
          gh release upload ota ota.json --clobber
```

Three things in it are deliberate:

- **Two jobs.** `npm ci` and `npm run build` run install and build scripts of
  every dependency. In a job that also holds the signing key, any of them
  could read it, or replace the `tauri` binary that is about to be handed it.
  The `build` job therefore has no secrets and a read-only token; the
  `publish` job has both and runs no code from the repository. It gets the
  Tauri CLI from npm instead of from `node_modules`; replace `@2` with the
  exact version your project uses. For an approval step before signing, put
  the secrets in a GitHub
  [environment](https://docs.github.com/en/actions/deployment/targeting-different-environments/using-environments-for-deployment)
  with required reviewers and name it in the `publish` job.
- **One run at a time, never backwards.** `ota.json` is one file that every
  publish overwrites. Without the `concurrency` group, two runs could finish
  in the other order than they were started, and the older release would be
  the one left published. The version check stops a run for an older version
  that is started later.
- **No `--clobber` for the tarball.** Publishing a version twice fails instead
  of replacing a file that devices may already have stored, which they would
  [not accept anyway](../explanation/how-it-works.md#a-version-is-one-bundle).

## Publishing

1. Get the frontend change onto the branch your released version line lives
   on. A frontend built from `main` may call Rust commands that the released
   binary does not have.
2. Run the workflow on that branch (**Actions → Publish frontend update → Run
   workflow**, or the command below) with the next patch version.

   ```bash
   gh workflow run frontend-update.yml --ref release/1.2 -f version=1.2.1
   ```

3. Check the result:

   ```bash
   curl -sL https://github.com/OWNER/REPO/releases/download/ota/ota.json | jq .version
   ```

## Things to know

- **One line at a time.** `ota.json` describes one release. While it says
  `1.2.3`, apps on `1.3.x` are offered nothing, and once it says `1.3.1`, apps
  still on `1.2.x` are offered nothing either (they need the full update).
  To patch two lines in parallel, use one release per line (`ota-1.2`,
  `ota-1.3`) and pick the endpoint in Rust:

  ```rust
  let v = &context.package_info().version;
  let endpoint = format!(
      "https://github.com/OWNER/REPO/releases/download/ota-{}.{}/ota.json",
      v.major, v.minor
  );
  tauri::Builder::default().plugin(
      tauri_plugin_ffp_ota_updater::Builder::new()
          .endpoints(vec![endpoint.parse().expect("valid endpoint")])
          .build(),
  )
  ```

- **Private repositories.** Release assets of a private repository need
  authentication, for this plugin and for the updater alike. Host the files
  elsewhere, or put a proxy in front that adds the token; do not ship a token
  in the app.
- **Full releases too?** If you also publish every patch as a full release
  with `tauri-action`, read
  [Use with the updater plugin](use-with-updater-plugin.md#every-release-is-also-a-full-release)
  so that users are not offered the same patch twice.
- **Old tarballs** can be deleted from the release once no manifest points at
  them. Devices keep their stored copy.
