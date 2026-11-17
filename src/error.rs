use serde::{Serialize, ser::Serializer};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Semver(#[from] semver::Error),
    #[error(transparent)]
    Base64(#[from] base64::DecodeError),
    #[error(transparent)]
    Minisign(#[from] minisign_verify::Error),
    #[error(transparent)]
    InvalidHeaderName(#[from] http::header::InvalidHeaderName),
    #[error(transparent)]
    InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),
    /// No endpoints are configured under `plugins.ffp-ota-updater.endpoints`.
    #[error("no OTA update endpoints are configured")]
    EmptyEndpoints,
    /// An endpoint is not `https` and `dangerousInsecureTransportProtocol` is not enabled.
    #[error(
        "the OTA endpoint `{0}` does not use https; set `dangerousInsecureTransportProtocol` to allow it"
    )]
    InsecureTransportProtocol(String),
    /// Every endpoint failed without a usable response.
    #[error("could not fetch a valid OTA release manifest from any endpoint: {0}")]
    ReleaseNotFound(String),
    /// The public key or signature is not valid base64-encoded minisign text.
    #[error("the public key or signature is not valid UTF-8 after base64 decoding")]
    SignatureUtf8,
    /// `requireSignedVersion` is on and the signature carries no version.
    #[error(
        "the signature does not record a version; sign with `tauri signer sign --app-version <version>` or disable `requireSignedVersion`"
    )]
    MissingSignedVersion,
    /// The version the bundle was signed for is not the one that was announced.
    #[error("the bundle was signed for version `{signed}` but announced as `{announced}`")]
    SignedVersionMismatch { signed: String, announced: String },
    /// The archive is not a usable frontend bundle.
    #[error("invalid frontend bundle: {0}")]
    InvalidBundle(String),
    /// `download`/`install` was called for a version `check` did not return.
    #[error("no pending OTA update for version `{0}`; call `check` first")]
    NoPendingUpdate(String),
    /// `install` was called before the bundle was downloaded.
    #[error("the frontend bundle for version `{0}` has not been downloaded")]
    NotDownloaded(String),
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}
