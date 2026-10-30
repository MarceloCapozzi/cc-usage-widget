//! Whole-file writes that a reader never sees half done: the bytes go to a
//! temporary file next to the target, which is synced and then renamed over
//! it. A crash or a full disk in the middle leaves the previous file whole.
//!
//! On Windows a rename over a file that another process holds open for a
//! moment (an antivirus or indexer scan of the file just written, a reader
//! without delete sharing) fails with access denied. [`rename_retrying`]
//! retries that for at most about three seconds, and every write here goes
//! through it.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How many times [`rename_retrying`] retries a rename refused with access
/// denied. Attempt `n` waits `25 * n` ms first, about 3 s in all.
const RENAME_RETRIES: u64 = 15;

/// Renames `from` over `to`. On Windows a rename refused with access denied
/// (the target held open for a moment by another process) is retried a few
/// times, for at most about three seconds, before the error is returned.
/// Elsewhere this is [`fs::rename`].
pub fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    rename_retrying_with(from, to, |from, to| fs::rename(from, to))
}

/// [`rename_retrying`] with the rename itself injected, for tests.
fn rename_retrying_with(
    from: &Path,
    to: &Path,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut attempt = 0u64;
    loop {
        match rename(from, to) {
            Err(e)
                if cfg!(windows)
                    && e.kind() == io::ErrorKind::PermissionDenied
                    && attempt < RENAME_RETRIES =>
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(25 * attempt));
            }
            other => return other,
        }
    }
}

/// A temporary path next to `path`: `.<name>.<pid>.<seq>.tmp`, unique per
/// call within the process (two threads may write the same file at once,
/// and sharing one temporary would let one rename the other's half-written
/// bytes into place) and across running processes.
pub fn temp_path(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    parent_dir(path).join(format!(".{name}.{}.{seq}.tmp", std::process::id()))
}

/// The folder `path` is in; `.` for a bare file name.
fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Writes `contents` to `path` through a temporary file and
/// [`rename_retrying`], creating the folder first. A new file gets the
/// default permissions.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_with(path, contents, false, rename_retrying)
}

/// [`write_atomic`] for a file only the current user may read: on Unix the
/// temporary file has mode `0600` from the moment it exists, so the target
/// never has wider permissions, even for an instant. On Windows the file
/// inherits the folder's ACL, as every file of the data folder does.
pub fn write_private_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_with(path, contents, true, rename_retrying)
}

/// The atomic write, with the final rename injected for tests.
fn write_with(
    path: &Path,
    contents: &[u8],
    private: bool,
    rename: fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    fs::create_dir_all(parent_dir(path))?;
    let tmp = temp_path(path);
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        // A leftover of a crashed process that had the same pid is replaced.
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut f = options.open(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
        drop(f);
        rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("cc-core-fsutil-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn an_atomic_write_creates_the_folder_replaces_the_file_and_leaves_no_temporary() {
        let root = temp_dir("replace");
        let path = root.join("sub").join("x.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(names(path.parent().unwrap()), ["x.json"]);
        write_private_atomic(&path, b"three").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"three");
        assert_eq!(names(path.parent().unwrap()), ["x.json"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn temporary_names_differ_per_call_and_sit_next_to_the_target() {
        let path = Path::new("dir").join("state.json");
        let (a, b) = (temp_path(&path), temp_path(&path));
        assert_ne!(a, b);
        assert_eq!(a.parent().unwrap(), Path::new("dir"));
        let name = a.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(".state.json.") && name.ends_with(".tmp"),
            "{name}"
        );
        assert_eq!(
            temp_path(Path::new("bare")).parent().unwrap(),
            Path::new(".")
        );
    }

    #[test]
    fn a_failed_write_removes_its_temporary() {
        let root = temp_dir("fail");
        let target = root.join("target");
        fs::create_dir_all(&root).unwrap();
        fs::write(&target, b"old").unwrap();
        let refuse = |_: &Path, _: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        for private in [false, true] {
            assert!(write_with(&target, b"new", private, refuse).is_err());
            assert_eq!(names(&root), ["target"]);
            assert_eq!(fs::read(&target).unwrap(), b"old");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn access_denied_is_retried_on_windows_only() {
        let denied = || io::Error::from(io::ErrorKind::PermissionDenied);
        let mut calls = 0;
        let result = rename_retrying_with(Path::new("a"), Path::new("b"), |_, _| {
            calls += 1;
            if calls < 3 {
                Err(denied())
            } else {
                Ok(())
            }
        });
        if cfg!(windows) {
            assert!(result.is_ok());
            assert_eq!(calls, 3);
        } else {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(calls, 1);
        }
        // Other errors are never retried.
        let mut calls = 0;
        let result = rename_retrying_with(Path::new("a"), Path::new("b"), |_, _| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(calls, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_write_has_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_dir("mode");
        let private = root.join("secret");
        write_private_atomic(&private, b"s").unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(&root);
    }
}
