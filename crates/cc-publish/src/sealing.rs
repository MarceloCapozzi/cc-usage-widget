//! What a publisher seals with: the envelope, always.
//!
//! Every value is sealed from the newest key of this machine's private key
//! file ([`Config::publisher_key`], `keys/publisher.key`) to the newest key
//! of the viewer's public key file ([`Config::viewer_pub`],
//! `keys/viewer.pub`): only the viewer can open it, and it proves which
//! machine wrote it. Both files are required; a missing or broken one is an
//! error that names the file and how to get it, never a fall back.

use std::path::{Path, PathBuf};

use cc_envelope::{KeyFileError, PublicKeys, Sealer, SecretKeys};

use crate::config::Config;

/// Why no sealer could be built.
#[derive(Debug, thiserror::Error)]
pub enum SealingError {
    /// A key file is there but does not load.
    #[error("cannot load {}: {source}", path.display())]
    Load {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: KeyFileError,
    },
    /// A key file is missing.
    #[error("{} is missing: {hint}", path.display())]
    Missing {
        /// The file.
        path: PathBuf,
        /// What it is and how to get it.
        hint: &'static str,
    },
}

/// What a missing [`Config::publisher_key`] means.
pub const PUBLISHER_KEY_HINT: &str =
    "this machine's private key; create it with `cc-publish keygen` and pin its public key line on the viewer";

/// What a missing [`Config::viewer_pub`] means.
pub const VIEWER_PUB_HINT: &str =
    "the viewer's public key; copy viewer.pub from the viewer (a new bundle or the update package carries it)";

/// The sealer for `publisher_key` and `viewer_pub`, or why there is none.
pub fn select_files(publisher_key: &Path, viewer_pub: &Path) -> Result<Sealer, SealingError> {
    for (path, hint) in [
        (publisher_key, PUBLISHER_KEY_HINT),
        (viewer_pub, VIEWER_PUB_HINT),
    ] {
        if !path.is_file() {
            return Err(SealingError::Missing {
                path: path.to_path_buf(),
                hint,
            });
        }
    }
    let sender = SecretKeys::load(publisher_key).map_err(|source| SealingError::Load {
        path: publisher_key.to_path_buf(),
        source,
    })?;
    let recipient = PublicKeys::load(viewer_pub).map_err(|source| SealingError::Load {
        path: viewer_pub.to_path_buf(),
        source,
    })?;
    Ok(Sealer::new(sender, recipient))
}

/// The sealer for `config` (see the module docs).
pub fn select(config: &Config) -> Result<Sealer, SealingError> {
    select_files(&config.publisher_key, &config.viewer_pub)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path) -> Config {
        Config {
            alias: "lab-01".into(),
            servers: vec!["wss://connect.ngs.global".into()],
            bucket: cc_remote::DEFAULT_BUCKET.into(),
            creds: dir.join("u.creds"),
            publisher_key: dir.join("keys").join("publisher.key"),
            viewer_pub: dir.join("keys").join("viewer.pub"),
            log_file: false,
            path: dir.join("publish.json"),
        }
    }

    #[test]
    fn both_files_are_required() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        let err = select(&c).unwrap_err();
        assert!(
            matches!(&err, SealingError::Missing { path, .. } if *path == c.publisher_key),
            "{err}"
        );
        assert!(err.to_string().contains("cc-publish keygen"), "{err}");

        SecretKeys::generate("p1")
            .unwrap()
            .save_new(&c.publisher_key)
            .unwrap();
        let err = select(&c).unwrap_err();
        assert!(
            matches!(&err, SealingError::Missing { path, .. } if *path == c.viewer_pub),
            "{err}"
        );
        assert!(err.to_string().contains("viewer.pub"), "{err}");

        SecretKeys::generate("k1")
            .unwrap()
            .public_keys()
            .save(&c.viewer_pub)
            .unwrap();
        let sealer = select(&c).unwrap();
        assert!(sealer.describe().contains("viewer key k1"));
        assert_eq!(sealer.sender.current().kid(), "p1");
        assert!(select_files(&c.publisher_key, &c.viewer_pub).is_ok());
    }

    #[test]
    fn broken_files_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        SecretKeys::generate("p1")
            .unwrap()
            .save_new(&c.publisher_key)
            .unwrap();
        std::fs::write(&c.viewer_pub, "garbage\n").unwrap();
        let err = select(&c).unwrap_err();
        assert!(
            matches!(&err, SealingError::Load { path, .. } if *path == c.viewer_pub),
            "{err}"
        );
        assert!(!err.to_string().contains("garbage"));
    }
}
