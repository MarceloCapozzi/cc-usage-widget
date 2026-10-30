//! `cc-publish keygen` and `cc-publish viewer-keygen`: the key files.
//!
//! - `keygen` creates this publisher's key pair for the envelope,
//!   `keys/publisher.key` (private, never leaves the machine), and prints
//!   its public key line, which the viewer is asked to accept. The key is
//!   the machine's identity: replacing an existing one (`--rotate`,
//!   `--force`) needs the alias typed back ([`check_confirmation`]).
//! - `viewer-keygen`, on the viewer machine, creates `keys/viewer.key` and
//!   `keys/viewer.pub`; every publisher gets a copy of `viewer.pub`. The
//!   cc-usage app also creates them on its own at startup.
//!
//! Only key ids, paths and public keys are ever printed.

use std::path::{Path, PathBuf};

use cc_envelope::{ensure_viewer_keys, KeyFileError, PublicKey, SecretKeys};

/// What to do when the key file is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IfExists {
    /// Fail with [`KeygenError::Exists`] (the default).
    #[default]
    Refuse,
    /// Keep it and report its newest key (`--if-missing`).
    Keep,
    /// Replace it with a single fresh key (`--force`).
    Replace,
    /// Add a fresh key as the newest and keep the old ones (`--rotate`).
    Rotate,
}

/// Why no key was written.
#[derive(Debug, thiserror::Error)]
pub enum KeygenError {
    /// A key file is already there and nothing allowed touching it.
    #[error("{} already exists; refusing to replace it ({why})", .path.display())]
    Exists {
        /// The file.
        path: PathBuf,
        /// What replacing it would break, and how to do it on purpose.
        why: &'static str,
    },
    /// Generating, loading or saving the key failed.
    #[error(transparent)]
    Key(#[from] KeyFileError),
}

/// What a key pair command did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// The newest public key, to print.
    pub public: PublicKey,
    /// False when an existing file was kept as it was.
    pub changed: bool,
}

/// Creates or updates a publisher's private key file at `path` by `mode`.
pub fn publisher_keygen(path: &Path, kid: &str, mode: IfExists) -> Result<Generated, KeygenError> {
    let exists = path.exists();
    let keys = match (exists, mode) {
        (false, _) => {
            let keys = SecretKeys::generate(kid)?;
            keys.save_new(path)?;
            keys
        }
        (true, IfExists::Refuse) => {
            return Err(KeygenError::Exists {
                path: path.to_path_buf(),
                why: "it is this machine's identity, and the viewer holds back a new key's values until it is accepted there; \
                      --if-missing keeps it, --rotate (or --force) --confirm <alias> replaces it on purpose",
            })
        }
        (true, IfExists::Keep) => {
            let keys = SecretKeys::load(path)?;
            return Ok(Generated {
                public: keys.current().public_key(),
                changed: false,
            });
        }
        (true, IfExists::Replace) => {
            let keys = SecretKeys::generate(kid)?;
            keys.save(path)?;
            keys
        }
        (true, IfExists::Rotate) => {
            let mut keys = SecretKeys::load(path)?;
            keys.rotate(kid)?;
            keys.save(path)?;
            keys
        }
    };
    Ok(Generated {
        public: keys.current().public_key(),
        changed: true,
    })
}

/// Whether `mode` would replace the publisher key at `path`: `--rotate` or
/// `--force` on an existing file. Only then is a confirmation asked for; a
/// first key and `--if-missing` need none.
pub fn replaces_identity(path: &Path, mode: IfExists) -> bool {
    matches!(mode, IfExists::Rotate | IfExists::Replace) && path.exists()
}

/// The warning shown before a publisher key is replaced.
pub fn identity_warning(alias: &str) -> String {
    format!(
        "This replaces {alias}'s identity. The viewer will hold back its values until you accept the new key there."
    )
}

/// Why a replacement of the publisher key was not confirmed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfirmError {
    /// Neither `--confirm` nor a terminal to type the alias in.
    #[error("{} Confirm it with --confirm {alias} (or run keygen in a terminal and type the alias); nothing changed", identity_warning(.alias))]
    Missing {
        /// The alias of the config.
        alias: String,
    },
    /// `--confirm` (or the typed answer) is not the alias.
    #[error("{given:?} is not this machine's alias {alias:?}; nothing changed")]
    Mismatch {
        /// The alias of the config.
        alias: String,
        /// What was given or typed.
        given: String,
    },
}

/// Checks the confirmation for replacing `alias`'s publisher key: the
/// `--confirm` value when there is one, else the line `typed` at a prompt
/// (`None` without a terminal). Surrounding whitespace is ignored; the alias
/// must match exactly otherwise.
pub fn check_confirmation(
    alias: &str,
    confirm: Option<&str>,
    typed: impl FnOnce() -> Option<String>,
) -> Result<(), ConfirmError> {
    let given = match confirm {
        Some(c) => c.to_string(),
        None => typed().ok_or_else(|| ConfirmError::Missing {
            alias: alias.to_string(),
        })?,
    };
    if given.trim() == alias {
        Ok(())
    } else {
        Err(ConfirmError::Mismatch {
            alias: alias.to_string(),
            given: given.trim().to_string(),
        })
    }
}

/// Creates or updates the viewer's key files: the private `key_path` by
/// `mode`, and `pub_path` always made to match it.
pub fn viewer_keygen(
    key_path: &Path,
    pub_path: &Path,
    kid: &str,
    mode: IfExists,
) -> Result<Generated, KeygenError> {
    let exists = key_path.exists();
    let keys = match (exists, mode) {
        (false, _) | (true, IfExists::Keep) => {
            let state = ensure_viewer_keys(key_path, pub_path, kid)?;
            return Ok(Generated {
                public: state.keys.current().public_key(),
                changed: state.created || state.wrote_public,
            });
        }
        (true, IfExists::Refuse) => return Err(KeygenError::Exists {
            path: key_path.to_path_buf(),
            why: "every publisher seals to it; use --rotate to add a new key and keep the old ones",
        }),
        (true, IfExists::Replace) => {
            let keys = SecretKeys::generate(kid)?;
            keys.save(key_path)?;
            keys
        }
        (true, IfExists::Rotate) => {
            let mut keys = SecretKeys::load(key_path)?;
            keys.rotate(kid)?;
            keys.save(key_path)?;
            keys
        }
    };
    keys.public_keys().save(pub_path)?;
    Ok(Generated {
        public: keys.current().public_key(),
        changed: true,
    })
}

/// A fresh key id for a key pair made at `now`: `<prefix><YYYYMMDD>-<4 hex>`
/// (`p` for a publisher, `v` for the viewer).
pub fn pair_kid(prefix: &str, now: f64) -> Result<String, KeygenError> {
    Ok(cc_envelope::new_kid(prefix, now.floor() as i64)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_envelope::PublicKeys;

    #[test]
    fn bad_key_ids_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            publisher_keygen(&dir.path().join("p.key"), "bad id", IfExists::Refuse),
            Err(KeygenError::Key(_))
        ));
        assert!(pair_kid("p", 1_791_894_896.0)
            .unwrap()
            .starts_with("p20261013-"));
    }

    #[test]
    fn publisher_keygen_refuses_keeps_replaces_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys").join("publisher.key");
        let first = publisher_keygen(&path, "p1", IfExists::Refuse).unwrap();
        assert!(first.changed);
        assert_eq!(first.public.kid(), "p1");
        #[cfg(unix)]
        assert_eq!(crate::util::unix_mode(&path), Some(0o600));
        let before = std::fs::read(&path).unwrap();

        let err = publisher_keygen(&path, "p2", IfExists::Refuse).unwrap_err();
        assert!(matches!(err, KeygenError::Exists { .. }), "{err}");
        assert!(err.to_string().contains("--confirm <alias>"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "untouched");

        let kept = publisher_keygen(&path, "p2", IfExists::Keep).unwrap();
        assert_eq!(
            kept,
            Generated {
                public: first.public.clone(),
                changed: false
            }
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "--if-missing changes nothing"
        );

        let rotated = publisher_keygen(&path, "p3", IfExists::Rotate).unwrap();
        assert_eq!(rotated.public.kid(), "p3");
        let keys = SecretKeys::load(&path).unwrap();
        let kids: Vec<_> = keys.keys().iter().map(|k| k.kid()).collect();
        assert_eq!(kids, ["p3", "p1"]);

        let replaced = publisher_keygen(&path, "p4", IfExists::Replace).unwrap();
        assert_eq!(SecretKeys::load(&path).unwrap().keys().len(), 1);
        assert_ne!(replaced.public, first.public);
        // The printed line is the public key of what is on disk.
        assert_eq!(
            PublicKey::parse_line(&replaced.public.to_line()).unwrap(),
            SecretKeys::load(&path).unwrap().current().public_key()
        );
    }

    #[test]
    fn only_replacing_an_existing_key_needs_a_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("publisher.key");
        for mode in [
            IfExists::Refuse,
            IfExists::Keep,
            IfExists::Replace,
            IfExists::Rotate,
        ] {
            assert!(!replaces_identity(&path, mode), "no file yet: {mode:?}");
        }
        publisher_keygen(&path, "p1", IfExists::Refuse).unwrap();
        assert!(replaces_identity(&path, IfExists::Rotate));
        assert!(replaces_identity(&path, IfExists::Replace));
        assert!(!replaces_identity(&path, IfExists::Keep));
        assert!(
            !replaces_identity(&path, IfExists::Refuse),
            "refused anyway"
        );
    }

    #[test]
    fn the_alias_must_be_given_or_typed_back() {
        let never = || -> Option<String> { panic!("no prompt when --confirm is given") };
        assert_eq!(check_confirmation("lab-01", Some("lab-01"), never), Ok(()));
        assert_eq!(
            check_confirmation("lab-01", Some("lab-02"), never),
            Err(ConfirmError::Mismatch {
                alias: "lab-01".into(),
                given: "lab-02".into()
            })
        );
        // The prompt's line, with its newline.
        assert_eq!(
            check_confirmation("lab-01", None, || Some("lab-01\n".into())),
            Ok(())
        );
        assert!(matches!(
            check_confirmation("lab-01", None, || Some("LAB-01\n".into())),
            Err(ConfirmError::Mismatch { .. })
        ));
        assert!(matches!(
            check_confirmation("lab-01", None, || Some(String::new())),
            Err(ConfirmError::Mismatch { .. })
        ));
        // No terminal: the message has the warning and the flag to pass.
        let err = check_confirmation("lab-01", None, || None).unwrap_err();
        let text = err.to_string();
        assert!(text.starts_with(&identity_warning("lab-01")), "{text}");
        assert!(text.contains("--confirm lab-01"), "{text}");
        assert_eq!(
            identity_warning("lab-01"),
            "This replaces lab-01's identity. The viewer will hold back its values until you accept the new key there."
        );
    }

    #[test]
    fn viewer_keygen_writes_both_files_and_keeps_them_in_step() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("keys").join("viewer.key");
        let public = dir.path().join("keys").join("viewer.pub");
        let made = viewer_keygen(&key, &public, "k1", IfExists::Refuse).unwrap();
        assert!(made.changed);
        assert_eq!(PublicKeys::load(&public).unwrap().current(), &made.public);

        assert!(matches!(
            viewer_keygen(&key, &public, "k2", IfExists::Refuse),
            Err(KeygenError::Exists { .. })
        ));
        let kept = viewer_keygen(&key, &public, "k2", IfExists::Keep).unwrap();
        assert_eq!(
            kept,
            Generated {
                public: made.public.clone(),
                changed: false
            }
        );

        std::fs::remove_file(&public).unwrap();
        let healed = viewer_keygen(&key, &public, "k2", IfExists::Keep).unwrap();
        assert!(healed.changed, "viewer.pub written again");
        assert_eq!(PublicKeys::load(&public).unwrap().current(), &made.public);

        let rotated = viewer_keygen(&key, &public, "k2", IfExists::Rotate).unwrap();
        let pubs = PublicKeys::load(&public).unwrap();
        assert_eq!(pubs.current(), &rotated.public);
        assert_eq!(
            pubs.keys().len(),
            2,
            "the old key stays, so old values still open"
        );
    }
}
