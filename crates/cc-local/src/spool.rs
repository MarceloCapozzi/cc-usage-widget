//! Draining the spool: the directory where `cc-hook` drops one
//! [`SpoolEvent`] per hook call.
//!
//! `cc-hook` writes each event to a dotted temp name and renames it to
//! `<15-digit ms>-<unique>.json`, so a sorted listing of non-dotted `*.json`
//! names is complete files in chronological order. [`Drainer::drain`] parses
//! each one, returns the events and deletes the files. Files that do not parse,
//! carry a schema version this build does not know, or are implausibly large
//! are moved to `failed/` instead of being retried forever; that folder keeps
//! at most [`MAX_FAILED_FILES`] files, the oldest are deleted first.
//!
//! Each event is returned once: a file whose delete fails (another process
//! holds it open without sharing delete) is remembered by the [`Drainer`],
//! never returned again, and deleted on a later pass. Replaying it after
//! newer events would bring back a state the session already left.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use cc_core::events::{SpoolEvent, SPOOL_SCHEMA_VERSION};

use crate::LocalError;

/// Most files kept in `failed/`.
pub const MAX_FAILED_FILES: usize = 200;

/// Spool files larger than this are not parsed (real events are well under
/// 4 KiB, since cc-hook truncates every text field).
pub const MAX_EVENT_BYTES: u64 = 256 * 1024;

/// The name of the folder, inside the spool, that holds rejected files.
pub const FAILED_DIR_NAME: &str = "failed";

/// Parses one spool file. Fails on invalid JSON, a missing required field, or
/// a schema version other than [`SPOOL_SCHEMA_VERSION`].
pub fn parse_event(text: &str) -> Result<SpoolEvent, LocalError> {
    let event: SpoolEvent = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| LocalError::Parse(format!("spool event: {e}")))?;
    if event.v != SPOOL_SCHEMA_VERSION {
        return Err(LocalError::Parse(format!(
            "spool schema version {} (this build reads {SPOOL_SCHEMA_VERSION})",
            event.v
        )));
    }
    Ok(event)
}

/// Drains a spool directory pass after pass, remembering the files already
/// returned whose delete failed.
#[derive(Debug, Default, Clone)]
pub struct Drainer {
    /// Names of files returned once and not deleted yet.
    returned: HashSet<String>,
}

impl Drainer {
    /// A drainer that has returned nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads, returns and deletes every complete event file in `dir`, in
    /// file name (chronological) order. Rejected files go to `dir/failed/`.
    /// A missing directory yields no events; so does one that cannot be
    /// listed right now, and the files remembered below stay remembered. A
    /// file that cannot be read right now (for example, locked by another
    /// process) is left for the next call. A file returned by an earlier
    /// call whose delete failed is not returned again; its delete is retried.
    pub fn drain(&mut self, dir: &Path) -> Vec<SpoolEvent> {
        self.drain_with(dir, |path| fs::remove_file(path))
    }

    /// [`Drainer::drain`] with the delete replaced (tests).
    fn drain_with(
        &mut self,
        dir: &Path,
        remove: impl Fn(&Path) -> io::Result<()>,
    ) -> Vec<SpoolEvent> {
        let removed = |path: &Path| match remove(path) {
            Ok(()) => true,
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        };
        let files = match pending_files(dir) {
            Ok(files) => files,
            // No spool folder: nothing remembered can still be in it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.returned.clear();
                return Vec::new();
            }
            // A listing that failed this once says nothing about the files:
            // every remembered one is kept, or it would be replayed later.
            Err(_) => return Vec::new(),
        };
        let mut events = Vec::new();
        let mut moved_to_failed = false;
        let mut seen: HashSet<String> = HashSet::new();
        for (name, path) in files {
            seen.insert(name.clone());
            if self.returned.contains(&name) {
                if removed(&path) {
                    self.returned.remove(&name);
                }
                continue;
            }
            let size = match fs::metadata(&path) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            let parsed = if size > MAX_EVENT_BYTES {
                Err(())
            } else {
                match fs::read_to_string(&path) {
                    Ok(text) => parse_event(&text).map_err(|_| ()),
                    Err(e) if e.kind() == io::ErrorKind::InvalidData => Err(()),
                    Err(_) => continue,
                }
            };
            match parsed {
                Ok(event) => {
                    events.push(event);
                    if !removed(&path) {
                        self.returned.insert(name);
                    }
                }
                Err(()) => {
                    move_to_failed(dir, &path);
                    moved_to_failed = true;
                }
            }
        }
        // Files deleted by someone else need no remembering.
        self.returned.retain(|name| seen.contains(name));
        if moved_to_failed {
            prune_failed(&dir.join(FAILED_DIR_NAME), MAX_FAILED_FILES);
        }
        events
    }
}

/// One pass of a fresh [`Drainer`]: reads, returns and deletes every
/// complete event file in `dir`. A file whose delete fails is returned
/// again by the next call; callers that drain repeatedly keep a [`Drainer`].
pub fn drain(dir: &Path) -> Vec<SpoolEvent> {
    Drainer::new().drain(dir)
}

/// Non-dotted `*.json` files directly in `dir`, with their names, sorted
/// by name. An error when the folder cannot be listed: unlike an empty
/// one, it says nothing about which files are there.
fn pending_files(dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut files: Vec<(String, PathBuf)> = fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            is_event_file_name(&name).then(|| (name, e.path()))
        })
        .collect();
    files.sort();
    Ok(files)
}

/// True for `*.json` names that are not dotted temp files.
fn is_event_file_name(name: &str) -> bool {
    !name.starts_with('.')
        && Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

fn move_to_failed(dir: &Path, path: &Path) {
    let failed = dir.join(FAILED_DIR_NAME);
    if fs::create_dir_all(&failed).is_err() {
        // Without a failed/ folder the file would be retried forever; drop it.
        let _ = fs::remove_file(path);
        return;
    }
    let Some(name) = path.file_name() else { return };
    if fs::rename(path, failed.join(name)).is_err() {
        let _ = fs::remove_file(path);
    }
}

/// Deletes the oldest files in `failed` (by modification time, then name)
/// until at most `max` remain.
fn prune_failed(failed: &Path, max: usize) {
    let Ok(read) = fs::read_dir(failed) else {
        return;
    };
    let mut files: Vec<(SystemTime, String, PathBuf)> = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| {
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (
                modified,
                e.file_name().to_string_lossy().into_owned(),
                e.path(),
            )
        })
        .collect();
    if files.len() <= max {
        return;
    }
    files.sort();
    let excess = files.len() - max;
    for (_, _, path) in files.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRE_TOOL_USE: &str = r#"{"v":1,"ts":1791856373.644,"event":"PreToolUse","sessionId":"c41d8e7f-5a2b-4e93-a6c0-9f1b3d7e5c03","cwd":"c:\\Users\\dev\\proj","transcriptPath":"c:\\Users\\dev\\.claude\\projects\\c--Users-dev-proj\\c41d8e7f-5a2b-4e93-a6c0-9f1b3d7e5c03.jsonl","toolName":"Bash","toolUseId":"toolu_01","toolSummary":"cargo test","pid":21728,"termProgram":"vscode"}"#;
    const NOTIFICATION: &str = r#"{"v":1,"ts":1791856374.1,"event":"Notification","sessionId":"c41d8e7f-5a2b-4e93-a6c0-9f1b3d7e5c03","notificationType":"permission_prompt","message":"Claude needs your permission to use Bash","futureField":[1,2]}"#;

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .map(|r| {
                r.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn parses_real_shapes_and_ignores_unknown_fields() {
        let e = parse_event(PRE_TOOL_USE).unwrap();
        assert_eq!(e.event, "PreToolUse");
        assert_eq!(e.tool_name.as_deref(), Some("Bash"));
        assert_eq!(e.pid, Some(21728));
        let n = parse_event(NOTIFICATION).unwrap();
        assert_eq!(n.notification_type.as_deref(), Some("permission_prompt"));
    }

    #[test]
    fn rejects_garbage_missing_fields_and_unknown_versions() {
        assert!(parse_event("").is_err());
        assert!(parse_event("{\"v\":1").is_err());
        assert!(
            parse_event(r#"{"v":1,"ts":1,"event":"Stop"}"#).is_err(),
            "no sessionId"
        );
        assert!(parse_event(r#"{"v":2,"ts":1,"event":"Stop","sessionId":"s"}"#).is_err());
        assert!(parse_event(r#"{"v":1,"ts":1,"event":"Stop","sessionId":"s"}"#).is_ok());
    }

    #[test]
    fn drains_in_name_order_and_deletes_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("001791856374100-b.json"), NOTIFICATION).unwrap();
        fs::write(dir.path().join("001791856373644-a.json"), PRE_TOOL_USE).unwrap();
        let events = drain(dir.path());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "PreToolUse");
        assert_eq!(events[1].event, "Notification");
        assert!(names(dir.path()).is_empty());
        assert!(drain(dir.path()).is_empty());
    }

    #[test]
    fn leaves_temp_and_other_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".001791856373644-a.json.tmp"), PRE_TOOL_USE).unwrap();
        fs::write(dir.path().join(".001791856373644-a.json"), PRE_TOOL_USE).unwrap();
        fs::write(dir.path().join("notes.txt"), "x").unwrap();
        assert!(drain(dir.path()).is_empty());
        assert_eq!(names(dir.path()).len(), 3);
    }

    #[test]
    fn moves_bad_files_to_failed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("001-a.json"), PRE_TOOL_USE).unwrap();
        fs::write(dir.path().join("002-bad.json"), "{not json").unwrap();
        fs::write(
            dir.path().join("003-v9.json"),
            r#"{"v":9,"ts":1,"event":"Stop","sessionId":"s"}"#,
        )
        .unwrap();
        fs::write(dir.path().join("004-bin.json"), [0xff, 0xfe, 0x00]).unwrap();
        let events = drain(dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(names(dir.path()), vec!["failed"]);
        assert_eq!(
            names(&dir.path().join("failed")),
            vec!["002-bad.json", "003-v9.json", "004-bin.json"]
        );
    }

    #[test]
    fn oversized_files_are_rejected_unparsed() {
        let dir = tempfile::tempdir().unwrap();
        let big = format!(
            r#"{{"v":1,"ts":1,"event":"Stop","sessionId":"s","message":"{}"}}"#,
            "x".repeat(MAX_EVENT_BYTES as usize)
        );
        fs::write(dir.path().join("001-big.json"), big).unwrap();
        assert!(drain(dir.path()).is_empty());
        assert_eq!(names(&dir.path().join("failed")), vec!["001-big.json"]);
    }

    #[test]
    fn failed_folder_is_capped_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let failed = dir.path().join("failed");
        fs::create_dir(&failed).unwrap();
        for i in 0..MAX_FAILED_FILES {
            fs::write(failed.join(format!("{i:03}-old.json")), "x").unwrap();
        }
        // Make the first one clearly the oldest.
        let oldest = fs::File::options()
            .write(true)
            .open(failed.join("000-old.json"))
            .unwrap();
        oldest
            .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        drop(oldest);
        fs::write(dir.path().join("999-new-bad.json"), "nope").unwrap();
        drain(dir.path());
        let left = names(&failed);
        assert_eq!(left.len(), MAX_FAILED_FILES);
        assert!(!left.contains(&"000-old.json".to_string()));
        assert!(left.contains(&"999-new-bad.json".to_string()));
    }

    #[test]
    fn an_event_whose_delete_fails_is_returned_once_and_deleted_later() {
        use std::cell::Cell;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("001-a.json"), PRE_TOOL_USE).unwrap();
        fs::write(dir.path().join("002-b.json"), NOTIFICATION).unwrap();
        let locked = Cell::new(true);
        let remove = |path: &Path| {
            if locked.get() && path.ends_with("001-a.json") {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                fs::remove_file(path)
            }
        };
        let mut drainer = Drainer::new();
        let first = drainer.drain_with(dir.path(), remove);
        let kinds: Vec<&str> = first.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(
            kinds,
            ["PreToolUse", "Notification"],
            "in order, the stuck one included"
        );
        assert_eq!(names(dir.path()), ["001-a.json"]);

        // Still locked: not returned again, still there.
        fs::write(dir.path().join("003-c.json"), PRE_TOOL_USE).unwrap();
        let second = drainer.drain_with(dir.path(), remove);
        assert_eq!(second.len(), 1, "only the new file");
        assert_eq!(names(dir.path()), ["001-a.json"]);

        // Unlocked: deleted without being returned.
        locked.set(false);
        assert!(drainer.drain_with(dir.path(), remove).is_empty());
        assert!(names(dir.path()).is_empty());
        assert!(drainer.returned.is_empty());
    }

    #[test]
    fn a_remembered_file_deleted_elsewhere_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("001-a.json"), PRE_TOOL_USE).unwrap();
        let mut drainer = Drainer::new();
        let failing = |_: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(drainer.drain_with(dir.path(), failing).len(), 1);
        fs::remove_file(dir.path().join("001-a.json")).unwrap();
        assert!(drainer.drain(dir.path()).is_empty());
        assert!(drainer.returned.is_empty());
        // The same name written again is a new event.
        fs::write(dir.path().join("001-a.json"), PRE_TOOL_USE).unwrap();
        assert_eq!(drainer.drain(dir.path()).len(), 1);
    }

    /// A real delete failure: the file is held open without
    /// `FILE_SHARE_DELETE`, as another process might.
    #[cfg(windows)]
    #[test]
    fn a_file_held_open_by_another_handle_is_not_replayed() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("001-a.json");
        fs::write(&path, PRE_TOOL_USE).unwrap();
        let holder = fs::File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();
        let mut drainer = Drainer::new();
        assert_eq!(drainer.drain(dir.path()).len(), 1);
        assert!(
            drainer.drain(dir.path()).is_empty(),
            "not replayed while held"
        );
        assert!(path.exists());
        drop(holder);
        assert!(drainer.drain(dir.path()).is_empty());
        assert!(!path.exists(), "deleted once released");
    }

    /// A pass whose listing fails forgets nothing: the stuck file is still
    /// not replayed once the folder can be listed again. A folder that is
    /// gone takes its files with it.
    #[test]
    fn a_failed_listing_keeps_the_remembered_files() {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("spool");
        fs::create_dir(&spool).unwrap();
        fs::write(spool.join("001-a.json"), PRE_TOOL_USE).unwrap();
        let mut drainer = Drainer::new();
        let failing = |_: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(drainer.drain_with(&spool, failing).len(), 1);

        // A file where the folder was: listing it fails, and not as NotFound.
        let aside = root.path().join("spool-aside");
        fs::rename(&spool, &aside).unwrap();
        fs::write(&spool, "not a folder").unwrap();
        assert!(pending_files(&spool).is_err_and(|e| e.kind() != io::ErrorKind::NotFound));
        assert!(drainer.drain(&spool).is_empty());
        assert!(drainer.returned.contains("001-a.json"));

        fs::remove_file(&spool).unwrap();
        fs::rename(&aside, &spool).unwrap();
        assert!(drainer.drain(&spool).is_empty(), "not replayed");
        assert!(names(&spool).is_empty(), "deleted");
        assert!(drainer.returned.is_empty());

        // Remembered again, then the folder goes: nothing is left to remember.
        fs::write(spool.join("002-b.json"), NOTIFICATION).unwrap();
        assert_eq!(drainer.drain_with(&spool, failing).len(), 1);
        fs::remove_dir_all(&spool).unwrap();
        assert!(drainer.drain(&spool).is_empty());
        assert!(drainer.returned.is_empty());
    }

    #[test]
    fn missing_directory_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(drain(&dir.path().join("absent")).is_empty());
    }

    #[test]
    #[ignore = "reads (a copy of) the real spool"]
    fn live_spool() {
        // Drains a copy, so the real spool and whoever consumes it are untouched.
        let real = cc_core::paths::spool_dir();
        let copy = tempfile::tempdir().unwrap();
        let pending: Vec<PathBuf> = pending_files(&real)
            .unwrap_or_default()
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        for p in &pending {
            let _ = fs::copy(p, copy.path().join(p.file_name().unwrap()));
        }
        let events = drain(copy.path());
        let failed = names(&copy.path().join(FAILED_DIR_NAME)).len();
        let real_failed = names(&real.join(FAILED_DIR_NAME)).len();
        let mut kinds: Vec<&str> = events.iter().map(|e| e.event.as_str()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        println!(
            "spool: dir exists {}, {} pending files, {} parsed, {} rejected, {} already in failed/; events {:?}",
            real.is_dir(),
            pending.len(),
            events.len(),
            failed,
            real_failed,
            kinds,
        );
    }
}
