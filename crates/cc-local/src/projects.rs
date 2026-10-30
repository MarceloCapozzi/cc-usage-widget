//! Token usage per project, read from Claude Code's transcripts: what the
//! widget's usage view lists for this machine, and what `cc-publish` sends
//! for the machine it runs on.
//!
//! `~/.claude/projects/<project>/` holds one JSON per line; response lines
//! carry `message.usage` with that turn's tokens. Prices are not applied
//! here: the widget prices the counters with its own table. The same pass
//! sums every token per local day and hour, for the activity calendar.
//!
//! Each API response is counted once. Claude Code writes one line per
//! content block of a response (thinking, text, tool use), each repeating
//! the response's `message.id`, `requestId` and usage, and a resumed
//! session can copy earlier lines into a new transcript. So the scan keeps
//! the counters of every response it has seen, keyed by that pair, across
//! all the files: a repeated line adds only what it reports above them (the
//! largest value of each counter wins), usually nothing. Lines with neither
//! field are counted as they are. The files are read oldest first (by last
//! modification, then path), so a response belongs to the transcript that
//! first wrote it, which also counts as the session: a resumed copy adds
//! only what is new, whatever the folder listing's order.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cc_core::projects::Tokens;

/// Folder levels walked under the projects folder. Real trees go five deep
/// (`<project>/<session>/subagents/workflows/<run>/`); the limit only stops
/// a pathological tree from recursing without end.
pub const MAX_DEPTH: usize = 8;

/// Only lines containing this are parsed: every response line with tokens
/// has a `message.usage` object.
const USAGE_NEEDLE: &[u8] = b"\"usage\"";

/// Lines longer than this are skipped unparsed, so a huge line is never
/// held whole. A response line carries one turn's output, well under a MiB;
/// what runs longer is a tool result or a pasted image, which carry no
/// usage. (The title reader of [`crate::transcript`] keeps a far smaller
/// limit: the records it wants are a few hundred bytes.)
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Placeholder for a project or model name that could not be determined. It
/// can be rendered in the flyout (project list, models without pricing), so it
/// is user-facing text.
pub const UNKNOWN: &str = "(unknown)";

/// One project's counters over the scanned window.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectScan {
    /// The transcript folder under `~/.claude/projects` (Claude Code's
    /// encoding of the project path). Stable, so it identifies the project.
    pub key: String,
    /// Readable name: the last component of the project's root folder.
    pub name: String,
    /// The project's root folder (the shortest `cwd` seen).
    pub path: String,
    /// All the project's tokens in the window.
    pub tokens: Tokens,
    /// The same tokens per model id, for pricing.
    pub by_model: HashMap<String, Tokens>,
    /// Transcripts directly in the folder with activity in the window.
    pub sessions: u32,
    /// Unix seconds of the newest activity seen.
    pub last_seen: Option<i64>,
}

/// What one pass over the transcripts found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scan {
    /// Projects, highest usage first.
    pub projects: Vec<ProjectScan>,
    /// Tokens per local calendar day (`YYYY-MM-DD`) and local hour, every
    /// model and project together.
    pub days: BTreeMap<String, [u64; 24]>,
}

/// `~/.claude/projects`, or `projects` under `CLAUDE_CONFIG_DIR` when set,
/// like every other Claude Code folder ([`crate::claude_dir`]).
pub fn projects_dir() -> PathBuf {
    crate::claude_dir().join("projects")
}

/// Walks the transcripts under `dir` and aggregates by project, highest
/// usage first.
///
/// `since_days` bounds the window; `None` reads everything. Parsing is
/// lenient: an unreadable line is skipped instead of aborting, because the
/// format changes between Claude Code versions and an old transcript should
/// not break the report.
pub fn scan_dir(dir: &Path, since_days: Option<i64>) -> Vec<ProjectScan> {
    scan_all(dir, since_days).projects
}

/// [`scan_dir`], plus the tokens per local day and hour.
pub fn scan_all(dir: &Path, since_days: Option<i64>) -> Scan {
    let mut days: BTreeMap<String, [u64; 24]> = BTreeMap::new();
    let cutoff = since_days.map(|d| chrono::Utc::now() - chrono::Duration::days(d));
    let mut by_project: HashMap<String, ProjectScan> = HashMap::new();
    // Every cwd seen per project: the shortest one is the root (see below).
    let mut roots: HashMap<String, String> = HashMap::new();
    // What each response counted so far, by its key (see the module docs).
    let mut responses: HashMap<String, Tokens> = HashMap::new();

    // A transcript last written before the cut-off has no line inside the
    // window (lines are appended as they happen), so it is not even opened.
    let file_cutoff = since_days.map(|d| {
        let secs = u64::try_from(d.max(0)).unwrap_or(0).saturating_mul(86_400);
        SystemTime::now()
            .checked_sub(Duration::from_secs(secs))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });

    for file in jsonl_files(dir, file_cutoff) {
        let Ok(handle) = File::open(&file) else {
            continue;
        };

        // The grouping key is the transcript's FOLDER, not the `cwd`.
        //
        // Claude Code stores all sessions of a project under the same folder in
        // ~/.claude/projects/, but each line's `cwd` changes when the session
        // enters a subdirectory. Grouping by `cwd` split one project into
        // several ("web-dashboard" and "src-tauri" as if they were different
        // things) and threw off the session count.
        //
        // It is the folder right under `dir`, not the file's own folder:
        // subagents and workflow agents write their transcripts deeper
        // (`<project>/<session>/subagents/[workflows/<run>/]agent-*.jsonl`),
        // and keyed by their own folder each run showed up as one more copy
        // of the project. Their tokens count for the project; only the
        // transcripts directly in the folder are sessions.
        let rel = file.strip_prefix(dir).ok();
        let key = rel
            .and_then(|r| r.components().next())
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_else(|| UNKNOWN.into());
        let is_session = rel.is_some_and(|r| r.components().count() == 2);

        let mut contributed_tokens = false;

        // Line by line: transcripts reach hundreds of MiB, and most lines
        // (tool results, user turns) carry no usage, so they are skipped
        // before any JSON is parsed. A line over the limit is read past
        // without being kept.
        let mut reader = BufReader::new(handle);
        let mut line = Vec::new();
        // Stops at the end of the file, or at a read error mid-file: what was
        // counted is kept.
        while let Ok(Some(fits)) = read_line_bounded(&mut reader, &mut line, MAX_LINE_BYTES) {
            if !fits || memchr::memmem::find(&line, USAGE_NEEDLE).is_none() {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue; // a corrupt line: ignored
            };

            let ts = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc));
            if let (Some(c), Some(t)) = (cutoff, ts) {
                if t < c {
                    continue;
                }
            }

            let Some(usage) = v.pointer("/message/usage") else {
                continue;
            };
            let tokens = Tokens {
                input: field(usage, "input_tokens"),
                output: field(usage, "output_tokens"),
                cache_creation: field(usage, "cache_creation_input_tokens"),
                cache_read: field(usage, "cache_read_input_tokens"),
            };
            if tokens.total() == 0 {
                continue;
            }
            // Another line of a response already counted adds only what it
            // reports above it.
            let tokens = match response_key(&v) {
                None => tokens,
                Some(key) => match responses.entry(key) {
                    Entry::Vacant(slot) => *slot.insert(tokens),
                    Entry::Occupied(mut slot) => {
                        let more = raise(slot.get_mut(), &tokens);
                        if more.total() == 0 {
                            continue;
                        }
                        more
                    }
                },
            };

            let cwd = v
                .get("cwd")
                .and_then(|c| c.as_str())
                .unwrap_or(UNKNOWN)
                .to_string();
            let model = v
                .pointer("/message/model")
                .and_then(|m| m.as_str())
                .unwrap_or(UNKNOWN)
                .to_string();

            // The project root is the shortest `cwd` ever seen: subdirectories
            // are always longer than the folder that contains them, so the
            // minimum by length is the root.
            roots
                .entry(key.clone())
                .and_modify(|current| {
                    if cwd.len() < current.len() {
                        *current = cwd.clone();
                    }
                })
                .or_insert_with(|| cwd.clone());

            if let Some(t) = ts {
                use chrono::Timelike;
                let local = t.with_timezone(&chrono::Local);
                let day = days
                    .entry(local.format("%Y-%m-%d").to_string())
                    .or_insert([0; 24]);
                let hour = &mut day[local.hour() as usize];
                *hour = hour.saturating_add(tokens.total());
            }

            let entry = by_project
                .entry(key.clone())
                .or_insert_with(|| ProjectScan {
                    key: key.clone(),
                    name: String::new(), // resolved at the end, once the root is known
                    path: String::new(),
                    tokens: Tokens::default(),
                    by_model: HashMap::new(),
                    sessions: 0,
                    last_seen: None,
                });
            entry.tokens.add(&tokens);
            entry.by_model.entry(model).or_default().add(&tokens);
            let at = ts.map(|t| t.timestamp());
            if at > entry.last_seen {
                entry.last_seen = at;
            }

            // One session per FILE, counted only once even if the `cwd` changes
            // from line to line within the same transcript.
            if !contributed_tokens {
                contributed_tokens = true;
                if is_session {
                    entry.sessions += 1;
                }
            }
        }
    }

    let mut projects: Vec<ProjectScan> = by_project
        .into_values()
        .map(|mut p| {
            let root = roots.get(&p.key).cloned().unwrap_or_else(|| p.key.clone());
            p.name = short_name(&root);
            p.path = root;
            p
        })
        .collect();
    // Highest usage first, then by key so equal ones keep a stable order.
    projects.sort_by(|a, b| {
        b.tokens
            .total()
            .cmp(&a.tokens.total())
            .then_with(|| a.key.cmp(&b.key))
    });
    Scan { projects, days }
}

/// Reads the next line of `reader` into `line`, without its `\n` (a `\r`
/// before it stays, as with [`BufRead::split`]). Returns `None` at the end
/// of the input, `Some(true)` for a line of at most `max` bytes, and
/// `Some(false)` for a longer one: it is read past, and `line` is left
/// empty instead of growing with it.
fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut started = false;
    let mut too_long = false;
    loop {
        let buf = match reader.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok(started.then_some(!too_long));
        }
        started = true;
        let (part, used, done) = match memchr::memchr(b'\n', buf) {
            Some(i) => (&buf[..i], i + 1, true),
            None => (buf, buf.len(), false),
        };
        if !too_long {
            if line.len() + part.len() > max {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(part);
            }
        }
        reader.consume(used);
        if done {
            return Ok(Some(!too_long));
        }
    }
}

fn field(v: &serde_json::Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// The API response a usage line belongs to: `message.id` and `requestId`
/// together. `None` when the line has neither.
fn response_key(v: &serde_json::Value) -> Option<String> {
    let id = v
        .pointer("/message/id")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let request = v.get("requestId").and_then(|x| x.as_str()).unwrap_or("");
    if id.is_empty() && request.is_empty() {
        return None;
    }
    Some(format!("{id}\n{request}"))
}

/// Raises each counter of `counted` to at least the one in `seen`, and
/// returns by how much each went up.
fn raise(counted: &mut Tokens, seen: &Tokens) -> Tokens {
    let up = |have: &mut u64, now: u64| {
        let more = now.saturating_sub(*have);
        *have = (*have).max(now);
        more
    };
    Tokens {
        input: up(&mut counted.input, seen.input),
        output: up(&mut counted.output, seen.output),
        cache_creation: up(&mut counted.cache_creation, seen.cache_creation),
        cache_read: up(&mut counted.cache_read, seen.cache_read),
    }
}

/// `C:\...\Projects\web-dashboard` → `web-dashboard`.
pub fn short_name(cwd: &str) -> String {
    cwd.replace('/', "\\")
        .trim_end_matches('\\')
        .rsplit('\\')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cwd)
        .to_string()
}

/// Every `*.jsonl` under `dir`, at most [`MAX_DEPTH`] folders down, skipping
/// files last modified before `cutoff`, oldest first: by last modification,
/// then by path (see the module docs). The folder listing's own order is
/// alphabetical on NTFS and arbitrary elsewhere, so it is never relied on.
/// Symbolic links, junctions and other reparse points are not followed: a
/// link pointing back up the tree (or out of it) would otherwise be walked
/// again and again.
fn jsonl_files(dir: &Path, cutoff: Option<SystemTime>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, 0, cutoff, &mut out);
    out.sort();
    out.into_iter().map(|(_, path)| path).collect()
}

/// Collects (last modification, path) pairs; a file whose time cannot be
/// read sorts first.
fn walk(
    dir: &Path,
    depth: usize,
    cutoff: Option<SystemTime>,
    out: &mut Vec<(SystemTime, PathBuf)>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        // symlink_metadata: the entry itself, never what a link points to.
        let Ok(meta) = fs::symlink_metadata(&p) else {
            continue;
        };
        if is_link(&meta) {
            continue;
        }
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                walk(&p, depth + 1, cutoff, out);
            }
        } else if meta.is_file() && p.extension().is_some_and(|x| x == "jsonl") {
            let modified = meta.modified().ok();
            let stale = match (cutoff, modified) {
                (Some(c), Some(m)) => m < c,
                _ => false,
            };
            if !stale {
                out.push((modified.unwrap_or(SystemTime::UNIX_EPOCH), p));
            }
        }
    }
}

/// A symbolic link, or on Windows any reparse point (junctions and mount
/// points included, which `is_symlink` does not always report).
fn is_link(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(cwd: &str, model: &str, input: u64) -> String {
        format!(
            r#"{{"timestamp":"{}","cwd":"{}","message":{{"model":"{}","usage":{{"input_tokens":{},"output_tokens":0}}}}}}"#,
            chrono::Utc::now().to_rfc3339(),
            cwd.replace('\\', "\\\\"),
            model,
            input
        )
    }

    /// A line of one API response, as Claude Code writes one per content block.
    fn response_line(message_id: &str, request_id: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"timestamp":"{}","cwd":"C:\\p\\app","requestId":"{request_id}","message":{{"id":"{message_id}","model":"m","content":[{{"type":"text","text":"x"}}],"usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":1000}}}}}}"#,
            chrono::Utc::now().to_rfc3339(),
        )
    }

    #[test]
    fn the_content_blocks_of_one_response_count_once() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        // Thinking, text and tool use of one response, then another response.
        let block = response_line("msg_01", "req_01", 10, 50);
        let text = format!(
            "{block}\n{block}\n{block}\n{}\n",
            response_line("msg_02", "req_02", 5, 7)
        );
        std::fs::write(project.join("s1.jsonl"), text).unwrap();
        let scan = scan_all(dir.path(), None);
        let p = &scan.projects[0];
        assert_eq!(
            p.tokens,
            Tokens {
                input: 15,
                output: 57,
                cache_creation: 0,
                cache_read: 2000
            }
        );
        assert_eq!(p.by_model["m"].total(), 2072);
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(scan.days[&today].iter().sum::<u64>(), 2072);
        assert_eq!(p.sessions, 1);
    }

    /// Sets `path`'s last modification to `secs_ago` seconds before now.
    fn set_age(path: &Path, secs_ago: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[test]
    fn a_response_copied_into_another_transcript_counts_once() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        let original = project.join("s1.jsonl");
        std::fs::write(&original, response_line("msg_01", "req_01", 10, 50)).unwrap();
        set_age(&original, 3600);
        // A resumed session starts with a copy of the old line, then goes on.
        let resumed = format!(
            "{}\n{}\n",
            response_line("msg_01", "req_01", 10, 50),
            response_line("msg_03", "req_03", 1, 2)
        );
        let copy = project.join("s2.jsonl");
        std::fs::write(&copy, resumed).unwrap();
        set_age(&copy, 60);
        let p = &scan_dir(dir.path(), None)[0];
        assert_eq!(
            p.tokens,
            Tokens {
                input: 11,
                output: 52,
                cache_creation: 0,
                cache_read: 2000
            }
        );
        assert_eq!(p.sessions, 2);

        // A copy alone adds no session: nothing in it is new.
        std::fs::write(&copy, response_line("msg_01", "req_01", 10, 50)).unwrap();
        set_age(&copy, 60);
        let p = &scan_dir(dir.path(), None)[0];
        assert_eq!((p.tokens.total(), p.sessions), (1060, 1));
    }

    /// The transcript written first owns its responses, even when the
    /// resumed one sorts before it by name (as it does in an NTFS listing).
    #[test]
    fn the_original_transcript_keeps_its_session_whatever_the_names() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        let original = project.join("f0e1d2c3.jsonl");
        std::fs::write(&original, response_line("msg_01", "req_01", 10, 50)).unwrap();
        set_age(&original, 3600);
        // Only a copy of the original's response: it adds nothing.
        let copy = project.join("0a1b2c3d.jsonl");
        std::fs::write(&copy, response_line("msg_01", "req_01", 10, 50)).unwrap();
        set_age(&copy, 60);
        for _ in 0..3 {
            let p = &scan_dir(dir.path(), None)[0];
            assert_eq!((p.tokens.total(), p.sessions), (1060, 1));
        }
        // The copy's own new response makes it a session too.
        let resumed = format!(
            "{}\n{}\n",
            response_line("msg_01", "req_01", 10, 50),
            response_line("msg_03", "req_03", 1, 2)
        );
        std::fs::write(&copy, resumed).unwrap();
        set_age(&copy, 60);
        let p = &scan_dir(dir.path(), None)[0];
        assert_eq!((p.tokens.total(), p.sessions), (2063, 2));

        // The files come oldest first, then by path for equal times.
        let same = project.join("e.jsonl");
        std::fs::write(&same, "").unwrap();
        let at = SystemTime::now() - Duration::from_secs(600);
        for f in [&copy, &same] {
            std::fs::File::options()
                .write(true)
                .open(f)
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
        assert_eq!(jsonl_files(dir.path(), None), [original, copy, same]);
    }

    #[test]
    fn repeated_lines_that_disagree_keep_the_largest_counters() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        let text = format!(
            "{}\n{}\n{}\n",
            response_line("msg_01", "req_01", 10, 1),
            response_line("msg_01", "req_01", 10, 50),
            response_line("msg_01", "req_01", 10, 20)
        );
        std::fs::write(project.join("s1.jsonl"), text).unwrap();
        let p = &scan_dir(dir.path(), None)[0];
        assert_eq!(
            p.tokens,
            Tokens {
                input: 10,
                output: 50,
                cache_creation: 0,
                cache_read: 1000
            }
        );
    }

    #[test]
    fn lines_without_ids_are_each_counted() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        let same = line(r"C:\p\app", "m", 4);
        std::fs::write(project.join("s1.jsonl"), format!("{same}\n{same}\n")).unwrap();
        assert_eq!(scan_dir(dir.path(), None)[0].tokens.total(), 8);
        // One id alone still identifies the response.
        let v: serde_json::Value =
            serde_json::from_str(r#"{"requestId":"req_9","message":{}}"#).unwrap();
        assert_eq!(response_key(&v).as_deref(), Some("\nreq_9"));
    }

    #[test]
    fn short_name_from_the_cwd() {
        assert_eq!(
            short_name(r"C:\Users\x\Projects\web-dashboard"),
            "web-dashboard"
        );
        assert_eq!(short_name(r"C:\Users\x\Projects\something\"), "something");
        assert_eq!(short_name("/home/x/project"), "project");
    }

    #[test]
    fn subagent_transcripts_belong_to_their_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("s1.jsonl"), line(r"C:\p\app", "m", 100)).unwrap();
        for sub in ["0badf00d/subagents", "0badf00d/subagents/workflows/wf_1"] {
            let d = project.join(sub);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("agent-a.jsonl"), line(r"C:\p\app", "m", 10)).unwrap();
        }
        let scans = scan_dir(dir.path(), None);
        assert_eq!(scans.len(), 1);
        let p = &scans[0];
        assert_eq!((p.key.as_str(), p.name.as_str()), ("c--p-app", "app"));
        assert_eq!(p.tokens.total(), 120);
        assert_eq!(p.sessions, 1);
        assert!(p.last_seen.is_some());

        // Every token lands in today's row, subagents included.
        let scan = scan_all(dir.path(), None);
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(scan.days.len(), 1);
        assert_eq!(scan.days[&today].iter().sum::<u64>(), 120);
    }

    #[test]
    fn lines_without_usage_and_crlf_endings_are_fine() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        let text = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":\"hi\"}}}}\r\nnot json\r\n{}\r\n{}",
            line(r"C:\p\app", "m", 7),
            line(r"C:\p\app", "m", 3)
        );
        std::fs::write(project.join("s1.jsonl"), text).unwrap();
        let scans = scan_dir(dir.path(), Some(30));
        assert_eq!(scans[0].tokens.total(), 10);
        assert_eq!(scans[0].sessions, 1);
    }

    #[test]
    fn the_bounded_reader_skips_long_lines_and_keeps_the_rest() {
        use std::io::Cursor;
        let text = b"short\r\n0123456789abcdef\nexactly10!\n\nlast";
        // A tiny buffer, so lines span several fills.
        let mut reader = BufReader::with_capacity(4, Cursor::new(&text[..]));
        let mut line = Vec::new();
        let mut got: Vec<(bool, String)> = Vec::new();
        while let Some(fits) = read_line_bounded(&mut reader, &mut line, 10).unwrap() {
            assert!(line.len() <= 10, "never held past the limit");
            got.push((fits, String::from_utf8(line.clone()).unwrap()));
        }
        let expected: Vec<(bool, String)> = [
            (true, "short\r"),
            (false, ""),
            (true, "exactly10!"),
            (true, ""),
            (true, "last"),
        ]
        .iter()
        .map(|(f, s)| (*f, s.to_string()))
        .collect();
        assert_eq!(got, expected);

        // A trailing newline ends the last line; nothing comes after it.
        let mut reader = BufReader::new(Cursor::new(&b"a\n"[..]));
        assert_eq!(
            read_line_bounded(&mut reader, &mut line, 10).unwrap(),
            Some(true)
        );
        assert_eq!(read_line_bounded(&mut reader, &mut line, 10).unwrap(), None);
        let mut empty = BufReader::new(Cursor::new(&b""[..]));
        assert_eq!(read_line_bounded(&mut empty, &mut line, 10).unwrap(), None);
    }

    #[test]
    fn a_line_over_the_limit_is_skipped_and_the_next_ones_count() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        // A usage line that would count, padded past the limit: skipped.
        let usage = line(r"C:\p\app", "m", 1000);
        let huge = format!(
            "{},\"pad\":\"{}\"}}",
            &usage[..usage.len() - 1],
            "x".repeat(MAX_LINE_BYTES)
        );
        let text = format!(
            "{}\n{huge}\n{}\n",
            line(r"C:\p\app", "m", 7),
            line(r"C:\p\app", "m", 3)
        );
        std::fs::write(project.join("s1.jsonl"), text).unwrap();
        let scans = scan_dir(dir.path(), None);
        assert_eq!(scans[0].tokens.total(), 10);

        // The same line under the limit counts.
        let fits = format!("{},\"pad\":\"x\"}}", &usage[..usage.len() - 1]);
        std::fs::write(project.join("s1.jsonl"), format!("{fits}\n")).unwrap();
        assert_eq!(scan_dir(dir.path(), None)[0].tokens.total(), 1000);
    }

    #[test]
    fn transcripts_last_written_before_the_window_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        // The lines are timestamped now, so only the file's age can exclude it.
        let old = project.join("old.jsonl");
        std::fs::write(&old, line(r"C:\p\app", "m", 50)).unwrap();
        let forty_days = std::time::Duration::from_secs(40 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - forty_days)
            .unwrap();
        std::fs::write(project.join("new.jsonl"), line(r"C:\p\app", "m", 5)).unwrap();

        let windowed = scan_dir(dir.path(), Some(30));
        assert_eq!((windowed[0].tokens.total(), windowed[0].sessions), (5, 1));
        let everything = scan_dir(dir.path(), None);
        assert_eq!(
            (everything[0].tokens.total(), everything[0].sessions),
            (55, 2)
        );
    }

    #[test]
    fn folders_past_the_depth_limit_are_not_walked() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        let mut deep = project.clone();
        for i in 0..MAX_DEPTH + 2 {
            deep = deep.join(format!("d{i}"));
            std::fs::create_dir_all(&deep).unwrap();
            // A transcript at every level: the ones past the limit are skipped.
            std::fs::write(deep.join("a.jsonl"), line(r"C:\p\app", "m", 1)).unwrap();
        }
        let scans = scan_dir(dir.path(), None);
        // `project` is level 1; levels 2..=MAX_DEPTH hold walked folders.
        assert_eq!(scans[0].tokens.total(), (MAX_DEPTH - 1) as u64);
    }

    #[test]
    fn linked_folders_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("c--p-app");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("s1.jsonl"), line(r"C:\p\app", "m", 4)).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("x.jsonl"), line(r"C:\p\app", "m", 1000)).unwrap();
        // A loop back to the root and a link out of the tree.
        let made = link_dir(dir.path(), &project.join("loop"))
            && link_dir(outside.path(), &project.join("out"));
        if !made {
            // CC_TEST_REQUIRE_ALL=1 (CI sets it) fails instead of skipping.
            if std::env::var_os("CC_TEST_REQUIRE_ALL").is_some_and(|v| v == "1") {
                panic!("cannot create directory links here, and CC_TEST_REQUIRE_ALL=1");
            }
            eprintln!("skipped: cannot create directory links here");
            return;
        }
        let scans = scan_dir(dir.path(), None);
        assert_eq!(scans.len(), 1);
        assert_eq!(scans[0].tokens.total(), 4);
    }

    #[cfg(unix)]
    fn link_dir(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    /// A junction: unlike a symbolic link it needs no privilege. Both paths
    /// are inside temp dirs the test owns.
    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) -> bool {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
            && link.exists()
    }
}
