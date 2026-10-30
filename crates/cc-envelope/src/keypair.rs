//! Key pairs of the envelope, and their files.
//!
//! Every machine has an X25519 key pair: the viewer to receive, each
//! publisher to prove who sealed a value (HPKE's Auth mode, see
//! [`crate::envelope`]). Private and public key files use the same format
//! ([`crate::keyfile`]), one `<kid>:<base64 of 32 bytes>` line per key,
//! newest first, `#` comments and blank lines ignored:
//!
//! ```text
//! v20261014-3f9a:<base64 of the 32-byte key>
//! ```
//!
//! The files, under [`cc_core::paths::keys_dir`]:
//!
//! - `viewer.key` on the viewer: its private keys. Private (mode `0600` on
//!   Unix), written atomically, never copied anywhere but into the app's
//!   password-sealed backup.
//! - `viewer.pub`: the matching public keys. Derived from `viewer.key` on the
//!   viewer ([`ensure_viewer_keys`] rewrites it when it drifts) and copied to
//!   every publisher, which seals to its newest line.
//! - `publisher.key` on a publisher: its own private keys, the newest one
//!   seals.
//! - `publishers/<alias>.pub` on the viewer: the public keys a publisher is
//!   pinned to, and `publishers/<alias>.rejected` the ones its user
//!   rejected for it (see [`crate::opener`]).
//!
//! Private key bytes are wiped from memory on drop and never appear in
//! `Debug` output or errors; a malformed line is reported by its number.

use std::fmt;
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use hpke::{Deserializable, Kem as _, Serializable};
use zeroize::Zeroizing;

use crate::keyfile::{
    parse_key_lines, read_key_file, validate_kid, write_key_file, KeyFileError, Privacy, Replace,
    KEY_LEN,
};

/// The HPKE KEM of the envelope: DHKEM(X25519, HKDF-SHA256).
pub(crate) type Kem = hpke::kem::X25519HkdfSha256;

/// Length of an X25519 public or private key in bytes.
pub const X25519_KEY_LEN: usize = KEY_LEN;

/// A public key: a key id and 32 bytes. Not secret.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicKey {
    kid: String,
    bytes: [u8; X25519_KEY_LEN],
}

impl PublicKey {
    /// A public key from its bytes. Fails if `kid` is invalid (see
    /// [`validate_kid`]).
    pub fn new(kid: &str, bytes: [u8; X25519_KEY_LEN]) -> Result<Self, KeyFileError> {
        validate_kid(kid)?;
        Ok(Self {
            kid: kid.to_string(),
            bytes,
        })
    }

    /// Parses one `<kid>:<base64>` line, as `cc-publish keygen` prints it.
    pub fn parse_line(line: &str) -> Result<Self, KeyFileError> {
        let keys = PublicKeys::parse(line)?;
        match keys.keys() {
            [one] => Ok(one.clone()),
            _ => Err(KeyFileError::Malformed {
                line: 2,
                reason: "expected a single <kid>:<base64 key> line",
            }),
        }
    }

    /// The key id, carried in every envelope as `rk` or `sk`.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The key bytes.
    pub fn bytes(&self) -> &[u8; X25519_KEY_LEN] {
        &self.bytes
    }

    /// The key file line, `<kid>:<base64>`.
    pub fn to_line(&self) -> String {
        format!("{}:{}", self.kid, STANDARD.encode(self.bytes))
    }

    pub(crate) fn to_hpke(&self) -> Result<<Kem as hpke::Kem>::PublicKey, hpke::HpkeError> {
        <Kem as hpke::Kem>::PublicKey::from_bytes(&self.bytes)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublicKey")
            .field("kid", &self.kid)
            .field("bytes", &STANDARD.encode(self.bytes))
            .finish()
    }
}

/// A private key with its key id, and the public key that goes with it.
/// The private bytes are wiped from memory on drop; `Debug` prints the key
/// id and the public key only.
#[derive(Clone)]
pub struct SecretKey {
    kid: String,
    bytes: Zeroizing<[u8; X25519_KEY_LEN]>,
    public: [u8; X25519_KEY_LEN],
}

impl SecretKey {
    /// A private key from its bytes. Fails if `kid` is invalid.
    pub fn new(kid: &str, bytes: [u8; X25519_KEY_LEN]) -> Result<Self, KeyFileError> {
        Self::from_secret(kid, Zeroizing::new(bytes))
    }

    fn from_secret(
        kid: &str,
        bytes: Zeroizing<[u8; X25519_KEY_LEN]>,
    ) -> Result<Self, KeyFileError> {
        validate_kid(kid)?;
        let sk = private_from(&bytes);
        let mut public = [0u8; X25519_KEY_LEN];
        public.copy_from_slice(&Kem::sk_to_pk(&sk).to_bytes());
        Ok(Self {
            kid: kid.to_string(),
            bytes,
            public,
        })
    }

    /// A fresh key from the OS random number generator.
    pub fn generate(kid: &str) -> Result<Self, KeyFileError> {
        validate_kid(kid)?;
        let mut bytes = Zeroizing::new([0u8; X25519_KEY_LEN]);
        getrandom::getrandom(bytes.as_mut()).map_err(KeyFileError::Rng)?;
        Self::from_secret(kid, bytes)
    }

    /// The key id.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public key of this private key, under the same key id.
    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            kid: self.kid.clone(),
            bytes: self.public,
        }
    }

    pub(crate) fn to_hpke(&self) -> <Kem as hpke::Kem>::PrivateKey {
        private_from(&self.bytes)
    }

    fn to_line(&self) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "{}:{}",
            self.kid,
            STANDARD.encode(self.bytes.as_ref())
        ))
    }
}

/// The HPKE private key for `bytes`. Any 32 bytes are a valid X25519 key
/// (it is clamped when used), so this cannot fail.
fn private_from(bytes: &[u8; X25519_KEY_LEN]) -> <Kem as hpke::Kem>::PrivateKey {
    match <Kem as hpke::Kem>::PrivateKey::from_bytes(bytes) {
        Ok(sk) => sk,
        Err(_) => unreachable!("an X25519 private key is any 32 bytes"),
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretKey")
            .field("kid", &self.kid)
            .field("bytes", &"<redacted>")
            .field("public", &STANDARD.encode(self.public))
            .finish()
    }
}

/// A machine's private keys, newest first. Never empty, no key id twice.
/// The newest seals (a publisher) or is the one publishers are given (the
/// viewer); any of them opens, so values sealed before a rotation stay
/// readable.
#[derive(Clone, Debug)]
pub struct SecretKeys {
    keys: Vec<SecretKey>,
}

impl SecretKeys {
    /// From keys ordered newest first. Fails if empty or a key id repeats.
    pub fn from_keys(keys: Vec<SecretKey>) -> Result<Self, KeyFileError> {
        check_kids(keys.iter().map(|k| k.kid()))?;
        Ok(Self { keys })
    }

    /// One fresh key.
    pub fn generate(kid: &str) -> Result<Self, KeyFileError> {
        Ok(Self {
            keys: vec![SecretKey::generate(kid)?],
        })
    }

    /// The newest key.
    pub fn current(&self) -> &SecretKey {
        &self.keys[0]
    }

    /// The key with this key id.
    pub fn get(&self, kid: &str) -> Option<&SecretKey> {
        self.keys.iter().find(|k| k.kid == kid)
    }

    /// All keys, newest first.
    pub fn keys(&self) -> &[SecretKey] {
        &self.keys
    }

    /// Rotation: a fresh key under `kid` becomes the newest. Fails if the
    /// key id is taken.
    pub fn rotate(&mut self, kid: &str) -> Result<(), KeyFileError> {
        if self.get(kid).is_some() {
            return Err(KeyFileError::DuplicateKid(kid.to_string()));
        }
        self.keys.insert(0, SecretKey::generate(kid)?);
        Ok(())
    }

    /// Adds the keys of `other` this set lacks, after its own: the newest key
    /// stays the newest. Returns how many were added. Fails, changing
    /// nothing, when a key id holds different keys in the two sets.
    pub fn append_missing(&mut self, other: &SecretKeys) -> Result<usize, KeyFileError> {
        for key in &other.keys {
            if let Some(have) = self.get(&key.kid) {
                if have.bytes != key.bytes {
                    return Err(KeyFileError::ConflictingKid(key.kid.clone()));
                }
            }
        }
        let missing: Vec<SecretKey> = other
            .keys
            .iter()
            .filter(|k| self.get(&k.kid).is_none())
            .cloned()
            .collect();
        let added = missing.len();
        self.keys.extend(missing);
        Ok(added)
    }

    /// The public keys, in the same order.
    pub fn public_keys(&self) -> PublicKeys {
        PublicKeys {
            keys: self.keys.iter().map(SecretKey::public_key).collect(),
        }
    }

    /// Parses a private key file.
    pub fn parse(text: &str) -> Result<Self, KeyFileError> {
        let keys = parse_key_lines(text)?
            .into_iter()
            .map(|(kid, bytes)| SecretKey::from_secret(&kid, bytes))
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_keys(keys)
    }

    /// The file text, one line per key, newest first. Secret; never log it.
    pub fn to_file_string(&self) -> Zeroizing<String> {
        let mut out = Zeroizing::new(String::new());
        for key in &self.keys {
            out.push_str(&key.to_line());
            out.push('\n');
        }
        out
    }

    /// Loads a private key file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, KeyFileError> {
        Self::parse(&read_key_file(path.as_ref())?)
    }

    /// Writes the file privately and atomically, replacing an existing one.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), KeyFileError> {
        let text = self.to_file_string();
        write_key_file(
            path.as_ref(),
            text.as_bytes(),
            Privacy::Private,
            Replace::Yes,
        )
    }

    /// Writes the file privately and atomically, failing with
    /// [`KeyFileError::Exists`] when there is one already (also when another
    /// process writes it at the same moment).
    pub fn save_new(&self, path: impl AsRef<Path>) -> Result<(), KeyFileError> {
        let text = self.to_file_string();
        write_key_file(
            path.as_ref(),
            text.as_bytes(),
            Privacy::Private,
            Replace::No,
        )
    }
}

/// Public keys, newest first. Never empty, no key id twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKeys {
    keys: Vec<PublicKey>,
}

impl PublicKeys {
    /// From keys ordered newest first. Fails if empty or a key id repeats.
    pub fn from_keys(keys: Vec<PublicKey>) -> Result<Self, KeyFileError> {
        check_kids(keys.iter().map(|k| k.kid()))?;
        Ok(Self { keys })
    }

    /// The newest key.
    pub fn current(&self) -> &PublicKey {
        &self.keys[0]
    }

    /// The key with this key id.
    pub fn get(&self, kid: &str) -> Option<&PublicKey> {
        self.keys.iter().find(|k| k.kid == kid)
    }

    /// The key with these bytes, whatever its key id.
    pub fn find(&self, bytes: &[u8; X25519_KEY_LEN]) -> Option<&PublicKey> {
        self.keys.iter().find(|k| &k.bytes == bytes)
    }

    /// All keys, newest first.
    pub fn keys(&self) -> &[PublicKey] {
        &self.keys
    }

    /// Adds `key` as the newest. Fails if its key id is taken.
    pub fn prepend(&mut self, key: PublicKey) -> Result<(), KeyFileError> {
        if self.get(&key.kid).is_some() {
            return Err(KeyFileError::DuplicateKid(key.kid.clone()));
        }
        self.keys.insert(0, key);
        Ok(())
    }

    /// Adds the keys of `other` this set lacks, after its own. Returns how
    /// many were added; fails, changing nothing, when a key id holds
    /// different keys in the two sets.
    pub fn append_missing(&mut self, other: &PublicKeys) -> Result<usize, KeyFileError> {
        for key in &other.keys {
            if self
                .get(&key.kid)
                .is_some_and(|have| have.bytes != key.bytes)
            {
                return Err(KeyFileError::ConflictingKid(key.kid.clone()));
            }
        }
        let missing: Vec<PublicKey> = other
            .keys
            .iter()
            .filter(|k| self.get(&k.kid).is_none())
            .cloned()
            .collect();
        let added = missing.len();
        self.keys.extend(missing);
        Ok(added)
    }

    /// Parses a public key file.
    pub fn parse(text: &str) -> Result<Self, KeyFileError> {
        let keys = parse_key_lines(text)?
            .into_iter()
            .map(|(kid, bytes)| PublicKey::new(&kid, *bytes))
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_keys(keys)
    }

    /// The file text, one line per key, newest first.
    pub fn to_file_string(&self) -> String {
        self.keys.iter().map(|k| k.to_line() + "\n").collect()
    }

    /// Loads a public key file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, KeyFileError> {
        Self::parse(&read_key_file(path.as_ref())?)
    }

    /// Writes the file atomically, replacing an existing one. Public keys
    /// are not secret, so it gets the usual permissions.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), KeyFileError> {
        let text = self.to_file_string();
        write_key_file(
            path.as_ref(),
            text.as_bytes(),
            Privacy::Public,
            Replace::Yes,
        )
    }

    /// Writes the file atomically, failing with [`KeyFileError::Exists`]
    /// when there is one already.
    pub fn save_new(&self, path: impl AsRef<Path>) -> Result<(), KeyFileError> {
        let text = self.to_file_string();
        write_key_file(path.as_ref(), text.as_bytes(), Privacy::Public, Replace::No)
    }
}

fn check_kids<'a>(kids: impl Iterator<Item = &'a str>) -> Result<(), KeyFileError> {
    let mut seen: Vec<&str> = Vec::new();
    for kid in kids {
        if seen.contains(&kid) {
            return Err(KeyFileError::DuplicateKid(kid.to_string()));
        }
        seen.push(kid);
    }
    if seen.is_empty() {
        return Err(KeyFileError::Empty);
    }
    Ok(())
}

/// A key id for a key made at `unix_secs`: `<prefix><YYYYMMDD>-<4 random hex>`,
/// e.g. `v20261014-3f9a`. The random part keeps two keys made the same day
/// on two machines apart (merging a backup would otherwise see one key id
/// with two keys). `prefix` is a short tag such as `v` (viewer) or `p`
/// (publisher).
pub fn new_kid(prefix: &str, unix_secs: i64) -> Result<String, KeyFileError> {
    let mut r = [0u8; 2];
    getrandom::getrandom(&mut r).map_err(KeyFileError::Rng)?;
    let kid = format!("{prefix}{}-{:02x}{:02x}", ymd(unix_secs), r[0], r[1]);
    validate_kid(&kid)?;
    Ok(kid)
}

/// `YYYYMMDD` (UTC) of Unix seconds.
fn ymd(unix_secs: i64) -> String {
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = unix_secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}")
}

/// What [`ensure_viewer_keys`] found or did.
#[derive(Debug)]
pub struct ViewerKeyState {
    /// The viewer's private keys, as on disk now.
    pub keys: SecretKeys,
    /// True when this call generated them (no `viewer.key` was there).
    pub created: bool,
    /// True when this call (re)wrote `viewer.pub`.
    pub wrote_public: bool,
}

/// Makes sure the viewer's key pair exists: loads `key_path`, or generates
/// a single key under `kid` and writes it (private, atomically, never
/// replacing a file another process wrote meanwhile: that one is loaded
/// instead). Then writes `pub_path` with the matching public keys when it
/// is missing or does not match, since it is derived from the private keys.
///
/// An existing private key file that does not load is an error, never
/// replaced: the publishers seal to the keys in it.
pub fn ensure_viewer_keys(
    key_path: &Path,
    pub_path: &Path,
    kid: &str,
) -> Result<ViewerKeyState, KeyFileError> {
    let (keys, created) = if key_path.exists() {
        (SecretKeys::load(key_path)?, false)
    } else {
        let fresh = SecretKeys::generate(kid)?;
        match fresh.save_new(key_path) {
            Ok(()) => (fresh, true),
            Err(KeyFileError::Exists(_)) => (SecretKeys::load(key_path)?, false),
            Err(e) => return Err(e),
        }
    };
    let public = keys.public_keys();
    let current = PublicKeys::load(pub_path).ok();
    let wrote_public = current.as_ref() != Some(&public);
    if wrote_public {
        public.save(pub_path)?;
    }
    Ok(ViewerKeyState {
        keys,
        created,
        wrote_public,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const B64_32: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    #[test]
    fn a_secret_key_knows_its_public_key_and_redacts_itself() {
        let a = SecretKey::generate("p1").unwrap();
        let b = SecretKey::new("p1", *a.bytes).unwrap();
        assert_eq!(a.public_key(), b.public_key());
        assert_ne!(a.public_key().bytes(), &[0u8; 32]);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("redacted") && dbg.contains("p1"), "{dbg}");
        assert!(!dbg.contains(&STANDARD.encode(a.bytes.as_ref())), "{dbg}");
        let keys = SecretKeys::from_keys(vec![a.clone()]).unwrap();
        let file = keys.to_file_string();
        assert!(!format!("{keys:?}").contains(&file.trim()[3..]));
        // The public key is the X25519 base-point multiple of the private
        // bytes, checked against x25519-dalek directly with fresh keys (no
        // private key, not even a published test vector, is in the source).
        for _ in 0..8 {
            let k = SecretKey::generate("k").unwrap();
            let expected = x25519_dalek::x25519(*k.bytes, x25519_dalek::X25519_BASEPOINT_BYTES);
            assert_eq!(k.public_key().bytes(), &expected);
        }
    }

    #[test]
    fn files_parse_in_order_and_reject_bad_lines() {
        let text = format!("# viewer\n\nk2:{B64_32}\r\n  k1:{B64_32}\n");
        let keys = SecretKeys::parse(&text).unwrap();
        let kids: Vec<_> = keys.keys().iter().map(|k| k.kid()).collect();
        assert_eq!(kids, ["k2", "k1"]);
        assert_eq!(keys.current().kid(), "k2");
        let public = PublicKeys::parse(&text).unwrap();
        assert_eq!(public.current().kid(), "k2");
        assert_eq!(public.get("k1").unwrap().bytes()[31], 31);
        assert!(public.find(&[7u8; 32]).is_none());

        for (bad, line) in [
            (format!("k1:{B64_32}\nnocolon"), 2),
            ("k1:not base64!!".to_string(), 1),
            (format!("k1:{}", STANDARD.encode([1u8; 31])), 1),
            (format!("bad kid:{B64_32}"), 1),
        ] {
            for err in [
                SecretKeys::parse(&bad).unwrap_err(),
                PublicKeys::parse(&bad).unwrap_err(),
            ] {
                match err {
                    KeyFileError::Malformed { line: l, .. } => assert_eq!(l, line, "{bad:?}"),
                    other => panic!("{bad:?}: {other:?}"),
                }
            }
        }
        assert!(matches!(
            PublicKeys::parse("# none\n"),
            Err(KeyFileError::Empty)
        ));
        let dup = format!("k1:{B64_32}\nk1:{B64_32}\n");
        assert!(matches!(
            SecretKeys::parse(&dup),
            Err(KeyFileError::DuplicateKid(_))
        ));
        assert!(PublicKey::parse_line(&format!("k1:{B64_32}")).is_ok());
        assert!(PublicKey::parse_line(&dup).is_err());
        assert!(PublicKey::parse_line("").is_err());
    }

    #[test]
    fn saved_files_load_back_and_save_new_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys").join("publisher.key");
        let mut keys = SecretKeys::generate("p1").unwrap();
        keys.save_new(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            SecretKeys::generate("p2").unwrap().save_new(&path),
            Err(KeyFileError::Exists(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before, "untouched");
        keys.rotate("p2").unwrap();
        assert!(matches!(
            keys.rotate("p1"),
            Err(KeyFileError::DuplicateKid(_))
        ));
        keys.save(&path).unwrap();
        let back = SecretKeys::load(&path).unwrap();
        assert_eq!(back.public_keys(), keys.public_keys());
        assert_eq!(back.current().kid(), "p2");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["publisher.key"], "no temporary files left");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let pub_path = dir.path().join("keys").join("viewer.pub");
        keys.public_keys().save_new(&pub_path).unwrap();
        assert_eq!(PublicKeys::load(&pub_path).unwrap(), keys.public_keys());
        assert!(matches!(
            keys.public_keys().save_new(&pub_path),
            Err(KeyFileError::Exists(_))
        ));
    }

    #[test]
    fn append_missing_keeps_the_newest_and_refuses_conflicts() {
        let mut mine = SecretKeys::generate("k2").unwrap();
        let theirs = SecretKeys::generate("k1").unwrap();
        assert_eq!(mine.append_missing(&theirs).unwrap(), 1);
        assert_eq!(mine.append_missing(&theirs).unwrap(), 0);
        let kids: Vec<_> = mine.keys().iter().map(|k| k.kid()).collect();
        assert_eq!(kids, ["k2", "k1"]);
        let clash = SecretKeys::generate("k1").unwrap();
        assert!(matches!(
            mine.append_missing(&clash),
            Err(KeyFileError::ConflictingKid(_))
        ));
        assert_eq!(mine.keys().len(), 2);

        let mut pins = clash.public_keys();
        assert!(matches!(
            pins.append_missing(&theirs.public_keys()),
            Err(KeyFileError::ConflictingKid(_))
        ));
        pins.prepend(mine.current().public_key()).unwrap();
        assert_eq!(pins.current().kid(), "k2");
        assert!(pins.prepend(mine.current().public_key()).is_err());
    }

    #[test]
    fn viewer_keys_are_created_once_and_the_public_file_follows_them() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("keys").join("viewer.key");
        let public = dir.path().join("keys").join("viewer.pub");
        let first = ensure_viewer_keys(&key, &public, "k1").unwrap();
        assert!(first.created && first.wrote_public);
        assert_eq!(PublicKeys::load(&public).unwrap(), first.keys.public_keys());

        let again = ensure_viewer_keys(&key, &public, "v9").unwrap();
        assert!(!again.created && !again.wrote_public);
        assert_eq!(again.keys.current().kid(), "k1", "never replaced");

        // A stale or broken public file is rewritten from the private keys.
        std::fs::write(&public, "junk").unwrap();
        let fixed = ensure_viewer_keys(&key, &public, "v9").unwrap();
        assert!(fixed.wrote_public);
        assert_eq!(PublicKeys::load(&public).unwrap(), first.keys.public_keys());

        // A broken private file is an error, and left alone.
        std::fs::write(&key, "broken").unwrap();
        assert!(ensure_viewer_keys(&key, &public, "v9").is_err());
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "broken");
    }

    #[test]
    fn key_ids_carry_the_date_and_a_random_part() {
        let kid = new_kid("v", 1_791_894_896).unwrap();
        assert!(kid.starts_with("v20261013-"), "{kid}");
        assert_eq!(kid.len(), "v20261013-".len() + 4);
        validate_kid(&kid).unwrap();
        assert_eq!(ymd(0), "19700101");
        assert_eq!(ymd(951_782_400), "20000229");
        assert!(new_kid("bad prefix", 0).is_err());
    }
}
