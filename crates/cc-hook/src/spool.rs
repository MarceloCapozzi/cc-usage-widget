//! Writing spool files and keeping the spool bounded.
//!
//! Each event is serialized to `.<final name>.tmp` in the spool directory and
//! renamed to [`SpoolEvent::file_name`], so readers (which skip dotted names)
//! never see a partial file. When nothing drains the spool (the widget is not
//! running), [`prune`] keeps it from growing without bound; the receiver runs
//! it on roughly one call in [`PRUNE_ONE_IN`].
//!
//! The spool holds prompt and message excerpts, so on Unix its folder is
//! created `0700` and its files `0600` ([`create_private_dir`],
//! [`write_private`]); on Windows they inherit the ACL of the data folder
//! under `%LOCALAPPDATA%`, which is private to the user.

use cc_core::events::SpoolEvent;
use cc_core::fsutil::rename_retrying;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Pruning by count starts when the spool holds at least this many finished files.
pub const PRUNE_CAP: usize = 500;

/// When pruning by count, files older than this are deleted first.
pub const PRUNE_MAX_AGE: Duration = Duration::from_secs(10 * 60);

/// Finished files older than this are deleted on every prune, however few
/// there are: an event nobody drained for an hour is of no use to the
/// widget, and should not linger on disk.
pub const SPOOL_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Files in `failed/` older than this are deleted on every prune.
pub const FAILED_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The folder, inside the spool, where the widget moves files it rejected.
pub const FAILED_DIR: &str = "failed";

/// The receiver prunes on one call in this many, chosen at random.
pub const PRUNE_ONE_IN: u64 = 32;

/// 64 random bits without a dependency: std's `RandomState` is seeded from the
/// OS, mixed here with the pid and the clock.
pub fn random_u64() -> u64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    h.finish()
}

/// The unique part of a spool file name: pid and 64 random bits, in hex.
pub fn unique_token() -> String {
    format!("{:x}-{:016x}", std::process::id(), random_u64())
}

/// Writes `event` into `dir` (created if missing) under
/// `event.file_name(unique)`, via a dotted temp file and a rename.
/// Returns the final path.
pub fn write_event(dir: &Path, event: &SpoolEvent, unique: &str) -> io::Result<PathBuf> {
    create_private_dir(dir)?;
    let bytes = serde_json::to_vec(event).map_err(io::Error::other)?;
    let name = event.file_name(unique);
    let tmp = dir.join(format!(".{name}.tmp"));
    let target = dir.join(&name);
    if let Err(e) = write_private(&tmp, &bytes).and_then(|()| rename_retrying(&tmp, &target)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(target)
}

/// `create_dir_all`, with mode `0700` for every folder it creates on Unix.
/// Folders that already exist keep their mode.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Writes a new file (replacing a leftover of the same name), with mode
/// `0600` on Unix from the moment it exists.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

/// Whether a spool directory entry name is a finished event file: `*.json`,
/// not dotted (temp files are).
pub fn is_event_file_name(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(".json")
}

/// Bounds the spool. Ages come from the millisecond timestamp a file name
/// starts with, or its modification time.
///
/// - Finished event files older than [`SPOOL_MAX_AGE`] are always deleted.
/// - If at least [`PRUNE_CAP`] remain, those older than [`PRUNE_MAX_AGE`] go,
///   then the oldest by name until fewer than [`PRUNE_CAP`] remain.
/// - Files in `failed/` older than [`FAILED_MAX_AGE`] are deleted.
///
/// Temp files and other subdirectories are never touched. Returns how many
/// files were deleted.
pub fn prune(dir: &Path, now: SystemTime) -> io::Result<usize> {
    let now_ms = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let max_age_ms = PRUNE_MAX_AGE.as_millis() as u64;
    let hard_age_ms = SPOOL_MAX_AGE.as_millis() as u64;
    let mut deleted = prune_failed(&dir.join(FAILED_DIR), now);

    let mut files: Vec<(String, Option<u64>)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_event_file_name(&name) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let ms = name_timestamp_ms(&name).or_else(|| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some(modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
        });
        let expired = ms.is_some_and(|ms| now_ms.saturating_sub(ms) > hard_age_ms);
        if expired && std::fs::remove_file(entry.path()).is_ok() {
            deleted += 1;
            continue;
        }
        files.push((name, ms));
    }
    if files.len() < PRUNE_CAP {
        return Ok(deleted);
    }

    files.sort();
    let mut kept = Vec::with_capacity(files.len());
    for (name, ms) in files {
        let old = ms.is_some_and(|ms| now_ms.saturating_sub(ms) > max_age_ms);
        if old && std::fs::remove_file(dir.join(&name)).is_ok() {
            deleted += 1;
        } else {
            kept.push(name);
        }
    }
    let excess = kept.len().saturating_sub(PRUNE_CAP - 1);
    for name in kept.iter().take(excess) {
        if std::fs::remove_file(dir.join(name)).is_ok() {
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Deletes the files in `failed` not modified for [`FAILED_MAX_AGE`]. A
/// missing folder is nothing to do. Returns how many were deleted.
fn prune_failed(failed: &Path, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(failed) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            e.metadata().is_ok_and(|m| {
                m.is_file()
                    && m.modified()
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                        .is_some_and(|age| age > FAILED_MAX_AGE)
            })
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// The leading 15-digit millisecond timestamp of a spool file name.
fn name_timestamp_ms(name: &str) -> Option<u64> {
    let digits = name.get(..15)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) || name.as_bytes().get(15) != Some(&b'-') {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;

    fn temp_dir(tag: &str) -> TestDir {
        TestDir::new(&format!("spool-{tag}"))
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    fn touch(dir: &Path, ms: u64, i: usize) {
        std::fs::write(dir.join(format!("{ms:015}-{i:04}.json")), b"{}").unwrap();
    }

    #[test]
    fn writes_via_rename_and_leaves_no_temp_file() {
        let root = temp_dir("write");
        let dir = root.join("nested");
        let e = SpoolEvent::new("Stop", "s-1", 1_791_856_373.644);
        let path = write_event(&dir, &e, "abc").unwrap();
        assert_eq!(path.file_name().unwrap(), "001791856373644-abc.json");
        assert_eq!(names(&dir), vec!["001791856373644-abc.json"]);
        let back: SpoolEvent = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn unique_tokens_differ() {
        assert_ne!(unique_token(), unique_token());
    }

    #[test]
    fn prune_does_nothing_below_the_cap() {
        let dir = temp_dir("below");
        let now_ms: u64 = 1_800_000_000_000;
        // Older than PRUNE_MAX_AGE but younger than SPOOL_MAX_AGE.
        for i in 0..PRUNE_CAP - 1 {
            touch(&dir, now_ms - 30 * 60 * 1000 + i as u64, i);
        }
        let now = UNIX_EPOCH + Duration::from_millis(now_ms);
        assert_eq!(prune(&dir, now).unwrap(), 0);
        assert_eq!(names(&dir).len(), PRUNE_CAP - 1);
    }

    #[test]
    fn prune_drops_expired_events_and_old_failures_whatever_the_count() {
        let dir = temp_dir("expired");
        let now_ms: u64 = 1_800_000_000_000;
        let now = UNIX_EPOCH + Duration::from_millis(now_ms);
        touch(&dir, now_ms - 61 * 60 * 1000, 1);
        touch(&dir, now_ms - 59 * 60 * 1000, 2);
        let failed = dir.join(FAILED_DIR);
        std::fs::create_dir(&failed).unwrap();
        for (name, age_days) in [("old.json", 8u64), ("new.json", 6)] {
            let path = failed.join(name);
            std::fs::write(&path, b"{").unwrap();
            let modified = now - Duration::from_secs(age_days * 24 * 3600);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        assert_eq!(prune(&dir, now).unwrap(), 2);
        assert_eq!(
            names(&dir),
            [
                format!("{:015}-0002.json", now_ms - 59 * 60 * 1000),
                FAILED_DIR.to_string()
            ]
        );
        assert_eq!(names(&failed), ["new.json"]);
    }

    #[cfg(unix)]
    #[test]
    fn spool_folder_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_dir("mode");
        let dir = root.join("spool");
        let path = write_event(&dir, &SpoolEvent::new("Stop", "s", 1.0), "x").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn prune_drops_old_files_then_the_oldest_and_spares_temp_and_failed() {
        let dir = temp_dir("prune");
        let now_ms: u64 = 1_800_000_000_000;
        let now = UNIX_EPOCH + Duration::from_millis(now_ms);
        // 100 old files and 600 recent ones.
        for i in 0..100 {
            touch(&dir, now_ms - 11 * 60 * 1000 - i as u64, i);
        }
        for i in 0..600 {
            touch(&dir, now_ms - 60_000 + i as u64, 1000 + i);
        }
        std::fs::write(dir.join(".000000000000001-x.json.tmp"), b"{").unwrap();
        std::fs::create_dir(dir.join("failed")).unwrap();
        let failed = dir.join("failed").join("000000000000001-y.json");
        std::fs::write(&failed, b"{").unwrap();
        // Recent as far as `now` goes, so the age limit of failed/ spares it.
        std::fs::File::options()
            .write(true)
            .open(&failed)
            .unwrap()
            .set_modified(now)
            .unwrap();

        let deleted = prune(&dir, now).unwrap();
        let left = names(&dir);
        let events: Vec<&String> = left.iter().filter(|n| is_event_file_name(n)).collect();
        assert_eq!(events.len(), PRUNE_CAP - 1);
        assert_eq!(deleted, 700 - (PRUNE_CAP - 1));
        // The newest survive.
        assert_eq!(
            events.last().unwrap().as_str(),
            format!("{:015}-{:04}.json", now_ms - 60_000 + 599, 1599)
        );
        assert!(left.contains(&".000000000000001-x.json.tmp".to_string()));
        assert!(left.contains(&"failed".to_string()));
        assert!(dir.join("failed").join("000000000000001-y.json").exists());
    }

    #[test]
    fn name_timestamps() {
        assert_eq!(
            name_timestamp_ms("001791856373644-abc.json"),
            Some(1_791_856_373_644)
        );
        assert_eq!(name_timestamp_ms("weird.json"), None);
        assert_eq!(name_timestamp_ms("0017918563736441abc.json"), None);
    }
}
