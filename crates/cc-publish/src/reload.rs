//! Key and creds changes picked up by a running publisher, without a
//! restart.
//!
//! [`Keys`] holds the sealer the pipeline uses and the files it came from:
//! `keys/publisher.key`, `keys/viewer.pub` and the creds file. Every
//! [`RECHECK_EVERY`] (and at once when the cc-usage app asks, after an
//! import) [`Keys::recheck`] compares a cheap signature of each file, its
//! length and modification time ([`Stamp`]), with the one it last saw:
//!
//! - a key file that changed is loaded again; keys that load and differ
//!   become the sealer ([`Change::Applied`]), keys that do not load are
//!   reported ([`Change::Rejected`]) and the last good sealer stays. There
//!   is never a fall back to anything weaker: a missing or broken file only
//!   means the old keys go on sealing.
//! - a creds file that changed is checked first ([`creds_for`]: this
//!   publisher's user, a seed, not expired). One that passes is a *trial*:
//!   the network side opens a second connection with it and binds the
//!   bucket ([`Keys::begin_trial`]), and only when that works does the new
//!   connection replace the one in use ([`Keys::trial_succeeded`]). Until
//!   then, and when it fails, the connection in use keeps the credentials
//!   async-nats holds in memory: a stale, expired or revoked file never
//!   takes a working publisher offline. A failed trial is recorded and
//!   tried again after [`trial_delay`] ([`Keys::trial_failed`]). A file
//!   that does not pass the checks is reported and never tried, and while
//!   it is the one on disk the network side does not rebuild its connection
//!   after failing writes ([`Keys::disk_creds_refused`]): a rebuild would
//!   read it. Only a connection that async-nats itself closed for good is
//!   rebuilt then, and that one fails until the file is fixed.
//!
//! What is in use goes to the state file as [`InUse`], so `cc-publish
//! status` can tell it from what the files on disk hold.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use cc_envelope::Sealer;

use crate::sealing::{select_files, SealingError};
use crate::state::InUse;

/// How often a running publisher looks at its key and creds files again.
pub const RECHECK_EVERY: Duration = Duration::from_secs(30);

/// Delay before the first retry of a creds file whose trial connection
/// failed; it doubles with every further failure, up to [`TRIAL_RETRY_MAX`].
pub const TRIAL_RETRY_FIRST: Duration = Duration::from_secs(30);

/// Upper bound for the delay between trials of the same creds file.
pub const TRIAL_RETRY_MAX: Duration = Duration::from_secs(10 * 60);

/// Largest creds file read; real ones are about 1 KiB.
const MAX_CREDS_LEN: u64 = 64 * 1024;

/// A file's cheap signature: its length and modification time. `None` (as
/// `Option<Stamp>`) for a missing file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// Length in bytes.
    pub len: u64,
    /// Modification time, when the file system has one.
    pub modified: Option<SystemTime>,
}

impl Stamp {
    /// The signature of `path` now; `None` when it cannot be read.
    pub fn of(path: &Path) -> Option<Stamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Stamp {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// The files a publisher seals and connects with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFiles {
    /// This machine's private key file, `keys/publisher.key`.
    pub publisher_key: PathBuf,
    /// The viewer's public key file, `keys/viewer.pub`.
    pub viewer_pub: PathBuf,
    /// The publisher user's `.creds` file.
    pub creds: PathBuf,
    /// The NATS user the creds must be for, `cc-sessions.pub-<alias>`.
    pub user: String,
}

/// What a recheck did with one kind of file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Nothing new: the file did not change, or holds what is in use.
    Unchanged,
    /// Keys: the change is in use now. Creds: the file passed the checks
    /// and a trial connection with it is due; it replaces the connection in
    /// use once that is up, which the state file records (`creds_user`, or
    /// `creds_error` when the trial fails).
    Applied,
    /// The file changed but cannot be used; what was in use stays. The text
    /// names the file and why, never its contents.
    Rejected(String),
}

/// What one [`Keys::recheck`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reload {
    /// The two key files.
    pub keys: Change,
    /// The creds file.
    pub creds: Change,
}

impl Reload {
    /// Whether anything was applied or rejected.
    pub fn changed(&self) -> bool {
        self.keys != Change::Unchanged || self.creds != Change::Unchanged
    }
}

/// Whether two sealers seal with the same keys: the same public halves of
/// this machine's keys, the same viewer keys.
pub fn same_keys(a: &Sealer, b: &Sealer) -> bool {
    a.recipient == b.recipient && a.sender.public_keys() == b.sender.public_keys()
}

/// What a freshly loaded key pair means next to `current`: the change, and
/// the new sealer when it is to be used.
pub fn key_change(
    current: &Sealer,
    loaded: Result<Sealer, SealingError>,
) -> (Change, Option<Sealer>) {
    match loaded {
        Err(err) => (Change::Rejected(err.to_string()), None),
        Ok(fresh) if same_keys(current, &fresh) => (Change::Unchanged, None),
        Ok(fresh) => (Change::Applied, Some(fresh)),
    }
}

/// Whether the text of a creds file is usable by `user` at Unix time
/// `now`: its user name, or why not (user-facing, without the file name;
/// the user name and expiry it found, never the secrets). The file needs
/// a user JWT whose `name` is `user` and whose `exp`, when it has one, is
/// still ahead, and a user seed block. The seed is only looked for, never
/// decoded or kept.
pub fn judge_creds(text: &str, user: &str, now: f64) -> Result<String, String> {
    let not_creds = || "is not a NATS credentials file".to_string();
    let claims = block_line(text, "BEGIN NATS USER JWT")
        .and_then(|jwt| jwt.split('.').nth(1))
        .and_then(|c| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(c.trim_end_matches('='))
                .ok()
        })
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .ok_or_else(not_creds)?;
    let name = claims
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(not_creds)?;
    if name != user {
        return Err(format!("is for {name}, not {user}"));
    }
    if !block_line(text, "BEGIN USER NKEY SEED").is_some_and(|seed| seed.starts_with("SU")) {
        return Err("has no user seed (the NKEY SEED block)".into());
    }
    // NATS JWTs carry `exp` as Unix seconds; none (or 0) never expires.
    if let Some(exp) = claims
        .get("exp")
        .and_then(|e| e.as_f64())
        .filter(|e| *e > 0.0)
    {
        if exp <= now {
            return Err(format!("expired on {}", crate::util::format_utc(exp)));
        }
    }
    Ok(name.to_string())
}

/// The first non-empty line after the line holding `begin`, when it is not
/// the block's end marker.
fn block_line<'a>(text: &'a str, begin: &str) -> Option<&'a str> {
    let mut lines = text.lines();
    lines.find(|l| l.contains(begin))?;
    lines
        .map(str::trim)
        .find(|l| !l.is_empty())
        .filter(|l| !l.starts_with('-'))
}

/// Whether the creds file at `path` is usable by `user` now ([`judge_creds`]):
/// its user name, or why not (user-facing, naming the file; never the
/// secrets).
pub fn creds_for(path: &Path, user: &str) -> Result<String, String> {
    let named = |why: String| format!("{} {why}", path.display());
    if !path.is_file() {
        return Err(named("is missing".into()));
    }
    let text = std::fs::metadata(path)
        .ok()
        .filter(|m| m.len() <= MAX_CREDS_LEN)
        .and_then(|_| std::fs::read_to_string(path).ok())
        .ok_or_else(|| named("is not a NATS credentials file".into()))?;
    judge_creds(&text, user, crate::util::now_secs()).map_err(named)
}

/// Delay before the next trial of a creds file that failed `failures`
/// times (1-based): [`TRIAL_RETRY_FIRST`], doubling, capped at
/// [`TRIAL_RETRY_MAX`].
pub fn trial_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    TRIAL_RETRY_FIRST
        .saturating_mul(1 << doublings)
        .min(TRIAL_RETRY_MAX)
}

/// A creds file that passed the checks and is not in use yet.
#[derive(Debug, Clone, Copy)]
struct Pending {
    /// Which accepted change it is: a newer file supersedes it.
    generation: u64,
    /// Failed trials so far.
    failures: u32,
    /// Not tried before this.
    next_at: Instant,
}

/// A trial under way, from [`Keys::begin_trial`] to
/// [`Keys::trial_succeeded`] or [`Keys::trial_failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredsTrial {
    generation: u64,
    /// The user the file is for.
    pub user: String,
}

/// What [`Keys::begin_trial`] says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrialStep {
    /// No creds file waiting, or not due yet.
    Wait,
    /// The waiting file no longer passes the checks (it expired meanwhile,
    /// say): dropped and recorded; the text says why.
    Rejected(String),
    /// Open a connection with the creds file now, and report how it went.
    Open(CredsTrial),
}

struct Inner {
    sealer: Arc<Sealer>,
    /// Stamps of `publisher_key` and `viewer_pub` when last loaded.
    key_stamps: (Option<Stamp>, Option<Stamp>),
    /// Stamp of the creds file when last looked at.
    creds_stamp: Option<Stamp>,
    /// The creds file waiting for its trial, when it passed the checks.
    pending: Option<Pending>,
    /// Counts accepted creds changes, for [`Pending::generation`].
    generation: u64,
    creds_user: Option<String>,
    key_error: Option<String>,
    creds_error: Option<String>,
    /// The creds file on disk failed the checks ([`creds_for`]).
    disk_refused: bool,
}

/// The sealer in use and the files it came from, shared by the network
/// side (which seals with it and rechecks it) and whoever may ask for a
/// recheck (the cc-usage app after an import). See the module docs.
pub struct Keys {
    files: KeyFiles,
    inner: Mutex<Inner>,
    /// Wakes the network side after a recheck from elsewhere.
    wake: tokio::sync::Notify,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("files", &self.files)
            .field("in_use", &self.in_use().describe())
            .finish_non_exhaustive()
    }
}

impl Keys {
    /// Loads the sealer of `files`. At start there is nothing to keep, so a
    /// missing or broken key file is an error here.
    pub fn load(files: KeyFiles) -> Result<Self, SealingError> {
        let key_stamps = (
            Stamp::of(&files.publisher_key),
            Stamp::of(&files.viewer_pub),
        );
        let sealer = select_files(&files.publisher_key, &files.viewer_pub)?;
        let creds_stamp = Stamp::of(&files.creds);
        Ok(Self {
            inner: Mutex::new(Inner {
                sealer: Arc::new(sealer),
                key_stamps,
                creds_stamp,
                pending: None,
                generation: 0,
                creds_user: None,
                key_error: None,
                creds_error: None,
                disk_refused: false,
            }),
            files,
            wake: tokio::sync::Notify::new(),
        })
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The files it watches.
    pub fn files(&self) -> &KeyFiles {
        &self.files
    }

    /// The sealer to seal with now.
    pub fn sealer(&self) -> Arc<Sealer> {
        Arc::clone(&self.inner().sealer)
    }

    /// What is in use, for the state file.
    pub fn in_use(&self) -> InUse {
        let inner = self.inner();
        InUse {
            publisher_kid: inner.sealer.sender.current().kid().to_string(),
            viewer_kid: inner.sealer.recipient.current().kid().to_string(),
            creds_user: inner.creds_user.clone(),
            key_error: inner.key_error.clone(),
            creds_error: inner.creds_error.clone(),
        }
    }

    /// Looks at the files again (see the module docs). Key files are loaded
    /// again when their stamps changed, or always with `force` (after an
    /// import, which may rewrite a file within one tick of its clock); the
    /// creds when their stamp changed. Creds that pass [`creds_for`] wait
    /// for a trial, due at once ([`Keys::begin_trial`]); a newer file
    /// replaces a waiting one. Wakes the network side when something
    /// changed.
    pub fn recheck(&self, force: bool) -> Reload {
        let key_stamps = (
            Stamp::of(&self.files.publisher_key),
            Stamp::of(&self.files.viewer_pub),
        );
        let creds_stamp = Stamp::of(&self.files.creds);
        let mut inner = self.inner();

        let keys = if force || key_stamps != inner.key_stamps {
            inner.key_stamps = key_stamps;
            let loaded = select_files(&self.files.publisher_key, &self.files.viewer_pub);
            let (change, fresh) = key_change(&inner.sealer, loaded);
            if let Some(fresh) = fresh {
                inner.sealer = Arc::new(fresh);
            }
            inner.key_error = match &change {
                Change::Rejected(why) => Some(why.clone()),
                _ => None,
            };
            change
        } else {
            Change::Unchanged
        };

        let creds = if creds_stamp != inner.creds_stamp {
            inner.creds_stamp = creds_stamp;
            match creds_for(&self.files.creds, &self.files.user) {
                Ok(_) => {
                    inner.disk_refused = false;
                    inner.generation += 1;
                    inner.pending = Some(Pending {
                        generation: inner.generation,
                        failures: 0,
                        next_at: Instant::now(),
                    });
                    inner.creds_error = None;
                    Change::Applied
                }
                Err(why) => {
                    inner.pending = None;
                    inner.disk_refused = true;
                    inner.creds_error = Some(why.clone());
                    Change::Rejected(why)
                }
            }
        } else {
            Change::Unchanged
        };
        drop(inner);

        let reload = Reload { keys, creds };
        if reload.changed() {
            self.wake.notify_one();
        }
        reload
    }

    /// Called by the network side right before it connects with no
    /// connection to keep (at start, after one closed), which reads the
    /// creds file: records its stamp and user as the ones in use, and drops
    /// a waiting trial (this connect is its trial).
    pub fn connecting(&self) {
        let stamp = Stamp::of(&self.files.creds);
        let user = creds_for(&self.files.creds, &self.files.user);
        let mut inner = self.inner();
        inner.creds_stamp = stamp;
        inner.pending = None;
        match user {
            Ok(name) => {
                inner.creds_user = Some(name);
                inner.creds_error = None;
                inner.disk_refused = false;
            }
            Err(why) => {
                inner.creds_user = crate::check::creds_user_name(&self.files.creds);
                inner.creds_error = Some(why);
                inner.disk_refused = true;
            }
        }
    }

    /// Makes a waiting trial due at `now`, when there is one: for the
    /// network side, which would otherwise rebuild its connection with the
    /// file on disk (see [`crate::pipeline::serve`]). Whether one waits.
    pub fn trial_now(&self, now: Instant) -> bool {
        match self.inner().pending.as_mut() {
            Some(p) => {
                p.next_at = p.next_at.min(now);
                true
            }
            None => false,
        }
    }

    /// Asked by the network side, which tries one file at a time, at `now`:
    /// whether to open a trial connection. The waiting file is checked
    /// again first, since it may have expired while waiting; one that no
    /// longer passes is dropped and recorded ([`TrialStep::Rejected`]).
    pub fn begin_trial(&self, now: Instant) -> TrialStep {
        let generation = match self.inner().pending {
            Some(p) if now >= p.next_at => p.generation,
            _ => return TrialStep::Wait,
        };
        let checked = creds_for(&self.files.creds, &self.files.user);
        let mut inner = self.inner();
        if !inner.pending.is_some_and(|p| p.generation == generation) {
            // Superseded while the file was read: the next call takes it.
            return TrialStep::Wait;
        }
        match checked {
            Ok(user) => TrialStep::Open(CredsTrial { generation, user }),
            Err(why) => {
                inner.pending = None;
                inner.disk_refused = true;
                inner.creds_error = Some(why.clone());
                TrialStep::Rejected(why)
            }
        }
    }

    /// The trial connection is up and replaces the one in use: its user is
    /// in use now. The waiting file is done with, unless a newer one
    /// superseded it meanwhile (that one still gets its trial).
    pub fn trial_succeeded(&self, trial: &CredsTrial) {
        let mut inner = self.inner();
        inner.creds_user = Some(trial.user.clone());
        if inner
            .pending
            .is_some_and(|p| p.generation == trial.generation)
        {
            inner.pending = None;
            inner.creds_error = None;
        }
    }

    /// The trial connection failed with `why` at `now`: the connection in
    /// use stays, the failure is recorded, and the file is tried again
    /// after [`trial_delay`], which is returned. `None` when a newer file
    /// superseded it meanwhile (nothing is recorded).
    pub fn trial_failed(&self, trial: &CredsTrial, why: &str, now: Instant) -> Option<Duration> {
        let mut guard = self.inner();
        let inner = &mut *guard;
        let pending = inner
            .pending
            .as_mut()
            .filter(|p| p.generation == trial.generation)?;
        pending.failures += 1;
        let delay = trial_delay(pending.failures);
        pending.next_at = now + delay;
        inner.creds_error = Some(format!(
            "{} did not connect: {why}",
            self.files.creds.display()
        ));
        Some(delay)
    }

    /// Whether the creds file on disk failed the checks, so a rebuild with
    /// it would take a working connection offline (see the module docs).
    pub fn disk_creds_refused(&self) -> bool {
        self.inner().disk_refused
    }

    /// Resolves after a recheck that changed something (immediately if one
    /// happened since the last wait).
    pub async fn changed(&self) {
        self.wake.notified().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_envelope::SecretKeys;

    /// Not real credentials: a JWT with only a `name` claim and a fake seed.
    fn fake_creds(name: &str) -> String {
        fake_creds_with(
            serde_json::json!({"name": name, "sub": "UABC"}),
            "SUAFAKEFAKEFAKE",
        )
    }

    /// Not real credentials either: a JWT with these `claims`, and `seed`
    /// in the seed block (empty: a block with nothing in it).
    fn fake_creds_with(claims: serde_json::Value, seed: &str) -> String {
        let b64 = |v: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        let jwt = format!(
            "{}.{}.c2ln",
            b64(serde_json::json!({"typ":"JWT","alg":"ed25519-nkey"})),
            b64(claims)
        );
        format!(
            "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
             -----BEGIN USER NKEY SEED-----\n{seed}\n------END USER NKEY SEED------\n"
        )
    }

    fn files(dir: &Path) -> KeyFiles {
        let keys = dir.join("keys");
        std::fs::create_dir_all(&keys).unwrap();
        KeyFiles {
            publisher_key: keys.join("publisher.key"),
            viewer_pub: keys.join("viewer.pub"),
            creds: dir.join("u.creds"),
            user: "cc-sessions.pub-lab-01".into(),
        }
    }

    /// A ready machine: publisher key `p1`, viewer key `k1`, creds for its user.
    fn ready(dir: &Path) -> KeyFiles {
        let f = files(dir);
        SecretKeys::generate("p1")
            .unwrap()
            .save_new(&f.publisher_key)
            .unwrap();
        SecretKeys::generate("k1")
            .unwrap()
            .public_keys()
            .save(&f.viewer_pub)
            .unwrap();
        std::fs::write(&f.creds, fake_creds(&f.user)).unwrap();
        f
    }

    #[test]
    fn a_stamp_follows_length_and_presence() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        assert_eq!(Stamp::of(&p), None);
        std::fs::write(&p, "a").unwrap();
        let one = Stamp::of(&p).unwrap();
        assert_eq!(one.len, 1);
        assert_eq!(Stamp::of(&p), Some(one), "stable while untouched");
        std::fs::write(&p, "ab").unwrap();
        assert_ne!(Stamp::of(&p), Some(one));
    }

    #[test]
    fn key_changes_are_judged_by_the_keys_not_the_file() {
        let p = SecretKeys::generate("p1").unwrap();
        let v = SecretKeys::generate("k1").unwrap().public_keys();
        let current = Sealer::new(p.clone(), v.clone());
        let (c, fresh) = key_change(&current, Ok(Sealer::new(p.clone(), v.clone())));
        assert_eq!((c, fresh.is_none()), (Change::Unchanged, true));

        let k2 = SecretKeys::generate("k2").unwrap().public_keys();
        let (c, fresh) = key_change(&current, Ok(Sealer::new(p.clone(), k2)));
        assert_eq!(c, Change::Applied);
        assert!(fresh.unwrap().describe().contains("viewer key k2"));

        let missing = SealingError::Missing {
            path: "viewer.pub".into(),
            hint: crate::sealing::VIEWER_PUB_HINT,
        };
        let (c, fresh) = key_change(&current, Err(missing));
        assert!(
            matches!(&c, Change::Rejected(why) if why.contains("viewer.pub")),
            "{c:?}"
        );
        assert!(fresh.is_none(), "nothing replaces the keys in use");
    }

    #[test]
    fn creds_must_be_for_this_publisher() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        assert!(creds_for(&f.creds, &f.user)
            .unwrap_err()
            .contains("missing"));
        std::fs::write(&f.creds, "hello").unwrap();
        assert!(creds_for(&f.creds, &f.user)
            .unwrap_err()
            .contains("not a NATS credentials file"));
        std::fs::write(&f.creds, fake_creds("cc-sessions.pub-other")).unwrap();
        let err = creds_for(&f.creds, &f.user).unwrap_err();
        assert!(
            err.contains("cc-sessions.pub-other") && !err.contains("SUAFAKE"),
            "{err}"
        );
        std::fs::write(&f.creds, fake_creds(&f.user)).unwrap();
        assert_eq!(creds_for(&f.creds, &f.user).unwrap(), f.user);
        let expired = fake_creds_with(
            serde_json::json!({"name": f.user, "exp": 1_000_000_000}),
            "SUAFAKE",
        );
        std::fs::write(&f.creds, expired).unwrap();
        let err = creds_for(&f.creds, &f.user).unwrap_err();
        assert!(
            err.starts_with(&f.creds.display().to_string()),
            "names the file: {err}"
        );
        assert!(err.ends_with("expired on 2001-09-09 01:46:40 UTC"), "{err}");
    }

    #[test]
    fn creds_need_a_seed_and_an_expiry_still_ahead() {
        let user = "cc-sessions.pub-lab-01";
        let now = 1_800_000_000.0;
        let creds = |claims: serde_json::Value, seed: &str| fake_creds_with(claims, seed);
        let named = serde_json::json!({"name": user});
        assert_eq!(
            judge_creds(&creds(named.clone(), "SUAFAKE"), user, now).as_deref(),
            Ok(user)
        );

        // No seed: the block is empty, holds no user seed, or is gone.
        for seed in ["", "SAAFAKE"] {
            let err = judge_creds(&creds(named.clone(), seed), user, now).unwrap_err();
            assert_eq!(err, "has no user seed (the NKEY SEED block)");
        }
        let jwt_only = creds(named.clone(), "SUAFAKE");
        let jwt_only = jwt_only.split("-----BEGIN USER NKEY SEED").next().unwrap();
        assert!(judge_creds(jwt_only, user, now)
            .unwrap_err()
            .contains("no user seed"));

        // The expiry: passed (to the second) is refused, ahead or none is fine.
        let exp = |exp: f64| creds(serde_json::json!({"name": user, "exp": exp}), "SUAFAKE");
        let err = judge_creds(&exp(now), user, now).unwrap_err();
        assert_eq!(err, format!("expired on {}", crate::util::format_utc(now)));
        assert!(judge_creds(&exp(now - 86_400.0), user, now).is_err());
        assert!(judge_creds(&exp(now + 1.0), user, now).is_ok());
        assert!(judge_creds(&exp(0.0), user, now).is_ok(), "0 never expires");

        // Another user's: named, whatever else is wrong with the file.
        let other = creds(
            serde_json::json!({"name": "cc-sessions.viewer", "exp": 1}),
            "",
        );
        assert_eq!(
            judge_creds(&other, user, now).unwrap_err(),
            format!("is for cc-sessions.viewer, not {user}")
        );
        let unnamed = creds(serde_json::json!({"sub": "UABC"}), "SUAFAKE");
        assert_eq!(
            judge_creds(&unnamed, user, now).unwrap_err(),
            "is not a NATS credentials file"
        );
    }

    #[test]
    fn a_failing_creds_file_is_tried_less_and_less_often() {
        assert_eq!(trial_delay(1), TRIAL_RETRY_FIRST);
        assert_eq!(trial_delay(2), TRIAL_RETRY_FIRST * 2);
        assert_eq!(trial_delay(3), TRIAL_RETRY_FIRST * 4);
        assert_eq!(trial_delay(0), TRIAL_RETRY_FIRST);
        for n in 1..200 {
            assert!(
                trial_delay(n) <= TRIAL_RETRY_MAX
                    && trial_delay(n) >= trial_delay(n.saturating_sub(1))
            );
        }
        assert_eq!(trial_delay(u32::MAX), TRIAL_RETRY_MAX);
        assert!(
            TRIAL_RETRY_FIRST >= RECHECK_EVERY,
            "never more often than the files are looked at"
        );
    }

    #[test]
    fn replaced_key_files_are_picked_up_and_broken_ones_keep_the_last_good_keys() {
        let dir = tempfile::tempdir().unwrap();
        let f = ready(dir.path());
        let keys = Keys::load(f.clone()).unwrap();
        assert_eq!(keys.in_use().describe(), keys.sealer().describe());
        assert_eq!(keys.in_use().viewer_kid, "k1");
        let unchanged = Reload {
            keys: Change::Unchanged,
            creds: Change::Unchanged,
        };
        assert_eq!(keys.recheck(false), unchanged, "nothing touched");
        assert_eq!(keys.recheck(true), unchanged, "forced, same keys");

        // A rotated viewer key (a longer kid, so the length changes too).
        let rotated = SecretKeys::generate("v20261014-bbbb")
            .unwrap()
            .public_keys();
        rotated.save(&f.viewer_pub).unwrap();
        assert_eq!(keys.recheck(false).keys, Change::Applied);
        assert_eq!(keys.in_use().viewer_kid, "v20261014-bbbb");
        assert!(keys
            .sealer()
            .describe()
            .contains("viewer key v20261014-bbbb"));
        assert_eq!(
            keys.begin_trial(Instant::now()),
            TrialStep::Wait,
            "keys alone never reconnect"
        );

        // Broken, then gone: reported, and the rotated key goes on sealing.
        std::fs::write(&f.viewer_pub, "garbage\n").unwrap();
        let r = keys.recheck(false);
        assert!(
            matches!(&r.keys, Change::Rejected(why) if !why.contains("garbage")),
            "{r:?}"
        );
        assert_eq!(keys.in_use().viewer_kid, "v20261014-bbbb");
        assert!(keys.in_use().key_error.is_some());
        assert_eq!(keys.recheck(false), unchanged, "reported once per change");
        assert!(keys.in_use().key_error.is_some(), "until fixed");
        std::fs::remove_file(&f.publisher_key).unwrap();
        assert!(matches!(keys.recheck(false).keys, Change::Rejected(_)));
        assert_eq!(keys.in_use().publisher_kid, "p1");

        // Fixed: the new files are in use and the error is gone.
        SecretKeys::generate("p2-new")
            .unwrap()
            .save_new(&f.publisher_key)
            .unwrap();
        rotated.save(&f.viewer_pub).unwrap();
        assert_eq!(keys.recheck(false).keys, Change::Applied);
        let in_use = keys.in_use();
        assert_eq!(
            (in_use.publisher_kid.as_str(), in_use.key_error),
            ("p2-new", None)
        );
    }

    fn opened(step: TrialStep) -> CredsTrial {
        match step {
            TrialStep::Open(trial) => trial,
            other => panic!("expected a trial, got {other:?}"),
        }
    }

    #[test]
    fn new_creds_are_tried_before_they_replace_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let f = ready(dir.path());
        let keys = Keys::load(f.clone()).unwrap();
        assert_eq!(keys.in_use().creds_user, None, "not connected yet");
        keys.connecting();
        assert_eq!(keys.in_use().creds_user.as_deref(), Some(f.user.as_str()));
        assert_eq!(
            keys.begin_trial(Instant::now()),
            TrialStep::Wait,
            "nothing waits"
        );

        // Another user's creds: reported, never tried, the connection keeps its own.
        std::fs::write(&f.creds, fake_creds("cc-sessions.pub-elsewhere")).unwrap();
        let r = keys.recheck(false);
        assert!(
            matches!(&r.creds, Change::Rejected(why) if why.contains("cc-sessions.pub-elsewhere")),
            "{r:?}"
        );
        assert_eq!(r.keys, Change::Unchanged);
        assert_eq!(keys.begin_trial(Instant::now()), TrialStep::Wait);
        assert_eq!(keys.in_use().creds_user.as_deref(), Some(f.user.as_str()));
        assert!(keys.in_use().creds_error.is_some());

        // This publisher's again (stale ones, say): a trial is due at once.
        std::fs::write(&f.creds, fake_creds(&f.user) + "\n").unwrap();
        assert_eq!(keys.recheck(false).creds, Change::Applied);
        assert_eq!(keys.in_use().creds_error, None);
        let t0 = Instant::now();
        let trial = opened(keys.begin_trial(t0));
        assert_eq!(trial.user, f.user);

        // It fails: recorded, the connection keeps what it had, retried later.
        assert_eq!(
            keys.trial_failed(&trial, "authorization violation", t0),
            Some(TRIAL_RETRY_FIRST)
        );
        let in_use = keys.in_use();
        let err = in_use.creds_error.unwrap();
        assert!(
            err.contains("did not connect: authorization violation") && !err.contains("SUAFAKE"),
            "{err}"
        );
        assert_eq!(in_use.creds_user.as_deref(), Some(f.user.as_str()));
        assert_eq!(keys.begin_trial(t0), TrialStep::Wait, "not on every pass");
        assert_eq!(
            keys.begin_trial(t0 + TRIAL_RETRY_FIRST / 2),
            TrialStep::Wait
        );
        assert_eq!(
            keys.recheck(false).creds,
            Change::Unchanged,
            "the same file, reported once"
        );
        let t1 = t0 + TRIAL_RETRY_FIRST;
        let trial = opened(keys.begin_trial(t1));
        assert_eq!(
            keys.trial_failed(&trial, "timed out", t1),
            Some(TRIAL_RETRY_FIRST * 2),
            "backing off"
        );
        assert!(
            keys.in_use().creds_error.unwrap().contains("timed out"),
            "the latest failure"
        );

        // Brought forward (the network side would rebuild), then it works.
        assert!(keys.trial_now(t1));
        let trial = opened(keys.begin_trial(t1));
        keys.trial_succeeded(&trial);
        let in_use = keys.in_use();
        assert_eq!(
            (in_use.creds_user.as_deref(), in_use.creds_error),
            (Some(f.user.as_str()), None)
        );
        assert_eq!(
            keys.begin_trial(t1 + TRIAL_RETRY_MAX),
            TrialStep::Wait,
            "done with"
        );
        assert!(!keys.trial_now(t1), "nothing to bring forward");
        assert_eq!(keys.recheck(false).creds, Change::Unchanged);
    }

    #[test]
    fn a_newer_creds_file_supersedes_the_one_being_tried() {
        let dir = tempfile::tempdir().unwrap();
        let f = ready(dir.path());
        let keys = Keys::load(f.clone()).unwrap();
        keys.connecting();
        std::fs::write(&f.creds, fake_creds(&f.user) + "\n").unwrap();
        keys.recheck(false);
        let first = opened(keys.begin_trial(Instant::now()));

        // Replaced while its trial connects: that outcome no longer counts.
        std::fs::write(&f.creds, fake_creds(&f.user) + "\n\n").unwrap();
        assert_eq!(keys.recheck(false).creds, Change::Applied);
        assert_eq!(
            keys.trial_failed(&first, "authorization violation", Instant::now()),
            None
        );
        assert_eq!(
            keys.in_use().creds_error,
            None,
            "nothing recorded for the newer file"
        );
        let second = opened(keys.begin_trial(Instant::now()));
        assert_ne!(first, second);

        // An older trial that connects is used, but the newer file is still tried.
        keys.trial_succeeded(&first);
        assert!(matches!(
            keys.begin_trial(Instant::now()),
            TrialStep::Open(_)
        ));
        keys.trial_succeeded(&second);
        assert_eq!(keys.begin_trial(Instant::now()), TrialStep::Wait);
    }

    #[test]
    fn a_refused_creds_file_on_disk_blocks_rebuilds_until_a_good_one_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = ready(dir.path());
        let keys = Keys::load(f.clone()).unwrap();
        keys.connecting();
        assert!(!keys.disk_creds_refused());

        let expired = fake_creds_with(
            serde_json::json!({"name": f.user, "exp": 1_000_000_000}),
            "SUAFAKE",
        );
        std::fs::write(&f.creds, expired).unwrap();
        assert!(matches!(keys.recheck(false).creds, Change::Rejected(_)));
        assert!(
            keys.disk_creds_refused(),
            "a rebuild would read the expired file"
        );
        assert!(
            !keys.trial_now(Instant::now()),
            "and there is no trial to stand in for it"
        );

        std::fs::write(&f.creds, fake_creds(&f.user) + "\n").unwrap();
        assert_eq!(keys.recheck(false).creds, Change::Applied);
        assert!(!keys.disk_creds_refused());
    }

    #[test]
    fn a_creds_file_that_expires_while_waiting_is_never_tried() {
        let dir = tempfile::tempdir().unwrap();
        let f = ready(dir.path());
        let keys = Keys::load(f.clone()).unwrap();
        keys.connecting();
        std::fs::write(&f.creds, fake_creds(&f.user) + "\n").unwrap();
        assert_eq!(keys.recheck(false).creds, Change::Applied);

        // Rewritten (expired) before the next recheck: the trial checks it again.
        let expired = fake_creds_with(
            serde_json::json!({"name": f.user, "exp": 1_000_000_000}),
            "SUAFAKE",
        );
        std::fs::write(&f.creds, expired).unwrap();
        let step = keys.begin_trial(Instant::now());
        assert!(
            matches!(&step, TrialStep::Rejected(why) if why.contains("expired on 2001-09-09")),
            "{step:?}"
        );
        assert!(keys.in_use().creds_error.unwrap().contains("expired"));
        assert_eq!(keys.begin_trial(Instant::now()), TrialStep::Wait, "dropped");
        assert_eq!(
            keys.in_use().creds_user.as_deref(),
            Some(f.user.as_str()),
            "the connection keeps its own"
        );

        // Seen by a recheck too: rejected there, not tried.
        let r = keys.recheck(false);
        assert!(
            matches!(&r.creds, Change::Rejected(why) if why.contains("expired")),
            "{r:?}"
        );
        assert_eq!(keys.begin_trial(Instant::now()), TrialStep::Wait);
    }

    #[test]
    fn connecting_without_a_connection_to_keep_takes_the_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let f = ready(dir.path());
        let keys = Keys::load(f.clone()).unwrap();
        keys.connecting();
        std::fs::write(&f.creds, fake_creds(&f.user) + "\n").unwrap();
        keys.recheck(false);
        keys.connecting();
        assert_eq!(
            keys.begin_trial(Instant::now()),
            TrialStep::Wait,
            "this connect was its trial"
        );
        assert_eq!(
            keys.recheck(false).creds,
            Change::Unchanged,
            "it records the file it read"
        );
    }

    #[test]
    fn a_change_wakes_the_network_side() {
        let dir = tempfile::tempdir().unwrap();
        let f = ready(dir.path());
        let keys = Keys::load(f.clone()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let woke = |keys: &Keys| {
            rt.block_on(async {
                tokio::time::timeout(Duration::from_millis(50), keys.changed())
                    .await
                    .is_ok()
            })
        };
        keys.recheck(true);
        assert!(!woke(&keys), "nothing changed");
        std::fs::write(&f.creds, fake_creds(&f.user) + "\n").unwrap();
        keys.recheck(false);
        assert!(woke(&keys), "a change made before the wait still wakes it");
    }

    #[test]
    fn a_missing_key_file_at_start_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        let err = Keys::load(f).unwrap_err();
        assert!(matches!(err, SealingError::Missing { .. }), "{err}");
    }
}
