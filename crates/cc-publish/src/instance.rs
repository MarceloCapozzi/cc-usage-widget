//! `<data_dir>/cc-publish.lock`: one publisher per data directory.
//!
//! A publisher (`cc-publish run`, or the cc-usage app when its role is
//! Publisher) holds an exclusive OS lock on this file for as long as it
//! publishes ([`InstanceLock`]). Two of them would split the cc-hook spool
//! between them and overwrite each other's values, so a second one that
//! finds the lock taken does not start. The OS releases the lock when the
//! holder exits, however it exits (a kill or a crash included), so a dead
//! publisher never holds up its replacement.
//!
//! The lock decides; the state file ([`crate::state`]) only names the
//! holder in the message, and still blocks next to a publisher that does
//! not take the lock.
//!
//! The file stays empty and cc-publish never removes it: removing it while
//! a publisher holds it would let a second one lock a new file of the same
//! name. Only the bundles' uninstallers remove it, after they stopped the
//! publisher.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// File name inside the data directory.
pub const LOCK_FILE: &str = "cc-publish.lock";

/// The lock file next to the state file `state_file`
/// (`<data_dir>/publish-state.json`).
pub fn lock_path_for(state_file: &Path) -> PathBuf {
    state_file.with_file_name(LOCK_FILE)
}

/// Why [`InstanceLock::acquire`] did not get the lock.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process (or another handle in this one) holds it.
    #[error("another publisher holds {}", .0.display())]
    Held(PathBuf),
    /// The file could not be opened or locked.
    #[error("cannot lock {}: {}", .0.display(), .1)]
    Io(PathBuf, #[source] std::io::Error),
}

/// The exclusive lock on the lock file, held until dropped. See the module
/// docs.
#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    /// Kept open: closing it releases the lock.
    _file: File,
}

impl InstanceLock {
    /// Takes the lock on `path` without waiting, creating the file (and its
    /// directory) when missing.
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        let io = |e| LockError::Io(path.to_path_buf(), e);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        // Never truncated: on Windows the holder's lock makes that fail.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                path: path.to_path_buf(),
                _file: file,
            }),
            Err(TryLockError::WouldBlock) => Err(LockError::Held(path.to_path_buf())),
            Err(TryLockError::Error(e)) => Err(io(e)),
        }
    }

    /// The locked file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path_for(&dir.path().join("sub").join(crate::state::STATE_FILE));
        assert_eq!(path, dir.path().join("sub").join(LOCK_FILE));

        let first = InstanceLock::acquire(&path).unwrap();
        assert_eq!(first.path(), path);
        assert!(path.is_file(), "created with its directory");
        // A second handle, even in this process, does not get it.
        let err = InstanceLock::acquire(&path).unwrap_err();
        assert!(matches!(&err, LockError::Held(p) if *p == path), "{err:?}");
        assert!(err.to_string().contains(LOCK_FILE), "{err}");

        drop(first);
        let again = InstanceLock::acquire(&path).unwrap();
        assert!(path.is_file(), "left in place");
        drop(again);
    }
}
