//! Session transcripts: `~/.claude/projects/<slug>/<session id>.jsonl`.
//!
//! A transcript is one JSON record per line, mostly conversation. Three record
//! types carry what nothing else reports, and Claude Code appends a new one
//! each time the value changes, so the last of each kind wins:
//!
//! ```json
//! {"type":"ai-title","aiTitle":"Fix the tray icon","sessionId":"..."}
//! {"type":"custom-title","customTitle":"release prep","sessionId":"..."}
//! {"type":"agent-color","agentColor":"green","sessionId":"..."}
//! ```
//!
//! [`TranscriptReader`] extracts those into [`TranscriptMarks`] while staying
//! away from the conversation: it reads the file backwards from the end in
//! [`CHUNK_BYTES`] chunks, at most [`MAX_SCAN_BYTES`] per call, skips lines
//! longer than [`MAX_LINE_BYTES`] without parsing them, and only parses lines
//! that contain one of the three `"type":"..."` markers. It remembers how far
//! it has read per session, so later calls only read newly appended bytes; a
//! mark found there replaces the cached one, otherwise the cached one stays.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use cc_core::session::TranscriptMarks;
use serde::Deserialize;

use crate::LocalError;

/// Bytes read per backwards step.
pub const CHUNK_BYTES: usize = 256 * 1024;

/// Most bytes read from one transcript per call.
pub const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024;

/// Lines longer than this are skipped without being parsed (mark records are
/// a few hundred bytes; long lines are conversation and tool output).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// The exact text each mark record type starts with. Inside a JSON string the
/// quotes would be escaped, so conversation text quoting these never matches.
const MARKERS: [&[u8]; 3] = [
    br#""type":"ai-title""#,
    br#""type":"custom-title""#,
    br#""type":"agent-color""#,
];

/// Claude Code's projects directory: `~/.claude/projects` (see [`crate::claude_dir`]).
pub fn projects_dir() -> PathBuf {
    crate::claude_dir().join("projects")
}

/// The project directory name Claude Code derives from a working directory:
/// every character that is not an ASCII letter, digit or `-` becomes `-`.
///
/// Case is preserved (`C:\x` and `c:\x` give different names; on Windows they
/// still resolve to the same directory because the file system ignores case).
/// Characters outside the Basic Multilingual Plane become two dashes, since
/// Claude Code replaces UTF-16 code units.
///
/// `c:\Users\ada\OneDrive - Acme Inc\proj` → `c--Users-ada-OneDrive---Acme-Inc-proj`
pub fn project_slug(cwd: &str) -> String {
    let mut slug = String::with_capacity(cwd.len());
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            slug.push(c);
        } else {
            slug.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    slug
}

/// The expected transcript path: `~/.claude/projects/<slug>/<session_id>.jsonl`.
/// `None` for a session id that is not a plain file name (see
/// [`crate::live::valid_session_id`]): ids come from hook payloads and files
/// other programs write, and one like `..\..\x` must not leave the folder.
pub fn path_for(session_id: &str, cwd: &str) -> Option<PathBuf> {
    path_in(&projects_dir(), session_id, cwd)
}

/// [`path_for`] under an explicit projects directory.
pub fn path_in(projects_dir: &Path, session_id: &str, cwd: &str) -> Option<PathBuf> {
    if !crate::live::valid_session_id(session_id) {
        return None;
    }
    Some(
        projects_dir
            .join(project_slug(cwd))
            .join(format!("{session_id}.jsonl")),
    )
}

/// Finds a session's transcript: the [`path_in`] location if it exists,
/// otherwise any project directory holding `<session_id>.jsonl` (Claude Code
/// shortens very long slugs, and a session may have moved directory). `None`
/// for an invalid session id.
pub fn locate(projects_dir: &Path, session_id: &str, cwd: &str) -> Option<PathBuf> {
    let expected = path_in(projects_dir, session_id, cwd)?;
    if expected.is_file() {
        return Some(expected);
    }
    let file_name = format!("{session_id}.jsonl");
    std::fs::read_dir(projects_dir)
        .ok()?
        .flatten()
        .map(|dir| dir.path().join(&file_name))
        .find(|p| p.is_file())
}

/// One mark record found in a transcript line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// `{"type":"ai-title","aiTitle":...}`; `None` when the value is empty.
    AiTitle(Option<String>),
    /// `{"type":"custom-title","customTitle":...}`; `None` when cleared.
    CustomTitle(Option<String>),
    /// `{"type":"agent-color","agentColor":...}`; `None` when cleared.
    AgentColor(Option<String>),
    /// The model and context size of a main-thread assistant message.
    Usage {
        /// `message.model`, unless it is a `<synthetic>` placeholder.
        model: Option<String>,
        /// Input plus cache-creation plus cache-read tokens.
        context_tokens: Option<u64>,
    },
}

/// Markers of an assistant message that carries token usage.
const ASSISTANT_MARKER: &[u8] = br#""type":"assistant""#;
const USAGE_MARKER: &[u8] = br#""usage":{"#;
/// Subagent messages are not the session's own context.
const SIDECHAIN_MARKER: &[u8] = br#""isSidechain":true"#;

/// Reads model and context size out of an assistant line WITHOUT parsing it:
/// the line also holds what the assistant said, and only these few numbers
/// are wanted, so they are found by searching the raw bytes for their keys.
fn parse_usage_line(line: &[u8]) -> Option<Mark> {
    if !contains(line, ASSISTANT_MARKER) || contains(line, SIDECHAIN_MARKER) {
        return None;
    }
    let usage_at = memchr::memmem::find(line, USAGE_MARKER)?;
    let usage = &line[usage_at..];
    let number = |key: &[u8]| -> Option<u64> {
        let at = memchr::memmem::find(usage, key)? + key.len();
        let digits: Vec<u8> = usage[at..]
            .iter()
            .copied()
            .take_while(u8::is_ascii_digit)
            .collect();
        std::str::from_utf8(&digits).ok()?.parse().ok()
    };
    let parts = [
        number(br#""input_tokens":"#),
        number(br#""cache_creation_input_tokens":"#),
        number(br#""cache_read_input_tokens":"#),
    ];
    let context_tokens = parts
        .iter()
        .any(Option::is_some)
        .then(|| parts.iter().flatten().sum::<u64>());

    let model = memchr::memmem::find(line, br#""model":""#).and_then(|at| {
        let rest = &line[at + br#""model":""#.len()..];
        let end = memchr::memchr(b'"', rest)?;
        let value = std::str::from_utf8(&rest[..end]).ok()?.trim();
        // Claude Code writes "<synthetic>" for messages it made up itself
        // (API errors, interruptions); those say nothing about the model.
        (!value.is_empty() && !value.starts_with('<') && value.len() <= 64)
            .then(|| value.to_string())
    });
    if model.is_none() && context_tokens.is_none() {
        return None;
    }
    Some(Mark::Usage {
        model,
        context_tokens,
    })
}

#[derive(Deserialize)]
struct MarkRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "aiTitle")]
    ai_title: Option<String>,
    #[serde(default, rename = "customTitle")]
    custom_title: Option<String>,
    #[serde(default, rename = "agentColor")]
    agent_color: Option<String>,
}

/// Parses one transcript line if it is a mark record. Lines longer than
/// [`MAX_LINE_BYTES`] or without a marker are rejected before any parsing.
pub fn parse_mark_line(line: &[u8]) -> Option<Mark> {
    if line.len() > MAX_LINE_BYTES {
        return None;
    }
    if !MARKERS.iter().any(|m| contains(line, m)) {
        return parse_usage_line(line);
    }
    let record: MarkRecord = serde_json::from_slice(line).ok()?;
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match record.kind.as_str() {
        "ai-title" => Some(Mark::AiTitle(clean(record.ai_title))),
        "custom-title" => Some(Mark::CustomTitle(clean(record.custom_title))),
        "agent-color" => Some(Mark::AgentColor(clean(record.agent_color))),
        _ => None,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(haystack, needle).is_some()
}

/// Read limits; the public constants in production, smaller ones in tests.
#[derive(Debug, Clone, Copy)]
struct Limits {
    chunk: usize,
    max_scan: u64,
    max_line: usize,
}

const LIMITS: Limits = Limits {
    chunk: CHUNK_BYTES,
    max_scan: MAX_SCAN_BYTES,
    max_line: MAX_LINE_BYTES,
};

/// What one backwards scan of a byte range found.
#[derive(Debug, Default, PartialEq, Eq)]
struct RegionScan {
    /// The last value of each mark in the range; the outer `Option` is
    /// "found", the inner one the value (a cleared mark is `Some(None)`).
    title: Option<Option<String>>,
    custom_title: Option<Option<String>>,
    color: Option<Option<String>>,
    /// The newest assistant usage: (model, context tokens).
    usage: Option<(Option<String>, Option<u64>)>,
    /// Offset just past the last complete line; the next scan starts here.
    complete_end: u64,
}

impl RegionScan {
    fn record(&mut self, mark: Mark) {
        // Scanning backwards: the first value seen of each kind is the newest.
        match mark {
            Mark::AiTitle(v) => {
                self.title.get_or_insert(v);
            }
            Mark::CustomTitle(v) => {
                self.custom_title.get_or_insert(v);
            }
            Mark::AgentColor(v) => {
                self.color.get_or_insert(v);
            }
            Mark::Usage {
                model,
                context_tokens,
            } => {
                self.usage.get_or_insert((model, context_tokens));
            }
        }
    }

    fn all_found(&self) -> bool {
        self.title.is_some()
            && self.custom_title.is_some()
            && self.color.is_some()
            && self.usage.is_some()
    }

    fn apply_to(self, marks: &mut TranscriptMarks) {
        if let Some(v) = self.title {
            marks.title = v;
        }
        if let Some(v) = self.custom_title {
            marks.custom_title = v;
        }
        if let Some(v) = self.color {
            marks.color = v;
        }
        if let Some((model, tokens)) = self.usage {
            // A newer message without a model keeps the one already known.
            if model.is_some() {
                marks.model = model;
            }
            if tokens.is_some() {
                marks.context_tokens = tokens;
            }
        }
    }
}

/// Scans `[start, end)` of `file` backwards for mark records.
///
/// `start` must be a line boundary (0 or a previous `complete_end`). At most
/// `limits.max_scan` bytes at the end of the range are read; a line cut by
/// that limit is dropped. The bytes after the last newline are a line still
/// being written: they are parsed if they form a mark, but not counted as
/// scanned, so the next call reads them again.
fn scan_region<R: Read + Seek>(
    file: &mut R,
    start: u64,
    end: u64,
    limits: Limits,
) -> std::io::Result<RegionScan> {
    let lower = start.max(end.saturating_sub(limits.max_scan));
    let mut scan = RegionScan {
        complete_end: start,
        ..RegionScan::default()
    };
    // The end of a line whose start lies before the current chunk.
    let mut carry: Vec<u8> = Vec::new();
    // True once `carry` exceeded the line limit; it is then only counted, not kept.
    let mut carry_too_long = false;
    let mut seen_newline = false;
    let mut pos = end;
    let mut buf = vec![0u8; limits.chunk];

    let handle = |line: &[u8], too_long: bool, scan: &mut RegionScan| {
        if !too_long && line.len() <= limits.max_line {
            if let Some(mark) = parse_mark_line(line) {
                scan.record(mark);
            }
        }
    };

    while pos > lower && !scan.all_found() {
        let chunk_start = lower.max(pos.saturating_sub(limits.chunk as u64));
        let len = (pos - chunk_start) as usize;
        file.seek(SeekFrom::Start(chunk_start))?;
        file.read_exact(&mut buf[..len])?;
        let chunk = &buf[..len];

        let mut seg_end = len;
        for i in memchr::memrchr_iter(b'\n', chunk) {
            let head = &chunk[i + 1..seg_end];
            let too_long = carry_too_long || head.len() + carry.len() > limits.max_line;
            let line = join(head, &carry, too_long);
            if !seen_newline {
                seen_newline = true;
                scan.complete_end = chunk_start + i as u64 + 1;
            }
            handle(trim_cr(&line), too_long, &mut scan);
            carry.clear();
            carry_too_long = false;
            seg_end = i;
            if scan.all_found() {
                return Ok(scan);
            }
        }
        // chunk[..seg_end] continues a line that starts further back.
        let head = &chunk[..seg_end];
        if carry_too_long || head.len() + carry.len() > limits.max_line {
            carry.clear();
            carry_too_long = true;
        } else {
            carry.splice(0..0, head.iter().copied());
        }
        pos = chunk_start;
    }

    // Whatever is left starts at `lower`: a whole line only if `lower` is the
    // line boundary `start`, otherwise a line cut by the scan limit.
    if pos == lower && lower == start && !scan.all_found() {
        handle(trim_cr(&carry), carry_too_long, &mut scan);
    }
    Ok(scan)
}

fn join(head: &[u8], carry: &[u8], too_long: bool) -> Vec<u8> {
    if too_long {
        return Vec::new();
    }
    let mut line = Vec::with_capacity(head.len() + carry.len());
    line.extend_from_slice(head);
    line.extend_from_slice(carry);
    line
}

fn trim_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[derive(Debug)]
struct Cached {
    path: PathBuf,
    scanned: u64,
    marks: TranscriptMarks,
}

/// Reads [`TranscriptMarks`] from transcripts, remembering per session how
/// much of the file it has already scanned.
///
/// Meant to be polled: a call on an unchanged file only costs a `stat`.
#[derive(Debug)]
pub struct TranscriptReader {
    projects_dir: PathBuf,
    limits: Limits,
    cache: HashMap<String, Cached>,
}

impl Default for TranscriptReader {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptReader {
    /// A reader over [`projects_dir`].
    pub fn new() -> Self {
        Self::with_projects_dir(projects_dir())
    }

    /// A reader over an explicit projects directory.
    pub fn with_projects_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            projects_dir: dir.into(),
            limits: LIMITS,
            cache: HashMap::new(),
        }
    }

    /// The marks of a session, locating its transcript from `cwd` (see
    /// [`locate`]). A session without a transcript yet, or one that cannot be
    /// read, gives its cached marks, or empty ones.
    pub fn marks(&mut self, session_id: &str, cwd: &str) -> TranscriptMarks {
        let path = match self.cache.get(session_id) {
            Some(c) if c.path.is_file() => Some(c.path.clone()),
            _ => locate(&self.projects_dir, session_id, cwd),
        };
        if let Some(marks) = path.and_then(|p| self.marks_at(session_id, &p).ok()) {
            return marks;
        }
        self.cache
            .get(session_id)
            .map(|c| c.marks.clone())
            .unwrap_or_default()
    }

    /// The marks of a session whose transcript is at `path` (for example the
    /// `transcript_path` of a spool event). Reads only what was appended since
    /// the previous call for this session; starts over if the path changed or
    /// the file shrank.
    ///
    /// The path comes from a spool file, so it is not trusted: anything but a
    /// regular `*.jsonl` file (a `.key` file next to the registry, a device, a
    /// pipe) is refused without being opened.
    pub fn marks_at(
        &mut self,
        session_id: &str,
        path: &Path,
    ) -> Result<TranscriptMarks, LocalError> {
        let is_jsonl = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"));
        if !is_jsonl || !std::fs::metadata(path)?.is_file() {
            return Err(LocalError::Parse(format!(
                "not a transcript file: {}",
                path.display()
            )));
        }
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let entry = self
            .cache
            .entry(session_id.to_string())
            .or_insert_with(|| Cached {
                path: path.to_path_buf(),
                scanned: 0,
                marks: TranscriptMarks::default(),
            });
        if entry.path != path || len < entry.scanned {
            entry.path = path.to_path_buf();
            entry.scanned = 0;
            entry.marks = TranscriptMarks::default();
        }
        if len > entry.scanned {
            let scan = scan_region(&mut file, entry.scanned, len, self.limits)?;
            entry.scanned = scan.complete_end;
            scan.apply_to(&mut entry.marks);
        }
        Ok(entry.marks.clone())
    }

    /// Drops the cache of one session.
    pub fn forget(&mut self, session_id: &str) {
        self.cache.remove(session_id);
    }

    /// Keeps the cache only for sessions for which `keep` returns true; call it
    /// with the live session ids so ended sessions do not accumulate.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.cache.retain(|id, _| keep(id));
    }

    /// Number of sessions currently cached.
    pub fn cached_sessions(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    const SID: &str = "c41d8e7f-5a2b-4e93-a6c0-9f1b3d7e5c03";

    fn ai_title(t: &str) -> String {
        format!(r#"{{"type":"ai-title","aiTitle":"{t}","sessionId":"{SID}"}}"#)
    }
    fn custom_title(t: &str) -> String {
        format!(r#"{{"type":"custom-title","customTitle":"{t}","sessionId":"{SID}"}}"#)
    }
    fn agent_color(c: &str) -> String {
        format!(r#"{{"type":"agent-color","agentColor":"{c}","sessionId":"{SID}"}}"#)
    }
    /// A conversation line shaped like the real ones, with filler content.
    fn user_line(text: &str) -> String {
        format!(
            r#"{{"parentUuid":null,"isSidechain":false,"type":"user","message":{{"role":"user","content":"{text}"}},"uuid":"u-1","timestamp":"2026-10-13T12:00:00.000Z","sessionId":"{SID}"}}"#
        )
    }
    fn assistant_line(model: &str, input: u64, created: u64, read: u64, sidechain: bool) -> String {
        format!(
            r#"{{"parentUuid":"p","isSidechain":{sidechain},"type":"assistant","message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"secret conversation text with \"usage\":{{\"input_tokens\":999}}"}}],"usage":{{"input_tokens":{input},"cache_creation_input_tokens":{created},"cache_read_input_tokens":{read},"output_tokens":12}}}},"sessionId":"{SID}"}}"#
        )
    }

    #[test]
    fn usage_comes_from_the_newest_main_thread_assistant_message() {
        let bytes = lines(&[
            assistant_line("claude-sonnet-5-5", 1, 2, 3, false),
            ai_title("Port the formula"),
            assistant_line("claude-opus-5-5", 2, 391, 552_961, false),
            // A subagent's message is not the session's context.
            assistant_line("claude-haiku-5-5", 5, 5, 5, true),
            user_line("thanks"),
        ]);
        let m = marks_of(&bytes, LIMITS);
        assert_eq!(m.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(m.context_tokens, Some(2 + 391 + 552_961));
        assert_eq!(m.title.as_deref(), Some("Port the formula"));
    }

    #[test]
    fn usage_is_read_from_raw_bytes_and_skips_synthetic_models() {
        // The conversation text mentions "usage" with 999 tokens, but inside a
        // JSON string its quotes are escaped, so only the real object matches.
        let line = assistant_line("claude-opus-5-5", 7, 0, 0, false);
        assert_eq!(
            parse_mark_line(line.as_bytes()),
            Some(Mark::Usage {
                model: Some("claude-opus-5-5".into()),
                context_tokens: Some(7)
            }),
        );
        let synthetic = assistant_line("<synthetic>", 0, 0, 0, false);
        match parse_mark_line(synthetic.as_bytes()) {
            Some(Mark::Usage { model, .. }) => assert_eq!(model, None),
            other => panic!("expected usage, got {other:?}"),
        }
        assert_eq!(parse_mark_line(user_line("no usage here").as_bytes()), None);
    }

    fn lines(ls: &[String]) -> Vec<u8> {
        let mut out = ls.join("\n").into_bytes();
        out.push(b'\n');
        out
    }
    fn scan(bytes: &[u8], limits: Limits) -> RegionScan {
        scan_region(&mut Cursor::new(bytes), 0, bytes.len() as u64, limits).unwrap()
    }
    fn marks_of(bytes: &[u8], limits: Limits) -> TranscriptMarks {
        let mut m = TranscriptMarks::default();
        scan(bytes, limits).apply_to(&mut m);
        m
    }
    const TINY: Limits = Limits {
        chunk: 7,
        max_scan: 1 << 20,
        max_line: 4096,
    };

    #[test]
    fn slug_matches_claude_code_directory_names() {
        assert_eq!(
            project_slug(r"c:\Users\ada\OneDrive - Acme Inc\Projects\demo-app-2"),
            "c--Users-ada-OneDrive---Acme-Inc-Projects-demo-app-2"
        );
        assert_eq!(
            project_slug(r"C:\a_b\c.d"),
            "C--a-b-c-d",
            "case is preserved"
        );
        assert_eq!(project_slug("/home/me/my repo"), "-home-me-my-repo");
        assert_eq!(
            project_slug("/tmp/añ😀"),
            "-tmp-a---",
            "non-ASCII by UTF-16 unit"
        );
    }

    #[test]
    fn path_for_joins_slug_and_session_file() {
        let p = path_in(Path::new("/p"), SID, r"c:\x y").unwrap();
        assert_eq!(
            p,
            Path::new("/p").join("c--x-y").join(format!("{SID}.jsonl"))
        );
    }

    #[test]
    fn crafted_session_ids_build_no_path() {
        let dir = tempfile::tempdir().unwrap();
        // A file outside the projects folder a crafted id would reach.
        std::fs::write(dir.path().join("secret.jsonl"), lines(&[ai_title("leak")])).unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("p")).unwrap();
        for id in [
            "../secret",
            r"..\secret",
            "/etc/passwd",
            "",
            ".hidden",
            "a/b",
            "x\0y",
        ] {
            assert_eq!(path_in(&projects, id, r"c:\x"), None, "{id:?}");
            assert_eq!(locate(&projects, id, r"c:\x"), None, "{id:?}");
        }
        let mut reader = TranscriptReader::with_projects_dir(&projects);
        assert_eq!(reader.marks("../secret", "p"), TranscriptMarks::default());
    }

    #[test]
    fn parses_each_mark_type() {
        assert_eq!(
            parse_mark_line(ai_title("Fix tray").as_bytes()),
            Some(Mark::AiTitle(Some("Fix tray".into())))
        );
        assert_eq!(
            parse_mark_line(custom_title("release").as_bytes()),
            Some(Mark::CustomTitle(Some("release".into())))
        );
        assert_eq!(
            parse_mark_line(agent_color("green").as_bytes()),
            Some(Mark::AgentColor(Some("green".into())))
        );
        assert_eq!(
            parse_mark_line(custom_title("").as_bytes()),
            Some(Mark::CustomTitle(None))
        );
    }

    #[test]
    fn ignores_conversation_lines_even_when_they_quote_markers() {
        assert_eq!(parse_mark_line(user_line("hello").as_bytes()), None);
        // Quoted inside a JSON string, the marker's quotes are escaped.
        let quoting = user_line(r#"look for \"type\":\"ai-title\" records"#);
        assert_eq!(parse_mark_line(quoting.as_bytes()), None);
        // A nested object with the marker parses, but its top-level type is "user".
        let nested = format!(
            r#"{{"type":"user","toolUseResult":{{"type":"ai-title","aiTitle":"x"}},"sessionId":"{SID}"}}"#
        );
        assert_eq!(parse_mark_line(nested.as_bytes()), None);
        assert_eq!(
            parse_mark_line(b"{\"type\":\"ai-title\""),
            None,
            "truncated JSON"
        );
    }

    #[test]
    fn skips_long_lines_without_parsing() {
        let long = format!(
            r#"{{"type":"ai-title","aiTitle":"{}"}}"#,
            "x".repeat(MAX_LINE_BYTES)
        );
        assert_eq!(parse_mark_line(long.as_bytes()), None);
    }

    #[test]
    fn the_last_record_of_each_kind_wins() {
        let bytes = lines(&[
            ai_title("first"),
            agent_color("red"),
            user_line("a"),
            custom_title("mine"),
            ai_title("second"),
            user_line("b"),
            agent_color("green"),
            user_line("c"),
        ]);
        for limits in [LIMITS, TINY] {
            let m = marks_of(&bytes, limits);
            assert_eq!(m.title.as_deref(), Some("second"));
            assert_eq!(m.custom_title.as_deref(), Some("mine"));
            assert_eq!(m.color.as_deref(), Some("green"));
        }
    }

    #[test]
    fn handles_crlf_and_missing_marks() {
        let bytes = format!("{}\r\n{}\r\n", user_line("a"), ai_title("t")).into_bytes();
        let m = marks_of(&bytes, TINY);
        assert_eq!(m.title.as_deref(), Some("t"));
        assert_eq!(m.custom_title, None);
        assert_eq!(m.color, None);
        assert_eq!(marks_of(b"", LIMITS), TranscriptMarks::default());
    }

    #[test]
    fn first_line_without_preceding_newline_is_read() {
        let bytes = format!("{}\n", custom_title("only")).into_bytes();
        assert_eq!(marks_of(&bytes, TINY).custom_title.as_deref(), Some("only"));
    }

    #[test]
    fn a_line_still_being_written_is_not_counted_as_scanned() {
        let mut bytes = lines(&[user_line("a")]);
        let complete = bytes.len() as u64;
        bytes.extend_from_slice(br#"{"type":"ai-title","aiTi"#);
        let s = scan(&bytes, TINY);
        assert_eq!(s.complete_end, complete);
        assert_eq!(s.title, None);

        // A finished mark without its newline yet is used, but re-read later.
        let mut bytes = lines(&[user_line("a")]);
        bytes.extend_from_slice(ai_title("tail").as_bytes());
        let s = scan(&bytes, TINY);
        assert_eq!(s.complete_end, complete);
        assert_eq!(s.title, Some(Some("tail".into())));
    }

    #[test]
    fn long_lines_spanning_chunks_are_skipped_and_neighbours_still_found() {
        let limits = Limits {
            chunk: 64,
            max_scan: 1 << 20,
            max_line: 200,
        };
        let long = format!(r#"{{"type":"ai-title","aiTitle":"{}"}}"#, "y".repeat(500));
        let bytes = lines(&[ai_title("before"), long, user_line("z")]);
        assert_eq!(marks_of(&bytes, limits).title.as_deref(), Some("before"));
    }

    #[test]
    fn scan_stops_at_the_byte_cap() {
        let limits = Limits {
            chunk: 32,
            max_scan: 400,
            max_line: 4096,
        };
        let mut ls = vec![ai_title("too-old")];
        ls.extend((0..20).map(|i| user_line(&format!("filler {i}"))));
        ls.push(agent_color("blue"));
        let bytes = lines(&ls);
        assert!(bytes.len() > 400);
        let m = marks_of(&bytes, limits);
        assert_eq!(m.title, None, "beyond the cap");
        assert_eq!(m.color.as_deref(), Some("blue"));
    }

    #[test]
    fn a_cleared_mark_overrides_the_cache() {
        let mut m = TranscriptMarks {
            custom_title: Some("old".into()),
            title: Some("kept".into()),
            color: None,
            ..Default::default()
        };
        scan(&lines(&[custom_title("")]), LIMITS).apply_to(&mut m);
        assert_eq!(m.custom_title, None);
        assert_eq!(m.title.as_deref(), Some("kept"));
    }

    #[test]
    fn reader_is_incremental_and_falls_back_to_cached_marks() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = r"c:\Users\dev\proj";
        let path = path_in(dir.path(), SID, cwd).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            lines(&[user_line("a"), ai_title("one"), agent_color("red")]),
        )
        .unwrap();

        let mut reader = TranscriptReader::with_projects_dir(dir.path());
        reader.limits = TINY;
        let m = reader.marks(SID, cwd);
        assert_eq!(m.title.as_deref(), Some("one"));
        assert_eq!(m.color.as_deref(), Some("red"));
        let scanned = reader.cache[SID].scanned;
        assert_eq!(scanned, std::fs::metadata(&path).unwrap().len());

        // Append a new title only: the color comes from the cache.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(lines(&[user_line("b"), ai_title("two")]).as_slice())
            .unwrap();
        drop(f);
        let m = reader.marks(SID, cwd);
        assert_eq!(m.title.as_deref(), Some("two"));
        assert_eq!(m.color.as_deref(), Some("red"));

        // Unchanged file: nothing new is read and the marks stay.
        assert_eq!(reader.marks(SID, cwd), m);

        // A shrunk (rewritten) file is scanned from scratch.
        std::fs::write(&path, lines(&[custom_title("fresh")])).unwrap();
        let m = reader.marks(SID, cwd);
        assert_eq!(m.custom_title.as_deref(), Some("fresh"));
        assert_eq!(m.title, None);
        assert_eq!(m.color, None);
    }

    #[test]
    fn reader_only_reads_appended_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, lines(&[ai_title("one")])).unwrap();
        let mut reader = TranscriptReader::with_projects_dir(dir.path());
        reader.marks_at(SID, &path).unwrap();
        // Corrupt the already-scanned region: if it were re-read the title would change.
        let mut bytes = std::fs::read(&path).unwrap();
        let tamper = ai_title("one").replace("one", "ONE");
        bytes[..tamper.len()].copy_from_slice(tamper.as_bytes());
        bytes.extend_from_slice(&lines(&[user_line("x")]));
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            reader.marks_at(SID, &path).unwrap().title.as_deref(),
            Some("one")
        );
    }

    #[test]
    fn marks_at_refuses_anything_but_a_jsonl_file() {
        let dir = tempfile::tempdir().unwrap();
        // A key file holding a line that would parse as a mark if it were read.
        let key = dir.path().join("21728.abc.key");
        std::fs::write(&key, lines(&[ai_title("secret")])).unwrap();
        let folder = dir.path().join("looks-like.jsonl");
        std::fs::create_dir(&folder).unwrap();
        let mut reader = TranscriptReader::with_projects_dir(dir.path());
        assert!(reader.marks_at(SID, &key).is_err());
        assert!(reader.marks_at(SID, &folder).is_err());
        assert_eq!(reader.cached_sessions(), 0);
    }

    #[test]
    fn locate_falls_back_to_any_project_directory() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("shortened-slug-abc123");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join(format!("{SID}.jsonl")),
            lines(&[ai_title("found")]),
        )
        .unwrap();
        assert_eq!(
            locate(dir.path(), SID, r"c:\elsewhere"),
            Some(other.join(format!("{SID}.jsonl")))
        );
        assert_eq!(locate(dir.path(), "missing", r"c:\elsewhere"), None);

        let mut reader = TranscriptReader::with_projects_dir(dir.path());
        assert_eq!(
            reader.marks(SID, r"c:\elsewhere").title.as_deref(),
            Some("found")
        );
        assert_eq!(reader.marks("missing", r"c:\x"), TranscriptMarks::default());
        reader.retain(|id| id != SID);
        assert_eq!(reader.cached_sessions(), 0);
    }

    #[test]
    #[ignore = "reads the real ~/.claude/projects and sessions"]
    fn live_transcripts() {
        let files = crate::registry::read_dir(&crate::registry::sessions_dir());
        let mut reader = TranscriptReader::new();
        let (mut located, mut title, mut custom, mut color) = (0, 0, 0, 0);
        let started = std::time::Instant::now();
        for f in &files {
            let cwd = f.cwd.as_deref().unwrap_or_default();
            if locate(&reader.projects_dir, &f.session_id, cwd).is_some() {
                located += 1;
            }
            let m = reader.marks(&f.session_id, cwd);
            title += m.title.is_some() as usize;
            custom += m.custom_title.is_some() as usize;
            color += m.color.is_some() as usize;
        }
        let first = started.elapsed();
        for f in &files {
            reader.marks(&f.session_id, f.cwd.as_deref().unwrap_or_default());
        }
        println!(
            "transcripts: {} sessions, {} located; with title {}, customTitle {}, color {}; \
             first pass {} ms, second pass {} ms",
            files.len(),
            located,
            title,
            custom,
            color,
            first.as_millis(),
            started.elapsed().saturating_sub(first).as_millis(),
        );
    }
}
