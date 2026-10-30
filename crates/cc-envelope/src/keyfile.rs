//! The key file format shared by every key file of the envelope (see
//! [`crate::keypair`]), and its errors.
//!
//! A key file is plain text, one key per line, newest first: a short key id
//! (`kid`) that travels in every envelope, and the 32 key bytes in base64.
//!
//! ```text
//! v20261014-3f9a:<base64 of 32 bytes>
//! v20260901-07c1:<base64 of 32 bytes>
//! ```
//!
//! Blank lines and lines starting with `#` are ignored. Key files are copied
//! between machines by hand (or in a publisher bundle) and never go through
//! NATS. Errors never include key material: a malformed line is reported by
//! its number only.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use cc_core::fsutil::rename_retrying;
use zeroize::Zeroizing;

/// Length of a key in a key file, in bytes.
pub(crate) const KEY_LEN: usize = 32;

/// Longest accepted key id.
pub const MAX_KID_LEN: usize = 32;

/// Key files larger than this are rejected without being parsed.
pub const MAX_KEY_FILE_BYTES: u64 = 64 * 1024;

/// Errors from building, loading or saving keys and key files.
#[derive(Debug, thiserror::Error)]
pub enum KeyFileError {
    /// The key file could not be read or written.
    #[error("key file {path}: {source}")]
    Io {
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The key file is larger than [`MAX_KEY_FILE_BYTES`].
    #[error("key file {path} is too large ({len} bytes)")]
    TooLarge {
        /// The key file.
        path: PathBuf,
        /// Its size in bytes.
        len: u64,
    },
    /// A line of the key file is not `<kid>:<base64 of 32 bytes>`.
    /// The line's content is deliberately not included.
    #[error("key file line {line}: {reason}")]
    Malformed {
        /// 1-based line number.
        line: usize,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A key id is empty, too long, or uses characters other than ASCII
    /// letters, digits, `-` and `_`.
    #[error("invalid key id {0:?}: use 1-32 ASCII letters, digits, '-' or '_'")]
    InvalidKid(String),
    /// Two keys share a key id.
    #[error("duplicate key id {0:?}")]
    DuplicateKid(String),
    /// Two key sets hold different keys under the same key id.
    #[error("key id {0:?} holds a different key in each file")]
    ConflictingKid(String),
    /// The key set would hold no keys.
    #[error("no keys")]
    Empty,
    /// A file that must not be replaced is already there.
    #[error("{} already exists", .0.display())]
    Exists(PathBuf),
    /// The operating system's random number generator failed.
    #[error("OS random number generator failed: {0}")]
    Rng(getrandom::Error),
}

/// Checks a key id: 1 to [`MAX_KID_LEN`] ASCII letters, digits, `-` or `_`.
pub fn validate_kid(kid: &str) -> Result<(), KeyFileError> {
    let ok = !kid.is_empty()
        && kid.len() <= MAX_KID_LEN
        && kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(KeyFileError::InvalidKid(kid.to_string()))
    }
}

/// One parsed key file line: the key id and the key bytes.
pub(crate) type KeyLine = (String, Zeroizing<[u8; KEY_LEN]>);

/// Parses the lines of a key file in the `<kid>:<base64 of 32 bytes>`
/// format (see the module docs), in file order. Shared by the private and
/// public key files. Errors name the line, never its content.
pub(crate) fn parse_key_lines(text: &str) -> Result<Vec<KeyLine>, KeyFileError> {
    let mut keys: Vec<KeyLine> = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let malformed = |reason| KeyFileError::Malformed {
            line: line_no,
            reason,
        };
        let (kid, b64) = line
            .split_once(':')
            .ok_or_else(|| malformed("expected <kid>:<base64 key>"))?;
        if validate_kid(kid).is_err() {
            return Err(malformed(
                "invalid key id: use 1-32 ASCII letters, digits, '-' or '_'",
            ));
        }
        let decoded = Zeroizing::new(
            STANDARD
                .decode(b64)
                .map_err(|_| malformed("key is not valid base64"))?,
        );
        let mut bytes = Zeroizing::new([0u8; KEY_LEN]);
        if decoded.len() != KEY_LEN {
            return Err(malformed("key is not 32 bytes"));
        }
        bytes.copy_from_slice(&decoded);
        if keys.iter().any(|(k, _)| k == kid) {
            return Err(KeyFileError::DuplicateKid(kid.to_string()));
        }
        keys.push((kid.to_string(), bytes));
    }
    Ok(keys)
}

/// Reads a key file, refusing one over [`MAX_KEY_FILE_BYTES`].
pub(crate) fn read_key_file(path: &Path) -> Result<Zeroizing<String>, KeyFileError> {
    let io = |source| KeyFileError::Io {
        path: path.to_path_buf(),
        source,
    };
    let len = fs::metadata(path).map_err(io)?.len();
    if len > MAX_KEY_FILE_BYTES {
        return Err(KeyFileError::TooLarge {
            path: path.to_path_buf(),
            len,
        });
    }
    Ok(Zeroizing::new(fs::read_to_string(path).map_err(io)?))
}

/// Who may read a key file written by [`write_key_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Privacy {
    /// Mode `0600` on Unix: private keys.
    Private,
    /// The process umask decides: public keys, which are not secret.
    Public,
}

/// Whether [`write_key_file`] may replace an existing file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Replace {
    /// Replace it atomically.
    Yes,
    /// Fail with [`KeyFileError::Exists`] when the file is there, also when
    /// another process creates it at the same moment.
    No,
}

/// Writes a key file: creates missing parent directories, writes a
/// temporary file next to it and moves it into place, so readers never see
/// a partial file. With [`Replace::No`] the move is a hard link, which
/// fails when the destination exists (two processes generating the same
/// key at once: one wins, the other gets [`KeyFileError::Exists`]); on a
/// file system without hard links it falls back to a check and a rename.
///
/// On Windows the file inherits the ACL of its directory; the default
/// location under `%LOCALAPPDATA%` is already private to the user.
pub(crate) fn write_key_file(
    path: &Path,
    contents: &[u8],
    privacy: Privacy,
    replace: Replace,
) -> Result<(), KeyFileError> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir).map_err(|source| KeyFileError::Io {
        path: dir.clone(),
        source,
    })?;
    if replace == Replace::No && path.exists() {
        return Err(KeyFileError::Exists(path.to_path_buf()));
    }

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "key".into());
    let mut suffix = [0u8; 6];
    getrandom::getrandom(&mut suffix).map_err(KeyFileError::Rng)?;
    let suffix: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = dir.join(format!(".{file_name}.{}-{suffix}.tmp", std::process::id()));

    let result = write_and_move(&tmp, path, contents, privacy, replace);
    let _ = fs::remove_file(&tmp);
    result
}

fn write_and_move(
    tmp: &Path,
    dest: &Path,
    contents: &[u8],
    privacy: Privacy,
    replace: Replace,
) -> Result<(), KeyFileError> {
    let io = |p: &Path| {
        let p = p.to_path_buf();
        move |source| KeyFileError::Io { path: p, source }
    };
    let mut file = open_new(tmp, privacy).map_err(io(tmp))?;
    file.write_all(contents).map_err(io(tmp))?;
    file.sync_all().map_err(io(tmp))?;
    drop(file);
    match replace {
        Replace::Yes => rename_retrying(tmp, dest).map_err(io(dest)),
        Replace::No => match fs::hard_link(tmp, dest) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(KeyFileError::Exists(dest.to_path_buf()))
            }
            Err(_) if !dest.exists() => fs::rename(tmp, dest).map_err(io(dest)),
            Err(_) => Err(KeyFileError::Exists(dest.to_path_buf())),
        },
    }
}

/// Creates a new file; with [`Privacy::Private`] only the current user can
/// read it, where the OS allows.
fn open_new(path: &Path, privacy: Privacy) -> std::io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(if privacy == Privacy::Private {
            0o600
        } else {
            0o644
        });
    }
    #[cfg(not(unix))]
    let _ = privacy;
    opts.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const B64_32: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    #[test]
    fn parse_skips_blank_and_comment_lines_and_keeps_order() {
        let text = format!("# keys\n\nk2:{B64_32}\r\n  k1:{B64_32}  \n");
        let lines = parse_key_lines(&text).unwrap();
        let kids: Vec<_> = lines.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kids, ["k2", "k1"]);
        assert_eq!(lines[1].1[31], 31);
        assert!(parse_key_lines("# only\n\n").unwrap().is_empty());
    }

    #[test]
    fn parse_rejects_malformed_lines_by_number() {
        let short = STANDARD.encode([0u8; 31]);
        let cases: Vec<(String, usize)> = vec![
            (format!("k1:{B64_32}\nnocolon"), 2),
            (format!(":{B64_32}"), 1),
            (format!("k 1:{B64_32}"), 1),
            (format!("k.1:{B64_32}"), 1),
            ("k1:not base64!!".to_string(), 1),
            (format!("k1:{short}"), 1),
            (format!("k1:{B64_32}x"), 1),
            (format!("{}:{B64_32}", "k".repeat(33)), 1),
        ];
        for (text, line) in cases {
            match parse_key_lines(&text) {
                Err(KeyFileError::Malformed { line: l, .. }) => assert_eq!(l, line, "{text:?}"),
                other => panic!(
                    "expected Malformed for {text:?}, got {:?}",
                    other.map(|v| v.len())
                ),
            }
        }
        let dup = format!("k1:{B64_32}\nk1:{B64_32}\n");
        assert!(matches!(parse_key_lines(&dup), Err(KeyFileError::DuplicateKid(k)) if k == "k1"));
    }

    #[test]
    fn errors_never_echo_key_material() {
        let text = format!("k1:{B64_32}");
        let bad = format!("{}garbage", &text[..20]);
        let msg = parse_key_lines(&bad)
            .map(|v| v.len())
            .unwrap_err()
            .to_string();
        assert!(!msg.contains(&B64_32[..10]), "{msg}");
    }

    #[test]
    fn kids_are_validated() {
        for bad in ["", "a:b", "k 1", &"k".repeat(MAX_KID_LEN + 1)] {
            assert!(
                matches!(validate_kid(bad), Err(KeyFileError::InvalidKid(_))),
                "{bad:?}"
            );
        }
        validate_kid(&"k".repeat(MAX_KID_LEN)).unwrap();
        validate_kid("v20261014-3f9a").unwrap();
    }

    #[test]
    fn read_reports_missing_and_oversize_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent");
        assert!(matches!(
            read_key_file(&missing),
            Err(KeyFileError::Io { .. })
        ));
        let big = dir.path().join("big");
        fs::write(&big, vec![b'#'; MAX_KEY_FILE_BYTES as usize + 1]).unwrap();
        assert!(matches!(
            read_key_file(&big),
            Err(KeyFileError::TooLarge { .. })
        ));
    }

    #[test]
    fn write_creates_dirs_replaces_and_leaves_no_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("viewer.key");
        write_key_file(&path, b"one\n", Privacy::Private, Replace::Yes).unwrap();
        write_key_file(&path, b"two\n", Privacy::Private, Replace::Yes).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        assert!(matches!(
            write_key_file(&path, b"three\n", Privacy::Private, Replace::No),
            Err(KeyFileError::Exists(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["viewer.key"], "temporary files left behind");
    }
}
