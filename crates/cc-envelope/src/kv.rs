//! KV key layout of the `cc-sessions` bucket, and machine aliases.
//!
//! ```text
//! <alias>.s.<sid>   one session of a publisher machine (RemotePayload)
//! <alias>.host      that machine's presence and heartbeat (HostPayload)
//! <alias>.p.<pid>   one of its projects' token usage (ProjectPayload)
//! <alias>.activity  its tokens per day, last 35 days (ActivityPayload)
//! ```
//!
//! The alias is the first token so each publisher's NATS permissions can be
//! scoped to `<alias>.>`. Neither the session id nor the project appears in
//! clear: `sid` and `pid` are the first 8 hex characters of a SHA-256 (of
//! the session id, of the project's transcript folder).

use sha2::{Digest, Sha256};

/// Longest accepted machine alias.
pub const MAX_ALIAS_LEN: usize = 31;

/// Length of a [`sid_for`] value in hex characters.
pub const SID_LEN: usize = 8;

/// Why a machine alias was rejected by [`validate_alias`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AliasError {
    /// The alias is empty.
    #[error("alias is empty")]
    Empty,
    /// The alias is longer than [`MAX_ALIAS_LEN`] characters.
    #[error("alias is longer than {MAX_ALIAS_LEN} characters")]
    TooLong,
    /// The alias does not start with a lowercase letter or a digit.
    #[error("alias must start with a lowercase letter or a digit")]
    BadStart,
    /// The alias contains something other than lowercase ASCII letters,
    /// digits and dashes (dots included: they separate KV key tokens).
    #[error("alias may only contain lowercase ASCII letters, digits and '-'")]
    BadChar,
}

/// Checks a machine alias: 1 to 31 lowercase ASCII letters, digits or dashes,
/// starting with a letter or digit. No dots, since the alias is a single
/// token of the KV key and of the NATS user name `cc-sessions.pub-<alias>`.
pub fn validate_alias(alias: &str) -> Result<(), AliasError> {
    let bytes = alias.as_bytes();
    let Some(&first) = bytes.first() else {
        return Err(AliasError::Empty);
    };
    if bytes.len() > MAX_ALIAS_LEN {
        return Err(AliasError::TooLong);
    }
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(AliasError::BadStart);
    }
    if !bytes
        .iter()
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(AliasError::BadChar);
    }
    Ok(())
}

/// Turns any name (a Windows computer name, a typed label) into something
/// [`validate_alias`] accepts: lowercase, every character that is not an
/// ASCII letter, digit or dash replaced by a dash, at most
/// [`MAX_ALIAS_LEN`] characters, and no dash at either end. `None` when
/// nothing usable is left (an empty or all-symbol name).
///
/// `OFFICE-PC-07` becomes `office-pc-07`, `Lab_01.local` `lab-01-local`.
pub fn normalize_alias(raw: &str) -> Option<String> {
    let mapped: String = raw
        .trim()
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-');
    // Every character is ASCII now, so slicing by bytes is slicing by chars.
    let cut = &trimmed[..trimmed.len().min(MAX_ALIAS_LEN)];
    let alias = cut.trim_end_matches('-');
    (!alias.is_empty()).then(|| alias.to_string())
}

/// KV key of one session: `<alias>.s.<sid>`.
pub fn session_key(alias: &str, sid: &str) -> String {
    format!("{alias}.s.{sid}")
}

/// KV key of one project's usage: `<alias>.p.<pid>`, see [`pid_for`].
pub fn project_key(alias: &str, pid: &str) -> String {
    format!("{alias}.p.{pid}")
}

/// The short, non-reversible project id: the first 8 lowercase hex
/// characters of SHA-256 of the project's transcript folder name (Claude
/// Code's encoding of its path). Same shape as [`sid_for`], checked by
/// [`is_valid_sid`].
pub fn pid_for(project_folder: &str) -> String {
    sid_for(&format!("project:{project_folder}"))
}

/// KV key of a machine's activity: `<alias>.activity`.
pub fn activity_key(alias: &str) -> String {
    format!("{alias}.activity")
}

/// KV key of a machine's presence record: `<alias>.host`.
pub fn host_key(alias: &str) -> String {
    format!("{alias}.host")
}

/// The short, non-reversible session id published instead of the real one:
/// the first 8 lowercase hex characters of SHA-256 of `session_id`.
pub fn sid_for(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    digest[..SID_LEN / 2]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// True for a well-formed `sid`: exactly 8 lowercase hex characters.
pub fn is_valid_sid(sid: &str) -> bool {
    sid.len() == SID_LEN
        && sid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A parsed KV key of the bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KvKey {
    /// `<alias>.s.<sid>`
    Session {
        /// Publisher machine alias.
        alias: String,
        /// Hashed session id, see [`sid_for`].
        sid: String,
    },
    /// `<alias>.host`
    Host {
        /// Publisher machine alias.
        alias: String,
    },
    /// `<alias>.activity`
    Activity {
        /// Publisher machine alias.
        alias: String,
    },
    /// `<alias>.p.<pid>`
    Project {
        /// Publisher machine alias.
        alias: String,
        /// Hashed project id, see [`pid_for`].
        pid: String,
    },
}

impl KvKey {
    /// Parses a KV key, returning `None` for anything that is not a valid
    /// session or host key (invalid alias, malformed sid, unknown shape).
    pub fn parse(key: &str) -> Option<KvKey> {
        let mut parts = key.split('.');
        let alias = parts.next()?;
        validate_alias(alias).ok()?;
        match (parts.next(), parts.next(), parts.next()) {
            (Some("host"), None, None) => Some(KvKey::Host {
                alias: alias.to_string(),
            }),
            (Some("activity"), None, None) => Some(KvKey::Activity {
                alias: alias.to_string(),
            }),
            (Some("s"), Some(sid), None) if is_valid_sid(sid) => Some(KvKey::Session {
                alias: alias.to_string(),
                sid: sid.to_string(),
            }),
            (Some("p"), Some(pid), None) if is_valid_sid(pid) => Some(KvKey::Project {
                alias: alias.to_string(),
                pid: pid.to_string(),
            }),
            _ => None,
        }
    }

    /// The machine alias this key belongs to.
    pub fn alias(&self) -> &str {
        match self {
            KvKey::Session { alias, .. }
            | KvKey::Host { alias }
            | KvKey::Activity { alias }
            | KvKey::Project { alias, .. } => alias,
        }
    }

    /// The KV key string, the inverse of [`KvKey::parse`].
    pub fn to_key_string(&self) -> String {
        match self {
            KvKey::Session { alias, sid } => session_key(alias, sid),
            KvKey::Host { alias } => host_key(alias),
            KvKey::Project { alias, pid } => project_key(alias, pid),
            KvKey::Activity { alias } => activity_key(alias),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_follow_the_rules() {
        for ok in ["a", "lab-01", "0box", "a-", &"x".repeat(31)] {
            assert_eq!(validate_alias(ok), Ok(()), "{ok}");
        }
        assert_eq!(validate_alias(""), Err(AliasError::Empty));
        assert_eq!(validate_alias(&"x".repeat(32)), Err(AliasError::TooLong));
        assert_eq!(validate_alias("-lab"), Err(AliasError::BadStart));
        assert_eq!(validate_alias("Lab"), Err(AliasError::BadStart));
        for bad in ["lab.01", "lab_01", "lAb", "lab 01", "lab*", "lab>", "labé"] {
            assert_eq!(validate_alias(bad), Err(AliasError::BadChar), "{bad}");
        }
    }

    #[test]
    fn names_normalize_into_valid_aliases() {
        let cases = [
            ("OFFICE-PC-07", Some("office-pc-07")),
            ("Lab_01.local", Some("lab-01-local")),
            ("  devbox  ", Some("devbox")),
            ("--Box--", Some("box")),
            ("Équipe 7", Some("quipe-7")),
            ("my pc", Some("my-pc")),
            ("", None),
            ("___", None),
            ("é", None),
        ];
        for (raw, want) in cases {
            assert_eq!(normalize_alias(raw).as_deref(), want, "{raw:?}");
        }
        // Long names are cut to the limit, without a dash left at the end.
        let long = normalize_alias(&format!("{}-tail", "a".repeat(30))).unwrap();
        assert_eq!(long, "a".repeat(30));
        let long = normalize_alias(&"B".repeat(40)).unwrap();
        assert_eq!(long.len(), MAX_ALIAS_LEN);
        // Whatever comes out is accepted by the validator.
        for raw in ["OFFICE-PC-07", "Lab_01.local", "x.y.z", &"Q".repeat(50)] {
            let alias = normalize_alias(raw).unwrap();
            assert_eq!(validate_alias(&alias), Ok(()), "{alias}");
        }
    }

    #[test]
    fn keys_have_the_documented_shape() {
        assert_eq!(session_key("lab-01", "3f9a1c2e"), "lab-01.s.3f9a1c2e");
        assert_eq!(host_key("lab-01"), "lab-01.host");
    }

    #[test]
    fn sid_is_the_sha256_prefix() {
        // SHA-256("abc") = ba7816bf 8f01cfea ...
        assert_eq!(sid_for("abc"), "ba7816bf");
        let sid = sid_for("0f8e7c1a-1234-4bcd-9abc-1234567890ab");
        assert!(is_valid_sid(&sid));
        assert_eq!(sid, sid_for("0f8e7c1a-1234-4bcd-9abc-1234567890ab"));
        assert!(!is_valid_sid("BA7816BF"));
        assert!(!is_valid_sid("ba7816b"));
    }

    #[test]
    fn kv_keys_parse_and_round_trip() {
        let s = KvKey::parse("lab-01.s.ba7816bf").unwrap();
        assert_eq!(
            s,
            KvKey::Session {
                alias: "lab-01".into(),
                sid: "ba7816bf".into()
            }
        );
        assert_eq!(s.alias(), "lab-01");
        assert_eq!(s.to_key_string(), "lab-01.s.ba7816bf");
        let h = KvKey::parse("lab-01.host").unwrap();
        assert_eq!(h.to_key_string(), "lab-01.host");
        let pid = pid_for("c--Users-x-Projects-app");
        let p = KvKey::parse(&project_key("lab-01", &pid)).unwrap();
        assert_eq!(
            p,
            KvKey::Project {
                alias: "lab-01".into(),
                pid: pid.clone()
            }
        );
        assert_eq!(p.to_key_string(), format!("lab-01.p.{pid}"));
        assert_ne!(
            pid,
            sid_for("c--Users-x-Projects-app"),
            "not the session hash of the same text"
        );
        let a = KvKey::parse("lab-01.activity").unwrap();
        assert_eq!(
            a,
            KvKey::Activity {
                alias: "lab-01".into()
            }
        );
        assert_eq!(a.to_key_string(), activity_key("lab-01"));
        for bad in [
            "",
            "lab-01",
            "lab-01.s",
            "lab-01.s.zz",
            "lab-01.s.ba7816bf.x",
            "lab-01.host.x",
            "lab-01.p",
            "lab-01.activity.x",
            "lab-01.p.zz",
            "lab-01.p.ba7816bf.x",
            "Lab.host",
            "lab-01.other",
        ] {
            assert_eq!(KvKey::parse(bad), None, "{bad}");
        }
    }
}
