use serde::{
    Serialize,
    ser::{SerializeStruct, Serializer},
};

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
    InvalidHeaderName(#[from] http::header::InvalidHeaderName),
    #[error(transparent)]
    InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error(transparent)]
    Http(#[from] http::Error),
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),
    /// No endpoints are configured under `plugins.ffp-ota-updater.endpoints`.
    #[error("no OTA update endpoints are configured")]
    EmptyEndpoints,
    /// No public key is configured under `plugins.ffp-ota-updater.pubkey` or with
    /// [`Builder::pubkey`](crate::Builder::pubkey), so no bundle can be verified.
    #[error("no OTA public key is configured")]
    EmptyPubkey,
    /// A downloaded frontend cannot be shown in this run of the app: the assets were not wrapped
    /// with [`wrap_assets`](crate::wrap_assets), the app runs in development mode from `devUrl`,
    /// or its `frontendDist` is a URL.
    /// Checking for, downloading and staging bundles still works.
    #[error("downloaded frontends cannot be served: {0}")]
    CannotServe(&'static str),
    /// An endpoint or bundle URL is not `https` and `dangerousInsecureTransportProtocol` is
    /// not enabled.
    #[error(
        "the OTA URL `{0}` does not use https; set `dangerousInsecureTransportProtocol` to allow it"
    )]
    InsecureTransportProtocol(String),
    /// An endpoint answered, but none with a usable manifest. When no endpoint could be reached
    /// at all (offline, say) the error is [`Error::Reqwest`] instead.
    #[error("could not fetch a valid OTA release manifest from any endpoint: {0}")]
    ReleaseNotFound(String),
    /// A manifest or bundle request was redirected more often than the plugin follows, or to
    /// nowhere. Holds the URL it stopped at.
    #[error("the request was redirected too often, or nowhere, at `{0}`")]
    TooManyRedirects(String),
    /// The public key or the signature cannot be read, or the signature is not one of this
    /// key for this bundle.
    #[error("the bundle signature is not valid: {0}")]
    InvalidSignature(String),
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
    /// A bundle, or what it unpacks to, exceeds its size limit, in bytes. A manifest that does
    /// is an unusable one, reported as [`Error::ReleaseNotFound`] if no other endpoint has a
    /// better one.
    #[error("the {0} is larger than the limit of {1} bytes")]
    TooLarge(&'static str, u64),
    /// The bundle version is not a frontend-only update for this binary.
    #[error("frontend version `{0}` is not compatible with this application version")]
    Incompatible(String),
    /// The version failed a trial or was rolled back before.
    #[error("frontend version `{0}` was rejected earlier and will not be used again")]
    Rejected(String),
    /// `download`/`install` was called for a version `check` did not return.
    #[error("no pending OTA update for version `{0}`; call `check` first")]
    NoPendingUpdate(String),
    /// The version is older than the frontend being served. Versions only move forward; to go
    /// back, roll back or publish the older build under a new version.
    #[error("frontend version `{0}` is older than the one being served")]
    NotNewer(String),
    /// The bundle is being served, or is the confirmed one kept to fall back on, and cannot be
    /// discarded; or other contents were downloaded or staged under a version that is stored,
    /// which a version cannot have; or a version that differs from a stored one only in letter
    /// case, which would be the same file on Windows and macOS.
    #[error("the frontend bundle for version `{0}` is in use")]
    InUse(String),
    /// `install` was called before the bundle was downloaded.
    #[error("the frontend bundle for version `{0}` has not been downloaded")]
    NotDownloaded(String),
}

impl Error {
    /// A stable, machine-readable name for what went wrong. This is the `kind` the webview
    /// receives next to the message; several variants that an app would handle the same way
    /// share one.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::Tauri(_) => "tauri",
            Self::Reqwest(_) | Self::TooManyRedirects(_) => "network",
            Self::Json(_) => "json",
            Self::Semver(_) => "invalidVersion",
            Self::InvalidSignature(_) => "signature",
            Self::InvalidHeaderName(_) | Self::InvalidHeaderValue(_) | Self::Http(_) => {
                "invalidHeader"
            }
            Self::UrlParse(_) => "invalidUrl",
            Self::EmptyEndpoints => "emptyEndpoints",
            Self::EmptyPubkey => "emptyPubkey",
            Self::CannotServe(_) => "cannotServe",
            Self::InsecureTransportProtocol(_) => "insecureTransportProtocol",
            Self::ReleaseNotFound(_) => "releaseNotFound",
            Self::MissingSignedVersion => "missingSignedVersion",
            Self::SignedVersionMismatch { .. } => "signedVersionMismatch",
            Self::InvalidBundle(_) => "invalidBundle",
            Self::TooLarge(..) => "tooLarge",
            Self::Incompatible(_) => "incompatible",
            Self::Rejected(_) => "rejected",
            Self::NoPendingUpdate(_) => "noPendingUpdate",
            Self::NotNewer(_) => "notNewer",
            Self::InUse(_) => "inUse",
            Self::NotDownloaded(_) => "notDownloaded",
        }
    }
}

impl Error {
    /// Whether a stored bundle that failed to load with this error is unusable for good, as
    /// opposed to unreadable for now (an I/O error, say), and may be deleted.
    pub(crate) fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::InvalidSignature(_)
                | Self::MissingSignedVersion
                | Self::SignedVersionMismatch { .. }
                | Self::InvalidBundle(_)
                | Self::TooLarge(..)
                | Self::Incompatible(_)
        )
    }
}

/// Reaches the webview as `{ "kind": "...", "message": "..." }`.
impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut error = serializer.serialize_struct("Error", 2)?;
        error.serialize_field("kind", self.kind())?;
        error.serialize_field("message", &self.to_string())?;
        error.end()
    }
}
