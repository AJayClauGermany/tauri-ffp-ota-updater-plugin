# Publish frontend updates with CrabNebula Cloud

A recipe for hosting frontend updates on
[CrabNebula Cloud](https://docs.crabnebula.dev/cloud/), next to the full
updates it serves to the updater plugin. The general steps are in
[Publish a frontend update](publish.md).

> **Status of this guide.** It is assembled from CrabNebula's documentation
> (generic assets, channels and latest-download links) and has not been run
> against the service by the maintainers of this plugin. Do the check in
> [step 4](#4-check-it) before you rely on it, and please report what you
> find.

## The layout

CrabNebula's update endpoint (`cdn.crabnebula.app/update/…`) is keyed by
operating system and architecture and is what the updater plugin talks to;
leave it to the installers. Its **download** links serve any file you upload,
and that is all this plugin needs:

- frontend releases go into their own **channel**, `ota`, so that they are
  never the "latest" release your installers and the updater plugin see;
- each frontend release carries two generic assets: the tarball and the
  manifest `ota.json`;
- the app fetches the manifest through the channel's latest-download link.

```json
{
  "plugins": {
    "ffp-ota-updater": {
      "endpoints": ["https://cdn.crabnebula.app/download/ORG/APP/latest/ota.json?channel=ota"],
      "pubkey": "…"
    }
  }
}
```

`ORG` and `APP` are your organization and application slugs. This is not the
URL your `plugins.updater.endpoints` uses, and the two must not be swapped:
the updater's `…/update/ORG/APP/{{target}}-{{arch}}/{{current_version}}`
endpoint returns installers.

Channels are not listed on the app's public page, but anyone who knows the
name can fetch from them.

## 1. Build, package and sign

As in [the general guide](publish.md#1-build-the-frontend), with the tarball's
URL pointing at the same channel:

```bash
VERSION=1.2.1
BASE_URL=https://cdn.crabnebula.app/download/ORG/APP/latest
```

```bash
tar -czf "frontend-$VERSION.tar.gz" -C dist .
```

```bash
npx tauri signer sign --app-version "$VERSION" "frontend-$VERSION.tar.gz"
```

```bash
jq -n --arg v "$VERSION" --arg u "$BASE_URL/frontend-$VERSION.tar.gz?channel=ota" \
  --rawfile s "frontend-$VERSION.tar.gz.sig" \
  '{version: $v, url: $u, signature: $s}' > ota.json
```

The signing key is the one CrabNebula's Tauri guide has you create for the
updater. `tauri signer sign` wants its content in `TAURI_SIGNING_PRIVATE_KEY`
or its path in `TAURI_SIGNING_PRIVATE_KEY_PATH`.

## 2. Create the release in the `ota` channel

With the [`cn` CLI](https://docs.crabnebula.dev/cloud/cli/install/) and
`CN_API_KEY` set:

```bash
cn release draft ORG/APP "$VERSION" --channel ota
```

Note the release `id` it prints, then upload both files as generic assets (no
`--update-platform`):

```bash
cn release upload ORG/APP RELEASE_ID --channel ota --file "frontend-$VERSION.tar.gz"
```

```bash
cn release upload ORG/APP RELEASE_ID --channel ota --file ota.json
```

`--channel` has to be repeated on every command for that release.

## 3. Publish

```bash
cn release publish ORG/APP RELEASE_ID --channel ota
```

A draft is invisible until it is published, so the manifest and the tarball
appear together.

## 4. Check it

The manifest of the channel:

```bash
curl -sL "https://cdn.crabnebula.app/download/ORG/APP/latest/ota.json?channel=ota" | jq '{version, url}'
```

The tarball it points to:

```bash
curl -sIL "$(curl -sL 'https://cdn.crabnebula.app/download/ORG/APP/latest/ota.json?channel=ota' | jq -r .url)" | grep -i '^HTTP'
```

And that the default channel is untouched, i.e. the updater plugin still gets
your last full release (use the current version of an installed app; expect
`204`, or `200` with an installer URL):

```bash
curl -si "https://cdn.crabnebula.app/update/ORG/APP/linux-x86_64/1.2.0" | head -n 1
```

## In GitHub Actions

CrabNebula's
[`cloud-release`](https://github.com/crabnebula-dev/cloud-release) action runs
one `cn` command per step. After the build, package and sign steps:

```yaml
      - uses: crabnebula-dev/cloud-release@v0.2.0
        with:
          command: release draft ${{ vars.CN_APP }} ${{ env.VERSION }} --channel ota
          api-key: ${{ secrets.CN_API_KEY }}
      - uses: crabnebula-dev/cloud-release@v0.2.0
        with:
          command: release upload ${{ vars.CN_APP }} ${{ env.VERSION }} --channel ota --file frontend-${{ env.VERSION }}.tar.gz
          api-key: ${{ secrets.CN_API_KEY }}
      - uses: crabnebula-dev/cloud-release@v0.2.0
        with:
          command: release upload ${{ vars.CN_APP }} ${{ env.VERSION }} --channel ota --file ota.json
          api-key: ${{ secrets.CN_API_KEY }}
      - uses: crabnebula-dev/cloud-release@v0.2.0
        with:
          command: release publish ${{ vars.CN_APP }} ${{ env.VERSION }} --channel ota
          api-key: ${{ secrets.CN_API_KEY }}
```

`CN_APP` is `ORG/APP`. CrabNebula's own generic-assets workflow addresses the
release by its version like this; its CLI pages use the release id instead.
If one form is refused, use the other.

## Things to know

- **The tarball URL is a "latest" link.** CrabNebula documents download links
  for the latest release of a channel only. Because the tarball's name
  contains its version, a device that reads an older manifest after you
  published again gets a clean download error, not the wrong file, and
  succeeds at its next check.
- **One line at a time**, as with any static manifest: the channel's latest
  `ota.json` describes one release. For parallel version lines use a channel
  per line (`ota-1.2`, `ota-1.3`) and choose the endpoint from Rust, as shown
  in the [GitHub guide](publish-github-releases.md#things-to-know).
- **Beta frontends** are another channel (`ota-beta`) and a
  [per-check endpoint](integrate.md#release-channels).
- **Full releases too?** See
  [Use with the updater plugin](use-with-updater-plugin.md).
