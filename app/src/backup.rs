//! Manual backup and restore of what only this machine keeps (Settings →
//! Data).
//!
//! The NATS bucket keeps a value for three minutes, so the viewer is the
//! only place where the plan usage over time (`history.json`), the tokens
//! per day of every machine (`activity.json`) and the other machines'
//! projects (`remote-projects.json`) survive. A backup puts them in one
//! `.ccbak` file, so they can be kept or moved to another PC:
//!
//! ```text
//! {"format":"cc-usage-backup","version":1,"created":"2026-10-14T12:00:00Z",
//!  "appVersion":"0.1.0","machine":"wk-01",
//!  "data":{"history":{…}|null,"activity":{…}|null,"remoteProjects":[…]|null,"pricing":{…}|null},
//!  "secrets":null|{"kdf":"argon2id","m":65536,"t":3,"p":1,"salt":"…","nonce":"…","ciphertext":"…"}}
//! ```
//!
//! The data sections are the files as they are on disk; a missing or
//! unreadable file is `null`. `secrets` is optional: the files that make a
//! machine the viewer, those of [`is_allowed_secret`] (the NATS
//! `nats/*.creds` and anything under `keys/`), as
//! `{"files":{"<path>":"<text>"}}` sealed with XChaCha20-Poly1305 under a
//! key Argon2id derives from a password. The parameters travel in the file,
//! so a later cost still opens a backup made with this one.
//!
//! A restore adds, it never replaces:
//!
//! - history: the samples this machine lacks ([`History::merge`]);
//! - activity: by the calendar's own rules, a day only grows
//!   ([`activity::restore`]). The backup's own days (key [`activity::LOCAL`])
//!   go under the backup's machine name when it was made on another
//!   machine, so they never mix with this machine's days; days the backup
//!   holds under this machine's name go to this machine's own;
//! - other machines' projects: through the hub given, a newer value wins
//!   ([`remote::restore_projects_of`]), checked and cut like a value heard
//!   from the bucket (name and models cut, timestamps clamped, an entry
//!   whose payload names another machine than its key dropped), and saved
//!   to `remote-projects.json`. The Viewer role passes its running hub; the
//!   Publisher role, which runs none, one that holds only that file
//!   ([`remote::projects_file_hub`]), so a later switch to Viewer finds
//!   them. This machine's own projects come from its transcripts and are
//!   skipped;
//! - `pricing.json`: only when this machine has none;
//! - secrets: a missing file is written, an identical one skipped. A
//!   different viewer key file (`keys/viewer.key`) gets the keys it lacks
//!   after its own (a viewer moved to a PC where the app already made its
//!   own key pair keeps that one as the newest and gains the old one, which
//!   the publishers still seal to); so does a different pin file
//!   (`keys/publishers/<alias>.pub`) or rejected-keys file
//!   (`<alias>.rejected`); the keys waiting for review (`<alias>.pending`)
//!   are never part of a backup; `keys/viewer.pub` is written again
//!   from the viewer key file. Any other different file, or a key id that
//!   holds different keys, is kept and reported as a conflict.
//!
//! Settings, window position and notification state are
//! machine-specific and never part of a backup. Every step is idempotent,
//! so a restore that stopped halfway can simply be run again.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use cc_core::projects::DayActivity;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

use crate::activity::{self, Store, LOCAL};
use crate::history::History;
use crate::remote::{self, SharedHub, StoredProject};

/// The `format` of every backup.
pub const FORMAT: &str = "cc-usage-backup";
/// The `version` this build writes and reads.
pub const VERSION: u32 = 1;
/// The file extension, without the dot.
pub const EXTENSION: &str = "ccbak";
/// Backups larger than this are refused, written or read.
pub const MAX_BACKUP_BYTES: u64 = 64 * 1024 * 1024;
/// A secret file larger than this is not a key or a credential.
pub const MAX_SECRET_FILE_BYTES: u64 = 1024 * 1024;
/// At most this many secret files.
pub const MAX_SECRET_FILES: usize = 256;
/// The shortest password that seals secrets, in characters.
pub const MIN_PASSWORD_CHARS: usize = 8;
/// The `kdf` of the secrets.
pub const KDF: &str = "argon2id";
/// The associated data of the sealed secrets.
const AAD: &[u8] = b"cc-usage-backup";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;

/// Argon2id's cost: memory in KiB, iterations and lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

/// What a new backup uses: 64 MiB, 3 passes, 1 lane, about a second.
pub const DEFAULT_KDF: KdfParams = KdfParams {
    m: 64 * 1024,
    t: 3,
    p: 1,
};

/// The most a backup may ask for, so a crafted file cannot make the
/// restore allocate gigabytes or run for minutes: 256 MiB, 16 passes, 8 lanes.
const MAX_KDF: KdfParams = KdfParams {
    m: 256 * 1024,
    t: 16,
    p: 8,
};

/// Why a backup could not be made, read or restored. User-facing; never
/// carries file contents or the password.
#[derive(Debug, PartialEq, Eq)]
pub enum BackupError {
    /// A file could not be read or written.
    Io(String),
    /// The backup is larger than [`MAX_BACKUP_BYTES`].
    TooLarge(u64),
    /// Not a cc-usage backup at all.
    NotABackup,
    /// Made by a newer cc-usage.
    NewerVersion(u32),
    /// A cc-usage backup with something wrong in it.
    Malformed(String),
    /// The password is too short, or missing where it is needed.
    Password(String),
    /// The secrets did not open: wrong password, or the file was altered.
    Decrypt,
    /// Including secrets was asked, but this machine has none.
    NoSecrets,
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(what) => write!(f, "{what}"),
            Self::TooLarge(len) => write!(
                f,
                "the backup is too large ({} MB, at most {} MB)",
                len / (1024 * 1024),
                MAX_BACKUP_BYTES / (1024 * 1024)
            ),
            Self::NotABackup => write!(f, "this is not a cc-usage backup"),
            Self::NewerVersion(v) => write!(
                f,
                "this backup was made by a newer cc-usage (format {v}); update the app to restore it"
            ),
            Self::Malformed(what) => write!(f, "the backup is damaged: {what}"),
            Self::Password(what) => write!(f, "{what}"),
            Self::Decrypt => write!(
                f,
                "wrong password, or the backup was altered: the keys and credentials did not open"
            ),
            Self::NoSecrets => write!(f, "this machine has no keys or credentials to include"),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<BackupError> for String {
    fn from(e: BackupError) -> Self {
        e.to_string()
    }
}

fn io_error(what: &str, path: &Path, err: std::io::Error) -> BackupError {
    BackupError::Io(format!("cannot {what} {}: {err}", path.display()))
}

/// A whole backup file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    pub format: String,
    pub version: u32,
    /// RFC 3339, UTC.
    pub created: String,
    pub app_version: String,
    /// The effective machine name of the machine that made it.
    pub machine: String,
    pub data: Data,
    #[serde(default)]
    pub secrets: Option<Sealed>,
}

/// The data files, as they were on disk; `None` (null) when missing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Data {
    #[serde(default)]
    pub history: Option<Value>,
    #[serde(default)]
    pub activity: Option<Value>,
    #[serde(default)]
    pub remote_projects: Option<Value>,
    #[serde(default)]
    pub pricing: Option<Value>,
}

/// The sealed secrets and how to open them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    pub kdf: String,
    pub m: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
    pub nonce: String,
    pub ciphertext: String,
}

/// The secret files, by path relative to the data folder with forward
/// slashes. Their text is wiped from memory on drop.
#[derive(Default, Serialize, Deserialize)]
pub struct SecretFiles {
    files: BTreeMap<String, String>,
}

impl SecretFiles {
    /// The paths, sorted.
    pub fn paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    /// Adds a file. Only for files that pass [`is_allowed_secret`].
    fn insert(&mut self, rel: String, text: String) {
        self.files.insert(rel, text);
    }
}

impl Drop for SecretFiles {
    fn drop(&mut self) {
        for text in self.files.values_mut() {
            text.zeroize();
        }
    }
}

impl std::fmt::Debug for SecretFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretFiles")
            .field("paths", &self.paths())
            .finish()
    }
}

/// Whether `rel` (relative, forward slashes) names a file a backup may
/// carry as a secret: `nats/<name>.creds` or anything under `keys/`.
/// Absolute paths, `.` and `..`, backslashes, drive letters and characters
/// Windows does not allow in names are refused.
pub fn is_allowed_secret(rel: &str) -> bool {
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') || rel.contains(':') {
        return false;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let bad_part = |p: &&str| {
        p.is_empty()
            || *p == "."
            || *p == ".."
            || p.chars().any(|c| c.is_control() || "<>\"|?*".contains(c))
    };
    if parts.iter().any(bad_part) {
        return false;
    }
    match parts.as_slice() {
        ["nats", name] => name.len() > ".creds".len() && name.ends_with(".creds"),
        // The publisher keys waiting for review are this run's questions,
        // not keys: never carried, so a restore asks nothing on its own.
        ["keys", "publishers", name] if name.ends_with(".pending") => false,
        ["keys", rest @ ..] => !rest.is_empty(),
        _ => false,
    }
}

/// `rel` under `data_dir`, one component at a time.
fn secret_path(data_dir: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(data_dir.to_path_buf(), |p, part| p.join(part))
}

/// Checks the password that seals secrets.
pub fn check_password(password: &str) -> Result<(), BackupError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(BackupError::Password(format!(
            "use a password of at least {MIN_PASSWORD_CHARS} characters"
        )));
    }
    Ok(())
}

// ── Making a backup ──────────────────────────────────────────────────────────

/// A backup and the data files left out because they could not be read.
#[derive(Debug)]
pub struct Created {
    pub backup: Backup,
    pub skipped: Vec<String>,
}

/// Reads a data file and checks it parses as what it is: `Ok(None)` when it
/// is missing, `Err(())` when it is there but unreadable.
fn read_data<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<Value>, ()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let value: Value = serde_json::from_str(&text).map_err(|_| ())?;
    serde_json::from_value::<T>(value.clone()).map_err(|_| ())?;
    Ok(Some(value))
}

/// The backup of the data folder `data_dir`, made by `machine` at `created`.
/// With `secrets`, the files of [`is_allowed_secret`] are sealed with that
/// password and cost; it fails if this machine has none. A data file that
/// cannot be read is left out (null) and named in [`Created::skipped`].
pub fn create(
    data_dir: &Path,
    machine: &str,
    secrets: Option<(&str, KdfParams)>,
    created: DateTime<Utc>,
) -> Result<Created, BackupError> {
    let mut skipped = Vec::new();
    let mut take = |name: &str, read: fn(&Path) -> Result<Option<Value>, ()>| {
        read(&data_dir.join(name)).unwrap_or_else(|()| {
            skipped.push(name.to_string());
            None
        })
    };
    let data = Data {
        history: take(crate::paths::HISTORY, read_data::<History>),
        activity: take(activity::FILE, read_data::<Store>),
        remote_projects: take(
            remote::REMOTE_PROJECTS_FILE,
            read_data::<Vec<StoredProject>>,
        ),
        pricing: take(
            crate::paths::PRICING,
            read_data::<serde_json::Map<String, Value>>,
        ),
    };
    let secrets = match secrets {
        None => None,
        Some((password, kdf)) => {
            check_password(password)?;
            let files = collect_secrets(data_dir)?;
            if files.files.is_empty() {
                return Err(BackupError::NoSecrets);
            }
            Some(seal(&files, password, kdf)?)
        }
    };
    Ok(Created {
        backup: Backup {
            format: FORMAT.into(),
            version: VERSION,
            created: created.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            app_version: env!("CARGO_PKG_VERSION").into(),
            machine: machine.into(),
            data,
            secrets,
        },
        skipped,
    })
}

/// The secret files under `data_dir`. Symbolic links are not followed.
pub fn collect_secrets(data_dir: &Path) -> Result<SecretFiles, BackupError> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(data_dir.join("nats")) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                let rel = format!("nats/{name}");
                if is_allowed_secret(&rel) && is_plain_file(&entry.path()) {
                    found.push((rel, entry.path()));
                }
            }
        }
    }
    walk_keys(&data_dir.join("keys"), "keys", 0, &mut found);
    if found.len() > MAX_SECRET_FILES {
        return Err(BackupError::Io(format!(
            "more than {MAX_SECRET_FILES} key and credential files"
        )));
    }
    let mut files = SecretFiles::default();
    for (rel, path) in found {
        let text = read_secret(&path).map_err(|why| BackupError::Io(format!("{rel}: {why}")))?;
        files.insert(rel, text);
    }
    Ok(files)
}

/// Files under `keys/`, down to a few levels.
fn walk_keys(dir: &Path, rel: &str, depth: usize, found: &mut Vec<(String, PathBuf)>) {
    if depth > 8 || found.len() > MAX_SECRET_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let child = format!("{rel}/{name}");
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() {
            walk_keys(&entry.path(), &child, depth + 1, found);
        } else if meta.is_file() && is_allowed_secret(&child) {
            found.push((child, entry.path()));
        }
    }
}

fn is_plain_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

/// A secret file's text, refusing large or non-text files. The reason never
/// includes the contents.
fn read_secret(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_SECRET_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_SECRET_FILE_BYTES {
        return Err("too large for a key or a credential".into());
    }
    match std::str::from_utf8(&bytes) {
        Ok(text) => Ok(text.to_string()),
        Err(_) => Err("not a text file".into()),
    }
}

/// The backup as written to disk. Fails if it would be too large to read
/// back.
pub fn to_bytes(backup: &Backup) -> Result<Vec<u8>, BackupError> {
    let bytes = serde_json::to_vec(backup).map_err(|e| BackupError::Io(e.to_string()))?;
    if bytes.len() as u64 > MAX_BACKUP_BYTES {
        return Err(BackupError::TooLarge(bytes.len() as u64));
    }
    Ok(bytes)
}

/// Writes the backup to `path`, atomically.
pub fn write(path: &Path, backup: &Backup) -> Result<(), BackupError> {
    let bytes = to_bytes(backup)?;
    crate::paths::write_atomic(path, &bytes).map_err(|e| io_error("write", path, e))
}

/// `cc-usage-backup-<machine>-<YYYYMMDD-HHMM>.ccbak`, at `now`.
pub fn default_file_name<Tz: chrono::TimeZone>(machine: &str, now: DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    format!(
        "cc-usage-backup-{machine}-{}.{EXTENSION}",
        now.format("%Y%m%d-%H%M")
    )
}

// ── Sealing the secrets ─────────────────────────────────────────────────────

fn derive_key(
    password: &str,
    salt: &[u8],
    kdf: KdfParams,
) -> Result<Zeroizing<[u8; 32]>, BackupError> {
    let bad = || BackupError::Malformed("invalid key derivation parameters".into());
    let params = Params::new(kdf.m, kdf.t, kdf.p, Some(32)).map_err(|_| bad())?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, key.as_mut())
        .map_err(|_| bad())?;
    Ok(key)
}

fn random<const N: usize>() -> Result<[u8; N], BackupError> {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| BackupError::Io(format!("the system random number generator failed: {e}")))?;
    Ok(bytes)
}

/// Seals `files` with `password`.
pub fn seal(files: &SecretFiles, password: &str, kdf: KdfParams) -> Result<Sealed, BackupError> {
    let salt = random::<SALT_LEN>()?;
    let nonce = random::<NONCE_LEN>()?;
    let key = derive_key(password, &salt, kdf)?;
    let plaintext =
        Zeroizing::new(serde_json::to_vec(files).map_err(|e| BackupError::Io(e.to_string()))?);
    let ciphertext = XChaCha20Poly1305::new((&*key).into())
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &plaintext,
                aad: AAD,
            },
        )
        .map_err(|_| BackupError::Io("encryption failed".into()))?;
    Ok(Sealed {
        kdf: KDF.into(),
        m: kdf.m,
        t: kdf.t,
        p: kdf.p,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(ciphertext),
    })
}

/// Opens sealed secrets with `password`. Every path must pass
/// [`is_allowed_secret`], or the whole set is refused.
pub fn open(sealed: &Sealed, password: &str) -> Result<SecretFiles, BackupError> {
    let bad = |what: &str| BackupError::Malformed(what.into());
    if sealed.kdf != KDF {
        return Err(bad("unknown key derivation"));
    }
    if sealed.m > MAX_KDF.m || sealed.t > MAX_KDF.t || sealed.p > MAX_KDF.p {
        return Err(bad("key derivation parameters out of range"));
    }
    let salt = STANDARD.decode(&sealed.salt).map_err(|_| bad("salt"))?;
    let nonce = STANDARD.decode(&sealed.nonce).map_err(|_| bad("nonce"))?;
    let ciphertext = STANDARD
        .decode(&sealed.ciphertext)
        .map_err(|_| bad("ciphertext"))?;
    if salt.len() != SALT_LEN {
        return Err(bad("salt or nonce length"));
    }
    let nonce = XNonce::try_from(nonce.as_slice()).map_err(|_| bad("salt or nonce length"))?;
    let kdf = KdfParams {
        m: sealed.m,
        t: sealed.t,
        p: sealed.p,
    };
    let key = derive_key(password, &salt, kdf)?;
    let plaintext = Zeroizing::new(
        XChaCha20Poly1305::new((&*key).into())
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: AAD,
                },
            )
            .map_err(|_| BackupError::Decrypt)?,
    );
    let files: SecretFiles =
        serde_json::from_slice(&plaintext).map_err(|_| bad("the keys and credentials"))?;
    if files.files.len() > MAX_SECRET_FILES {
        return Err(bad("too many key and credential files"));
    }
    if let Some(rel) = files.files.keys().find(|rel| !is_allowed_secret(rel)) {
        return Err(BackupError::Malformed(format!(
            "it names a file that is not a key or a credential: {rel:?}"
        )));
    }
    Ok(files)
}

// ── Reading a backup ─────────────────────────────────────────────────────────

/// Reads and checks a backup file.
pub fn read(path: &Path) -> Result<Backup, BackupError> {
    let file = std::fs::File::open(path).map_err(|e| io_error("open", path, e))?;
    let len = file
        .metadata()
        .map_err(|e| io_error("read", path, e))?
        .len();
    if len > MAX_BACKUP_BYTES {
        return Err(BackupError::TooLarge(len));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BACKUP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read", path, e))?;
    parse(&bytes)
}

/// Parses and checks a backup.
pub fn parse(bytes: &[u8]) -> Result<Backup, BackupError> {
    if bytes.len() as u64 > MAX_BACKUP_BYTES {
        return Err(BackupError::TooLarge(bytes.len() as u64));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| BackupError::NotABackup)?;
    if value.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(BackupError::NotABackup);
    }
    match value.get("version").and_then(Value::as_u64) {
        Some(v) if v == u64::from(VERSION) => {}
        Some(v) if v > u64::from(VERSION) => {
            return Err(BackupError::NewerVersion(
                u32::try_from(v).unwrap_or(u32::MAX),
            ))
        }
        _ => return Err(BackupError::Malformed("unknown version".into())),
    }
    let backup: Backup =
        serde_json::from_value(value).map_err(|e| BackupError::Malformed(e.to_string()))?;
    if cc_envelope::validate_alias(&backup.machine).is_err() {
        return Err(BackupError::Malformed(
            "the machine name is not valid".into(),
        ));
    }
    if DateTime::parse_from_rfc3339(&backup.created).is_err() {
        return Err(BackupError::Malformed(
            "the creation date is not valid".into(),
        ));
    }
    Ok(backup)
}

/// The data of a backup, parsed.
#[derive(Debug, Default)]
pub struct Contents {
    pub history: Option<History>,
    pub activity: Option<Store>,
    pub remote_projects: Option<Vec<StoredProject>>,
    pub pricing: Option<serde_json::Map<String, Value>>,
}

/// Parses the data sections of `backup`.
pub fn contents(backup: &Backup) -> Result<Contents, BackupError> {
    fn section<T: serde::de::DeserializeOwned>(
        v: &Option<Value>,
        name: &str,
    ) -> Result<Option<T>, BackupError> {
        v.as_ref()
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|_| BackupError::Malformed(format!("the {name} section")))
    }
    Ok(Contents {
        history: section(&backup.data.history, "history")?,
        activity: section(&backup.data.activity, "activity")?,
        remote_projects: section(&backup.data.remote_projects, "projects")?,
        pricing: section(&backup.data.pricing, "pricing")?,
    })
}

/// What Settings → Data shows before a restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub path: String,
    pub machine: String,
    pub created: String,
    pub app_version: String,
    pub has_secrets: bool,
    /// Made on a machine with this machine's name.
    pub same_machine: bool,
    pub counts: Counts,
}

/// How much a backup holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub history_samples: usize,
    pub activity_machines: usize,
    pub activity_days: usize,
    pub remote_projects: usize,
    pub has_pricing: bool,
}

/// The summary of `backup`, read from `path`, for a restore on `this_machine`.
pub fn summary(backup: &Backup, contents: &Contents, path: &Path, this_machine: &str) -> Summary {
    let activity = contents.activity.as_ref();
    Summary {
        path: path.to_string_lossy().into_owned(),
        machine: backup.machine.clone(),
        created: backup.created.clone(),
        app_version: backup.app_version.clone(),
        has_secrets: backup.secrets.is_some(),
        same_machine: backup.machine == this_machine,
        counts: Counts {
            history_samples: contents.history.as_ref().map_or(0, |h| h.samples.len()),
            activity_machines: activity.map_or(0, |a| a.machines.len()),
            activity_days: activity.map_or(0, |a| a.machines.values().map(|d| d.len()).sum()),
            remote_projects: contents.remote_projects.as_ref().map_or(0, Vec::len),
            has_pricing: contents.pricing.is_some(),
        },
    }
}

// ── Restoring ───────────────────────────────────────────────────────────────

/// A backup checked and opened, ready to merge. Nothing has changed yet.
#[derive(Debug)]
pub struct Prepared {
    machine: String,
    contents: Contents,
    secrets: Secrets,
}

#[derive(Debug)]
enum Secrets {
    /// The backup has none.
    None,
    /// The backup has some, but no password was given.
    Withheld,
    Files(SecretFiles),
}

/// Checks `backup` and opens its secrets with `password`, before anything
/// is merged: a wrong password fails the restore with nothing changed.
/// Without a password the data is restored and the secrets are not.
pub fn prepare(backup: &Backup, password: Option<&str>) -> Result<Prepared, BackupError> {
    let contents = contents(backup)?;
    let secrets = match (&backup.secrets, password.filter(|p| !p.is_empty())) {
        (None, _) => Secrets::None,
        (Some(_), None) => Secrets::Withheld,
        (Some(sealed), Some(password)) => Secrets::Files(open(sealed, password)?),
    };
    Ok(Prepared {
        machine: backup.machine.clone(),
        contents,
        secrets,
    })
}

/// Where and when a restore happens.
#[derive(Debug, Clone)]
pub struct Target<'a> {
    /// The data folder.
    pub data_dir: &'a Path,
    /// This machine's effective name.
    pub this_machine: &'a str,
    /// Unix seconds, for the projects' rules.
    pub now: i64,
    /// The activity days kept, `YYYY-MM-DD` (see [`activity::restore`]).
    pub oldest: String,
    pub latest: String,
}

impl<'a> Target<'a> {
    /// A restore into `data_dir` on `this_machine`, now.
    pub fn new(data_dir: &'a Path, this_machine: &'a str) -> Self {
        let oldest = (chrono::Local::now() - chrono::Duration::days(activity::KEEP_DAYS))
            .format("%Y-%m-%d")
            .to_string();
        Self {
            data_dir,
            this_machine,
            now: remote::now_unix(),
            oldest,
            latest: activity::latest_day(),
        }
    }
}

/// What a restore did, for Settings → Data.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub history_added: usize,
    pub activity: Vec<ActivityRestored>,
    pub projects_added: usize,
    pub projects_updated: usize,
    /// This machine's own projects, which its transcripts provide.
    pub projects_skipped: usize,
    pub pricing: PricingOutcome,
    /// `None` when the backup carries no secrets, or they were withheld.
    pub secrets: Option<SecretsReport>,
    /// The backup has secrets, but no password was given.
    pub secrets_withheld: bool,
}

/// One machine's days in the [`Report`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRestored {
    /// The name the calendar shows.
    pub machine: String,
    /// This machine's own days.
    pub local: bool,
    pub added: usize,
    pub raised: usize,
}

/// What happened to `pricing.json`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PricingOutcome {
    /// The backup has none.
    #[default]
    None,
    /// Written: this machine had none.
    Restored,
    /// This machine's own was kept.
    Kept,
}

/// What happened to each secret file, by path.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsReport {
    /// Missing here, written.
    pub written: Vec<String>,
    /// A key file (viewer keys, pins), given the keys it lacked.
    pub merged: Vec<String>,
    /// Already here with the same content (or, for a key file, every key).
    pub skipped: Vec<String>,
    /// Here with other content: this machine's version was kept.
    pub conflicts: Vec<String>,
}

/// Merges a prepared backup into this machine: `history` (the running
/// app's, saved to the data folder), `activity.json`, the hub's projects
/// (saved by the hub), `pricing.json` and the secrets, as the module docs
/// describe.
pub fn restore(
    prepared: &Prepared,
    history: &mut History,
    hub: &SharedHub,
    target: &Target<'_>,
) -> Result<Report, BackupError> {
    let dir = target.data_dir;
    let mut report = Report::default();

    if let Some(incoming) = &prepared.contents.history {
        let incoming = History {
            samples: incoming.samples.clone(),
        };
        report.history_added = history.merge(incoming);
        if report.history_added > 0 {
            let path = dir.join(crate::paths::HISTORY);
            history
                .save_to(&path)
                .map_err(|e| io_error("save", &path, e))?;
        }
    }

    if let Some(store) = &prepared.contents.activity {
        let incoming = activity_for(store, &prepared.machine, target.this_machine);
        let path = dir.join(activity::FILE);
        report.activity = activity::restore(&path, &incoming, &target.oldest, &target.latest)
            .map_err(|e| BackupError::Io(e.to_string()))?
            .into_iter()
            .map(|r| ActivityRestored {
                local: r.machine == LOCAL,
                machine: if r.machine == LOCAL {
                    target.this_machine.to_string()
                } else {
                    r.machine
                },
                added: r.added,
                raised: r.raised,
            })
            .collect();
    }

    if let Some(projects) = &prepared.contents.remote_projects {
        let (own, others): (Vec<StoredProject>, Vec<StoredProject>) = projects
            .iter()
            .cloned()
            .partition(|p| p.alias == target.this_machine);
        report.projects_skipped = own.len();
        (report.projects_added, report.projects_updated) =
            remote::restore_projects_of(hub, others, target.now);
    }

    if let Some(pricing) = &prepared.contents.pricing {
        let path = dir.join(crate::paths::PRICING);
        report.pricing = if std::fs::symlink_metadata(&path).is_ok() {
            PricingOutcome::Kept
        } else {
            let json = serde_json::to_string_pretty(pricing)
                .map_err(|e| BackupError::Io(e.to_string()))?;
            crate::paths::write_atomic(&path, json.as_bytes())
                .map_err(|e| io_error("write", &path, e))?;
            PricingOutcome::Restored
        };
    }

    match &prepared.secrets {
        Secrets::None => {}
        Secrets::Withheld => report.secrets_withheld = true,
        Secrets::Files(files) => report.secrets = Some(restore_secrets(files, dir)?),
    }
    Ok(report)
}

/// The backup's activity by the key each machine goes under here. The
/// backup's own days ([`LOCAL`]) are its machine's; a machine with this
/// machine's name is this machine ([`LOCAL`]). Keys that are not a valid
/// machine name are dropped.
fn activity_for(
    store: &Store,
    backup_machine: &str,
    this_machine: &str,
) -> Vec<activity::Incoming> {
    store
        .machines
        .iter()
        .filter_map(|(key, days)| {
            let name = if key == LOCAL {
                backup_machine
            } else {
                key.as_str()
            };
            if cc_envelope::validate_alias(name).is_err() {
                return None;
            }
            let target = if name == this_machine { LOCAL } else { name };
            let days = days
                .iter()
                .map(|(day, d)| DayActivity {
                    day: day.clone(),
                    tokens: d.t,
                    peak_hour: d.h,
                })
                .collect();
            Some(activity::Incoming {
                machine: target.to_string(),
                days,
            })
        })
        .collect()
}

/// Writes the secret files under `data_dir` by the module's rules.
fn restore_secrets(files: &SecretFiles, data_dir: &Path) -> Result<SecretsReport, BackupError> {
    let mut report = SecretsReport::default();
    for (rel, text) in &files.files {
        // `open` already refused any other path; checked again here, where
        // it is written.
        if !is_allowed_secret(rel) {
            return Err(BackupError::Malformed(format!(
                "it names a file that is not a key or a credential: {rel:?}"
            )));
        }
        let path = secret_path(data_dir, rel);
        match std::fs::symlink_metadata(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                write_private(&path, text.as_bytes()).map_err(|e| io_error("write", &path, e))?;
                report.written.push(rel.clone());
            }
            Ok(meta) if meta.is_file() => match read_secret(&path) {
                Ok(have) if have == *text => {
                    let mut have = have;
                    have.zeroize();
                    report.skipped.push(rel.clone());
                }
                Ok(have) => {
                    let have = Zeroizing::new(have);
                    let merged = match rel.as_str() {
                        VIEWER_KEY_FILE => Some(merge_viewer_key(&path, &have, text)),
                        VIEWER_PUB_FILE => Some(derive_viewer_pub(data_dir, &path)),
                        _ if is_pin_file(rel) || is_rejected_file(rel) => {
                            Some(merge_public_keys(&path, &have, text))
                        }
                        _ => None,
                    };
                    match merged {
                        Some(Ok(0)) => report.skipped.push(rel.clone()),
                        Some(Ok(_)) => report.merged.push(rel.clone()),
                        Some(Err(_)) | None => report.conflicts.push(rel.clone()),
                    }
                }
                Err(_) => report.conflicts.push(rel.clone()),
            },
            _ => report.conflicts.push(rel.clone()),
        }
    }
    Ok(report)
}

/// The viewer's private keys, relative to the data folder.
const VIEWER_KEY_FILE: &str = "keys/viewer.key";

/// The viewer's public keys, derived from [`VIEWER_KEY_FILE`].
const VIEWER_PUB_FILE: &str = "keys/viewer.pub";

/// Whether `rel` is a publisher's pin file, `keys/publishers/<name>.pub`.
fn is_pin_file(rel: &str) -> bool {
    matches!(rel.split('/').collect::<Vec<_>>().as_slice(),
        ["keys", "publishers", name] if name.len() > ".pub".len() && name.ends_with(".pub"))
}

/// Whether `rel` is a publisher's rejected keys, `keys/publishers/<name>.rejected`
/// (the key file format, like a pin file).
fn is_rejected_file(rel: &str) -> bool {
    matches!(rel.split('/').collect::<Vec<_>>().as_slice(),
        ["keys", "publishers", name] if name.len() > ".rejected".len() && name.ends_with(".rejected"))
}

/// The viewer key file of this machine (holding `have`) gets the keys of
/// the backup's that it lacks, after its own: a viewer moved to a new PC,
/// where the app made a key of its own on first start, opens what the
/// publishers still seal to the old key. Returns how many were added.
fn merge_viewer_key(
    path: &Path,
    have: &str,
    incoming: &str,
) -> Result<usize, cc_envelope::KeyFileError> {
    let mut keys = cc_envelope::SecretKeys::parse(have)?;
    let added = keys.append_missing(&cc_envelope::SecretKeys::parse(incoming)?)?;
    if added > 0 {
        keys.save(path)?;
    }
    Ok(added)
}

/// `viewer.pub` follows `viewer.key` (restored first: the files go in path
/// order): it is written again from the private keys on disk instead of
/// being taken from the backup. Returns 1 when it changed.
fn derive_viewer_pub(data_dir: &Path, path: &Path) -> Result<usize, cc_envelope::KeyFileError> {
    let keys = cc_envelope::SecretKeys::load(secret_path(data_dir, VIEWER_KEY_FILE))?;
    let public = keys.public_keys();
    if cc_envelope::PublicKeys::load(path).ok().as_ref() == Some(&public) {
        return Ok(0);
    }
    public.save(path)?;
    Ok(1)
}

/// A publisher's pins here get the pins of the backup that they lack.
fn merge_public_keys(
    path: &Path,
    have: &str,
    incoming: &str,
) -> Result<usize, cc_envelope::KeyFileError> {
    let mut keys = cc_envelope::PublicKeys::parse(have)?;
    let added = keys.append_missing(&cc_envelope::PublicKeys::parse(incoming)?)?;
    if added > 0 {
        keys.save(path)?;
    }
    Ok(added)
}

/// Writes a new file only the current user can read, where the OS allows
/// (mode `0600` on Unix; on Windows it inherits the data folder's ACL, as
/// the key files do), through a temporary file and a rename:
/// [`cc_core::fsutil::write_private_atomic`].
pub(crate) fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    cc_core::fsutil::write_private_atomic(path, contents)
}

#[cfg(test)]
mod tests;
