//! Frontend bundles: signature verification, unpacking and on-disk storage.
//!
//! A bundle is a gzipped tarball of the frontend `dist` directory. It is stored on disk exactly
//! as downloaded, next to its signature, and is verified and unpacked into memory every time it
//! is loaded. Nothing is ever extracted to disk, so a tampered or corrupted file can at worst be
//! rejected, in which case the app falls back to an older bundle or its built-in assets.
//!
//! Next to the bundles, `state.json` records which bundle was confirmed to work, which one is
//! on trial, and which versions were rejected. A bundle goes through these stages:
//!
//! - *stored*: downloaded, and no page has been loaded from it yet. Launching the app any
//!   number of times without showing it changes nothing. With `requireApproval` it is not even
//!   picked at a launch until the app has *approved* it ([`Store::approve`]).
//! - *on trial*: a page was loaded from it ([`Store::begin_trial`]). Every run of the app that
//!   does so without a confirmation following counts as a failed attempt.
//! - *confirmed* by the frontend ([`Store::confirm`]), which makes it the fallback, or
//!   *rejected*: when the attempts are used up ([`Store::select`]), or at once by a rollback
//!   or an expired ready timeout ([`Store::fail_trial`]).
//!
//! Everything that reads and then writes the store holds [`Store::guard`], which also locks the
//! directory against other processes of the same app.
//!
//! Two rules keep the record and the files in step. A version names one bundle: once stored, its
//! archive is not replaced by another ([`Store::save`]). And the record comes first: nothing is
//! decided on a state that could not be read, and a bundle is only deleted for having been
//! rejected once that rejection is on disk ([`Store::select`]).

use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
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
/// Held locked by the process that is changing the store.
const LOCK_FILE: &str = ".lock";
/// Entries (files and directories) a bundle may contain.
const MAX_ENTRIES: usize = 20_000;
/// What the tar stream of a bundle may hold besides file contents, per entry: its header and
/// padding, plus those of a metadata entry (a PAX header or GNU long name) and a long name.
const MAX_OVERHEAD_PER_ENTRY: u64 = 2048;

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
/// Receives `(binary_version, bundle_version)`. Unlike the updater plugin's version comparator
/// this does not decide whether a release is an update: the plugin only considers bundles that
/// are newer than the binary and than the frontend being served, whatever the rule says.
pub type Compatibility = Arc<dyn Fn(&Version, &Version) -> bool + Send + Sync>;

/// The default [`Compatibility`] rule: a binary runs the bundles of its own major and minor
/// version. Since bundles also have to be newer than the binary, that makes them its later
/// *patch* releases (`1.2.0` runs `1.2.1`, not `1.3.0`). Anything else needs a full update.
pub fn default_compatibility(binary: &Version, bundle: &Version) -> bool {
    bundle.major == binary.major && bundle.minor == binary.minor
}

/// How versions are compared to decide which is newer.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Order {
    /// Order versions that differ only in build metadata by it, instead of treating them as
    /// the same version.
    pub build_metadata: bool,
}

impl Order {
    pub fn cmp(&self, a: &Version, b: &Version) -> Ordering {
        if self.build_metadata {
            a.cmp(b)
        } else {
            a.cmp_precedence(b)
        }
    }

    /// Whether `a` is newer than `b`.
    pub fn newer(&self, a: &Version, b: &Version) -> bool {
        self.cmp(a, b) == Ordering::Greater
    }

    /// Whether `a` and `b` count as the same version.
    pub fn same(&self, a: &Version, b: &Version) -> bool {
        self.cmp(a, b) == Ordering::Equal
    }
}

/// A verified frontend bundle, unpacked in memory.
pub(crate) struct Bundle {
    pub version: Version,
    /// Keyed by normalized asset key (`/index.html`).
    pub files: HashMap<String, Vec<u8>>,
    /// Filled in by [`crate::csp::prepare`] when the bundle is served.
    pub csp: crate::csp::Hashes,
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
        required: &[String],
    ) -> Result<Self> {
        if archive.len() as u64 > limits.archive {
            return Err(Error::TooLarge("bundle", limits.archive));
        }
        verify_signature(archive, signature, pubkey, version, require_signed_version)?;

        // `tar` reads metadata entries into memory before handing out the entry they describe,
        // and `limits.unpacked` only counts file contents: what is decompressed is capped, too.
        let mut stream = Capped {
            inner: flate2::read::GzDecoder::new(archive),
            left: limits
                .unpacked
                .saturating_add(MAX_ENTRIES as u64 * MAX_OVERHEAD_PER_ENTRY),
            exceeded: false,
        };
        let files = unpack(&mut stream, limits.unpacked);
        // `tar` stops at the end of the archive. What follows it (padding, as a rule) is not
        // used, but is read all the same, under the same cap: a bundle is a whole, sound gzip
        // stream, not the start of one.
        let rest = files
            .is_ok()
            .then(|| std::io::copy(&mut stream, &mut std::io::sink()));
        if stream.exceeded {
            return Err(Error::TooLarge("unpacked bundle", limits.unpacked));
        }
        let files = files?;
        if let Some(Err(error)) = rest {
            return Err(invalid(error));
        }

        if !files.contains_key("/index.html") {
            return Err(Error::InvalidBundle(
                "no index.html at the root of the archive".into(),
            ));
        }
        // Tauri answers a request for a page that is not there with `index.html`: a window
        // whose page is missing would show the wrong one instead of failing.
        if let Some(missing) = required.iter().find(|key| !files.contains_key(*key)) {
            return Err(Error::InvalidBundle(format!(
                "no {} in the archive, which `requiredFiles` asks for",
                missing.trim_start_matches('/')
            )));
        }

        Ok(Self {
            version: version.clone(),
            files,
            csp: Default::default(),
        })
    }
}

/// A reader that fails once it has passed on `left` bytes.
struct Capped<R> {
    inner: R,
    left: u64,
    exceeded: bool,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            // more data, rather than its end, is what exceeds the cap
            let read = self.inner.read(&mut [0])?;
            self.exceeded = read > 0;
            return if self.exceeded {
                Err(std::io::Error::other(
                    "the archive decompresses to too much data",
                ))
            } else {
                Ok(0)
            };
        }
        let len = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buf[..len])?;
        self.left -= read as u64;
        Ok(read)
    }
}

/// Unpacks a tar stream into memory, see [`Bundle::from_archive`]. `limit` is the sum of the
/// file sizes allowed.
fn unpack(stream: impl Read, limit: u64) -> Result<HashMap<String, Vec<u8>>> {
    let mut files = HashMap::new();
    let mut unpacked = 0u64;
    let mut tar = tar::Archive::new(stream);
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
        if unpacked > limit {
            return Err(Error::TooLarge("unpacked bundle", limit));
        }
        let mut contents = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut contents).map_err(invalid)?;
        if files.insert(key.clone(), contents).is_some() {
            return Err(Error::InvalidBundle(format!(
                "`{key}` is in the archive twice"
            )));
        }
    }
    Ok(files)
}

/// Turns an archive path into the key Tauri looks assets up by: `./assets/app.js` (what
/// `tar -C dist .` writes) becomes `/assets/app.js`.
pub(crate) fn asset_key(path: &[u8]) -> Result<String> {
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
    let bad = |error: minisign_verify::Error| Error::InvalidSignature(error.to_string());
    let public_key = parse_pubkey(pubkey)?;
    let signature = Signature::decode(&base64_to_string(signature)?).map_err(bad)?;
    public_key.verify(data, &signature, true).map_err(bad)?;

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

/// Reads a public key as `tauri signer generate` prints it.
pub(crate) fn parse_pubkey(pubkey: &str) -> Result<PublicKey> {
    if pubkey.trim().is_empty() {
        return Err(Error::EmptyPubkey);
    }
    PublicKey::decode(&base64_to_string(pubkey)?)
        .map_err(|error| Error::InvalidSignature(format!("the public key: {error}")))
}

fn base64_to_string(value: &str) -> Result<String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|error| Error::InvalidSignature(error.to_string()))?;
    String::from_utf8(decoded)
        .map_err(|_| Error::InvalidSignature("not UTF-8 after base64 decoding".into()))
}

/// What is known about the stored bundles beyond their files.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// The last bundle the frontend confirmed to work; kept as the fallback.
    confirmed: Option<Version>,
    /// The bundle a page was loaded from without it having been confirmed yet. Recorded before
    /// that page is served, so that a launch that finds it still here knows the attempt failed.
    trial: Option<Version>,
    /// Runs of the app in which a page was loaded from `trial`.
    attempts: u32,
    /// Versions that failed a trial or were rolled back. They are never served or offered
    /// again; a fixed frontend needs a new version.
    rejected: BTreeSet<Version>,
    /// The stored bundle the app chose to move to: approved for the next launch, or, with
    /// `requireApproval`, switched to while running. With `requireApproval` a launch serves no
    /// bundle that is neither this one nor on record above.
    approved: Option<Version>,
}

/// The directory downloaded bundles live in: `<version>.tar.gz` plus `<version>.tar.gz.sig`,
/// and `state.json`.
pub(crate) struct Store {
    pub dir: PathBuf,
    pub pubkey: String,
    pub require_signed_version: bool,
    pub limits: Limits,
    /// Files a bundle has to contain besides `index.html`, as asset keys.
    pub required_files: Vec<String>,
    /// With rollback off every bundle counts as confirmed as soon as it is served.
    pub rollback: bool,
    /// A launch only serves a bundle the app chose, see [`State::approved`].
    pub require_approval: bool,
    /// Failed attempts after which a bundle on trial is rejected.
    pub trial_attempts: u32,
    /// The trial this run of the app already counts as an attempt of.
    pub shown: Mutex<Shown>,
    pub binary: Version,
    pub compatibility: Compatibility,
    pub order: Order,
    /// Serializes everything that reads and then writes the store, within this process; see
    /// [`Store::guard`].
    pub lock: Mutex<()>,
}

/// See [`Store::shown`].
#[derive(Default)]
pub(crate) struct Shown {
    /// The bundle on trial, or `None` once a reset ended that trial.
    version: Option<Version>,
    /// Counts the trials this process began. A deadline set for one of them
    /// ([`Store::fail_trial`]) must not end a later one of the same version.
    trial: u64,
}

/// Exclusive access to the store, see [`Store::guard`].
pub(crate) struct Guard<'a> {
    _process: MutexGuard<'a, ()>,
    /// Unlocked when closed.
    _file: Option<fs::File>,
}

/// Writes `contents` so that `path` either keeps its old content or has all of the new one,
/// also across a crash or power loss, and has the new one once this returns.
fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(PARTIAL_EXT);
    let mut file = fs::File::create(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    // The new name is an entry of the directory, which has to reach the disk as well. Windows
    // has no such call for a directory.
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

impl Store {
    /// Locks the store against other threads and against other processes of the same app, which
    /// share the directory. If the directory cannot be locked (a file system without locks, say)
    /// the store is used without, as if a single instance were running: refusing to work there
    /// would turn updates off for good on such a system.
    ///
    /// The lock covers one operation, not a trial. Several instances of the app each count
    /// their own run as an attempt, so more of them than `trialAttempts`, started while a trial
    /// is open, reject a bundle that may be fine. That errs on the safe side (the device goes
    /// back to the frontend it had) and is documented as a limitation; an owner for a trial
    /// across processes can be added without changing the store's format.
    pub fn guard(&self) -> Guard<'_> {
        let process = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let file = fs::create_dir_all(&self.dir)
            .and_then(|()| {
                fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(self.dir.join(LOCK_FILE))
            })
            .and_then(|file| file.lock().map(|()| file));
        Guard {
            _process: process,
            _file: file
                .inspect_err(|error| log::debug!("could not lock the OTA store: {error}"))
                .ok(),
        }
    }

    fn archive_path(&self, version: &Version) -> PathBuf {
        self.dir.join(format!("{version}{ARCHIVE_EXT}"))
    }

    fn signature_path(&self, version: &Version) -> PathBuf {
        self.dir.join(format!("{version}{SIGNATURE_EXT}"))
    }

    /// Whether `version` is a frontend-only update for this binary: newer than it, and allowed
    /// by the compatibility rule.
    pub fn compatible(&self, version: &Version) -> bool {
        self.order.newer(version, &self.binary) && (self.compatibility)(&self.binary, version)
    }

    pub fn is_rejected(&self, version: &Version) -> Result<bool> {
        Ok(self.rejected_in(&self.read_state()?, version))
    }

    /// Whether `state` rejects `version`, or a version that counts as the same.
    fn rejected_in(&self, state: &State, version: &Version) -> bool {
        state
            .rejected
            .iter()
            .any(|rejected| self.order.same(rejected, version))
    }

    /// Versions that failed a trial or were rolled back, oldest first.
    pub fn rejected(&self) -> Vec<Version> {
        self.read_state()
            .map(|state| state.rejected.into_iter().collect())
            .unwrap_or_default()
    }

    /// The newest stored bundle that is newer than `current` and may be served, i.e. the one
    /// the next launch would pick, if it does not have to be approved first. Goes by the files
    /// present; nothing is verified.
    ///
    /// Takes no lock: it only reads, and `status` should not wait for a download that is being
    /// verified.
    pub fn staged(&self, current: &Version) -> Option<Version> {
        let state = self.read_state().ok()?;
        self.versions().into_iter().find(|version| {
            self.order.newer(version, current)
                && self.compatible(version)
                && !self.rejected_in(&state, version)
        })
    }

    /// The bundle approved for the next launch, while it is stored, ahead of `current` and may
    /// be served. Like [`Self::staged`] it takes no lock and verifies nothing.
    pub fn approved(&self, current: &Version) -> Option<Version> {
        let state = self.read_state().ok()?;
        state.approved.clone().filter(|version| {
            self.order.newer(version, current)
                && self.compatible(version)
                && !self.rejected_in(&state, version)
                && self.archive_path(version).is_file()
        })
    }

    /// Records that the app chose the stored bundle `version`: the next launch serves it,
    /// which with `requireApproval` it would not do otherwise. The bundle is verified first, so
    /// that what is approved is something that can be served.
    pub fn approve(&self, version: &Version) -> Result<()> {
        let _guard = self.guard();
        let mut state = self.read_state()?;
        if self.rejected_in(&state, version) {
            return Err(Error::Rejected(version.to_string()));
        }
        self.load(version)?;
        if state.approved.as_ref() != Some(version) {
            state.approved = Some(version.clone());
            self.write_state(&state)?;
        }
        Ok(())
    }

    /// Takes back the approval of `version`, which stays stored. Another version's approval is
    /// left alone, as is a bundle that was served before: that one is abandoned by a rollback.
    pub fn unapprove(&self, version: &Version) -> Result<()> {
        let _guard = self.guard();
        let mut state = self.read_state()?;
        if state.approved.as_ref() == Some(version) {
            state.approved = None;
            self.write_state(&state)?;
        }
        Ok(())
    }

    /// Whether serving `version` is a trial, i.e. it still has to be confirmed. It is unless
    /// the record says otherwise.
    pub fn unproven(&self, version: &Version) -> bool {
        self.rollback
            && !self
                .read_state()
                .is_ok_and(|state| state.confirmed.as_ref() == Some(version))
    }

    /// Records that a page is about to be loaded from the unconfirmed bundle `version`, which
    /// puts it on trial and counts as one attempt per run of the app.
    ///
    /// Returns the number of the trial, for [`Self::fail_trial`], or `None` for a bundle that
    /// was confirmed in the meantime. On an error the trial is not on record and the bundle
    /// must not be served: it could not be rolled back.
    pub fn begin_trial(&self, version: &Version) -> Result<Option<u64>> {
        let _guard = self.guard();
        let mut shown = self.shown.lock().unwrap();
        let mut state = self.read_state()?;
        if state.confirmed.as_ref() == Some(version) {
            return Ok(None);
        }
        // `shown` alone is not enough: a reset or discard in the meantime ended that trial, and
        // a bundle of the same version installed since starts a new one
        if shown.version.as_ref() == Some(version) && state.trial.as_ref() == Some(version) {
            return Ok(Some(shown.trial));
        }
        if state.trial.as_ref() == Some(version) {
            state.attempts += 1;
        } else {
            state.trial = Some(version.clone());
            state.attempts = 1;
        }
        self.write_state(&state)?;
        shown.version = Some(version.clone());
        shown.trial += 1;
        Ok(Some(shown.trial))
    }

    /// The last confirmed bundle, to serve when a trial cannot go ahead.
    pub fn fallback(&self) -> Option<Bundle> {
        let _guard = self.guard();
        self.read_state()
            .ok()?
            .confirmed
            .and_then(|version| self.load(&version).ok())
    }

    /// Whether `version` is stored with exactly this signature. With `verify` the stored
    /// archive is also checked against it, which reads and unpacks the whole bundle.
    pub fn is_stored(&self, version: &Version, signature: &str, verify: bool) -> bool {
        let _guard = self.guard();
        self.archive_path(version).is_file()
            && fs::read_to_string(self.signature_path(version))
                .is_ok_and(|stored| stored.trim() == signature.trim())
            && (!verify || self.load(version).is_ok())
    }

    /// What `state.json` records. Three cases are told apart:
    ///
    /// - There is no file: the store is new, and the record empty.
    /// - The file cannot be read (permissions, an I/O error). What it says is unknown, and may
    ///   be readable again later, so this is an error: acting on an empty record instead would
    ///   delete the confirmed fallback as superseded and offer rejected versions again.
    /// - The file is read but makes no sense: `None`. What it said is lost for good, and waiting
    ///   will not bring it back.
    fn load_state(&self) -> Result<Option<State>> {
        match fs::read(self.dir.join(STATE_FILE)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)
                .inspect_err(|error| log::warn!("the OTA state is damaged, ignoring it: {error}"))
                .ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Some(State::default()))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// [`Self::load_state`], with a damaged file counting as an empty record. That is safe to
    /// go on with: every stored bundle is still verified against its signature when loaded, and
    /// one that is served without being on record as confirmed goes on trial. The next launch
    /// writes the file anew ([`Self::select`]).
    fn read_state(&self) -> Result<State> {
        Ok(self.load_state()?.unwrap_or_default())
    }

    /// Whether the archive of `version` is known not to exist, as opposed to unreadable.
    fn is_gone(&self, version: &Version) -> bool {
        fs::metadata(self.archive_path(version))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
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
            &self.required_files,
        )
    }

    /// Verifies a downloaded archive and, only if it is valid, persists it.
    ///
    /// A version names one bundle, from the moment it is stored. Other contents under a version
    /// that is stored are refused with [`Error::InUse`], whether or not a page was loaded from
    /// it yet: the stored bundle may already be in memory, about to be shown and confirmed, and
    /// what replaced it on disk would inherit that confirmation at the next launch without
    /// having run. The exception is a stored archive that is unusable for good (it fails
    /// verification), which is not a bundle and is replaced, unless it is on record as
    /// confirmed or on trial; [`Self::select`] clears that record at the next launch.
    pub fn save(&self, archive: &[u8], signature: &str, version: &Version) -> Result<()> {
        let _guard = self.guard();
        let state = self.read_state()?;
        if self.rejected_in(&state, version) {
            return Err(Error::Rejected(version.to_string()));
        }
        self.verify(archive, signature, version)?;
        // `1.2.1-RC` and `1.2.1-rc` are two versions but one file where the file system ignores
        // case, as on Windows and macOS. Refused everywhere, so that a release behaves the same
        // on every platform.
        let name = version.to_string();
        if self
            .versions()
            .iter()
            .any(|stored| stored != version && stored.to_string().eq_ignore_ascii_case(&name))
        {
            return Err(Error::InUse(name));
        }
        let recorded = [&state.confirmed, &state.trial].contains(&&Some(version.clone()));
        match fs::read(self.archive_path(version)) {
            Ok(stored) if stored == archive => {
                // the same bundle; its signature file may be missing, or be another valid one
                let signed = fs::read_to_string(self.signature_path(version))
                    .is_ok_and(|stored| stored.trim() == signature.trim());
                if !signed {
                    write_atomic(&self.signature_path(version), signature.as_bytes())?;
                }
                return Ok(());
            }
            Ok(_) => {
                let unusable = self.load(version).is_err_and(|error| error.is_permanent());
                if recorded || !unusable {
                    // a mistake in publishing, which the error alone does not explain
                    log::warn!(
                        "frontend version {version} was published with other contents than the bundle stored under it; keeping the stored one. A version names one bundle: publish a changed build under a new version"
                    );
                    return Err(Error::InUse(name));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if recorded {
                    return Err(Error::InUse(name));
                }
            }
            Err(error) => return Err(error.into()),
        }
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
        let signature = match fs::read_to_string(self.signature_path(version)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::InvalidBundle("its signature is missing".into()));
            }
            other => other?,
        };
        self.verify(&archive, &signature, version)
    }

    /// Versions with an archive on disk, newest first. Versions that count as the same are
    /// ordered by their build metadata.
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
        versions.sort_by(|a, b| self.order.cmp(b, a).then_with(|| b.cmp(a)));
        versions
    }

    /// Deletes the stored bundle `version`. One that is not stored is not an error. The archive
    /// goes first: without it the version is no longer stored, see [`Self::versions`].
    fn remove(&self, version: &Version) -> Result<()> {
        for path in [self.archive_path(version), self.signature_path(version)] {
            match fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// [`Self::remove`] where a failure is not the caller's concern: what is left is removed
    /// again by a later cleanup, or found invalid when loaded.
    fn remove_quietly(&self, version: &Version) {
        if let Err(error) = self.remove(version) {
            log::warn!("could not delete frontend bundle {version}: {error}");
        }
    }

    /// Removes what `serving` supersedes: everything that is not newer, except the confirmed
    /// fallback.
    fn prune(&self, state: &State, serving: &Version) {
        for version in self.versions() {
            if version != *serving
                && !self.order.newer(&version, serving)
                && state.confirmed.as_ref() != Some(&version)
            {
                self.remove_quietly(&version);
            }
        }
    }

    /// Removes every stored bundle. The rejected versions stay on record.
    ///
    /// Fails if a bundle could not be deleted (on Windows, another program may hold it open), as
    /// that bundle would be served again at the next launch. The others are deleted and the
    /// state is reset all the same.
    pub fn clear(&self) -> Result<()> {
        let _guard = self.guard();
        self.shown.lock().unwrap().version = None;
        // a store that cannot be listed is not an empty one
        let mut result = match fs::read_dir(&self.dir) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        };
        for version in self.versions() {
            if let Err(error) = self.remove(&version) {
                result = result.and(Err(error));
            }
        }
        let state = self.load_state()?;
        if state.as_ref().is_none_or(|state| {
            state.confirmed.is_some() || state.trial.is_some() || state.approved.is_some()
        }) {
            self.write_state(&State {
                rejected: state.map(|state| state.rejected).unwrap_or_default(),
                ..State::default()
            })?;
        }
        result
    }

    /// Deletes the stored bundle `version`, unless it is `serving` or the confirmed fallback.
    /// A version that is not stored is not an error.
    pub fn discard(&self, version: &Version, serving: Option<&Version>) -> Result<()> {
        let _guard = self.guard();
        let mut state = self.read_state()?;
        if serving == Some(version) || state.confirmed.as_ref() == Some(version) {
            return Err(Error::InUse(version.to_string()));
        }
        self.remove(version)?;
        let old_state = state.clone();
        if state.trial.as_ref() == Some(version) {
            state.trial = None;
            state.attempts = 0;
        }
        if state.approved.as_ref() == Some(version) {
            state.approved = None;
        }
        if state != old_state {
            self.write_state(&state)?;
        }
        Ok(())
    }

    /// Loads the stored bundle `version` to switch to it while the app is running. Its trial
    /// starts with the first page loaded from it.
    pub fn activate(&self, version: &Version) -> Result<Bundle> {
        let _guard = self.guard();
        let mut state = self.read_state()?;
        if self.rejected_in(&state, version) {
            return Err(Error::Rejected(version.to_string()));
        }
        let bundle = self.load(version)?;
        let old_state = state.clone();
        if !self.rollback {
            state.confirmed = Some(version.clone());
        }
        if !self.rollback || state.trial.as_ref() != Some(version) {
            // an earlier trial is superseded
            state.trial = None;
            state.attempts = 0;
        }
        if self.require_approval && state.confirmed.as_ref() != Some(version) {
            // Switching to it is the app's choice as well. Without this, an app that is closed
            // before a page was loaded from the bundle would find it waiting for approval
            // again: its trial, which is what a launch goes by otherwise, has not begun.
            state.approved = Some(version.clone());
        }
        if state != old_state {
            self.write_state(&state)?;
        }
        self.prune(&state, version);
        Ok(bundle)
    }

    /// Records that the frontend of `serving` works. It becomes the fallback and the previous
    /// one is deleted. Only a bundle on trial can be confirmed: before a page was loaded from
    /// it, nothing can vouch for it.
    pub fn confirm(&self, serving: &Version) -> Result<()> {
        let _guard = self.guard();
        let mut state = self.read_state()?;
        if state.trial.as_ref() == Some(serving) {
            state.confirmed = state.trial.take();
            state.attempts = 0;
            if state
                .approved
                .as_ref()
                .is_some_and(|approved| !self.order.newer(approved, serving))
            {
                state.approved = None;
            }
            self.write_state(&state)?;
            self.prune(&state, serving);
        }
        Ok(())
    }

    /// Picks the bundle to serve: the newest stored one that is compatible with the binary, was
    /// not rejected and passes verification. With `requireApproval`, only among those the app
    /// chose: the approved bundle, and the ones that were served before (on trial or
    /// confirmed). The others wait where they are, unverified; of those only the newest is
    /// kept, as it is the one an approval would be asked for.
    ///
    /// Called at startup, where a trial that earlier runs left unconfirmed is rejected once its
    /// attempts are used up, and to roll back, where `reject` is the version to abandon. A
    /// rollback only goes back to the confirmed bundle, or else the built-in assets: a newer
    /// bundle that is stored but was never shown stays stored for the next launch.
    ///
    /// A bundle chosen without having been confirmed goes on trial when the first page is loaded
    /// from it, see [`Self::begin_trial`]. Bundles that are incompatible (e.g. left over from
    /// before a full update), rejected, invalid or superseded are deleted; one that cannot be
    /// read for now (an I/O error) is skipped and kept.
    ///
    /// Always says what to serve, `None` being the built-in assets. The second value is whether
    /// that decision is on record. When it is not, what was decided still holds for this run of
    /// the app, but nothing was deleted on the strength of it, and a version rejected here is
    /// not rejected at the next launch. A state that cannot be read at all leaves the store
    /// untouched and the built-in assets served.
    pub fn select(&self, reject: Option<&Version>) -> (Option<Bundle>, Result<()>) {
        let _guard = self.guard();
        self.select_locked(reject)
    }

    /// Rejects `version` and [selects](Self::select) its replacement, if `trial` (from
    /// [`Self::begin_trial`]) is still the trial going on. If the bundle was confirmed or
    /// replaced in the meantime, or its trial ended and another began, nothing happens and
    /// `None` is returned.
    pub fn fail_trial(
        &self,
        version: &Version,
        trial: u64,
    ) -> Option<(Option<Bundle>, Result<()>)> {
        let _guard = self.guard();
        let current = {
            let shown = self.shown.lock().unwrap();
            shown.version.as_ref() == Some(version) && shown.trial == trial
        };
        (current
            && self
                .read_state()
                .is_ok_and(|state| state.trial.as_ref() == Some(version)))
        .then(|| self.select_locked(Some(version)))
    }

    fn select_locked(&self, reject: Option<&Version>) -> (Option<Bundle>, Result<()>) {
        let stored = match self.load_state() {
            Ok(stored) => stored,
            Err(error) => {
                log::warn!(
                    "cannot read the OTA state; serving the built-in frontend and leaving the stored bundles alone: {error}"
                );
                return (None, Err(error));
            }
        };
        // a damaged file is written anew, so that it is not in the way from here on
        let rewrite = stored.is_none();
        let old_state = stored.unwrap_or_default();
        let mut state = old_state.clone();

        if let Some(version) = reject {
            state.rejected.insert(version.clone());
        }
        if state.attempts >= self.trial_attempts.max(1)
            && let Some(version) = &state.trial
            && state.rejected.insert(version.clone())
        {
            log::warn!(
                "frontend bundle {version} was not confirmed to work in {} attempt(s); rejecting it",
                state.attempts
            );
        }
        // a full update makes the history of the previous binary irrelevant
        state.rejected.retain(|version| self.compatible(version));
        if state
            .confirmed
            .as_ref()
            .is_some_and(|v| !self.compatible(v) || self.rejected_in(&state, v))
        {
            state.confirmed = None;
        }
        if state
            .approved
            .as_ref()
            .is_some_and(|v| !self.compatible(v) || self.rejected_in(&state, v))
        {
            state.approved = None;
        }

        // a rollback goes back to what is known to work: the confirmed bundle or the built-in
        // assets
        let only = reject.map(|_| state.confirmed.clone());
        // with `requireApproval`, what a launch may serve
        let chosen_by_app: Option<Vec<Version>> = self.require_approval.then(|| {
            [&state.approved, &state.trial, &state.confirmed]
                .into_iter()
                .flatten()
                .cloned()
                .collect()
        });
        let mut chosen = None;
        // deleted once the rejection is on record, not before
        let mut rejected = Vec::new();
        // bundles waiting for approval that are of no use: deleted with the rejected ones
        let mut superseded = Vec::new();
        let mut waiting = false;
        for version in self.versions() {
            if self.rejected_in(&state, &version) {
                rejected.push(version);
                continue;
            }
            if only
                .as_ref()
                .is_some_and(|only| only.as_ref() != Some(&version))
            {
                continue;
            }
            if chosen_by_app
                .as_ref()
                .is_some_and(|chosen_by_app| !chosen_by_app.contains(&version))
            {
                // Waits for the app's approval, and is verified when it gets it: doing so here
                // would unpack a bundle that is not going to be served. Older ones than the
                // newest that waits are never offered, and neither is one that this binary
                // cannot run; without this they would pile up on a device whose user never
                // approves.
                if waiting || !self.compatible(&version) {
                    superseded.push(version);
                } else {
                    waiting = true;
                }
                continue;
            }
            match self.load(&version) {
                Ok(bundle) => {
                    chosen = Some(bundle);
                    break;
                }
                Err(error) if error.is_permanent() => {
                    log::warn!("discarding frontend bundle {version}: {error}");
                    self.remove_quietly(&version);
                }
                Err(error) => log::warn!("skipping frontend bundle {version} for now: {error}"),
            }
        }

        // A fallback whose archive is gone, deleted just now as invalid or by something else,
        // is none. Left on record it would keep that version from being stored again.
        if state.confirmed.as_ref().is_some_and(|v| self.is_gone(v)) {
            state.confirmed = None;
        }
        let serving = chosen.as_ref().map(|bundle| &bundle.version);
        if !self.rollback && serving.is_some() {
            state.confirmed = serving.cloned();
        }
        if state.trial.as_ref() != serving || state.confirmed.as_ref() == serving {
            // a trial goes on only if the same bundle gets another attempt
            state.trial = None;
            state.attempts = 0;
        }
        // An approval is for a stored bundle that is ahead of the confirmed one. It stays on
        // record while that bundle is served without having been confirmed: until a page is
        // loaded from it, the approval is all that makes the next launch pick it again.
        if state.approved.as_ref().is_some_and(|approved| {
            self.is_gone(approved)
                || state
                    .confirmed
                    .as_ref()
                    .is_some_and(|confirmed| !self.order.newer(approved, confirmed))
        }) {
            state.approved = None;
        }
        let recorded = if state != old_state || rewrite {
            self.write_state(&state)
        } else {
            Ok(())
        };
        match &recorded {
            Ok(()) => {
                for version in rejected.iter().chain(&superseded) {
                    self.remove_quietly(version);
                }
                if let Some(bundle) = &chosen {
                    self.prune(&state, &bundle.version);
                }
            }
            // What is served in this run is decided either way. At the next launch the record
            // is the old one: a trial out of attempts is rejected again, a version rejected on
            // request (a rollback) is not, which is why the caller is told.
            Err(error) => log::warn!("could not record the OTA state: {error}"),
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
        (chosen, recorded)
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
