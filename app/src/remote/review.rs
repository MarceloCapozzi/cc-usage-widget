//! Publisher keys waiting for the viewer's user: [`PendingKeys`], what the
//! flyout's notice offers to accept or reject, kept in
//! `keys/publishers/<alias>.pending` so it survives a restart.
//!
//! Every new publisher key needs the user's consent, the first key of a new
//! machine included. A value whose sender key is not pinned for its alias
//! is not shown: the opener refuses it as [`EnvelopeError::NewSender`] (it
//! did open with the key it claims, so the sender holds that key), and
//! [`RemoteMap`] notes the key here with when it was first and last seen
//! and how many values it sent. At most [`MAX_PENDING_PER_ALIAS`] keys per
//! machine (a further one replaces the one seen least recently) and
//! [`MAX_PENDING_ALIASES`] machines (values of further ones are counted in
//! the tooltip, not noted).
//!
//! Nothing is pinned or rejected without a click: [`accept_key_of`] and
//! [`reject_key_of`], the notice's buttons, write the answer with
//! [`cc_envelope::pin_key`] (the line goes at the top of `<alias>.pub`) or
//! [`cc_envelope::reject_key`] (`<alias>.rejected`, whose values are then
//! dropped unopened), make the running viewer's keys read it at once
//! ([`ViewerKeys::refresh_alias`]) and take the entry off the list: the
//! machine's values open from its next write, or are dropped. A key added
//! to a pin file by hand meanwhile counts too; its entry stays until it is
//! accepted (which then changes nothing) or rejected.
//!
//! The `.pending` files are written off the hub's lock, on tokio's blocking
//! pool: soon after an entry comes or goes, and every
//! [`PENDING_SAVE_INTERVAL`] at most while only the times and counts move.
//! A backup never carries them.
//!
//! [`EnvelopeError::NewSender`]: cc_envelope::EnvelopeError::NewSender
//! [`RemoteMap`]: super::RemoteMap
//! [`ViewerKeys::refresh_alias`]: cc_envelope::ViewerKeys::refresh_alias

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cc_envelope::{validate_alias, PublicKey};
use serde::{Deserialize, Serialize};

use super::hub::{lock, RemoteHub, SharedHub};

/// Keys waiting per machine. A real machine has one; a few leave room for a
/// key replaced twice before anyone looked.
pub const MAX_PENDING_PER_ALIAS: usize = 3;

/// Machines with keys waiting. Values of a further machine are not noted
/// (counted in the tooltip) until one of these is accepted or rejected.
pub const MAX_PENDING_ALIASES: usize = 16;

/// While only the seen times and counts of the waiting keys move, the
/// `.pending` files are written at most this often.
pub const PENDING_SAVE_INTERVAL: Duration = Duration::from_secs(600);

/// The extension of a waiting-keys file, `<alias>.pending`.
pub const PENDING_EXT: &str = "pending";

/// Largest `.pending` file read; a few keys take a few hundred bytes.
const MAX_PENDING_FILE_BYTES: u64 = 64 * 1024;

/// Characters of the key's base64 shown as its fingerprint.
const FINGERPRINT_CHARS: usize = 8;

/// One publisher key waiting for the viewer's user. Also the shape of an
/// entry of a `.pending` file and of `RemoteStatus.pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingKey {
    /// The machine alias, from the KV keys its values came under.
    pub alias: String,
    /// The key id the publisher gave the key.
    pub kid: String,
    /// The public key line, `<kid>:<base64>` (not secret): what Accept
    /// writes to the pin file, and what identifies the entry.
    pub key: String,
    /// The first characters of the key's base64, to compare with what the
    /// publisher shows (`cc-publish keygen`, Settings → Generate key).
    pub fingerprint: String,
    /// The alias has pinned keys already: a known machine with a new key,
    /// not a new machine.
    pub known: bool,
    /// Unix seconds of the first value sealed with it.
    pub first_seen: i64,
    /// Unix seconds of the latest one.
    pub last_seen: i64,
    /// Values it sealed, held back.
    pub count: u64,
}

impl PendingKey {
    fn new(alias: &str, key: &PublicKey, known: bool, now: i64) -> Self {
        let line = key.to_line();
        Self {
            alias: alias.to_string(),
            kid: key.kid().to_string(),
            fingerprint: fingerprint(&line),
            key: line,
            known,
            first_seen: now,
            last_seen: now,
            count: 1,
        }
    }

    /// An entry read from `<alias>.pending`, rebuilt from its key line and
    /// the alias of the file name; `None` when the line does not parse.
    fn from_file(alias: &str, stored: &PendingKey) -> Option<Self> {
        let key = PublicKey::parse_line(&stored.key).ok()?;
        let mut entry = Self::new(alias, &key, stored.known, stored.first_seen);
        entry.last_seen = stored.last_seen.max(stored.first_seen);
        entry.count = stored.count.max(1);
        Some(entry)
    }
}

/// The fingerprint of a key line: the first [`FINGERPRINT_CHARS`] of its
/// base64.
pub fn fingerprint(line: &str) -> String {
    let b64 = line.split_once(':').map_or(line, |(_, b)| b);
    b64.chars().take(FINGERPRINT_CHARS).collect()
}

/// The tray tooltip's words for `n` keys waiting; `None` for none.
pub fn review_text(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 key to review".into()),
        n => Some(format!("{n} keys to review")),
    }
}

/// The keys waiting for review, by alias. See the module docs.
#[derive(Debug, Default)]
pub struct PendingKeys {
    by_alias: BTreeMap<String, Vec<PendingKey>>,
    /// An entry came, went, or changed `known` since the last save.
    structural: bool,
    /// Only times and counts moved since the last save.
    touched: bool,
    /// Values not noted because [`MAX_PENDING_ALIASES`] machines wait.
    over_cap: u64,
}

impl PendingKeys {
    /// Notes a value of `alias` sealed by `key`, not pinned for it, at
    /// `now`. Returns true when the list the notice shows changed (a key
    /// noted for the first time, or `known` changed), false when only the
    /// times and counts moved or the value was not noted (the cap).
    pub fn note(&mut self, alias: &str, key: &PublicKey, known: bool, now: i64) -> bool {
        if !self.by_alias.contains_key(alias) && self.by_alias.len() >= MAX_PENDING_ALIASES {
            self.over_cap = self.over_cap.saturating_add(1);
            return false;
        }
        let line = key.to_line();
        let entries = self.by_alias.entry(alias.to_string()).or_default();
        if let Some(e) = entries.iter_mut().find(|e| e.key == line) {
            e.last_seen = e.last_seen.max(now);
            e.count = e.count.saturating_add(1);
            self.touched = true;
            if e.known != known {
                e.known = known;
                self.structural = true;
                return true;
            }
            return false;
        }
        if entries.len() >= MAX_PENDING_PER_ALIAS {
            // The one seen least recently makes room: a key that keeps
            // publishing is never crowded out by keys sent once.
            if let Some(oldest) = entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_seen)
                .map(|(i, _)| i)
            {
                entries.remove(oldest);
            }
        }
        entries.push(PendingKey::new(alias, key, known, now));
        self.structural = true;
        tracing::info!(
            "remote: {alias} presents the key {line}, not pinned: waiting for review in the flyout"
        );
        true
    }

    /// The entry of `alias` with the key line `key`.
    pub fn get(&self, alias: &str, key: &str) -> Option<&PendingKey> {
        self.by_alias.get(alias)?.iter().find(|e| e.key == key)
    }

    /// Takes the entry of `alias` with the key line `key` off the list.
    pub fn remove(&mut self, alias: &str, key: &str) -> Option<PendingKey> {
        let entries = self.by_alias.get_mut(alias)?;
        let at = entries.iter().position(|e| e.key == key)?;
        let entry = entries.remove(at);
        if entries.is_empty() {
            self.by_alias.remove(alias);
        }
        self.structural = true;
        Some(entry)
    }

    /// Marks the other keys of `alias` as new keys of a known machine (one
    /// of its keys was just accepted).
    pub fn mark_known(&mut self, alias: &str) {
        for e in self.by_alias.get_mut(alias).into_iter().flatten() {
            if !e.known {
                e.known = true;
                self.structural = true;
            }
        }
    }

    /// Forgets every key of `alias`. Returns true if there was one.
    pub fn drop_alias(&mut self, alias: &str) -> bool {
        let had = self.by_alias.remove(alias).is_some();
        self.structural |= had;
        had
    }

    /// Every waiting key, by alias, then the first seen first.
    pub fn list(&self) -> Vec<PendingKey> {
        let mut all: Vec<PendingKey> = self.by_alias.values().flatten().cloned().collect();
        all.sort_by(|a, b| (&a.alias, a.first_seen, &a.key).cmp(&(&b.alias, b.first_seen, &b.key)));
        all
    }

    /// How many keys wait.
    pub fn len(&self) -> usize {
        self.by_alias.values().map(Vec::len).sum()
    }

    /// True when no key waits.
    pub fn is_empty(&self) -> bool {
        self.by_alias.is_empty()
    }

    /// The machines with keys waiting.
    pub fn aliases(&self) -> impl Iterator<Item = &String> {
        self.by_alias.keys()
    }

    /// Values not noted because too many machines wait.
    pub fn over_cap(&self) -> u64 {
        self.over_cap
    }

    /// Adds entries read from the `.pending` files, by the caps (one past
    /// them is skipped). An entry already held keeps what it has.
    pub(super) fn load(&mut self, entries: Vec<PendingKey>) {
        for e in entries {
            if !self.by_alias.contains_key(&e.alias) && self.by_alias.len() >= MAX_PENDING_ALIASES {
                continue;
            }
            let held = self.by_alias.entry(e.alias.clone()).or_default();
            if held.len() < MAX_PENDING_PER_ALIAS && !held.iter().any(|h| h.key == e.key) {
                held.push(e);
            }
        }
    }

    /// Whether a save is due at `now`, the last one handed out at
    /// `saved_at` (see the module docs); `force` saves any change.
    fn save_due(&self, saved_at: Option<Instant>, now: Instant, force: bool) -> bool {
        if self.structural || (force && self.touched) {
            return true;
        }
        self.touched
            && saved_at.is_none_or(|at| now.saturating_duration_since(at) >= PENDING_SAVE_INTERVAL)
    }

    /// What to write: every machine's entries, and resets the change marks.
    fn take_snapshot(&mut self) -> BTreeMap<String, Vec<PendingKey>> {
        self.structural = false;
        self.touched = false;
        self.by_alias.clone()
    }
}

/// The `.pending` files read from `dir`: every `<alias>.pending` with a
/// valid alias, up to the caps. A file that is too large or does not parse
/// is skipped (its keys are noted again with their next value).
pub fn read_pending(dir: &Path) -> Vec<PendingKey> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(PENDING_EXT) {
                return None;
            }
            let alias = path.file_stem()?.to_str()?.to_string();
            validate_alias(&alias).ok()?;
            std::fs::symlink_metadata(&path)
                .ok()?
                .is_file()
                .then_some((alias, path))
        })
        .collect();
    files.sort();
    files.truncate(MAX_PENDING_ALIASES);
    let mut out = Vec::new();
    for (alias, path) in files {
        let mut bytes = Vec::new();
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        if file
            .take(MAX_PENDING_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_PENDING_FILE_BYTES
        {
            continue;
        }
        let stored: Vec<PendingKey> = serde_json::from_slice(&bytes).unwrap_or_default();
        out.extend(
            stored
                .iter()
                .take(MAX_PENDING_PER_ALIAS)
                .filter_map(|s| PendingKey::from_file(&alias, s)),
        );
    }
    out
}

/// Where the waiting keys are kept (the pin folder) and the bookkeeping of
/// their writes.
#[derive(Debug)]
pub(super) struct PendingFile {
    pub(super) dir: PathBuf,
    saved_at: Option<Instant>,
    /// Numbers the writes handed out; of two running at once the older one
    /// cannot finish last and win.
    generation: u64,
    written: Arc<Mutex<u64>>,
}

/// One write of the `.pending` files, taken under the hub's lock and run by
/// [`PendingWrite::run`] after it is let go.
#[derive(Debug)]
pub struct PendingWrite {
    dir: PathBuf,
    entries: BTreeMap<String, Vec<PendingKey>>,
    generation: u64,
    written: Arc<Mutex<u64>>,
}

impl PendingWrite {
    /// Makes the folder's `.pending` files say what this write holds: one
    /// per machine with keys waiting, written atomically when it changed;
    /// the others removed. Blocking. Skipped when a newer write got there
    /// first; a failure costs only the copy on disk.
    pub fn run(self) {
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if *written >= self.generation {
            return;
        }
        let mut ok = true;
        for (alias, entries) in &self.entries {
            let path = self.dir.join(format!("{alias}.{PENDING_EXT}"));
            let Ok(json) = serde_json::to_string_pretty(entries) else {
                continue;
            };
            if std::fs::read(&path).is_ok_and(|have| have == json.as_bytes()) {
                continue;
            }
            if let Err(err) = crate::paths::write_atomic(&path, json.as_bytes()) {
                tracing::warn!("remote: cannot save {}: {err}", path.display());
                ok = false;
            }
        }
        if let Ok(listing) = std::fs::read_dir(&self.dir) {
            for entry in listing.flatten() {
                let path = entry.path();
                let stale = path.extension().and_then(|e| e.to_str()) == Some(PENDING_EXT)
                    && path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|alias| !self.entries.contains_key(alias));
                if stale {
                    if let Err(err) = std::fs::remove_file(&path) {
                        tracing::warn!("remote: cannot remove {}: {err}", path.display());
                        ok = false;
                    }
                }
            }
        }
        if ok {
            *written = self.generation;
        }
    }
}

impl RemoteHub {
    /// Loads the keys waiting for review from the pin folder `dir` and
    /// saves them there whenever they change. Without it (tests) nothing
    /// touches the disk, and the answers cannot be written.
    pub fn keep_pending_in(&mut self, dir: PathBuf) {
        let entries = read_pending(&dir);
        self.map.load_pending(entries);
        self.pending_file = Some(PendingFile {
            dir,
            saved_at: None,
            generation: 0,
            written: Arc::new(Mutex::new(0)),
        });
    }

    /// The write due after a change of the waiting keys at `now` (see the
    /// module docs), or `None`; `force` writes any change at once.
    pub fn pending_to_save(&mut self, now: Instant, force: bool) -> Option<PendingWrite> {
        let file = self.pending_file.as_mut()?;
        if !self.map.pending.save_due(file.saved_at, now, force) {
            return None;
        }
        let entries = self.map.pending.take_snapshot();
        file.saved_at = Some(now);
        file.generation += 1;
        Some(PendingWrite {
            dir: file.dir.clone(),
            entries,
            generation: file.generation,
            written: file.written.clone(),
        })
    }
}

/// Hands the `.pending` files to tokio's blocking pool when a write is due.
/// Never waits for the disk. Needs a tokio runtime.
pub(super) fn save_pending_soon(hub: &SharedHub) {
    if let Some(write) = lock(hub).pending_to_save(Instant::now(), false) {
        tokio::task::spawn_blocking(move || write.run());
    }
}

/// Writes the `.pending` files now if anything changed, on the calling
/// thread: for the app's exit. Blocking.
pub fn flush_pending_of(hub: &SharedHub) {
    let write = lock(hub).pending_to_save(Instant::now(), true);
    if let Some(write) = write {
        write.run();
    }
}

/// The keys waiting for review in `hub`.
pub fn pending_keys_of(hub: &SharedHub) -> Vec<PendingKey> {
    lock(hub).map().pending_keys()
}

/// How many keys wait for review in `hub`.
pub fn pending_count_of(hub: &SharedHub) -> usize {
    lock(hub).map().pending.len()
}

/// Accept: pins the waiting key `key` (its line) of `alias`. See
/// `decide`.
pub fn accept_key_of(hub: &SharedHub, alias: &str, key: &str) -> Result<String, String> {
    decide(hub, alias, key, true)
}

/// Reject: adds the waiting key `key` (its line) of `alias` to its
/// rejected file. See `decide`.
pub fn reject_key_of(hub: &SharedHub, alias: &str, key: &str) -> Result<String, String> {
    decide(hub, alias, key, false)
}

/// Writes the user's answer about a waiting key, which must be on the list
/// (the notice offers nothing else): the pin or rejected file first, with
/// the hub let go, then the running viewer's keys read it, and only then
/// the entry leaves the list, so a value arriving meanwhile can only note
/// it again before it goes. The `.pending` files are written at once.
/// Blocking. Returns a line for the log; the error is user-facing.
fn decide(hub: &SharedHub, alias: &str, line: &str, accept: bool) -> Result<String, String> {
    let (key, dir, keys) = {
        let held = lock(hub);
        let entry =
            held.map().pending.get(alias, line).ok_or_else(|| {
                format!("No key of {alias} is waiting for review under that line")
            })?;
        let key = PublicKey::parse_line(&entry.key).map_err(|e| e.to_string())?;
        let dir = held
            .pending_file
            .as_ref()
            .map(|f| f.dir.clone())
            .ok_or_else(|| "This viewer keeps no pin folder".to_string())?;
        (key, dir, held.keys.clone())
    };
    let done = if accept {
        cc_envelope::pin_key(&dir, alias, &key)?;
        format!(
            "accepted the key {} of {alias} (in {})",
            key.to_line(),
            cc_envelope::pin_file(&dir, alias).display()
        )
    } else {
        cc_envelope::reject_key(&dir, alias, &key)?;
        format!(
            "rejected the key {} of {alias} (in {})",
            key.to_line(),
            cc_envelope::rejected_file(&dir, alias).display()
        )
    };
    if let Some(keys) = keys {
        keys.refresh_alias(alias);
    }
    let write = {
        let mut held = lock(hub);
        held.map_mut().resolve_pending(alias, line, accept);
        held.pending_to_save(Instant::now(), true)
    };
    if let Some(write) = write {
        write.run();
    }
    tracing::info!("remote: {done}");
    Ok(done)
}
