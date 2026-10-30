//! End-to-end encryption for everything publishers put in the `cc-sessions`
//! NATS KV bucket. Synadia only ever stores and relays ciphertext; keys are
//! copied between machines by hand (or in a publisher bundle) and never go
//! through NATS.
//!
//! - [`keypair`]: an X25519 key pair per machine, and the key files under
//!   [`cc_core::paths::keys_dir`].
//! - [`keyfile`]: the key file format they share (`<kid>:<base64>` per line,
//!   newest first) and its errors.
//! - [`envelope`]: sealing with the KV key as associated data, so a value
//!   cannot be replayed under another key: the envelope, HPKE Auth mode
//!   from a publisher's key to the viewer's,
//!   `{"v":1,"rk":..,"sk":..,"spk":..,"e":..,"c":..}` (`v` is the format
//!   version, [`ENVELOPE_VERSION`]). Only the viewer decrypts, and the sender
//!   key is proven. At most 4096 bytes. Any other version is rejected.
//! - [`opener`]: the viewer's [`ViewerKeys`], which open values with each
//!   publisher's key pinned to its alias and refuse any key not pinned yet
//!   until the viewer's user accepts it ([`pin_key`], [`reject_key`]).
//! - [`Sealer`]: what a publisher seals with.
//! - [`kv`]: the bucket's key layout (`<alias>.s.<sid>`, `<alias>.host`,
//!   `<alias>.p.<pid>`, `<alias>.activity`), machine alias validation and
//!   the hashed ids.
//!
//! Typical use:
//!
//! ```
//! use cc_envelope::{session_key, sid_for, OpenKeys, Sealer, SecretKeys, ViewerKeys};
//! # use cc_core::session::{RemotePayload, SessionState};
//! // Normally loaded from keys/viewer.key and keys/publisher.key.
//! let viewer = SecretKeys::generate("k1").unwrap();
//! let publisher = SecretKeys::generate("p1").unwrap();
//! let sealer = Sealer::new(publisher, viewer.public_keys());
//! let sid = sid_for("0f8e7c1a-1234-4bcd-9abc-1234567890ab");
//! let key = session_key("lab-01", &sid);
//! let payload = RemotePayload {
//!     machine: "lab-01".into(), sid, alias: None, title: None, dir: None,
//!     text: None, agents: None, ags: None, state: SessionState::Working,
//!     since: 1_791_856_000, tool: Some("Bash".into()), model: None, ctx: None,
//!     hb: 1_791_856_060,
//! };
//! let bytes = sealer.seal(&payload, &key).unwrap();
//! let keys = ViewerKeys::new(viewer, None);
//! assert_eq!(keys.open_any::<RemotePayload>(&bytes, &key).unwrap().value, payload);
//! ```

#![deny(missing_docs)]

pub mod envelope;
pub mod keyfile;
pub mod keypair;
pub mod kv;
pub mod opener;

use serde::{Deserialize, Serialize};

pub use cc_core::projects::{ActivityPayload, ProjectPayload};
pub use cc_core::session::RemotePayload;
pub use envelope::{
    envelope_version, open_value, parse_envelope, seal_value, EnvelopeError, Sealed,
    ENVELOPE_VERSION, HPKE_INFO, MAX_ENVELOPE_BYTES,
};
pub use keyfile::{validate_kid, KeyFileError};
pub use keypair::{
    ensure_viewer_keys, new_kid, PublicKey, PublicKeys, SecretKey, SecretKeys, ViewerKeyState,
};
pub use opener::{
    pin_file, pin_key, reject_key, rejected_file, OpenKeys, Opened, ViewerKeys,
    MAX_REJECTED_PER_ALIAS,
};

/// What a publisher seals its values with: this machine's private keys (the
/// newest seals) and the viewer's public keys (the newest is sealed to).
#[derive(Debug, Clone)]
pub struct Sealer {
    /// The publisher's own keys, `keys/publisher.key`.
    pub sender: SecretKeys,
    /// The viewer's keys, `keys/viewer.pub`.
    pub recipient: PublicKeys,
}

impl Sealer {
    /// A sealer from this machine's keys to the viewer's.
    pub fn new(sender: SecretKeys, recipient: PublicKeys) -> Self {
        Self { sender, recipient }
    }

    /// Seals `value` for storage under `kv_key`, as an envelope.
    pub fn seal<T: Serialize + ?Sized>(
        &self,
        value: &T,
        kv_key: &str,
    ) -> Result<Vec<u8>, EnvelopeError> {
        seal_value(
            value,
            kv_key,
            self.sender.current(),
            self.recipient.current(),
        )
    }

    /// A one-line description for logs, e.g. `publisher key p…, sealed to
    /// viewer key v…`. Key ids only.
    pub fn describe(&self) -> String {
        format!(
            "publisher key {}, sealed to viewer key {}",
            self.sender.current().kid(),
            self.recipient.current().kid()
        )
    }
}
pub use kv::{
    activity_key, host_key, is_valid_sid, normalize_alias, pid_for, project_key, session_key,
    sid_for, validate_alias, AliasError, KvKey, MAX_ALIAS_LEN,
};

/// What a publisher writes under [`host_key`]: the machine's presence and
/// heartbeat, sealed like a session. Field names are short because every byte
/// is stored in the bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPayload {
    /// Publisher alias, same as the first token of the KV key.
    pub machine: String,
    /// Publisher version, e.g. `0.1.0`.
    pub version: String,
    /// Operating system, e.g. `windows` or `linux` (`std::env::consts::OS`).
    pub os: String,
    /// Unix seconds of this write; the viewer marks the machine offline when stale.
    pub hb: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::projects::{
        DayActivity, Tokens, ACTIVITY_DAYS, MAX_PROJECT_MODELS, MAX_PROJECT_NAME,
    };
    use std::collections::HashMap;

    /// A sealer with the longest key ids, and a viewer that opens what it
    /// seals.
    fn sealer() -> (Sealer, ViewerKeys) {
        let kid = |c: &str| c.repeat(keyfile::MAX_KID_LEN);
        let viewer = SecretKeys::generate(&kid("v")).unwrap();
        let publisher = SecretKeys::generate(&kid("p")).unwrap();
        let sealer = Sealer::new(publisher, viewer.public_keys());
        (sealer, ViewerKeys::new(viewer, None))
    }

    /// The largest project a publisher can send, with the longest name and
    /// every model slot taken by huge counters, still fits a bucket value.
    #[test]
    fn the_largest_project_payload_fits_the_value_limit() {
        let mut by_model = HashMap::new();
        for i in 0..(MAX_PROJECT_MODELS + 5) {
            let n = u64::MAX / 64 - i as u64;
            let t = Tokens {
                input: n,
                output: n,
                cache_creation: n,
                cache_read: n,
            };
            by_model.insert(
                format!("claude-some-very-long-model-name-{i:02}-20261001"),
                t,
            );
        }
        let name = "長".repeat(MAX_PROJECT_NAME);
        let key = project_key(&"a".repeat(MAX_ALIAS_LEN), &pid_for("folder"));
        let p = ProjectPayload::new(
            &"a".repeat(MAX_ALIAS_LEN),
            &name,
            u32::MAX,
            &by_model,
            Some(i64::MAX),
            i64::MAX,
        );
        let (sealer, viewer) = sealer();
        let bytes = sealer.seal(&p, &key).expect("fits");
        println!(
            "largest project, {}: {} bytes",
            sealer.describe(),
            bytes.len()
        );
        assert!(bytes.len() <= MAX_ENVELOPE_BYTES, "{} bytes", bytes.len());
        let opened: Opened<ProjectPayload> = viewer.open_any(&bytes, &key).unwrap();
        assert_eq!(opened.value, p);
    }

    /// A full activity value, every day with huge counters, fits too.
    #[test]
    fn the_largest_activity_payload_fits_the_value_limit() {
        let alias = "a".repeat(MAX_ALIAS_LEN);
        let days = (0..ACTIVITY_DAYS)
            .map(|i| DayActivity {
                day: format!("2026-10-{:02}", i % 31 + 1),
                tokens: u64::MAX / 2,
                peak_hour: 23,
            })
            .collect();
        let p = ActivityPayload {
            machine: alias.clone(),
            days,
            hb: i64::MAX,
        };
        let key = activity_key(&alias);
        let (sealer, viewer) = sealer();
        let bytes = sealer.seal(&p, &key).expect("fits");
        println!(
            "largest activity, {}: {} bytes",
            sealer.describe(),
            bytes.len()
        );
        assert!(bytes.len() <= MAX_ENVELOPE_BYTES, "{} bytes", bytes.len());
        let opened: Opened<ActivityPayload> = viewer.open_any(&bytes, &key).unwrap();
        assert_eq!(opened.value, p);
    }

    #[test]
    fn the_sealer_names_its_key_ids_only() {
        let (sealer, _) = sealer();
        let d = sealer.describe();
        assert!(
            d.starts_with("publisher key p") && d.contains("viewer key v"),
            "{d}"
        );
        assert!(!d.contains(&sealer.sender.to_file_string()[33..50]));
    }
}
