//! Frontend bundles: signature verification, unpacking and on-disk storage.
//!
//! A bundle is a gzipped tarball of the frontend `dist` directory. It is stored on disk exactly
//! as downloaded, next to its signature, and is verified and unpacked into memory every time it
//! is loaded. Nothing is ever extracted to disk, so a tampered or corrupted file can at worst be
//! rejected, in which case the app falls back to an older bundle or its built-in assets.
//!
//! Next to the bundles, `state.json` records which bundle was confirmed to work, which one is
//! on trial, and which versions were rejected. See [`Store::select`].

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const ARCHIVE_EXT: &str = ".tar.gz";
const SIGNATURE_EXT: &str = ".tar.gz.sig";
const PARTIAL_EXT: &str = ".part";
const STATE_FILE: &str = "state.json";
/// Entries (files and directories) a bundle may contain.
const MAX_ENTRIES: usize = 20_000;

/// Size limits applied to a bundle, in bytes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// The compressed archive, as downloaded and stored.
    pub archive: u64,
    /// The sum of the unpacked files, which are held in memory.
    pub unpacked: u64,
}

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
    ///
    /// The archive has to be unambiguous: anything but plain files and directories, paths that
    /// are absolute or climb out, and two entries for the same file are errors, not skipped.
    pub fn from_archive(
        archive: &[u8],
        signature: &str,
        pubkey: &str,
        version: &Version,
        require_signed_version: bool,
        limits: Limits,
    ) -> Result<Self> {
        if archive.len() as u64 > limits.archive {
            return Err(Error::TooLarge("bundle", limits.archive));
        }
        verify_signature(archive, signature, pubkey, version, require_signed_version)?;

        let mut files = HashMap::new();
        let mut unpacked = 0u64;
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        for (index, entry) in tar.entries().map_err(invalid)?.enumerate() {
            if index >= MAX_ENTRIES {
                return Err(Error::InvalidBundle(format!(
                    "more than {MAX_ENTRIES} entries"
                )));
            }
            let mut entry = entry.map_err(invalid)?;
            let kind = entry.header().entry_type();
            // `git archive` leads with a global pax header; it carries no file
            if kind.is_dir() || kind.is_pax_global_extensions() {
                continue;
            }
            let key = asset_key(&entry.path_bytes())?;
            if !kind.is_file() {
                return Err(Error::InvalidBundle(format!(
                    "`{key}` is a link or special file"
                )));
            }
            unpacked = unpacked.saturating_add(entry.size());
            if unpacked > limits.unpacked {
                return Err(Error::TooLarge("unpacked bundle", limits.unpacked));
            }
            let mut contents = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut contents).map_err(invalid)?;
            if files.insert(key.clone(), contents).is_some() {
                return Err(Error::InvalidBundle(format!(
                    "`{key}` is in the archive twice"
                )));
            }
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

/// Turns an archive path into the key Tauri looks assets up by: `./assets/app.js` (what
/// `tar -C dist .` writes) becomes `/assets/app.js`.
fn asset_key(path: &[u8]) -> Result<String> {
    let bad = || Error::InvalidBundle(format!("bad path `{}`", String::from_utf8_lossy(path)));
    let path = std::str::from_utf8(path).map_err(|_| bad())?;
    if path.starts_with('/') || path.contains('\\') || path.chars().any(char::is_control) {
        return Err(bad());
    }
    let mut key = String::with_capacity(path.len() + 1);
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(bad()),
            part => {
                key.push('/');
                key.push_str(part);
            }
        }
    }
    if key.is_empty() {
        return Err(bad());
    }
    Ok(key)
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

/// What is known about the stored bundles beyond their files.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// The last bundle the frontend confirmed to work; kept as the fallback.
    confirmed: Option<Version>,
    /// The bundle being served without having been confirmed yet. Recorded before it is
    /// served, so that a launch that finds it still here knows the trial failed.
    trial: Option<Version>,
    /// Versions that failed a trial or were rolled back. They are never served or offered
    /// again; a fixed frontend needs a new version.
    rejected: BTreeSet<Version>,
}

/// The directory downloaded bundles live in: `<version>.tar.gz` plus `<version>.tar.gz.sig`,
/// and `state.json`.
pub(crate) struct Store {
    pub dir: PathBuf,
    pub pubkey: String,
    pub require_signed_version: bool,
    pub limits: Limits,
    /// With rollback off every bundle counts as confirmed as soon as it is served.
    pub rollback: bool,
    pub binary: Version,
    pub comparator: VersionComparator,
    /// Serializes everything that reads and then writes the state.
    pub lock: Mutex<()>,
}

/// Writes `contents` so that `path` either keeps its old content or has all of the new one,
/// also across a crash or power loss.
fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(PARTIAL_EXT);
    let mut file = fs::File::create(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

impl Store {
    fn archive_path(&self, version: &Version) -> PathBuf {
        self.dir.join(format!("{version}{ARCHIVE_EXT}"))
    }

    fn signature_path(&self, version: &Version) -> PathBuf {
        self.dir.join(format!("{version}{SIGNATURE_EXT}"))
    }

    /// Whether `version` is a frontend-only update for this binary.
    pub fn compatible(&self, version: &Version) -> bool {
        (self.comparator)(&self.binary, version)
    }

    pub fn is_rejected(&self, version: &Version) -> bool {
        self.read_state().rejected.contains(version)
    }

    /// The version being served without having been confirmed yet.
    pub fn trial(&self) -> Option<Version> {
        self.read_state().trial
    }

    /// Whether `version` is stored with exactly this signature. With `verify` the stored
    /// archive is also checked against it, which reads and unpacks the whole bundle.
    pub fn is_stored(&self, version: &Version, signature: &str, verify: bool) -> bool {
        let _guard = self.lock.lock().unwrap();
        self.archive_path(version).is_file()
            && fs::read_to_string(self.signature_path(version))
                .is_ok_and(|stored| stored.trim() == signature.trim())
            && (!verify || self.load(version).is_ok())
    }

    fn read_state(&self) -> State {
        match fs::read(self.dir.join(STATE_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                log::warn!("ignoring unreadable OTA state: {error}");
                State::default()
            }),
            Err(_) => State::default(),
        }
    }

    fn write_state(&self, state: &State) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        write_atomic(&self.dir.join(STATE_FILE), &serde_json::to_vec(state)?)?;
        Ok(())
    }

    fn verify(&self, archive: &[u8], signature: &str, version: &Version) -> Result<Bundle> {
        if !self.compatible(version) {
            return Err(Error::Incompatible(version.to_string()));
        }
        Bundle::from_archive(
            archive,
            signature,
            &self.pubkey,
            version,
            self.require_signed_version,
            self.limits,
        )
    }

    /// Verifies a downloaded archive and, only if it is valid, persists it.
    pub fn save(&self, archive: &[u8], signature: &str, version: &Version) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        if self.is_rejected(version) {
            return Err(Error::Rejected(version.to_string()));
        }
        self.verify(archive, signature, version)?;
        fs::create_dir_all(&self.dir)?;
        // Each file appears complete or not at all, and the archive, which `versions` goes by,
        // comes last. Even a half-saved bundle would only fail verification in `load`.
        write_atomic(&self.signature_path(version), signature.as_bytes())?;
        write_atomic(&self.archive_path(version), archive)?;
        Ok(())
    }

    /// Loads and verifies a stored bundle.
    fn load(&self, version: &Version) -> Result<Bundle> {
        let path = self.archive_path(version);
        let size = match fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotDownloaded(version.to_string()));
            }
            other => other?.len(),
        };
        if size > self.limits.archive {
            return Err(Error::TooLarge("bundle", self.limits.archive));
        }
        let archive = fs::read(path)?;
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

    fn remove(&self, version: &Version) {
        let _ = fs::remove_file(self.archive_path(version));
        let _ = fs::remove_file(self.signature_path(version));
    }

    /// Removes what `serving` supersedes: everything older, except the confirmed fallback.
    fn prune(&self, state: &State, serving: &Version) {
        for version in self.versions() {
            if version < *serving && state.confirmed.as_ref() != Some(&version) {
                self.remove(&version);
            }
        }
    }

    /// Removes every stored bundle. The rejected versions stay on record.
    pub fn clear(&self) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        for version in self.versions() {
            self.remove(&version);
        }
        let state = self.read_state();
        if state.confirmed.is_some() || state.trial.is_some() {
            self.write_state(&State {
                rejected: state.rejected,
                ..State::default()
            })?;
        }
        Ok(())
    }

    /// Loads the stored bundle `version` to switch to it while the app is running, recording
    /// it as on trial before it is returned for serving.
    pub fn activate(&self, version: &Version) -> Result<Bundle> {
        let _guard = self.lock.lock().unwrap();
        let mut state = self.read_state();
        if state.rejected.contains(version) {
            return Err(Error::Rejected(version.to_string()));
        }
        let bundle = self.load(version)?;
        if !self.rollback {
            state.confirmed = Some(version.clone());
            state.trial = None;
        } else if state.confirmed.as_ref() != Some(version) {
            state.trial = Some(version.clone());
        }
        self.write_state(&state)?;
        self.prune(&state, version);
        Ok(bundle)
    }

    /// Records that the frontend of `serving` works. It becomes the fallback and the previous
    /// one is deleted.
    pub fn confirm(&self, serving: &Version) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut state = self.read_state();
        if state.trial.as_ref() == Some(serving) {
            state.confirmed = state.trial.take();
            self.write_state(&state)?;
            self.prune(&state, serving);
        }
        Ok(())
    }

    /// Picks the bundle to serve: the newest stored one that is compatible with the binary, was
    /// not rejected and passes verification.
    ///
    /// Called at startup, where a trial left unconfirmed by the previous run is rejected, and
    /// to roll back, where `reject` is the version to abandon. A bundle chosen without having
    /// been confirmed before goes on trial. Bundles that are incompatible (e.g. left over from
    /// before a full update), rejected, invalid or superseded are deleted. Never fails: with no
    /// usable bundle the built-in assets are used.
    pub fn select(&self, reject: Option<&Version>) -> Option<Bundle> {
        let _guard = self.lock.lock().unwrap();
        self.select_locked(reject)
    }

    /// Rejects `version` and [selects](Self::select) its replacement, unless it was confirmed
    /// or replaced in the meantime, in which case nothing happens and `None` is returned.
    pub fn fail_trial(&self, version: &Version) -> Option<Option<Bundle>> {
        let _guard = self.lock.lock().unwrap();
        (self.read_state().trial.as_ref() == Some(version))
            .then(|| self.select_locked(Some(version)))
    }

    fn select_locked(&self, reject: Option<&Version>) -> Option<Bundle> {
        let old_state = self.read_state();
        let mut state = old_state.clone();

        if let Some(version) = state.trial.take() {
            log::warn!("frontend bundle {version} was not confirmed to work; rejecting it");
            state.rejected.insert(version);
        }
        if let Some(version) = reject {
            state.rejected.insert(version.clone());
        }
        // a full update makes the history of the previous binary irrelevant
        state.rejected.retain(|version| self.compatible(version));
        if state
            .confirmed
            .as_ref()
            .is_some_and(|v| !self.compatible(v) || state.rejected.contains(v))
        {
            state.confirmed = None;
        }

        let mut chosen = None;
        for version in self.versions() {
            if state.rejected.contains(&version) {
                self.remove(&version);
                continue;
            }
            match self.load(&version) {
                Ok(bundle) => {
                    chosen = Some(bundle);
                    break;
                }
                Err(error) => {
                    log::warn!("discarding frontend bundle {version}: {error}");
                    self.remove(&version);
                }
            }
        }

        match &chosen {
            Some(bundle) if !self.rollback => state.confirmed = Some(bundle.version.clone()),
            Some(bundle) if state.confirmed.as_ref() != Some(&bundle.version) => {
                state.trial = Some(bundle.version.clone());
            }
            _ => {}
        }
        if state != old_state
            && let Err(error) = self.write_state(&state)
        {
            // a trial that is not on record could not be rolled back
            log::warn!("could not record the OTA state: {error}");
            if state.trial.take().is_some() {
                chosen = state.confirmed.as_ref().and_then(|v| self.load(v).ok());
            }
        }
        if let Some(bundle) = &chosen {
            self.prune(&state, &bundle.version);
        }
        if let Ok(entries) = fs::read_dir(&self.dir) {
            // left behind by an interrupted write: partial files, and signatures whose archive
            // was never completed
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let orphan = name
                    .strip_suffix(".sig")
                    .is_some_and(|archive| !path.with_file_name(archive).exists());
                if name.ends_with(PARTIAL_EXT) || orphan {
                    let _ = fs::remove_file(path);
                }
            }
        }
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::asset_key;

    #[test]
    fn asset_keys_are_canonical() {
        for path in [
            "./assets/app.js",
            "assets/app.js",
            "assets//app.js",
            "./assets/./app.js",
        ] {
            assert_eq!(asset_key(path.as_bytes()).unwrap(), "/assets/app.js");
        }
        for path in ["../x", "a/../x", "/x", "a\\x", "a\nb", ".", "", "./"] {
            assert!(asset_key(path.as_bytes()).is_err(), "{path:?} was accepted");
        }
        assert!(asset_key(&[0xff, b'a']).is_err());
    }
}
