//! The viewer's side: opening whatever the bucket holds, and deciding which
//! publisher key may write which alias.
//!
//! [`ViewerKeys`] holds the viewer's private keys and the pinned publisher
//! keys. A value opens only when:
//!
//! 1. it is an envelope of the current version (any other is rejected as
//!    [`EnvelopeError::UnsupportedVersion`]);
//! 2. it was sealed to one of the viewer's keys (`rk`);
//! 3. its sender key `spk` is pinned for the alias of its KV key, the first
//!    token of the key, which NATS lets only that publisher write. The
//!    alias inside the payload is never trusted for this;
//! 4. it authenticates (HPKE Auth mode) with that sender key and the KV key.
//!
//! Every publisher key needs the consent of the viewer's user, the first
//! key of a new alias included: there is no trust on first use. With a pin
//! folder (`keys/publishers`, the app's viewer), a value whose sender key is
//! not pinned for its alias is not shown. It must still open with the key
//! it claims, which proves the sender holds that key's private half (so
//! nobody can make the viewer ask about a key they do not have); then it is
//! refused as [`EnvelopeError::NewSender`], which carries the key for the
//! caller to put to the user. Nothing is written here: the caller keeps
//! what awaits review (the app: `<alias>.pending`). The answer is written
//! with [`pin_key`], which puts the key line at the top of `<alias>.pub`
//! (creating it), or [`reject_key`], which adds it to `<alias>.rejected`; a
//! value from a rejected key is refused unopened as
//! [`EnvelopeError::RejectedSender`]. [`ViewerKeys::refresh_alias`] makes
//! either count at once.
//!
//! A pin file is consent as it stands, whoever wrote it (an Accept or a
//! line added by hand). Adding a rotated key's line at the top by hand
//! accepts it too. Deleting a line does NOT
//! revoke that key: its next value puts it up for review again, one Accept
//! away. A key is revoked by rejecting it: [`reject_key`] on a pinned key
//! moves its line from `<alias>.pub` to `<alias>.rejected` (by hand: move
//! the line yourself). A pin file that exists but holds no key (or does not
//! parse) refuses the alias altogether ([`EnvelopeError::Pins`]) without
//! asking, so a revoked machine stays revoked; revoking the last pinned key
//! leaves such a file. A rejected file that does not parse
//! counts as empty: its keys are asked about again, never let in.
//!
//! Pins kept in memory only (no folder: tests and examples) trust the first
//! key of an alias, report it in [`Opened::pinned`], and refuse any other
//! one as [`EnvelopeError::UnknownSender`].
//!
//! The files of an alias (`.pub` and `.rejected`) are read together and
//! cached, missing ones too. A value of an alias not cached yet reads them
//! while it is being opened; a caller that holds a lock of its own around
//! the opening (the app's viewer) reads them ahead with
//! [`ViewerKeys::load_pins`] instead, and picks up lines added by hand with
//! [`ViewerKeys::reload_pins`]. Both read the files with no lock held and
//! swap the result in; a reload never undoes a [`ViewerKeys::refresh_alias`]
//! made while it was reading. [`ViewerKeys::forget_pins`] drops the cache
//! altogether.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::de::DeserializeOwned;

use crate::envelope::{
    envelope_version, parse_envelope, EnvelopeError, ENVELOPE_VERSION, MAX_ENVELOPE_BYTES,
};
use crate::keyfile::{write_key_file, KeyFileError, Privacy, Replace};
use crate::keypair::{PublicKey, PublicKeys, SecretKeys, X25519_KEY_LEN};
use crate::kv::validate_alias;

/// Keys kept in a rejected file (`<alias>.rejected`), newest first; adding
/// one more drops the oldest, which is asked about again if it comes back.
pub const MAX_REJECTED_PER_ALIAS: usize = 16;

/// A value that opened.
#[derive(Debug, Clone, PartialEq)]
pub struct Opened<T> {
    /// The payload.
    pub value: T,
    /// The sender key this value pinned, when pins live in memory and it
    /// was the first one for its alias (trust on first use, for tests and
    /// examples). Worth a log line. A viewer with a pin folder never pins
    /// by itself, so this is always `None` there.
    pub pinned: Option<PublicKey>,
}

/// Something that opens sealed values: the viewer's [`ViewerKeys`].
pub trait OpenKeys {
    /// Opens a value read from `kv_key` and deserializes its payload.
    fn open_any<T: DeserializeOwned>(
        &self,
        bytes: &[u8],
        kv_key: &str,
    ) -> Result<Opened<T>, EnvelopeError>;
}

/// What a pin file says: its keys, `None` when there is no file (nothing
/// accepted for the alias yet), or why it cannot be used.
type PinState = Result<Option<PublicKeys>, String>;

/// The pin file of `alias` in `dir`, `<dir>/<alias>.pub`.
pub fn pin_file(dir: &Path, alias: &str) -> PathBuf {
    dir.join(format!("{alias}.pub"))
}

/// The rejected keys of `alias` in `dir`, `<dir>/<alias>.rejected`: the
/// key file format, newest first.
pub fn rejected_file(dir: &Path, alias: &str) -> PathBuf {
    dir.join(format!("{alias}.rejected"))
}

/// Loads a key file, `None` when there is none.
fn load_keys(path: &Path) -> Result<Option<PublicKeys>, KeyFileError> {
    match PublicKeys::load(path) {
        Ok(keys) => Ok(Some(keys)),
        Err(KeyFileError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// What the viewer knows of one alias: its pin file and its rejected keys.
#[derive(Debug, Clone)]
struct AliasFiles {
    pins: PinState,
    /// The bytes of the rejected keys; empty without a rejected file, or
    /// when it does not parse.
    rejected: Vec<[u8; X25519_KEY_LEN]>,
}

impl AliasFiles {
    /// Nothing pinned, nothing rejected: a missing file, or pins in memory.
    fn none() -> Self {
        Self {
            pins: Ok(None),
            rejected: Vec::new(),
        }
    }

    /// Reads the files of `alias` in `dir`.
    fn read(dir: &Path, alias: &str) -> Self {
        let rejected = match load_keys(&rejected_file(dir, alias)) {
            Ok(Some(keys)) => keys.keys().iter().map(|k| *k.bytes()).collect(),
            _ => Vec::new(),
        };
        Self {
            pins: load_keys(&pin_file(dir, alias)).map_err(|e| e.to_string()),
            rejected,
        }
    }

    fn is_rejected(&self, bytes: &[u8; X25519_KEY_LEN]) -> bool {
        self.rejected.iter().any(|r| r == bytes)
    }
}

/// Accepts `key` for `alias` in the pin folder `dir`: puts its line at the
/// top of `<alias>.pub`, creating the file, and takes it off
/// `<alias>.rejected` if it is there. Returns false when it was pinned
/// already. Refuses an invalid alias, a pin file that does not load (an
/// empty one is a revoked machine: fix or remove it by hand first), and a
/// key whose key id another pinned key holds. The files are written
/// atomically. Call [`ViewerKeys::refresh_alias`] afterwards so a running
/// viewer counts it at once.
pub fn pin_key(dir: &Path, alias: &str, key: &PublicKey) -> Result<bool, String> {
    validate_alias(alias).map_err(|e| e.to_string())?;
    let path = pin_file(dir, alias);
    let added = match load_keys(&path).map_err(|e| format!("{}: {e}", path.display()))? {
        Some(keys) if keys.find(key.bytes()).is_some() => false,
        Some(mut keys) => {
            keys.prepend(key.clone())
                .map_err(|e| format!("{}: {e}", path.display()))?;
            keys.save(&path).map_err(|e| e.to_string())?;
            true
        }
        None => {
            let keys = PublicKeys::from_keys(vec![key.clone()]).map_err(|e| e.to_string())?;
            keys.save(&path).map_err(|e| e.to_string())?;
            true
        }
    };
    // No longer rejected. A rejected file that does not load is left as it
    // is: the pin wins anyway.
    let rejected = rejected_file(dir, alias);
    if let Ok(Some(keys)) = load_keys(&rejected) {
        if keys.find(key.bytes()).is_some() {
            let rest: Vec<PublicKey> = keys
                .keys()
                .iter()
                .filter(|k| k.bytes() != key.bytes())
                .cloned()
                .collect();
            if rest.is_empty() {
                std::fs::remove_file(&rejected)
                    .map_err(|e| format!("{}: {e}", rejected.display()))?;
            } else {
                PublicKeys::from_keys(rest)
                    .and_then(|k| k.save(&rejected))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(added)
}

/// Rejects `key` for `alias` in the pin folder `dir`: puts its line at the
/// top of `<alias>.rejected`, creating the file, and keeps the newest
/// [`MAX_REJECTED_PER_ALIAS`]. A rejected key under the same key id is
/// replaced. Returns false when it was rejected already. A key pinned for
/// the alias is revoked: its line leaves `<alias>.pub` (the last one leaves
/// a file with no key, which refuses the alias) before it is rejected.
/// Refuses an invalid alias and a rejected file that does not load. Written atomically; call [`ViewerKeys::refresh_alias`] afterwards.
pub fn reject_key(dir: &Path, alias: &str, key: &PublicKey) -> Result<bool, String> {
    validate_alias(alias).map_err(|e| e.to_string())?;
    let pins_path = pin_file(dir, alias);
    // A pin file that does not load already refuses the alias: nothing to
    // revoke there, the key is only rejected.
    if let Ok(Some(pins)) = load_keys(&pins_path) {
        if pins.find(key.bytes()).is_some() {
            let rest: Vec<PublicKey> = pins
                .keys()
                .iter()
                .filter(|k| k.bytes() != key.bytes())
                .cloned()
                .collect();
            if rest.is_empty() {
                let note =
                    format!("# Every key of {alias} was revoked; this file refuses its values.\n");
                write_key_file(&pins_path, note.as_bytes(), Privacy::Public, Replace::Yes)
                    .map_err(|e| e.to_string())?;
            } else {
                PublicKeys::from_keys(rest)
                    .and_then(|k| k.save(&pins_path))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    let path = rejected_file(dir, alias);
    let held = load_keys(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if held.as_ref().is_some_and(|k| k.find(key.bytes()).is_some()) {
        return Ok(false);
    }
    let mut keys = vec![key.clone()];
    if let Some(held) = held {
        keys.extend(held.keys().iter().filter(|k| k.kid() != key.kid()).cloned());
    }
    keys.truncate(MAX_REJECTED_PER_ALIAS);
    PublicKeys::from_keys(keys)
        .and_then(|k| k.save(&path))
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Pinned publisher keys, by alias: what each alias's files said when last
/// read.
#[derive(Debug, Default)]
struct Pins {
    /// `keys/publishers`; `None` keeps pins in memory only (tests), which
    /// trust the first key of an alias.
    dir: Option<PathBuf>,
    cache: BTreeMap<String, AliasFiles>,
    /// Counts the calls of [`ViewerKeys::refresh_alias`].
    epoch: u64,
    /// The epoch each alias was last refreshed at, so that a reload which
    /// read the files before that keeps the refreshed entry.
    refreshed: BTreeMap<String, u64>,
}

impl Pins {
    /// The files of `alias`, read when it is not cached.
    fn get(&mut self, alias: &str) -> AliasFiles {
        if let Some(hit) = self.cache.get(alias) {
            return hit.clone();
        }
        let Some(dir) = &self.dir else {
            return AliasFiles::none();
        };
        let loaded = AliasFiles::read(dir, alias);
        self.cache.insert(alias.to_string(), loaded.clone());
        loaded
    }

    /// Pins `key` for `alias`, which has no pins yet, and returns the pins
    /// in force and whether this call wrote them. With a folder the file is
    /// written atomically and never replaces one written meanwhile (by
    /// hand, or by an Accept); then that one counts.
    fn pin(&mut self, alias: &str, key: &PublicKey) -> Result<(PublicKeys, bool), String> {
        if let Some(AliasFiles {
            pins: Ok(Some(there)),
            ..
        }) = self.cache.get(alias)
        {
            return Ok((there.clone(), false));
        }
        let keys = PublicKeys::from_keys(vec![key.clone()]).map_err(|e| e.to_string())?;
        if let Some(dir) = &self.dir {
            match keys.save_new(pin_file(dir, alias)) {
                Ok(()) => {}
                Err(KeyFileError::Exists(_)) => {
                    self.cache.remove(alias);
                    return match self.get(alias).pins? {
                        Some(there) => Ok((there, false)),
                        None => Err("pin file vanished".into()),
                    };
                }
                Err(err) => return Err(err.to_string()),
            }
        }
        let rejected = self
            .cache
            .get(alias)
            .map(|f| f.rejected.clone())
            .unwrap_or_default();
        self.cache.insert(
            alias.to_string(),
            AliasFiles {
                pins: Ok(Some(keys.clone())),
                rejected,
            },
        );
        Ok((keys, true))
    }

    /// Makes `fresh` the whole cache, read from the files after `epoch`:
    /// an alias refreshed since keeps what the refresh put there.
    fn swap_reloaded(&mut self, epoch: u64, mut fresh: BTreeMap<String, AliasFiles>) {
        for (alias, at) in &self.refreshed {
            if *at > epoch {
                if let (Some(entry), Some(slot)) = (self.cache.get(alias), fresh.get_mut(alias)) {
                    *slot = entry.clone();
                }
            }
        }
        self.refreshed.retain(|_, at| *at > epoch);
        self.cache = fresh;
    }
}

/// Everything the viewer opens values with. See the module docs.
#[derive(Debug)]
pub struct ViewerKeys {
    secrets: SecretKeys,
    pins: Mutex<Pins>,
}

impl ViewerKeys {
    /// The viewer's keys: its own private keys and the folder of pinned
    /// publisher keys. With a folder every new publisher key waits for
    /// consent ([`EnvelopeError::NewSender`]); `None` keeps pins in memory
    /// only and trusts the first key of an alias, for tests and examples.
    pub fn new(secrets: SecretKeys, pins_dir: Option<PathBuf>) -> Self {
        Self {
            secrets,
            pins: Mutex::new(Pins {
                dir: pins_dir,
                ..Pins::default()
            }),
        }
    }

    /// The viewer's private keys.
    pub fn secrets(&self) -> &SecretKeys {
        &self.secrets
    }

    /// The pin folder, `None` when pins live in memory only.
    pub fn pins_dir(&self) -> Option<PathBuf> {
        self.pins().dir.clone()
    }

    fn pins(&self) -> MutexGuard<'_, Pins> {
        self.pins.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forgets the cached pin files, so the next value of each alias reads
    /// its files again (a line added or removed by hand).
    pub fn forget_pins(&self) {
        self.pins().cache.clear();
    }

    /// Which of `aliases` have no cached pin file yet: those whose next
    /// value would read the disk while it is opened. Always empty when pins
    /// live in memory only.
    pub fn uncached_pins<'a>(&self, aliases: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let pins = self.pins();
        if pins.dir.is_none() {
            return Vec::new();
        }
        let mut missing: Vec<String> = aliases
            .into_iter()
            .filter(|a| validate_alias(a).is_ok() && !pins.cache.contains_key(*a))
            .map(str::to_string)
            .collect();
        missing.sort();
        missing.dedup();
        missing
    }

    /// Reads the files of those of `aliases` not cached yet and caches
    /// them, so their values open without touching the disk. The files are
    /// read with no lock held; an alias cached meanwhile keeps what it has.
    pub fn load_pins(&self, aliases: &[String]) {
        let Some(dir) = self.pins_dir() else {
            return;
        };
        let missing = self.uncached_pins(aliases.iter().map(String::as_str));
        let read: Vec<(String, AliasFiles)> = missing
            .into_iter()
            .map(|alias| {
                let files = AliasFiles::read(&dir, &alias);
                (alias, files)
            })
            .collect();
        let mut pins = self.pins();
        for (alias, files) in read {
            pins.cache.entry(alias).or_insert(files);
        }
    }

    /// Reads the files of `aliases` again and makes them the whole cache:
    /// lines added or removed by hand count from now on, and an alias left
    /// out is forgotten (its files are read again when it next shows up),
    /// so the cache holds only the aliases the caller still keeps. The files
    /// are read with no lock held, then swapped in at once; an alias
    /// refreshed by [`ViewerKeys::refresh_alias`] while they were being read
    /// keeps what the refresh found.
    pub fn reload_pins(&self, aliases: &[String]) {
        let (dir, epoch) = {
            let pins = self.pins();
            let Some(dir) = pins.dir.clone() else {
                return;
            };
            (dir, pins.epoch)
        };
        let mut fresh: BTreeMap<String, AliasFiles> = BTreeMap::new();
        for alias in aliases {
            if validate_alias(alias).is_ok() && !fresh.contains_key(alias) {
                fresh.insert(alias.clone(), AliasFiles::read(&dir, alias));
            }
        }
        self.pins().swap_reloaded(epoch, fresh);
    }

    /// Reads the files of `alias` again now, with no lock held, and caches
    /// them: what [`pin_key`] or [`reject_key`] just wrote counts from the
    /// next value on. Does nothing when pins live in memory only.
    pub fn refresh_alias(&self, alias: &str) {
        let Some(dir) = self.pins_dir() else {
            return;
        };
        if validate_alias(alias).is_err() {
            return;
        }
        let files = AliasFiles::read(&dir, alias);
        let mut pins = self.pins();
        pins.epoch += 1;
        let epoch = pins.epoch;
        pins.refreshed.insert(alias.to_string(), epoch);
        pins.cache.insert(alias.to_string(), files);
    }

    /// How many aliases have their files cached (or a cached absence of
    /// them).
    pub fn cached_pins(&self) -> usize {
        self.pins().cache.len()
    }

    /// Pins `key` for `alias` as if it had been accepted, writing the pin
    /// file when there is a folder: fails if the alias already has pins.
    /// For tests and tools that pin up front; the viewer's user accepts
    /// keys with [`pin_key`].
    pub fn pin(&self, alias: &str, key: &PublicKey) -> Result<(), String> {
        let mut pins = self.pins();
        if pins.get(alias).pins?.is_some() {
            return Err(format!("{alias} already has pinned keys"));
        }
        match pins.pin(alias, key)? {
            (_, true) => Ok(()),
            (_, false) => Err(format!("{alias} already has pinned keys")),
        }
    }

    /// The keys pinned for `alias`, `None` when it has none yet.
    pub fn pinned(&self, alias: &str) -> Result<Option<PublicKeys>, String> {
        self.pins().get(alias).pins
    }

    fn open_sealed<T: DeserializeOwned>(
        &self,
        bytes: &[u8],
        kv_key: &str,
    ) -> Result<Opened<T>, EnvelopeError> {
        let sealed = parse_envelope(bytes)?;
        let recipient = self
            .secrets
            .get(&sealed.recipient_kid)
            .ok_or_else(|| EnvelopeError::UnknownKid(sealed.recipient_kid.clone()))?;
        let alias = kv_key.split('.').next().unwrap_or_default();
        if validate_alias(alias).is_err() {
            return Err(EnvelopeError::Malformed(
                "the KV key does not start with a machine alias".into(),
            ));
        }
        let (files, asks) = {
            let mut pins = self.pins();
            (pins.get(alias), pins.dir.is_some())
        };
        let pinned = files.pins.clone().map_err(|reason| EnvelopeError::Pins {
            alias: alias.to_string(),
            reason,
        })?;
        if let Some(sender) = pinned
            .as_ref()
            .and_then(|keys| keys.find(sealed.sender.bytes()))
        {
            return Ok(Opened {
                value: sealed.open(kv_key, recipient, sender)?,
                pinned: None,
            });
        }
        if !asks {
            return match pinned {
                Some(_) => Err(EnvelopeError::UnknownSender {
                    alias: alias.to_string(),
                }),
                None => self.trust_first(&sealed, kv_key, recipient, alias),
            };
        }
        if files.is_rejected(sealed.sender.bytes()) {
            return Err(EnvelopeError::RejectedSender {
                alias: alias.to_string(),
            });
        }
        // Only a key whose private half the sender holds is put to the user.
        sealed.open::<serde::de::IgnoredAny>(kv_key, recipient, &sealed.sender)?;
        Err(EnvelopeError::NewSender {
            alias: alias.to_string(),
            key: sealed.sender.clone(),
            known: pinned.is_some(),
        })
    }

    /// Pins in memory only: the first value of an alias pins its key.
    fn trust_first<T: DeserializeOwned>(
        &self,
        sealed: &crate::envelope::Sealed,
        kv_key: &str,
        recipient: &crate::keypair::SecretKey,
        alias: &str,
    ) -> Result<Opened<T>, EnvelopeError> {
        // It must open with the key it claims (which proves the sender
        // holds its private key) before that key is pinned.
        let value = sealed.open(kv_key, recipient, &sealed.sender)?;
        let (keys, wrote) =
            self.pins()
                .pin(alias, &sealed.sender)
                .map_err(|reason| EnvelopeError::Pins {
                    alias: alias.to_string(),
                    reason,
                })?;
        if keys.find(sealed.sender.bytes()).is_none() {
            // Another key was pinned for this alias meanwhile.
            return Err(EnvelopeError::UnknownSender {
                alias: alias.to_string(),
            });
        }
        Ok(Opened {
            value,
            pinned: wrote.then(|| sealed.sender.clone()),
        })
    }
}

impl OpenKeys for ViewerKeys {
    /// The envelope, with the viewer's keys and the pins (see the module docs);
    /// any other version is unsupported.
    fn open_any<T: DeserializeOwned>(
        &self,
        bytes: &[u8],
        kv_key: &str,
    ) -> Result<Opened<T>, EnvelopeError> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(EnvelopeError::TooLarge { len: bytes.len() });
        }
        match envelope_version(bytes) {
            Some(v) if v == u64::from(ENVELOPE_VERSION) => self.open_sealed(bytes, kv_key),
            Some(v) => Err(EnvelopeError::UnsupportedVersion(v)),
            None => Err(EnvelopeError::Malformed("not an envelope".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::seal_value;
    use crate::keypair::SecretKey;
    use crate::{session_key, RemotePayload};
    use cc_core::session::SessionState;

    fn payload(machine: &str) -> RemotePayload {
        RemotePayload {
            machine: machine.into(),
            sid: "3f9a1c2e".into(),
            alias: None,
            title: None,
            dir: None,
            text: None,
            agents: None,
            ags: None,
            state: SessionState::Working,
            since: 1_791_856_000,
            tool: Some("Bash".into()),
            model: None,
            ctx: Some(7),
            hb: 1_791_856_060,
        }
    }

    struct World {
        viewer: SecretKeys,
        devbox: SecretKey,
        other: SecretKey,
    }

    fn world() -> World {
        World {
            viewer: SecretKeys::generate("k1").unwrap(),
            devbox: SecretKey::generate("p-devbox").unwrap(),
            other: SecretKey::generate("p-lab").unwrap(),
        }
    }

    fn seal_as(sender: &SecretKey, to: &SecretKeys, kv_key: &str) -> Vec<u8> {
        seal_value(
            &payload("devbox"),
            kv_key,
            sender,
            &to.current().public_key(),
        )
        .unwrap()
    }

    /// A pin file written before (by an Accept or by hand).
    fn prepin(dir: &Path, alias: &str, key: &SecretKey) {
        PublicKeys::from_keys(vec![key.public_key()])
            .unwrap()
            .save_new(pin_file(dir, alias))
            .unwrap();
    }

    fn is_new_sender<T: std::fmt::Debug>(
        r: Result<Opened<T>, EnvelopeError>,
        key: &SecretKey,
        want_known: bool,
    ) -> bool {
        matches!(r, Err(EnvelopeError::NewSender { alias, key: k, known })
            if alias == "devbox" && k == key.public_key() && known == want_known)
    }

    #[test]
    fn the_first_key_of_a_new_alias_waits_for_consent_and_writes_nothing() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        let first = keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key);
        assert!(is_new_sender(first, &w.devbox, false));
        // Not opened, not pinned, nothing on disk; asked again next time.
        assert!(!pins.exists(), "no file and no folder written");
        assert!(keys.pinned("devbox").unwrap().is_none());
        let again = keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key);
        assert!(is_new_sender(again, &w.devbox, false));
    }

    #[test]
    fn a_new_key_must_open_with_the_key_it_claims_to_be_asked_about() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let keys = ViewerKeys::new(w.viewer.clone(), Some(dir.path().join("publishers")));
        let key = session_key("devbox", "3f9a1c2e");
        // Sealed by lab, relabelled as devbox's key: nothing to ask about.
        let mut v: serde_json::Value =
            serde_json::from_slice(&seal_as(&w.other, &w.viewer, &key)).unwrap();
        v["sk"] = w.devbox.kid().into();
        v["spk"] = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            w.devbox.public_key().bytes(),
        )
        .into();
        assert!(matches!(
            keys.open_any::<RemotePayload>(&serde_json::to_vec(&v).unwrap(), &key),
            Err(EnvelopeError::Decrypt)
        ));
        // Sealed for another KV key, then moved here: the same.
        let moved = seal_as(&w.devbox, &w.viewer, &session_key("lab", "3f9a1c2e"));
        assert!(matches!(
            keys.open_any::<RemotePayload>(&moved, &key),
            Err(EnvelopeError::Decrypt)
        ));
    }

    #[test]
    fn an_accepted_key_opens_from_the_next_value_and_survives_a_restart() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        let sealed = seal_as(&w.devbox, &w.viewer, &key);
        assert!(keys.open_any::<RemotePayload>(&sealed, &key).is_err());

        assert!(pin_key(&pins, "devbox", &w.devbox.public_key()).unwrap());
        assert!(
            !pin_key(&pins, "devbox", &w.devbox.public_key()).unwrap(),
            "pinned already"
        );
        // Cached until refreshed.
        assert!(keys.open_any::<RemotePayload>(&sealed, &key).is_err());
        keys.refresh_alias("devbox");
        let opened: Opened<RemotePayload> = keys.open_any(&sealed, &key).unwrap();
        assert_eq!(opened.value, payload("devbox"));
        assert_eq!(opened.pinned, None, "accepted, not pinned on first use");

        let fresh = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        assert_eq!(
            fresh.pinned("devbox").unwrap().unwrap().current(),
            &w.devbox.public_key()
        );
        fresh.open_any::<RemotePayload>(&sealed, &key).unwrap();
    }

    #[test]
    fn a_new_key_of_a_known_alias_waits_and_is_prepended_when_accepted() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        std::fs::create_dir_all(&pins).unwrap();
        prepin(&pins, "devbox", &w.devbox);
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        // The existing pin keeps working as it is.
        keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();

        let rotated = SecretKey::generate("p-devbox2").unwrap();
        let r = keys.open_any::<RemotePayload>(&seal_as(&rotated, &w.viewer, &key), &key);
        assert!(is_new_sender(r, &rotated, true));
        assert_eq!(
            PublicKeys::load(pin_file(&pins, "devbox"))
                .unwrap()
                .keys()
                .len(),
            1
        );

        pin_key(&pins, "devbox", &rotated.public_key()).unwrap();
        keys.refresh_alias("devbox");
        let file = PublicKeys::load(pin_file(&pins, "devbox")).unwrap();
        let kids: Vec<_> = file.keys().iter().map(|k| k.kid()).collect();
        assert_eq!(kids, ["p-devbox2", "p-devbox"], "newest first");
        keys.open_any::<RemotePayload>(&seal_as(&rotated, &w.viewer, &key), &key)
            .unwrap();
        keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();

        // A different key under a key id that is pinned cannot be accepted.
        let clash = SecretKey::generate("p-devbox").unwrap();
        assert!(pin_key(&pins, "devbox", &clash.public_key()).is_err());
    }

    #[test]
    fn a_rejected_key_is_dropped_unopened_and_stays_rejected() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        let sealed = seal_as(&w.devbox, &w.viewer, &key);
        assert!(reject_key(&pins, "devbox", &w.devbox.public_key()).unwrap());
        assert!(
            !reject_key(&pins, "devbox", &w.devbox.public_key()).unwrap(),
            "rejected already"
        );
        keys.refresh_alias("devbox");
        assert!(matches!(
            keys.open_any::<RemotePayload>(&sealed, &key),
            Err(EnvelopeError::RejectedSender { alias }) if alias == "devbox"
        ));
        // Another key of the alias is still asked about.
        let other = keys.open_any::<RemotePayload>(&seal_as(&w.other, &w.viewer, &key), &key);
        assert!(matches!(other, Err(EnvelopeError::NewSender { .. })));
        // After a restart too.
        let fresh = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        assert!(matches!(
            fresh.open_any::<RemotePayload>(&sealed, &key),
            Err(EnvelopeError::RejectedSender { .. })
        ));

        // Accepting it after all takes it off the rejected file.
        pin_key(&pins, "devbox", &w.devbox.public_key()).unwrap();
        assert!(!rejected_file(&pins, "devbox").exists());
        fresh.refresh_alias("devbox");
        fresh.open_any::<RemotePayload>(&sealed, &key).unwrap();
        // Rejecting a pinned key revokes it: it leaves the pin file, which
        // then refuses the alias, and stays rejected.
        assert!(reject_key(&pins, "devbox", &w.devbox.public_key()).unwrap());
        assert!(
            load_keys(&pin_file(&pins, "devbox")).is_err(),
            "no key left: the alias is refused"
        );
        assert!(load_keys(&rejected_file(&pins, "devbox"))
            .unwrap()
            .unwrap()
            .find(w.devbox.public_key().bytes())
            .is_some());
        fresh.refresh_alias("devbox");
        assert!(
            fresh.open_any::<RemotePayload>(&sealed, &key).is_err(),
            "a revoked key no longer opens"
        );
    }

    #[test]
    fn the_rejected_file_keeps_the_newest_keys() {
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        let made: Vec<SecretKey> = (0..MAX_REJECTED_PER_ALIAS + 2)
            .map(|i| SecretKey::generate(&format!("p{i:02}")).unwrap())
            .collect();
        for k in &made {
            reject_key(&pins, "devbox", &k.public_key()).unwrap();
        }
        let file = PublicKeys::load(rejected_file(&pins, "devbox")).unwrap();
        assert_eq!(file.keys().len(), MAX_REJECTED_PER_ALIAS);
        assert_eq!(file.current(), &made.last().unwrap().public_key());
        assert!(
            file.find(made[0].public_key().bytes()).is_none(),
            "the oldest went"
        );
        // A new key under a rejected key id replaces the old one.
        let again = SecretKey::generate("p17").unwrap();
        reject_key(&pins, "devbox", &again.public_key()).unwrap();
        let file = PublicKeys::load(rejected_file(&pins, "devbox")).unwrap();
        assert_eq!(file.current(), &again.public_key());
        assert_eq!(file.keys().iter().filter(|k| k.kid() == "p17").count(), 1);
        assert!(pin_key(&pins, "Not An Alias", &again.public_key()).is_err());
        assert!(reject_key(&pins, "Not An Alias", &again.public_key()).is_err());
    }

    #[test]
    fn an_unreadable_rejected_file_lets_nothing_in() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        std::fs::create_dir_all(&pins).unwrap();
        std::fs::write(rejected_file(&pins, "devbox"), "junk\n").unwrap();
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        let r = keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key);
        assert!(
            is_new_sender(r, &w.devbox, false),
            "asked again, not opened"
        );
        assert!(
            reject_key(&pins, "devbox", &w.devbox.public_key()).is_err(),
            "not clobbered"
        );
        assert_eq!(
            std::fs::read_to_string(rejected_file(&pins, "devbox")).unwrap(),
            "junk\n"
        );
    }

    #[test]
    fn pins_in_memory_trust_the_first_key() {
        let w = world();
        let keys = ViewerKeys::new(w.viewer.clone(), None);
        let key = session_key("devbox", "3f9a1c2e");
        let opened: Opened<RemotePayload> = keys
            .open_any(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
        assert_eq!(opened.pinned.as_ref(), Some(&w.devbox.public_key()));
        let again: Opened<RemotePayload> = keys
            .open_any(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
        assert_eq!(again.pinned, None);
        assert_eq!(keys.pins_dir(), None);
    }

    #[test]
    fn another_key_claiming_a_pinned_alias_is_rejected() {
        let w = world();
        let keys = ViewerKeys::new(w.viewer.clone(), None);
        let key = session_key("devbox", "3f9a1c2e");
        keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
        // lab's key (a real, valid publisher key) writing under devbox's alias.
        let forged = seal_as(&w.other, &w.viewer, &key);
        assert!(matches!(
            keys.open_any::<RemotePayload>(&forged, &key),
            Err(EnvelopeError::UnknownSender { alias }) if alias == "devbox"
        ));
        // Relabelling the forged value with devbox's key id or key does not help:
        // with devbox's public key it no longer authenticates.
        let mut v: serde_json::Value = serde_json::from_slice(&forged).unwrap();
        v["sk"] = w.devbox.kid().into();
        v["spk"] = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            w.devbox.public_key().bytes(),
        )
        .into();
        assert!(matches!(
            keys.open_any::<RemotePayload>(&serde_json::to_vec(&v).unwrap(), &key),
            Err(EnvelopeError::Decrypt)
        ));
    }

    #[test]
    fn a_rotated_key_is_accepted_once_its_line_is_added_by_hand() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        std::fs::create_dir_all(&pins).unwrap();
        prepin(&pins, "devbox", &w.devbox);
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
        let rotated = SecretKey::generate("p-devbox2").unwrap();
        assert!(keys
            .open_any::<RemotePayload>(&seal_as(&rotated, &w.viewer, &key), &key)
            .is_err());

        let mut file = PublicKeys::load(pins.join("devbox.pub")).unwrap();
        file.prepend(rotated.public_key()).unwrap();
        file.save(pins.join("devbox.pub")).unwrap();
        // Cached until forgotten.
        assert!(keys
            .open_any::<RemotePayload>(&seal_as(&rotated, &w.viewer, &key), &key)
            .is_err());
        keys.forget_pins();
        keys.open_any::<RemotePayload>(&seal_as(&rotated, &w.viewer, &key), &key)
            .unwrap();
        keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
    }

    #[test]
    fn a_broken_pin_file_rejects_the_alias_and_is_left_alone() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        std::fs::create_dir_all(&pins).unwrap();
        std::fs::write(pins.join("devbox.pub"), "not a key\n").unwrap();
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        assert!(matches!(
            keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key),
            Err(EnvelopeError::Pins { .. })
        ));
        // Not asked about, and accepting it by command is refused too: a
        // revoked machine stays revoked until the file is fixed by hand.
        assert!(pin_key(&pins, "devbox", &w.devbox.public_key()).is_err());
        assert_eq!(
            std::fs::read_to_string(pins.join("devbox.pub")).unwrap(),
            "not a key\n"
        );
        std::fs::write(pins.join("devbox.pub"), "# revoked\n").unwrap();
        assert!(pin_key(&pins, "devbox", &w.devbox.public_key()).is_err());
    }

    #[test]
    fn only_the_envelope_version_opens() {
        let w = world();
        let key = session_key("devbox", "3f9a1c2e");
        let sealed = seal_as(&w.devbox, &w.viewer, &key);
        let keys = ViewerKeys::new(w.viewer.clone(), None);
        assert_eq!(
            keys.open_any::<RemotePayload>(&sealed, &key).unwrap().value,
            payload("devbox")
        );

        // Any other version is unsupported, and pins nothing.
        let other = br#"{"v":2,"x":"y"}"#;
        let key_lab = session_key("lab", "3f9a1c2e");
        assert!(matches!(
            keys.open_any::<RemotePayload>(other, &key_lab),
            Err(EnvelopeError::UnsupportedVersion(2))
        ));
        assert!(keys.pinned("lab").unwrap().is_none());

        let mut v: serde_json::Value = serde_json::from_slice(&sealed).unwrap();
        v["v"] = 3.into();
        assert!(matches!(
            keys.open_any::<RemotePayload>(&serde_json::to_vec(&v).unwrap(), &key),
            Err(EnvelopeError::UnsupportedVersion(3))
        ));
        assert!(matches!(
            keys.open_any::<RemotePayload>(b"junk", &key),
            Err(EnvelopeError::Malformed(_))
        ));
        assert!(matches!(
            keys.open_any::<RemotePayload>(&vec![b' '; MAX_ENVELOPE_BYTES + 1], &key),
            Err(EnvelopeError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_value_for_another_viewer_key_is_unknown() {
        let w = world();
        let elsewhere = SecretKeys::generate("v9").unwrap();
        let dir = tempfile::tempdir().unwrap();
        for keys in [
            ViewerKeys::new(w.viewer.clone(), None),
            ViewerKeys::new(w.viewer.clone(), Some(dir.path().join("publishers"))),
        ] {
            let key = session_key("devbox", "3f9a1c2e");
            assert!(matches!(
                keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &elsewhere, &key), &key),
                Err(EnvelopeError::UnknownKid(k)) if k == "v9"
            ));
            // Same key id, different key: it does not authenticate, and is
            // not asked about either.
            let same_kid = SecretKeys::generate("k1").unwrap();
            assert!(matches!(
                keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &same_kid, &key), &key),
                Err(EnvelopeError::Decrypt)
            ));
            assert!(
                keys.pinned("devbox").unwrap().is_none(),
                "nothing pinned on failure"
            );
        }
    }

    #[test]
    fn pins_loaded_ahead_open_without_reading_the_disk_again() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        pin_key(&pins, "devbox", &w.devbox.public_key()).unwrap();
        let key = session_key("devbox", "3f9a1c2e");

        let fresh = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let wanted = [
            "devbox".to_string(),
            "lab".to_string(),
            "Not An Alias".to_string(),
        ];
        assert_eq!(
            fresh.uncached_pins(wanted.iter().map(String::as_str)),
            ["devbox", "lab"]
        );
        fresh.load_pins(&wanted);
        assert!(fresh
            .uncached_pins(wanted.iter().map(String::as_str))
            .is_empty());
        assert_eq!(fresh.cached_pins(), 2, "a missing file is cached too");

        // Cached: removing the file now does not change what opens.
        std::fs::remove_file(pins.join("devbox.pub")).unwrap();
        let opened: Opened<RemotePayload> = fresh
            .open_any(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
        assert_eq!(opened.pinned, None);
        assert!(matches!(
            fresh.open_any::<RemotePayload>(&seal_as(&w.other, &w.viewer, &key), &key),
            Err(EnvelopeError::NewSender { known: true, .. })
        ));
        // In-memory pins never need loading.
        assert!(ViewerKeys::new(w.viewer.clone(), None)
            .uncached_pins(["devbox"])
            .is_empty());
    }

    #[test]
    fn reloading_the_pins_picks_up_edits_and_keeps_only_the_given_aliases() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        pin_key(&pins, "devbox", &w.devbox.public_key()).unwrap();
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
        keys.load_pins(&["lab".to_string(), "gone".to_string()]);
        assert_eq!(keys.cached_pins(), 3);

        // A rotated key added by hand counts after a reload.
        let rotated = SecretKey::generate("p-devbox2").unwrap();
        let mut file = PublicKeys::load(pins.join("devbox.pub")).unwrap();
        file.prepend(rotated.public_key()).unwrap();
        file.save(pins.join("devbox.pub")).unwrap();
        assert!(keys
            .open_any::<RemotePayload>(&seal_as(&rotated, &w.viewer, &key), &key)
            .is_err());
        keys.reload_pins(&["devbox".to_string(), "lab".to_string()]);
        keys.open_any::<RemotePayload>(&seal_as(&rotated, &w.viewer, &key), &key)
            .unwrap();
        assert_eq!(keys.cached_pins(), 2, "an alias left out is forgotten");

        // lab was cached as having no file; once accepted and refreshed it
        // opens, and a rejected key of it is dropped.
        let lab = session_key("lab", "3f9a1c2e");
        pin_key(&pins, "lab", &w.other.public_key()).unwrap();
        reject_key(&pins, "lab", &w.devbox.public_key()).unwrap();
        keys.refresh_alias("lab");
        keys.open_any::<RemotePayload>(&seal_as(&w.other, &w.viewer, &lab), &lab)
            .unwrap();
        assert!(matches!(
            keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &lab), &lab),
            Err(EnvelopeError::RejectedSender { .. })
        ));
    }

    #[test]
    fn a_reload_that_read_before_a_refresh_keeps_the_refresh() {
        let w = world();
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        let keys = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        let key = session_key("devbox", "3f9a1c2e");
        // A reload reads the files (no pin yet)…
        let epoch = keys.pins().epoch;
        let stale: BTreeMap<String, AliasFiles> =
            [("devbox".to_string(), AliasFiles::read(&pins, "devbox"))].into();
        // …while the user accepts devbox's key…
        pin_key(&pins, "devbox", &w.devbox.public_key()).unwrap();
        keys.refresh_alias("devbox");
        // …and swaps in what it read: the accepted key stays in force.
        keys.pins().swap_reloaded(epoch, stale.clone());
        keys.open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
        // A reload that starts after the refresh is believed as it is.
        let epoch = keys.pins().epoch;
        keys.pins().swap_reloaded(epoch, stale);
        assert!(keys
            .open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .is_err());
        assert!(keys.pins().refreshed.is_empty());
    }

    #[test]
    fn pin_refuses_an_alias_that_has_pins() {
        let w = world();
        let keys = ViewerKeys::new(w.viewer.clone(), None);
        keys.pin("devbox", &w.devbox.public_key()).unwrap();
        assert!(keys.pin("devbox", &w.other.public_key()).is_err());
        let key = session_key("devbox", "3f9a1c2e");
        assert!(matches!(
            keys.open_any::<RemotePayload>(&seal_as(&w.other, &w.viewer, &key), &key),
            Err(EnvelopeError::UnknownSender { .. })
        ));
        // With a folder it writes the pin file, and the key opens.
        let dir = tempfile::tempdir().unwrap();
        let pins = dir.path().join("publishers");
        let on_disk = ViewerKeys::new(w.viewer.clone(), Some(pins.clone()));
        on_disk.pin("devbox", &w.devbox.public_key()).unwrap();
        assert!(pin_file(&pins, "devbox").is_file());
        on_disk
            .open_any::<RemotePayload>(&seal_as(&w.devbox, &w.viewer, &key), &key)
            .unwrap();
    }
}
