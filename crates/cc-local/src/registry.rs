//! `~/.claude/sessions/*.json`: Claude Code's per-process session registry.
//!
//! Each live Claude Code process keeps a `<pid>.json` file there, in an
//! undocumented format such as:
//!
//! ```json
//! {"pid":21728,"sessionId":"...","cwd":"c:\\...","startedAt":1791855985658,
//!  "version":"2.1.296","kind":"interactive","entrypoint":"claude-vscode",
//!  "name":"my-repo-1","nameSource":"derived","status":"busy","updatedAt":...}
//! ```
//!
//! Its main use is [`enrich`]: `claude agents --json` does not report
//! `entrypoint`, which the focus logic needs. [`snapshot`] builds entries from
//! the files alone, a cheap fallback when the CLI is slow or missing; a file
//! can outlive a crashed process, so those entries may include dead sessions.
//!
//! The directory also holds `<pid>.<hash>.key` files with secrets. Only names
//! ending in `.json` are ever opened.

use std::path::{Path, PathBuf};

use cc_core::session::{RegistryEntry, RegistryStatus};
use serde_json::Value;

use crate::fields;

/// Registry files larger than this are skipped unread (real ones are ~650 bytes).
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// The registry directory: `~/.claude/sessions` (see [`crate::claude_dir`]).
pub fn sessions_dir() -> PathBuf {
    crate::claude_dir().join("sessions")
}

/// One parsed registry file. Every field but `session_id` is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFile {
    /// `sessionId`.
    pub session_id: String,
    /// `pid`.
    pub pid: Option<u32>,
    /// `cwd`.
    pub cwd: Option<String>,
    /// `name`, the registry's name for the session.
    pub name: Option<String>,
    /// `kind`: `interactive` or `background`.
    pub kind: Option<String>,
    /// `entrypoint`: `claude-vscode`, `cli`, ...
    pub entrypoint: Option<String>,
    /// `status`, when present.
    pub status: Option<RegistryStatus>,
    /// `startedAt`, Unix milliseconds.
    pub started_at_ms: Option<i64>,
    /// `updatedAt` (or `statusUpdatedAt`), Unix milliseconds.
    pub updated_at_ms: Option<i64>,
    /// `version`, the Claude Code version that wrote the file.
    pub version: Option<String>,
}

impl SessionFile {
    /// This file as a [`RegistryEntry`]; a missing status becomes
    /// `RegistryStatus::Other("unknown")`.
    pub fn to_entry(&self) -> RegistryEntry {
        RegistryEntry {
            session_id: self.session_id.clone(),
            pid: self.pid,
            cwd: self.cwd.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            status: self
                .status
                .clone()
                .unwrap_or_else(|| RegistryStatus::Other("unknown".into())),
            waiting_for: None,
            entrypoint: self.entrypoint.clone(),
            started_at_ms: self.started_at_ms,
            background_id: None,
        }
    }
}

/// Parses one registry file. Returns `None` when the text is not a JSON
/// object or has no `sessionId`; unknown fields are ignored.
pub fn parse_session_file(text: &str) -> Option<SessionFile> {
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let obj = value.as_object()?;
    Some(SessionFile {
        session_id: fields::string(obj, "sessionId")?,
        pid: fields::u32_field(obj, "pid"),
        cwd: fields::string(obj, "cwd"),
        name: fields::string(obj, "name"),
        kind: fields::string(obj, "kind"),
        entrypoint: fields::string(obj, "entrypoint"),
        status: fields::status(obj, "status"),
        started_at_ms: fields::i64_field(obj, "startedAt"),
        updated_at_ms: fields::i64_field(obj, "updatedAt")
            .or_else(|| fields::i64_field(obj, "statusUpdatedAt")),
        version: fields::string(obj, "version"),
    })
}

/// Reads and parses every `*.json` file in `dir`. Missing directories,
/// unreadable files, oversized files and files that do not parse are skipped.
/// The result is sorted by session id; when two files claim the same session,
/// the most recently updated one is kept.
pub fn read_dir(dir: &Path) -> Vec<SessionFile> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<SessionFile> = read
        .flatten()
        .filter(|e| is_registry_file_name(&e.file_name().to_string_lossy()))
        .filter(|e| {
            e.metadata()
                .is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES)
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| parse_session_file(&text))
        .collect();
    files.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then(b.updated_at_ms.cmp(&a.updated_at_ms))
    });
    files.dedup_by(|later, first| later.session_id == first.session_id);
    files
}

/// True for `<anything>.json` that is not a dotted temp file. Rejects the
/// `.key` files that sit next to the registry files.
fn is_registry_file_name(name: &str) -> bool {
    !name.starts_with('.')
        && Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

/// Fills in `entries` from the registry files in [`sessions_dir`].
pub fn enrich(entries: &mut [RegistryEntry]) {
    apply(entries, &read_dir(&sessions_dir()));
}

/// Fills in `entries` from the registry files in `dir`.
pub fn enrich_from(dir: &Path, entries: &mut [RegistryEntry]) {
    apply(entries, &read_dir(dir));
}

/// Fills in missing fields of each entry from the file with the same session
/// id, or failing that, the same pid (a process keeps its file when its
/// session id changes on `/clear` or a resume). Fields already set are kept:
/// `entrypoint`, `name`, `cwd`, `kind`, `pid` and `started_at_ms` are only
/// filled when missing, and `status` is never touched.
pub fn apply(entries: &mut [RegistryEntry], files: &[SessionFile]) {
    for entry in entries.iter_mut() {
        let file = files
            .iter()
            .find(|f| f.session_id == entry.session_id)
            .or_else(|| {
                entry
                    .pid
                    .and_then(|pid| files.iter().find(|f| f.pid == Some(pid)))
            });
        let Some(file) = file else { continue };
        fill(&mut entry.entrypoint, &file.entrypoint);
        fill(&mut entry.name, &file.name);
        fill(&mut entry.cwd, &file.cwd);
        fill(&mut entry.kind, &file.kind);
        if entry.pid.is_none() {
            entry.pid = file.pid;
        }
        if entry.started_at_ms.is_none() {
            entry.started_at_ms = file.started_at_ms;
        }
    }
}

fn fill(slot: &mut Option<String>, value: &Option<String>) {
    if slot.is_none() {
        slot.clone_from(value);
    }
}

/// Entries built from the registry files in [`sessions_dir`] alone, without
/// running the CLI. See the module docs for the stale-file caveat.
pub fn snapshot() -> Vec<RegistryEntry> {
    snapshot_from(&sessions_dir())
}

/// Entries built from the registry files in `dir` alone.
pub fn snapshot_from(dir: &Path) -> Vec<RegistryEntry> {
    read_dir(dir).iter().map(SessionFile::to_entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../fixtures/session-21728.json");
    const SID: &str = "c41d8e7f-5a2b-4e93-a6c0-9f1b3d7e5c03";

    fn bare_entry(session_id: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: session_id.into(),
            pid: None,
            cwd: None,
            name: None,
            kind: None,
            status: RegistryStatus::Idle,
            waiting_for: None,
            entrypoint: None,
            started_at_ms: None,
            background_id: None,
        }
    }

    #[test]
    fn parses_the_real_shape() {
        let f = parse_session_file(FIXTURE).unwrap();
        assert_eq!(f.session_id, SID);
        assert_eq!(f.pid, Some(21728));
        assert_eq!(f.entrypoint.as_deref(), Some("claude-vscode"));
        assert_eq!(f.kind.as_deref(), Some("interactive"));
        assert_eq!(f.name.as_deref(), Some("cc-usage-widget-1"));
        assert_eq!(
            f.cwd.as_deref(),
            Some("c:\\Users\\dev\\Projects\\cc-usage-widget")
        );
        assert_eq!(f.status, Some(RegistryStatus::Busy));
        assert_eq!(f.started_at_ms, Some(1_791_855_985_658));
        assert_eq!(f.updated_at_ms, Some(1_791_856_373_644));
        assert_eq!(f.version.as_deref(), Some("2.1.296"));
    }

    #[test]
    fn rejects_garbage_and_files_without_session_id() {
        assert_eq!(parse_session_file(""), None);
        assert_eq!(parse_session_file("{\"pid\":1"), None);
        assert_eq!(parse_session_file("[1,2]"), None);
        assert_eq!(parse_session_file(r#"{"pid":1,"entrypoint":"cli"}"#), None);
    }

    #[test]
    fn tolerates_missing_and_mistyped_fields_and_a_bom() {
        let f =
            parse_session_file("\u{feff}{\"sessionId\":\"s\",\"pid\":null,\"status\":3}").unwrap();
        assert_eq!(f.session_id, "s");
        assert_eq!(f.pid, None);
        assert_eq!(f.status, None);
        assert_eq!(f.to_entry().status, RegistryStatus::Other("unknown".into()));
    }

    #[test]
    fn apply_fills_missing_fields_by_session_id_and_keeps_existing_ones() {
        let file = parse_session_file(FIXTURE).unwrap();
        let mut entries = vec![bare_entry(SID), bare_entry("other")];
        entries[0].name = Some("renamed".into());
        apply(&mut entries, &[file]);
        assert_eq!(entries[0].entrypoint.as_deref(), Some("claude-vscode"));
        assert_eq!(entries[0].name.as_deref(), Some("renamed"));
        assert!(entries[0].cwd.is_some());
        assert_eq!(entries[0].pid, Some(21728));
        assert_eq!(
            entries[0].status,
            RegistryStatus::Idle,
            "status is never touched"
        );
        assert_eq!(entries[1], bare_entry("other"));
    }

    #[test]
    fn apply_falls_back_to_the_pid() {
        let file = parse_session_file(FIXTURE).unwrap();
        let mut entries = vec![bare_entry("new-session-after-clear")];
        entries[0].pid = Some(21728);
        apply(&mut entries, &[file]);
        assert_eq!(entries[0].entrypoint.as_deref(), Some("claude-vscode"));
    }

    #[test]
    fn read_dir_skips_key_files_temp_files_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("21728.json"), FIXTURE).unwrap();
        // A .key file whose content would parse if it were ever read.
        std::fs::write(
            dir.path().join("21728.0123abcd.key"),
            r#"{"sessionId":"from-key"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join(".21728.json.tmp"), r#"{"sessionId":"tmp"}"#).unwrap();
        std::fs::write(dir.path().join(".hidden.json"), r#"{"sessionId":"hidden"}"#).unwrap();
        std::fs::write(dir.path().join("5400.json"), "{not json").unwrap();
        std::fs::create_dir(dir.path().join("sub.json")).unwrap();
        let files = read_dir(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].session_id, SID);
    }

    #[test]
    fn read_dir_keeps_the_newest_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("1.json"),
            r#"{"sessionId":"s","pid":1,"updatedAt":10}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("2.json"),
            r#"{"sessionId":"s","pid":2,"updatedAt":20}"#,
        )
        .unwrap();
        let files = read_dir(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].pid, Some(2));
    }

    #[test]
    fn missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_dir(&dir.path().join("nope")).is_empty());
        let mut entries = vec![bare_entry("x")];
        enrich_from(&dir.path().join("nope"), &mut entries);
        assert_eq!(entries[0], bare_entry("x"));
    }

    #[test]
    fn snapshot_from_builds_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("21728.json"), FIXTURE).unwrap();
        let entries = snapshot_from(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, RegistryStatus::Busy);
        assert_eq!(entries[0].entrypoint.as_deref(), Some("claude-vscode"));
    }

    #[test]
    #[ignore = "reads the real ~/.claude/sessions"]
    fn live_registry() {
        let files = read_dir(&sessions_dir());
        let count = |f: &dyn Fn(&SessionFile) -> bool| files.iter().filter(|s| f(s)).count();
        let mut entrypoints: Vec<&str> = files
            .iter()
            .filter_map(|f| f.entrypoint.as_deref())
            .collect();
        entrypoints.sort_unstable();
        entrypoints.dedup();
        println!(
            "registry: {} files; with pid {}, cwd {}, name {}, kind {}, entrypoint {}, status {}, \
             startedAt {}, updatedAt {}, version {}; entrypoints {:?}",
            files.len(),
            count(&|f| f.pid.is_some()),
            count(&|f| f.cwd.is_some()),
            count(&|f| f.name.is_some()),
            count(&|f| f.kind.is_some()),
            count(&|f| f.entrypoint.is_some()),
            count(&|f| f.status.is_some()),
            count(&|f| f.started_at_ms.is_some()),
            count(&|f| f.updated_at_ms.is_some()),
            count(&|f| f.version.is_some()),
            entrypoints,
        );
    }
}
