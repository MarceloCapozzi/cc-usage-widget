//! Sealing and opening KV values.
//!
//! A sealed value is a small JSON document, at most [`MAX_ENVELOPE_BYTES`].
//! It encrypts the JSON of the payload and uses the KV key the value is
//! stored under as the associated data, so a value copied to another key
//! (another machine's alias, another session) fails to open. Base64 is the
//! standard alphabet with padding.
//!
//! The format, with a key pair per machine (`v` is the format version,
//! [`ENVELOPE_VERSION`]):
//!
//! ```text
//! {"v":1,"rk":"<viewer key id>","sk":"<sender key id>","spk":"<base64 sender public key>",
//!  "e":"<base64 encapsulated key>","c":"<base64 ciphertext+tag>"}
//! ```
//!
//! HPKE (RFC 9180) in Auth mode: DHKEM(X25519, HKDF-SHA256), HKDF-SHA256,
//! ChaCha20-Poly1305, info [`HPKE_INFO`], one encapsulation (a fresh
//! ephemeral key) per value. Only the holder of the viewer's private key
//! `rk` can open it, and it opens only with the sender's public key `spk`,
//! so it also proves which key pair sealed it. The viewer decides whether
//! that key may write the alias of the KV key (see [`crate::opener`]); the
//! envelope's own fields are never trusted for that.
//!
//! A value naming any other version is rejected as unsupported.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::keyfile::validate_kid;
use crate::keypair::{Kem, PublicKey, PublicKeys, SecretKey, SecretKeys, X25519_KEY_LEN};

/// The envelope's format version, its `v`: the only version sealed and
/// opened.
pub const ENVELOPE_VERSION: u32 = 1;

/// The HPKE `info` of the envelope, binding the keys derived for a value to
/// this format. Its bytes are part of the format: changing them breaks
/// opening every value sealed so far.
pub const HPKE_INFO: &[u8] = b"cc-sessions envelope";

type HpkeAead = hpke::aead::ChaCha20Poly1305;
type HpkeKdf = hpke::kdf::HkdfSha256;

/// Largest sealed value accepted by [`open_value`] or produced by
/// [`seal_value`]: the bucket's per-value limit.
pub const MAX_ENVELOPE_BYTES: usize = 4096;

/// Poly1305 tag length in bytes, appended to the ciphertext.
pub const TAG_LEN: usize = 16;

/// Why a value could not be sealed or opened.
#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    /// The sealed value is larger than [`MAX_ENVELOPE_BYTES`].
    #[error("envelope is {len} bytes, over the {MAX_ENVELOPE_BYTES}-byte limit")]
    TooLarge {
        /// Size of the offending value in bytes.
        len: usize,
    },
    /// The value is not an envelope: not JSON, missing or extra fields, or
    /// a key id that is not well formed.
    #[error("malformed envelope: {0}")]
    Malformed(String),
    /// The envelope's `v` is not 2.
    #[error("unsupported envelope version {0}")]
    UnsupportedVersion(u64),
    /// This machine holds no viewer private keys (`viewer.key`).
    #[error("no viewer keys on this machine")]
    NoKeys,
    /// The envelope was sealed to a viewer key this machine does not have.
    #[error("unknown key id {0:?}")]
    UnknownKid(String),
    /// A base64 field does not decode.
    #[error("field {field:?} is not valid base64")]
    BadBase64 {
        /// `c`, `spk` or `e`.
        field: &'static str,
    },
    /// A key field of an envelope does not decode to 32 bytes.
    #[error("field {field:?} is {len} bytes, expected {X25519_KEY_LEN}")]
    BadKeyLength {
        /// `spk` or `e`.
        field: &'static str,
        /// Its decoded length.
        len: usize,
    },
    /// A value whose sender key is not one pinned for the alias
    /// of its KV key: another machine's key, or the machine got a new key
    /// the viewer was not told about. Rejected without being opened.
    #[error("the sender key of {alias:?} is not pinned on this viewer (unknown sender key)")]
    UnknownSender {
        /// The alias of the KV key.
        alias: String,
    },
    /// A value whose sender key is not pinned for the alias of
    /// its KV key, on a viewer that asks before it pins (see
    /// [`crate::opener`]): a new machine, or a machine with a new key. It
    /// opened with the key it claims, so the sender holds that key's private
    /// half, but it is not shown until the key is accepted.
    #[error("the sender key {} of {alias:?} awaits the viewer's consent", .key.kid())]
    NewSender {
        /// The alias of the KV key.
        alias: String,
        /// The sender key, as the envelope carries it.
        key: PublicKey,
        /// Whether the alias has pinned keys already (a new key of a known
        /// machine) or none (a new machine).
        known: bool,
    },
    /// A value whose sender key the viewer's user rejected for
    /// the alias of its KV key (`keys/publishers/<alias>.rejected`). Dropped
    /// without being opened.
    #[error("the sender key of {alias:?} was rejected on this viewer")]
    RejectedSender {
        /// The alias of the KV key.
        alias: String,
    },
    /// The pinned keys of the KV key's alias could not be read, so nothing
    /// from that alias is accepted until the file is fixed.
    #[error("pinned keys of {alias:?} unusable: {reason}")]
    Pins {
        /// The alias of the KV key.
        alias: String,
        /// Why (paths and line numbers only).
        reason: String,
    },
    /// Authentication failed: the value was tampered with, was sealed for a
    /// different KV key, or was sealed with a different key under the same id.
    /// The AEAD cannot tell these cases apart, by design.
    #[error("envelope failed authentication (tampered, wrong KV key, or wrong key)")]
    Decrypt,
    /// The value decrypted but its plaintext is not the expected payload type.
    #[error("decrypted payload does not match the expected type: {0}")]
    Payload(String),
    /// The payload could not be serialized.
    #[error("cannot serialize payload: {0}")]
    Serialize(String),
    /// Encryption failed (not expected with valid inputs).
    #[error("encryption failed")]
    Encrypt,
    /// The operating system's random number generator failed.
    #[error("OS random number generator failed: {0}")]
    Rng(getrandom::Error),
}

/// Only the version, read first so unknown versions are reported as such.
#[derive(Deserialize)]
struct VersionProbe {
    v: u64,
}

/// The version `v` of a sealed value, read without opening it; `None` when
/// it is not an envelope of at most [`MAX_ENVELOPE_BYTES`].
pub fn envelope_version(bytes: &[u8]) -> Option<u64> {
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return None;
    }
    serde_json::from_slice::<VersionProbe>(bytes)
        .ok()
        .map(|p| p.v)
}

/// The wire form of an envelope.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEnvelope {
    v: u32,
    rk: String,
    sk: String,
    spk: String,
    e: String,
    c: String,
}

/// An envelope, checked and decoded but not opened.
#[derive(Debug, Clone)]
pub struct Sealed {
    /// Key id of the viewer key it was sealed to.
    pub recipient_kid: String,
    /// The sender's public key and its key id, as the envelope claims them.
    /// Proven only once the value opens with it.
    pub sender: PublicKey,
    encapped: [u8; X25519_KEY_LEN],
    ciphertext: Vec<u8>,
}

/// One HPKE random draw: 32 bytes from the OS RNG, taken up front so a
/// failing RNG is an error rather than the panic of hpke's own `getrandom`
/// path. An X25519 encapsulation asks for exactly one private key's worth
/// of bytes; anything beyond the seed is expanded with SHA-256 in counter
/// mode, which keeps it unpredictable without the seed.
struct SeededRng {
    seed: zeroize::Zeroizing<[u8; 32]>,
    block: u64,
}

impl SeededRng {
    fn from_os() -> Result<Self, EnvelopeError> {
        let mut seed = zeroize::Zeroizing::new([0u8; 32]);
        getrandom::getrandom(seed.as_mut()).map_err(EnvelopeError::Rng)?;
        Ok(Self { seed, block: 0 })
    }
}

impl rand_core::TryRng for SeededRng {
    type Error = core::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0u8; 4];
        self.try_fill_bytes(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0u8; 8];
        self.try_fill_bytes(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        use sha2::{Digest, Sha256};
        for chunk in dst.chunks_mut(32) {
            let block = if self.block == 0 {
                *self.seed
            } else {
                let mut h = Sha256::new();
                h.update(b"cc-envelope rng");
                h.update(self.seed.as_ref());
                h.update(self.block.to_le_bytes());
                h.finalize().into()
            };
            let block = zeroize::Zeroizing::new(block);
            chunk.copy_from_slice(&block[..chunk.len()]);
            self.block += 1;
        }
        Ok(())
    }
}

impl rand_core::TryCryptoRng for SeededRng {}

/// Serializes `value` to JSON and seals it as an envelope for
/// storage under `kv_key`: from `sender` (this machine's private key) to
/// `recipient` (the viewer's public key), with a fresh ephemeral key.
///
/// Fails with [`EnvelopeError::TooLarge`] if the result would exceed
/// [`MAX_ENVELOPE_BYTES`].
pub fn seal_value<T: Serialize + ?Sized>(
    value: &T,
    kv_key: &str,
    sender: &SecretKey,
    recipient: &PublicKey,
) -> Result<Vec<u8>, EnvelopeError> {
    use hpke::Serializable as _;
    let plaintext = zeroize::Zeroizing::new(
        serde_json::to_vec(value).map_err(|e| EnvelopeError::Serialize(e.to_string()))?,
    );
    let pk_recipient = recipient.to_hpke().map_err(|_| EnvelopeError::Encrypt)?;
    let me = sender.public_key();
    let pk_sender = me.to_hpke().map_err(|_| EnvelopeError::Encrypt)?;
    let mode = hpke::OpModeS::<Kem>::Auth((sender.to_hpke(), pk_sender));
    let mut rng = SeededRng::from_os()?;
    let (encapped, mut ctx) = hpke::setup_sender_with_rng::<HpkeAead, HpkeKdf, Kem>(
        &mode,
        &pk_recipient,
        HPKE_INFO,
        &mut rng,
    )
    .map_err(|_| EnvelopeError::Encrypt)?;
    let ciphertext = ctx
        .seal(&plaintext, kv_key.as_bytes())
        .map_err(|_| EnvelopeError::Encrypt)?;
    let envelope = WireEnvelope {
        v: ENVELOPE_VERSION,
        rk: recipient.kid().to_string(),
        sk: me.kid().to_string(),
        spk: STANDARD.encode(me.bytes()),
        e: STANDARD.encode(encapped.to_bytes()),
        c: STANDARD.encode(ciphertext),
    };
    let out = serde_json::to_vec(&envelope).map_err(|e| EnvelopeError::Serialize(e.to_string()))?;
    if out.len() > MAX_ENVELOPE_BYTES {
        return Err(EnvelopeError::TooLarge { len: out.len() });
    }
    Ok(out)
}

/// Checks and decodes an envelope without opening it. Rejects, in
/// this order: input over [`MAX_ENVELOPE_BYTES`], anything that is not an
/// envelope, a version other than [`ENVELOPE_VERSION`], missing or extra
/// fields, malformed key ids, bad base64, and key fields that are not 32
/// bytes.
pub fn parse_envelope(bytes: &[u8]) -> Result<Sealed, EnvelopeError> {
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(EnvelopeError::TooLarge { len: bytes.len() });
    }
    let probe: VersionProbe =
        serde_json::from_slice(bytes).map_err(|e| EnvelopeError::Malformed(e.to_string()))?;
    if probe.v != u64::from(ENVELOPE_VERSION) {
        return Err(EnvelopeError::UnsupportedVersion(probe.v));
    }
    let env: WireEnvelope =
        serde_json::from_slice(bytes).map_err(|e| EnvelopeError::Malformed(e.to_string()))?;
    if validate_kid(&env.rk).is_err() || validate_kid(&env.sk).is_err() {
        return Err(EnvelopeError::Malformed("invalid key id".into()));
    }
    let key = |field: &'static str, b64: &str| -> Result<[u8; X25519_KEY_LEN], EnvelopeError> {
        let raw = STANDARD
            .decode(b64)
            .map_err(|_| EnvelopeError::BadBase64 { field })?;
        raw.as_slice()
            .try_into()
            .map_err(|_| EnvelopeError::BadKeyLength {
                field,
                len: raw.len(),
            })
    };
    let spk = key("spk", &env.spk)?;
    let encapped = key("e", &env.e)?;
    let ciphertext = STANDARD
        .decode(&env.c)
        .map_err(|_| EnvelopeError::BadBase64 { field: "c" })?;
    let sender = PublicKey::new(&env.sk, spk)
        .map_err(|_| EnvelopeError::Malformed("invalid key id".into()))?;
    Ok(Sealed {
        recipient_kid: env.rk,
        sender,
        encapped,
        ciphertext,
    })
}

impl Sealed {
    /// Opens the value with the viewer key `recipient` (the one named by
    /// `recipient_kid`), as sealed by the holder of the private key of
    /// `sender`, for `kv_key`, and deserializes its JSON payload. Any
    /// mismatch (another sender key, another recipient, another KV key,
    /// tampering) fails with [`EnvelopeError::Decrypt`].
    pub fn open<T: DeserializeOwned>(
        &self,
        kv_key: &str,
        recipient: &SecretKey,
        sender: &PublicKey,
    ) -> Result<T, EnvelopeError> {
        use hpke::Deserializable as _;
        let pk_sender = sender.to_hpke().map_err(|_| EnvelopeError::Decrypt)?;
        let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(&self.encapped)
            .map_err(|_| EnvelopeError::Decrypt)?;
        let mode = hpke::OpModeR::<Kem>::Auth(pk_sender);
        let mut ctx = hpke::setup_receiver::<HpkeAead, HpkeKdf, Kem>(
            &mode,
            &recipient.to_hpke(),
            &encapped,
            HPKE_INFO,
        )
        .map_err(|_| EnvelopeError::Decrypt)?;
        let plaintext = zeroize::Zeroizing::new(
            ctx.open(&self.ciphertext, kv_key.as_bytes())
                .map_err(|_| EnvelopeError::Decrypt)?,
        );
        serde_json::from_slice(&plaintext).map_err(|e| EnvelopeError::Payload(e.to_string()))
    }
}

/// Opens a value read from `kv_key` with the viewer's private
/// keys, accepting it only from one of `senders` (the public keys allowed to
/// write that key; see [`crate::opener::ViewerKeys`] for the viewer's
/// pinning rules). The sender key is matched by its bytes.
pub fn open_value<T: DeserializeOwned>(
    bytes: &[u8],
    kv_key: &str,
    recipients: &SecretKeys,
    senders: &PublicKeys,
) -> Result<T, EnvelopeError> {
    let sealed = parse_envelope(bytes)?;
    let recipient = recipients
        .get(&sealed.recipient_kid)
        .ok_or_else(|| EnvelopeError::UnknownKid(sealed.recipient_kid.clone()))?;
    let alias = kv_key.split('.').next().unwrap_or_default().to_string();
    let sender = senders
        .find(sealed.sender.bytes())
        .ok_or(EnvelopeError::UnknownSender { alias })?;
    sealed.open(kv_key, recipient, sender)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{host_key, session_key, HostPayload};
    use cc_core::session::{RemotePayload, SessionState};

    fn payload() -> RemotePayload {
        RemotePayload {
            machine: "lab-01".into(),
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
            model: Some("claude-opus-5-5".into()),
            ctx: Some(42),
            hb: 1_791_856_060,
        }
    }

    fn key_s() -> String {
        session_key("lab-01", "3f9a1c2e")
    }

    fn envelope_json(bytes: &[u8]) -> serde_json::Value {
        serde_json::from_slice(bytes).unwrap()
    }

    fn reencode(v: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(v).unwrap()
    }

    struct Pair {
        viewer: SecretKeys,
        publisher: SecretKeys,
    }

    fn pair() -> Pair {
        Pair {
            viewer: SecretKeys::generate("k1").unwrap(),
            publisher: SecretKeys::generate("p1").unwrap(),
        }
    }

    impl Pair {
        fn seal(&self, kv_key: &str) -> Vec<u8> {
            seal_value(
                &payload(),
                kv_key,
                self.publisher.current(),
                &self.viewer.current().public_key(),
            )
            .unwrap()
        }

        fn open(&self, bytes: &[u8], kv_key: &str) -> Result<RemotePayload, EnvelopeError> {
            open_value(bytes, kv_key, &self.viewer, &self.publisher.public_keys())
        }
    }

    #[test]
    fn round_trip_and_fields() {
        let p = pair();
        let sealed = p.seal(&key_s());
        assert_eq!(p.open(&sealed, &key_s()).unwrap(), payload());
        let v = envelope_json(&sealed);
        let mut fields: Vec<_> = v.as_object().unwrap().keys().map(String::as_str).collect();
        fields.sort();
        assert_eq!(fields, ["c", "e", "rk", "sk", "spk", "v"]);
        assert_eq!(v["v"], 1);
        assert_eq!(v["rk"], "k1");
        assert_eq!(v["sk"], "p1");
        assert_eq!(
            STANDARD.decode(v["spk"].as_str().unwrap()).unwrap(),
            p.publisher.current().public_key().bytes()
        );
        assert_eq!(STANDARD.decode(v["e"].as_str().unwrap()).unwrap().len(), 32);
        assert_eq!(envelope_version(&sealed), Some(1));
        let s = String::from_utf8(sealed).unwrap();
        for clear in ["lab-01", "3f9a1c2e", "working", "Bash", "claude", "machine"] {
            assert!(!s.contains(clear), "{clear} visible in {s}");
        }
    }

    #[test]
    fn only_the_recipient_key_opens() {
        let p = pair();
        let sealed = p.seal(&key_s());
        let senders = p.publisher.public_keys();
        // Another viewer, another key id.
        let elsewhere = SecretKeys::generate("k2").unwrap();
        assert!(matches!(
            open_value::<RemotePayload>(&sealed, &key_s(), &elsewhere, &senders),
            Err(EnvelopeError::UnknownKid(k)) if k == "k1"
        ));
        // Another viewer key under the same key id.
        let impostor = SecretKeys::generate("k1").unwrap();
        assert!(matches!(
            open_value::<RemotePayload>(&sealed, &key_s(), &impostor, &senders),
            Err(EnvelopeError::Decrypt)
        ));
        // A publisher cannot open what it or another publisher sealed.
        assert!(matches!(
            open_value::<RemotePayload>(&sealed, &key_s(), &p.publisher, &senders),
            Err(EnvelopeError::UnknownKid(_))
        ));
    }

    #[test]
    fn sender_must_be_allowed_and_must_match() {
        let p = pair();
        let sealed = p.seal(&key_s());
        let other = SecretKeys::generate("p1").unwrap();
        assert!(matches!(
            open_value::<RemotePayload>(&sealed, &key_s(), &p.viewer, &other.public_keys()),
            Err(EnvelopeError::UnknownSender { alias }) if alias == "lab-01"
        ));
        // Swapping in another (allowed) sender key breaks authentication.
        let mut v = envelope_json(&sealed);
        v["spk"] = STANDARD.encode(other.current().public_key().bytes()).into();
        let mut allowed = p.publisher.public_keys();
        allowed
            .prepend(PublicKey::new("p2", *other.current().public_key().bytes()).unwrap())
            .unwrap();
        assert!(matches!(
            open_value::<RemotePayload>(&reencode(&v), &key_s(), &p.viewer, &allowed),
            Err(EnvelopeError::Decrypt)
        ));
    }

    #[test]
    fn tampering_fails() {
        let p = pair();
        let sealed = p.seal(&key_s());
        let good = envelope_json(&sealed);
        for field in ["c", "e"] {
            let mut raw = STANDARD.decode(good[field].as_str().unwrap()).unwrap();
            for i in [0, raw.len() / 2, raw.len() - 1] {
                raw[i] ^= 0x01;
                let mut v = good.clone();
                v[field] = STANDARD.encode(&raw).into();
                assert!(
                    matches!(p.open(&reencode(&v), &key_s()), Err(EnvelopeError::Decrypt)),
                    "{field}[{i}]"
                );
                raw[i] ^= 0x01;
            }
        }
        let c = STANDARD.decode(good["c"].as_str().unwrap()).unwrap();
        for len in [c.len() - 1, TAG_LEN - 1, 0] {
            let mut v = good.clone();
            v["c"] = STANDARD.encode(&c[..len]).into();
            assert!(matches!(
                p.open(&reencode(&v), &key_s()),
                Err(EnvelopeError::Decrypt)
            ));
        }
        // The key ids are not secret, but a changed rk names another key.
        let mut v = good.clone();
        v["rk"] = "v9".into();
        assert!(matches!(
            p.open(&reencode(&v), &key_s()),
            Err(EnvelopeError::UnknownKid(_))
        ));
    }

    #[test]
    fn value_moved_to_another_kv_key_fails() {
        let p = pair();
        let sealed = p.seal(&key_s());
        for other in [
            session_key("lab-02", "3f9a1c2e"),
            session_key("lab-01", "00000000"),
            host_key("lab-01"),
        ] {
            let opened =
                open_value::<RemotePayload>(&sealed, &other, &p.viewer, &p.publisher.public_keys());
            assert!(matches!(opened, Err(EnvelopeError::Decrypt)), "{other}");
        }
    }

    #[test]
    fn every_seal_uses_a_fresh_ephemeral_key() {
        let p = pair();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            let v = envelope_json(&p.seal(&key_s()));
            assert!(seen.insert(v["e"].as_str().unwrap().to_string()));
        }
    }

    #[test]
    fn malformed_envelopes_are_rejected() {
        let p = pair();
        let good = envelope_json(&p.seal(&key_s()));
        let mut extra = good.clone();
        extra["kid"] = "k1".into();
        assert!(matches!(
            p.open(&reencode(&extra), &key_s()),
            Err(EnvelopeError::Malformed(_))
        ));
        for field in ["rk", "sk", "spk", "e", "c"] {
            let mut missing = good.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                matches!(
                    p.open(&reencode(&missing), &key_s()),
                    Err(EnvelopeError::Malformed(_))
                ),
                "{field}"
            );
        }
        for field in ["rk", "sk"] {
            let mut v = good.clone();
            v[field] = "bad id".into();
            assert!(matches!(
                p.open(&reencode(&v), &key_s()),
                Err(EnvelopeError::Malformed(_))
            ));
        }
        for field in ["spk", "e", "c"] {
            let mut v = good.clone();
            v[field] = "not*base64".into();
            assert!(matches!(
                p.open(&reencode(&v), &key_s()),
                Err(EnvelopeError::BadBase64 { field: f }) if f == field
            ));
        }
        for field in ["spk", "e"] {
            let mut v = good.clone();
            v[field] = STANDARD.encode([1u8; 31]).into();
            assert!(matches!(
                p.open(&reencode(&v), &key_s()),
                Err(EnvelopeError::BadKeyLength { field: f, len: 31 }) if f == field
            ));
        }
        for version in [0, 2, 3, 99] {
            let mut v = good.clone();
            v["v"] = version.into();
            assert!(matches!(
                p.open(&reencode(&v), &key_s()),
                Err(EnvelopeError::UnsupportedVersion(x)) if x == version
            ));
        }
        // Another version in a shape of its own: still only its version.
        let other = br#"{"v":7,"x":"y"}"#;
        assert!(matches!(
            p.open(other, &key_s()),
            Err(EnvelopeError::UnsupportedVersion(7))
        ));
        assert_eq!(envelope_version(other), Some(7));
        for junk in [&b""[..], b"null", b"[]", b"{\"v\":\"1\"}", b"\xff\xfe"] {
            assert!(matches!(
                p.open(junk, &key_s()),
                Err(EnvelopeError::Malformed(_))
            ));
        }
        let mut padded = reencode(&good);
        padded.resize(MAX_ENVELOPE_BYTES + 1, b' ');
        assert!(matches!(
            p.open(&padded, &key_s()),
            Err(EnvelopeError::TooLarge { .. })
        ));
        assert_eq!(envelope_version(&padded), None);
    }

    #[test]
    fn host_payload_round_trip() {
        let p = pair();
        let host = HostPayload {
            machine: "lab-01".into(),
            version: "0.1.0".into(),
            os: "linux".into(),
            hb: 1_791_856_060,
        };
        let key = host_key("lab-01");
        let to = p.viewer.current().public_key();
        let sealed = seal_value(&host, &key, p.publisher.current(), &to).unwrap();
        let back: HostPayload =
            open_value(&sealed, &key, &p.viewer, &p.publisher.public_keys()).unwrap();
        assert_eq!(back, host);
        // A host record does not open as a session payload, even under its own key.
        assert!(matches!(
            p.open(&sealed, &key),
            Err(EnvelopeError::Payload(_))
        ));
    }

    #[test]
    fn maximal_payload_fits_the_limit() {
        use cc_core::session::{
            remote_text, RemoteAgent, MAX_REMOTE_AGENTS, MAX_REMOTE_AGENT_ROWS, MAX_REMOTE_DIR,
            MAX_REMOTE_MODEL, MAX_REMOTE_PAYLOAD_BYTES, MAX_REMOTE_TEXT, MAX_REMOTE_TITLE,
            MAX_REMOTE_TOOL,
        };
        let kid = |c: &str| c.repeat(crate::keyfile::MAX_KID_LEN);
        let viewer = SecretKeys::generate(&kid("v")).unwrap();
        let publisher = SecretKeys::generate(&kid("p")).unwrap();
        let machine = "m".repeat(crate::kv::MAX_ALIAS_LEN);
        let key = session_key(&machine, "ffffffff");
        let to = viewer.current().public_key();
        let long = "x".repeat(200);
        // A payload as the publisher builds it, every text past its cap,
        // then as many of six subagent lines at their caps as fit.
        let build = |text: &str, tool_and_model: &str, agent_text: &str| {
            let mut p = RemotePayload {
                machine: machine.clone(),
                sid: "ffffffff".into(),
                alias: None,
                title: remote_text(text, MAX_REMOTE_TITLE),
                dir: remote_text(text, MAX_REMOTE_DIR),
                text: remote_text(text, MAX_REMOTE_TEXT),
                agents: Some([MAX_REMOTE_AGENTS; 2]),
                ags: None,
                state: SessionState::NeedsYou,
                since: i64::MAX,
                tool: remote_text(tool_and_model, MAX_REMOTE_TOOL),
                model: remote_text(tool_and_model, MAX_REMOTE_MODEL),
                ctx: Some(100),
                hb: i64::MAX,
            };
            p.fit_agents((0..MAX_REMOTE_AGENT_ROWS).map(|_| {
                let t = Some(agent_text);
                RemoteAgent::new(t, t, t, t, false)
            }));
            p
        };
        let seal = |p: &RemotePayload| {
            let sealed = seal_value(p, &key, publisher.current(), &to).unwrap();
            println!(
                "RemotePayload with {} subagent lines: {} bytes of JSON, {} bytes sealed \
                 (limit {MAX_ENVELOPE_BYTES})",
                p.ags.as_ref().map_or(0, Vec::len),
                p.json_len(),
                sealed.len()
            );
            assert!(p.json_len() <= MAX_REMOTE_PAYLOAD_BYTES);
            let back: RemotePayload =
                open_value(&sealed, &key, &viewer, &publisher.public_keys()).unwrap();
            assert_eq!(&back, p);
            sealed.len()
        };

        // Six full subagent lines travel next to texts at their caps.
        let ascii = build(&long, &long, &long);
        assert_eq!(
            ascii.ags.as_ref().map(Vec::len),
            Some(MAX_REMOTE_AGENT_ROWS)
        );
        seal(&ascii);

        // Four-byte characters everywhere: fewer lines travel, and the
        // sealed value still fits.
        let emoji = "😀".repeat(200);
        let worst = build(&emoji, &emoji, &long);
        assert!(worst.ags.as_ref().map_or(0, Vec::len) < MAX_REMOTE_AGENT_ROWS);
        assert!(seal(&worst) <= MAX_ENVELOPE_BYTES);

        // The budget itself, met exactly, still seals with room to spare:
        // whatever the fields hold, a payload `fit_agents` built is never
        // refused.
        let mut exact = worst.clone();
        exact.ags = None;
        exact.alias = Some(String::new());
        let fill = MAX_REMOTE_PAYLOAD_BYTES - exact.json_len();
        exact.alias = Some("a".repeat(fill));
        assert_eq!(exact.json_len(), MAX_REMOTE_PAYLOAD_BYTES);
        let size = seal(&exact);
        assert!(
            size <= MAX_ENVELOPE_BYTES - 128,
            "{size} bytes sealed leave too little room under {MAX_ENVELOPE_BYTES}"
        );
    }

    #[test]
    fn seal_refuses_values_over_the_limit() {
        let p = pair();
        let to = p.viewer.current().public_key();
        let fits = "x".repeat(2_800);
        let sealed = seal_value(&fits, &key_s(), p.publisher.current(), &to).unwrap();
        assert!(sealed.len() <= MAX_ENVELOPE_BYTES);
        let back: String =
            open_value(&sealed, &key_s(), &p.viewer, &p.publisher.public_keys()).unwrap();
        assert_eq!(back, fits);
        let too_big = "x".repeat(3_100);
        assert!(matches!(
            seal_value(&too_big, &key_s(), p.publisher.current(), &to),
            Err(EnvelopeError::TooLarge { .. })
        ));
    }

    #[test]
    fn rng_expands_past_its_seed() {
        use rand_core::TryRng as _;
        let mut rng = SeededRng::from_os().unwrap();
        let mut a = [0u8; 80];
        rng.try_fill_bytes(&mut a).unwrap();
        assert_eq!(&a[..32], &rng.seed[..], "the first block is the OS seed");
        assert_ne!(&a[32..64], &a[..32]);
        assert_ne!(a[64..], [0u8; 16]);
        assert_ne!(rng.try_next_u64().unwrap(), rng.try_next_u64().unwrap());
    }
}
