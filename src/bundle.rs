//! Frontend bundles: signature verification, unpacking and on-disk storage.
//!
//! A bundle is a gzipped tarball of the frontend `dist` directory. It is stored on disk exactly
//! as downloaded, next to its signature, and is verified and unpacked into memory every time it
//! is loaded. Nothing is ever extracted to disk, so a tampered or corrupted file can at worst be
//! rejected, in which case the app falls back to an older bundle or its built-in assets.

use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Component, PathBuf},
    str::FromStr,
    sync::Arc,
};

use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use tauri::utils::assets::AssetKey;

use crate::{Error, Result};

const ARCHIVE_EXT: &str = ".tar.gz";
const SIGNATURE_EXT: &str = ".tar.gz.sig";

/// Decides whether a frontend bundle version may run on a given binary version.
///
/// Receives `(binary_version, bundle_version)`.
pub type VersionComparator = Arc<dyn Fn(&Version, &Version) -> bool + Send + Sync>;

/// The default [`VersionComparator`]: a bundle is frontend-only when it is a newer *patch*
/// release of the binary (same major and minor). Anything else needs a full update.
pub fn default_version_comparator(binary: &Version, bundle: &Version) -> bool {
    bundle.major == binary.major && bundle.minor == binary.minor && bundle > binary
}

/// A verified frontend bundle, unpacked in memory.
pub(crate) struct Bundle {
    pub version: Version,
    /// Keyed by normalized asset key (`/index.html`).
    pub files: HashMap<String, Vec<u8>>,
}

impl Bundle {
    /// Verifies `archive` against `signature` and unpacks it.
    pub fn from_archive(
        archive: &[u8],
        signature: &str,
        pubkey: &str,
        version: &Version,
        require_signed_version: bool,
    ) -> Result<Self> {
        verify_signature(archive, signature, pubkey, version, require_signed_version)?;

        let mut files = HashMap::new();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        for entry in tar.entries().map_err(invalid)? {
            let mut entry = entry.map_err(invalid)?;
            // directories, links and other special entries have no place in a set of assets
            if !entry.header().entry_type().is_file() {
                continue;
            }
            // `tar -C dist .` names entries `./index.html`; keep only the plain components so
            // the key is `/index.html`. Paths that try to climb out are not assets.
            let path = entry.path().map_err(invalid)?;
            if path.components().any(|c| matches!(c, Component::ParentDir)) {
                continue;
            }
            let key = AssetKey::from(
                path.components()
                    .filter(|c| matches!(c, Component::Normal(_)))
                    .collect::<PathBuf>(),
            );
            let mut contents = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut contents).map_err(invalid)?;
            files.insert(key.as_ref().to_string(), contents);
        }

        if !files.contains_key("/index.html") {
            return Err(Error::InvalidBundle(
                "no index.html at the root of the archive".into(),
            ));
        }

        Ok(Self {
            version: version.clone(),
            files,
        })
    }
}

fn invalid(error: std::io::Error) -> Error {
    Error::InvalidBundle(error.to_string())
}

/// Validates the minisign signature of `data`, then the version recorded in its trusted comment.
fn verify_signature(
    data: &[u8],
    signature: &str,
    pubkey: &str,
    announced: &Version,
    require_signed_version: bool,
) -> Result<()> {
    let public_key = PublicKey::decode(&base64_to_string(pubkey)?)?;
    let signature = Signature::decode(&base64_to_string(signature)?)?;
    public_key.verify(data, &signature, true)?;

    // Only trustworthy after `verify`: minisign's global signature covers the trusted comment,
    // which the Tauri CLI writes as `timestamp:<unix>\tfile:<name>\tversion:<version>`.
    let signed = signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("version:"));
    match signed {
        None if require_signed_version => Err(Error::MissingSignedVersion),
        None => Ok(()),
        Some(signed) => match Version::from_str(signed.trim_start_matches('v')) {
            Ok(v) if v == *announced => Ok(()),
            _ => Err(Error::SignedVersionMismatch {
                signed: signed.to_string(),
                announced: announced.to_string(),
            }),
        },
    }
}

fn base64_to_string(value: &str) -> Result<String> {
    let decoded = base64::engine::general_purpose::STANDARD.decode(value.trim())?;
    String::from_utf8(decoded).map_err(|_| Error::SignatureUtf8)
}

/// The directory downloaded bundles live in: `<version>.tar.gz` plus `<version>.tar.gz.sig`.
pub(crate) struct Store {
    pub dir: PathBuf,
    pub pubkey: String,
    pub require_signed_version: bool,
}

impl Store {
    fn archive_path(&self, version: &Version) -> PathBuf {
        self.dir.join(format!("{version}{ARCHIVE_EXT}"))
    }

    fn signature_path(&self, version: &Version) -> PathBuf {
        self.dir.join(format!("{version}{SIGNATURE_EXT}"))
    }

    fn verify(&self, archive: &[u8], signature: &str, version: &Version) -> Result<Bundle> {
        Bundle::from_archive(
            archive,
            signature,
            &self.pubkey,
            version,
            self.require_signed_version,
        )
    }

    /// Verifies a downloaded archive and, only if it is valid, persists it.
    pub fn save(&self, archive: &[u8], signature: &str, version: &Version) -> Result<Bundle> {
        let bundle = self.verify(archive, signature, version)?;
        fs::create_dir_all(&self.dir)?;
        // The signature is written last and the archive is renamed into place, so an
        // interrupted save never leaves something that looks like a complete bundle. Even if it
        // did, `load` verifies it again.
        let archive_path = self.archive_path(version);
        let tmp = archive_path.with_extension("part");
        fs::write(&tmp, archive)?;
        fs::rename(&tmp, &archive_path)?;
        fs::write(self.signature_path(version), signature)?;
        Ok(bundle)
    }

    /// Loads and verifies a stored bundle.
    pub fn load(&self, version: &Version) -> Result<Bundle> {
        let archive = match fs::read(self.archive_path(version)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotDownloaded(version.to_string()));
            }
            other => other?,
        };
        let signature = fs::read_to_string(self.signature_path(version))?;
        self.verify(&archive, &signature, version)
    }

    /// Versions with an archive on disk, newest first.
    fn versions(&self) -> Vec<Version> {
        let mut versions: Vec<Version> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                Version::parse(name.to_str()?.strip_suffix(ARCHIVE_EXT)?).ok()
            })
            .collect();
        versions.sort_by(|a, b| b.cmp(a));
        versions
    }

    pub fn remove(&self, version: &Version) {
        let _ = fs::remove_file(self.archive_path(version));
        let _ = fs::remove_file(self.signature_path(version));
    }

    /// Removes every stored bundle.
    pub fn clear(&self) {
        for version in self.versions() {
            self.remove(&version);
        }
    }

    /// Removes every stored bundle older than `keep`.
    pub fn prune_older_than(&self, keep: &Version) {
        for version in self.versions().iter().filter(|v| *v < keep) {
            self.remove(version);
        }
    }

    /// Picks the newest stored bundle that is compatible with `binary` and passes verification.
    ///
    /// Bundles that are incompatible (e.g. left over from before a full update), invalid, or
    /// superseded are deleted. Never fails: with no usable bundle the built-in assets are used.
    pub fn load_newest(&self, binary: &Version, comparator: &VersionComparator) -> Option<Bundle> {
        for version in self.versions() {
            if !comparator(binary, &version) {
                self.remove(&version);
                continue;
            }
            match self.load(&version) {
                Ok(bundle) => {
                    self.prune_older_than(&version);
                    return Some(bundle);
                }
                Err(error) => {
                    log::warn!("discarding frontend bundle {version}: {error}");
                    self.remove(&version);
                }
            }
        }
        None
    }
}
