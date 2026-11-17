use serde::Deserialize;
use url::Url;

/// Plugin configuration, read from `plugins.ffp-ota-updater` in `tauri.conf.json`.
///
/// The keys deliberately match `tauri-plugin-updater`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// URLs queried in order for a release manifest. Supports the `{{current_version}}`,
    /// `{{binary_version}}`, `{{target}}` and `{{arch}}` variables.
    pub endpoints: Vec<Url>,
    /// Minisign public key, as printed by `tauri signer generate`. The same key the updater
    /// plugin uses can be reused here.
    pub pubkey: String,
    /// Allow non-`https` endpoints.
    #[serde(alias = "dangerous-insecure-transport-protocol")]
    pub dangerous_insecure_transport_protocol: bool,
    /// Require the bundle signature to record the version it was signed for
    /// (`tauri signer sign --app-version`). Defaults to `true`; without it the unsigned manifest
    /// could pair a new version number with an older signed bundle.
    #[serde(alias = "require-signed-version")]
    pub require_signed_version: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoints: Vec::new(),
            pubkey: String::new(),
            dangerous_insecure_transport_protocol: false,
            require_signed_version: true,
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
        Ok(())
    }
}
