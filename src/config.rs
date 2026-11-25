use serde::Deserialize;
use url::Url;

/// Plugin configuration, read from `plugins.ffp-ota-updater` in `tauri.conf.json`.
///
/// The keys deliberately match `tauri-plugin-updater`. A key the plugin does not know is a
/// setup error rather than ignored: a misspelled `requireApproval` or `pubkey` would otherwise
/// leave the app running with the default, which may be the less safe setting.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
    /// URLs queried in order for a release manifest. Supports the `{{current_version}}`,
    /// `{{binary_version}}`, `{{target}}` and `{{arch}}` variables.
    pub endpoints: Vec<Url>,
    /// Minisign public key, as printed by `tauri signer generate`. The same key the updater
    /// plugin uses can be reused here.
    pub pubkey: String,
    /// Allow non-`https` endpoints, bundle URLs and redirects.
    #[serde(alias = "dangerous-insecure-transport-protocol")]
    pub dangerous_insecure_transport_protocol: bool,
    /// Require the bundle signature to record the version it was signed for
    /// (`tauri signer sign --app-version`). Defaults to `true`; without it the unsigned manifest
    /// could pair a new version number with an older signed bundle.
    #[serde(alias = "require-signed-version")]
    pub require_signed_version: bool,
    /// Revert a bundle the frontend does not confirm with `notifyReady()`. Defaults to `true`.
    pub rollback: bool,
    /// Runs of the app in which a new bundle may be shown without confirming itself before it
    /// is reverted. Runs that end before a page was loaded from it do not count. Defaults to 2,
    /// which forgives an app that was closed while starting once.
    #[serde(alias = "trial-attempts")]
    pub trial_attempts: u32,
    /// Milliseconds a bundle on trial gets, from the first page loaded from it, to be confirmed
    /// with `notifyReady()` before it is rolled back while the app is running. Off by default:
    /// an unconfirmed bundle is then only rolled back at the next launch.
    #[serde(alias = "ready-timeout")]
    pub ready_timeout: Option<u64>,
    /// Treat versions that differ only in build metadata as different versions, ordered by it:
    /// `1.2.1+2` is then newer than `1.2.1+1`, and `1.2.0+1` newer than the binary `1.2.0`.
    /// Off by default, where build metadata is ignored when deciding what is newer, as semantic
    /// versioning prescribes, so a release that only changes it is not an update.
    #[serde(alias = "compare-build-metadata")]
    pub compare_build_metadata: bool,
    /// Serve a downloaded bundle only once the app has approved it: with `approve` for the next
    /// launch, or with `install` / `activate` at once. Until then it stays stored, across
    /// restarts. Off by default, where the next launch switches to the newest stored bundle by
    /// itself.
    #[serde(alias = "require-approval")]
    pub require_approval: bool,
    /// Files every bundle has to contain besides `index.html`, as paths from the root of the
    /// frontend build (`settings.html`, `editor/index.html`): the page of each window, say. A
    /// bundle that lacks one is refused. Tauri answers a request for a page that is missing
    /// with `index.html`, so without this a window would silently show the wrong page.
    #[serde(alias = "required-files")]
    pub required_files: Vec<String>,
    /// Largest bundle (the `.tar.gz`) accepted, in bytes. Defaults to 64 MiB.
    #[serde(alias = "max-bundle-size")]
    pub max_bundle_size: u64,
    /// Largest total size of the files in a bundle, in bytes. They are held in memory.
    /// Defaults to 256 MiB.
    #[serde(alias = "max-unpacked-size")]
    pub max_unpacked_size: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoints: Vec::new(),
            pubkey: String::new(),
            dangerous_insecure_transport_protocol: false,
            require_signed_version: true,
            rollback: true,
            trial_attempts: 2,
            ready_timeout: None,
            compare_build_metadata: false,
            require_approval: false,
            required_files: Vec::new(),
            max_bundle_size: 64 << 20,
            max_unpacked_size: 256 << 20,
        }
    }
}

impl Config {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        if !self.dangerous_insecure_transport_protocol
            && let Some(url) = self.endpoints.iter().find(|u| u.scheme() != "https")
        {
            return Err(crate::Error::InsecureTransportProtocol(url.to_string()));
        }
        if !self.pubkey.trim().is_empty() {
            // a key that cannot be read would make every bundle fail verification
            crate::bundle::parse_pubkey(&self.pubkey)?;
        }
        self.required_keys()?;
        Ok(())
    }

    /// [`Self::required_files`] as the keys a bundle's files go by (`/settings.html`).
    pub(crate) fn required_keys(&self) -> crate::Result<Vec<String>> {
        self.required_files
            .iter()
            .map(|path| {
                crate::bundle::asset_key(path.trim_start_matches('/').as_bytes()).map_err(|_| {
                    crate::Error::InvalidConfig(format!(
                        "`requiredFiles` has `{path}`, which is not a path inside a bundle"
                    ))
                })
            })
            .collect()
    }
}
