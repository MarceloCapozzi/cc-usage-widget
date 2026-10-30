//! Registers cc-hook in Claude Code's `settings.json`, and removes it again.
//!
//! The file belongs to the user and other tools, so every edit is surgical:
//!
//! - only hook entries whose `command` runs an executable named [`MARKER`]
//!   (`cc-hook` or `cc-hook.exe`, see [`is_cc_hook_command`]) are ours; every
//!   other entry, group, matcher and key stays where it is, and
//!   key order is preserved (serde_json's `preserve_order`);
//! - install appends one group `{"hooks":[{"type":"command","command":CMD}]}`
//!   per event in [`HOOK_EVENTS`], or updates our existing entry in place, so
//!   running it twice changes nothing; cc-hook entries left in events that are
//!   no longer in the list are removed;
//! - uninstall removes our entries, a group only if that leaves it empty, and
//!   an event key only if that leaves its array empty;
//! - before writing, the original bytes are copied to
//!   `settings.json.bak-cc-hook-<yyyyMMdd-HHmmss>` (UTC), keeping only the
//!   newest [`MAX_BACKUPS`]; the new content is written to a temp file with
//!   the original's permissions (Unix) and renamed over the original (over
//!   the file a symbolic link points to, so the link stays), re-read, parsed
//!   and verified, and the original is restored if anything is off;
//! - the file is re-read just before the rename: if another program changed
//!   it since cc-hook read it, the edit is abandoned and the file left as is;
//! - the commands written are quoted for the shell Claude Code runs them with
//!   ([`quote_path`]); a path that cannot be quoted safely is refused;
//! - a file that is not valid JSON, or whose `hooks` has an unexpected shape,
//!   is refused untouched.
//!
//! The status line ([`StatusLineSetup`], used by the CLI unless
//! `--no-statusline`): install replaces only `statusLine.command` with
//! `"<cc-hook>" statusline` (keeping `type`, `padding` and any other key, in
//! place) and records the previous `statusLine` value verbatim in the chain
//! file ([`crate::chain`]) before writing settings; with no `statusLine`, it
//! adds one that chains nothing. A `statusLine` that already runs cc-hook only
//! gets its path updated, so install is idempotent. Uninstall puts the
//! recorded value back exactly (or removes the key if there was none), but
//! only while `statusLine` still runs cc-hook: if the user changed it since,
//! it is left alone and the stale record dropped.
//!
//! The pure document edits ([`apply_install`], [`apply_uninstall`],
//! [`hook_status`], [`apply_statusline_install`],
//! [`apply_statusline_uninstall`]) are separate from the file handling
//! ([`install_file_with`], [`uninstall_file_with`], [`status_file`],
//! [`statusline_status_file`]) so both are testable.

use crate::chain;
use cc_core::events::HOOK_EVENTS;
use cc_core::fsutil::rename_retrying;
use serde_json::{Map, Value};
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A hook entry is cc-hook's when its command runs an executable with this
/// file stem.
pub const MARKER: &str = "cc-hook";

/// What went wrong while reading, editing or writing settings.
#[derive(Debug)]
pub enum Error {
    /// The existing file is not valid JSON; it was left untouched.
    InvalidJson {
        /// The settings file.
        path: PathBuf,
        /// The parse error.
        source: serde_json::Error,
    },
    /// The document is JSON but not shaped as Claude Code settings.
    Shape(String),
    /// The requested hook command cannot be recognised as cc-hook's.
    BadCommand(String),
    /// A path that cannot be written into a shell command safely.
    UnsafePath {
        /// The path.
        path: String,
        /// Why.
        reason: String,
    },
    /// The settings file changed between reading and replacing it (another
    /// program wrote it); it was left as that program wrote it.
    Changed(PathBuf),
    /// A file system operation failed.
    Io {
        /// What was being done.
        context: String,
        /// The underlying error.
        source: io::Error,
    },
    /// The written file did not read back as expected; the original was restored
    /// (or `restore_error` says why it could not be).
    Verify {
        /// What did not match.
        problem: String,
        /// Why restoring the original failed, if it did.
        restore_error: Option<String>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidJson { path, source } => write!(
                f,
                "{} is not valid JSON ({source}); refusing to touch it. Fix the file and try again.",
                path.display()
            ),
            Error::Shape(msg) => write!(f, "unexpected settings shape: {msg}; refusing to touch the file"),
            Error::BadCommand(cmd) => write!(
                f,
                "hook command {cmd:?} does not run an executable named {MARKER:?}, so it could not be recognised later; \
                 use a path to the cc-hook executable"
            ),
            Error::UnsafePath { path, reason } => write!(
                f,
                "cannot put {path:?} into a shell command safely: {reason}; \
                 move cc-hook (or the settings file) to a folder whose path does not need it"
            ),
            Error::Changed(path) => write!(
                f,
                "{} changed while cc-hook was editing it (another program wrote it); \
                 it was left untouched, try again",
                path.display()
            ),
            Error::Io { context, source } => write!(f, "{context}: {source}"),
            Error::Verify { problem, restore_error: None } => {
                write!(f, "verification failed ({problem}); the original file was restored")
            }
            Error::Verify { problem, restore_error: Some(r) } => write!(
                f,
                "verification failed ({problem}) and restoring the original also failed ({r}); \
                 restore it from the backup by hand"
            ),
        }
    }
}

impl std::error::Error for Error {}

fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> Error {
    let context = context.into();
    move |source| Error::Io { context, source }
}

/// What an edit did to one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A new cc-hook group was appended.
    Added,
    /// An existing cc-hook entry was changed (command, type, or duplicates removed).
    Updated,
    /// Already as wanted.
    Unchanged,
    /// cc-hook entries were removed.
    Removed,
}

/// Per-event result of an edit, in processing order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// `(event, action)` pairs.
    pub actions: Vec<(String, Action)>,
}

impl Report {
    /// Whether the document changed at all.
    pub fn changed(&self) -> bool {
        self.actions.iter().any(|(_, a)| *a != Action::Unchanged)
    }
}

/// Result of a file-level install or uninstall.
#[derive(Debug)]
pub struct Outcome {
    /// What happened per event.
    pub report: Report,
    /// Whether the file was rewritten (false when nothing needed changing).
    pub written: bool,
    /// The backup of the previous content, when one was taken.
    pub backup: Option<PathBuf>,
    /// Things the user should know, such as a status line left alone.
    pub notes: Vec<String>,
}

/// One event's cc-hook registration as found in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStatus {
    /// Hook event name.
    pub event: String,
    /// Whether the event is one cc-hook should be registered for.
    pub expected: bool,
    /// Commands of the cc-hook entries found (normally zero or one).
    pub commands: Vec<String>,
}

/// Whether a hook command string is cc-hook's: its first token (a quoted or
/// bare path) names a file `cc-hook` or `cc-hook.exe`, ignoring case. A mere
/// substring is not enough, so another tool's `/opt/my-cc-hooks/notify.sh` or
/// `node cc-hook-relay.js` is never mistaken for ours.
pub fn is_cc_hook_command(command: &str) -> bool {
    let program = crate::shell::program_of(command);
    let name = program.rsplit(['/', '\\']).next().unwrap_or(&program);
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower) == MARKER
}

/// Characters a double-quoted Windows path cannot carry safely through every
/// shell Claude Code may run the command with: `"` ends the quotes; `$` and
/// `` ` `` expand inside double quotes in Git Bash and PowerShell; `%`
/// expands in `cmd`.
pub const WINDOWS_UNSAFE_CHARS: &[char] = &['"', '%', '$', '`'];

/// A path quoted for POSIX `sh` (and Git Bash): in single quotes, where
/// nothing expands, with each embedded `'` written as `'\''`.
pub fn quote_posix(text: &str) -> Result<String, Error> {
    refuse_control_chars(text)?;
    Ok(format!("'{}'", text.replace('\'', r"'\''")))
}

/// A path quoted for the shells Claude Code uses on Windows (Git Bash, or
/// PowerShell): in double quotes, which both read the same way as long as
/// the path has none of [`WINDOWS_UNSAFE_CHARS`]. Such a path is refused
/// rather than escaped, since no escaping works in all of them.
pub fn quote_windows(text: &str) -> Result<String, Error> {
    refuse_control_chars(text)?;
    if let Some(c) = text.chars().find(|c| WINDOWS_UNSAFE_CHARS.contains(c)) {
        return Err(Error::UnsafePath {
            path: text.to_string(),
            reason: format!("it contains {c:?}, which the shell would expand or end the quotes at"),
        });
    }
    Ok(format!("\"{text}\""))
}

fn refuse_control_chars(text: &str) -> Result<(), Error> {
    match text.chars().find(|c| c.is_control()) {
        Some(c) => Err(Error::UnsafePath {
            path: text.to_string(),
            reason: format!("it contains the control character {c:?}"),
        }),
        None => Ok(()),
    }
}

/// A path as a shell word for this platform's [`quote_windows`] or
/// [`quote_posix`]. On Windows the `\\?\` prefix is dropped and backslashes
/// become forward slashes, which Git Bash and PowerShell both accept.
pub fn quote_path(path: &Path) -> Result<String, Error> {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        quote_windows(&text.replace('\\', "/"))
    } else {
        quote_posix(&text)
    }
}

/// The default `settings.json`: `settings.json` in Claude Code's folder
/// ([`claude_config_dir`]), so `$CLAUDE_CONFIG_DIR/settings.json` when that
/// is an absolute path, else `%USERPROFILE%\.claude\settings.json` on Windows and
/// `~/.claude/settings.json` elsewhere. `None` when no folder can be found.
pub fn default_settings_path() -> Option<PathBuf> {
    claude_config_dir().map(|dir| dir.join("settings.json"))
}

/// Claude Code's configuration folder ([`cc_core::paths::claude_config_dir`],
/// the rule Claude Code itself follows): `CLAUDE_CONFIG_DIR` when set to an
/// absolute path, else `.claude` in the home folder (`USERPROFILE` on Windows,
/// `HOME` elsewhere, each falling back to the other, absolute paths only).
/// `None` without any of them.
pub fn claude_config_dir() -> Option<PathBuf> {
    cc_core::paths::claude_config_dir()
}

/// The hook command for an executable, quoted by [`quote_path`]: on Windows
/// its path with forward slashes in double quotes, e.g.
/// `"C:/Users/x/AppData/Local/cc-usage/bin/cc-hook.exe"`; elsewhere in single
/// quotes, e.g. `'/home/x/.local/bin/cc-hook'`. Fails for a path that cannot
/// be quoted safely ([`Error::UnsafePath`]).
pub fn command_for_exe(exe: &Path) -> Result<String, Error> {
    quote_path(exe)
}

/// The default hook command: [`command_for_exe`] of the running executable.
pub fn default_command() -> Result<String, Error> {
    let exe = std::env::current_exe().map_err(io_err("cannot locate the running executable"))?;
    command_for_exe(&exe)
}

fn is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(is_cc_hook_command)
}

/// The `hooks` object of a settings document, checking shapes. `create`
/// appends an empty one when missing.
fn hooks_mut(doc: &mut Value, create: bool) -> Result<Option<&mut Map<String, Value>>, Error> {
    let root = doc
        .as_object_mut()
        .ok_or_else(|| Error::Shape("the top level is not a JSON object".into()))?;
    if !root.contains_key("hooks") {
        if !create {
            return Ok(None);
        }
        root.insert("hooks".into(), Value::Object(Map::new()));
    }
    match root.get_mut("hooks") {
        Some(Value::Object(hooks)) => {
            for (event, groups) in hooks.iter() {
                if !groups.is_array() {
                    return Err(Error::Shape(format!("hooks.{event} is not an array")));
                }
            }
            Ok(Some(hooks))
        }
        _ => Err(Error::Shape("\"hooks\" is not an object".into())),
    }
}

/// Removes cc-hook entries from an event's groups; a group is dropped only if
/// that removal empties its `hooks`. When `keep_first` is given, the first
/// cc-hook entry is kept and set to that command instead. Returns
/// `(found_one, changed)`.
fn strip_ours(groups: &mut Vec<Value>, keep_first: Option<&str>) -> (bool, bool) {
    let mut found = false;
    let mut changed = false;
    let mut emptied = vec![false; groups.len()];
    for (i, group) in groups.iter_mut().enumerate() {
        let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = entries.len();
        entries.retain_mut(|entry| {
            if !is_ours(entry) {
                return true;
            }
            match keep_first {
                Some(command) if !found => {
                    found = true;
                    if entry.get("type").and_then(Value::as_str) != Some("command") {
                        entry["type"] = Value::from("command");
                        changed = true;
                    }
                    if entry.get("command").and_then(Value::as_str) != Some(command) {
                        entry["command"] = Value::from(command);
                        changed = true;
                    }
                    true
                }
                _ => {
                    changed = true;
                    false
                }
            }
        });
        emptied[i] = entries.is_empty() && entries.len() != before;
    }
    let mut i = 0;
    groups.retain(|_| {
        i += 1;
        !emptied[i - 1]
    });
    (found, changed)
}

/// Registers `command` for every event in [`HOOK_EVENTS`] in a settings
/// document, idempotently, and removes cc-hook entries from any other event.
pub fn apply_install(doc: &mut Value, command: &str) -> Result<Report, Error> {
    if !is_cc_hook_command(command) {
        return Err(Error::BadCommand(command.into()));
    }
    let hooks = hooks_mut(doc, true)?.ok_or_else(|| Error::Shape("no hooks object".into()))?;
    let mut report = Report::default();
    for &event in HOOK_EVENTS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| Error::Shape(format!("hooks.{event} is not an array")))?;
        let (found, changed) = strip_ours(groups, Some(command));
        let action = if !found {
            groups
                .push(serde_json::json!({ "hooks": [{ "type": "command", "command": command }] }));
            Action::Added
        } else if changed {
            Action::Updated
        } else {
            Action::Unchanged
        };
        report.actions.push((event.to_string(), action));
    }
    let stale: Vec<String> = hooks
        .keys()
        .filter(|k| !HOOK_EVENTS.contains(&k.as_str()))
        .cloned()
        .collect();
    for event in stale {
        if remove_from_event(hooks, &event) {
            report.actions.push((event, Action::Removed));
        }
    }
    Ok(report)
}

/// Removes our entries from one event; drops the event key if that empties
/// it. Returns whether anything was removed.
fn remove_from_event(hooks: &mut Map<String, Value>, event: &str) -> bool {
    let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
        return false;
    };
    let had = groups.len();
    let (_, changed) = strip_ours(groups, None);
    if changed && groups.is_empty() && had > 0 {
        hooks.shift_remove(event);
    }
    changed
}

/// Removes every cc-hook entry from a settings document. A group goes only if
/// it becomes empty, an event key only if its array becomes empty, and the
/// `hooks` key only if this removal emptied it.
pub fn apply_uninstall(doc: &mut Value) -> Result<Report, Error> {
    let mut report = Report::default();
    let Some(hooks) = hooks_mut(doc, false)? else {
        return Ok(report);
    };
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        if remove_from_event(hooks, &event) {
            report.actions.push((event, Action::Removed));
        }
    }
    let emptied = hooks.is_empty() && report.changed();
    if emptied {
        if let Some(root) = doc.as_object_mut() {
            root.shift_remove("hooks");
        }
    }
    Ok(report)
}

/// cc-hook entries per event: every event in [`HOOK_EVENTS`], then any other
/// event that holds a cc-hook entry.
pub fn hook_status(doc: &Value) -> Vec<EventStatus> {
    let hooks = doc.get("hooks").and_then(Value::as_object);
    let commands_of = |event: &str| -> Vec<String> {
        hooks
            .and_then(|h| h.get(event))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter(|e| is_ours(e))
            .filter_map(|e| e.get("command").and_then(Value::as_str).map(str::to_string))
            .collect()
    };
    let mut out: Vec<EventStatus> = HOOK_EVENTS
        .iter()
        .map(|&event| EventStatus {
            event: event.to_string(),
            expected: true,
            commands: commands_of(event),
        })
        .collect();
    if let Some(h) = hooks {
        for event in h.keys().filter(|k| !HOOK_EVENTS.contains(&k.as_str())) {
            let commands = commands_of(event);
            if !commands.is_empty() {
                out.push(EventStatus {
                    event: event.clone(),
                    expected: false,
                    commands,
                });
            }
        }
    }
    out
}

/// Checks that a document holds exactly one cc-hook entry, with `command`, for
/// every expected event and none elsewhere.
pub fn verify_installed(doc: &Value, command: &str) -> Result<(), String> {
    for s in hook_status(doc) {
        match (s.expected, s.commands.as_slice()) {
            (true, [c]) if c == command => {}
            (true, cmds) => {
                return Err(format!(
                    "{}: expected one cc-hook entry, found {cmds:?}",
                    s.event
                ))
            }
            (false, _) => return Err(format!("{}: unexpected cc-hook entry", s.event)),
        }
    }
    Ok(())
}

/// Checks that a document holds no cc-hook entry at all.
pub fn verify_uninstalled(doc: &Value) -> Result<(), String> {
    match hook_status(doc)
        .into_iter()
        .find(|s| !s.commands.is_empty())
    {
        Some(s) => Err(format!("{} still has a cc-hook entry", s.event)),
        None => Ok(()),
    }
}

/// A settings file as read: original bytes (None when it does not exist) and
/// the parsed document.
struct Loaded {
    original: Option<Vec<u8>>,
    doc: Value,
}

fn parse_settings(path: &Path, bytes: &[u8]) -> Result<Value, Error> {
    let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if text.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(text).map_err(|source| Error::InvalidJson {
        path: path.to_path_buf(),
        source,
    })
}

fn load(path: &Path) -> Result<Loaded, Error> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let doc = parse_settings(path, &bytes)?;
            Ok(Loaded {
                original: Some(bytes),
                doc,
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Loaded {
            original: None,
            doc: Value::Object(Map::new()),
        }),
        Err(e) => Err(io_err(format!("cannot read {}", path.display()))(e)),
    }
}

/// Serializes like the original: same indentation unit, CRLF if it used CRLF,
/// and a trailing newline unless the original lacked one.
fn render(doc: &Value, original: Option<&[u8]>) -> Result<Vec<u8>, Error> {
    let orig = original.map(String::from_utf8_lossy);
    let orig = orig.as_deref().unwrap_or("");
    let indent = detect_indent(orig);
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(doc, &mut ser).map_err(|e| Error::Shape(e.to_string()))?;
    let mut text = String::from_utf8(out).map_err(|e| Error::Shape(e.to_string()))?;
    if original.is_none()
        || orig.trim_end_matches([' ', '\t']).ends_with('\n')
        || orig.trim().is_empty()
    {
        text.push('\n');
    }
    if orig.contains("\r\n") {
        text = text.replace('\n', "\r\n");
    }
    Ok(text.into_bytes())
}

/// The whitespace before the first indented line, or two spaces.
fn detect_indent(text: &str) -> String {
    text.lines()
        .skip(1)
        .map(|l| {
            let n = l.len() - l.trim_start_matches([' ', '\t']).len();
            &l[..n]
        })
        .find(|ws| !ws.is_empty())
        .map_or_else(|| "  ".to_string(), str::to_string)
}

/// `yyyyMMdd-HHmmss` in UTC for a Unix time.
pub fn timestamp_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant), valid for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".into())
}

/// Backups of one settings file kept: the oldest go once there are more.
pub const MAX_BACKUPS: usize = 5;

/// The name prefix of `path`'s backups, `settings.json.bak-cc-hook-`.
fn backup_prefix(path: &Path) -> String {
    format!("{}.bak-cc-hook-", file_name(path))
}

/// Creates a new file for settings content: on Unix with mode `0600` from
/// the start, so it is never readable by others even before
/// [`copy_permissions`] gives it the original's mode.
fn create_new_private(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Gives `to` the permission bits of `from` (Unix only: on Windows the file
/// inherits its folder's ACL, like the original did).
fn copy_permissions(from: Option<&std::fs::Metadata>, to: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(meta) = from {
        std::fs::set_permissions(to, meta.permissions())?;
    }
    #[cfg(not(unix))]
    let _ = (from, to);
    Ok(())
}

/// Writes the backup next to `path` without overwriting an older one, with
/// the original's permissions, then drops all but the newest
/// [`MAX_BACKUPS`].
fn write_backup(path: &Path, bytes: &[u8]) -> Result<PathBuf, Error> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let original = std::fs::metadata(path).ok();
    let base = format!("{}{}", backup_prefix(path), timestamp_utc(secs));
    for n in 0..100 {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let candidate = path.with_file_name(name);
        match create_new_private(&candidate) {
            Ok(mut f) => {
                f.write_all(bytes)
                    .and_then(|()| f.sync_all())
                    .and_then(|()| copy_permissions(original.as_ref(), &candidate))
                    .map_err(io_err(format!(
                        "cannot write backup {}",
                        candidate.display()
                    )))?;
                prune_backups(path, MAX_BACKUPS);
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(io_err(format!(
                    "cannot create backup {}",
                    candidate.display()
                ))(e))
            }
        }
    }
    Err(Error::Io {
        context: "cannot find a free backup name".into(),
        source: io::Error::from(io::ErrorKind::AlreadyExists),
    })
}

/// Deletes `path`'s backups (`<name>.bak-cc-hook-*` next to it) but the
/// newest `keep`, newest by modification time, then by name. Best effort.
pub fn prune_backups(path: &Path, keep: usize) -> usize {
    let prefix = backup_prefix(path);
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut backups: Vec<(SystemTime, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = e.metadata().ok()?;
            (name.starts_with(&prefix) && meta.is_file())
                .then(|| (meta.modified().unwrap_or(UNIX_EPOCH), name))
        })
        .collect();
    backups.sort();
    let excess = backups.len().saturating_sub(keep);
    backups
        .iter()
        .take(excess)
        .filter(|(_, name)| std::fs::remove_file(dir.join(name)).is_ok())
        .count()
}

/// Where a write to `path` lands: the file a symbolic link points to (so the
/// link stays a link, as the user set it up), else `path` itself.
fn write_target(path: &Path) -> io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path),
        _ => Ok(path.to_path_buf()),
    }
}

/// Replaces `path` (or the file it links to) with `bytes` through a temp file
/// in the same directory, with the original's permissions. With `expect`,
/// the file is re-read just before the rename and left alone, with
/// [`Error::Changed`], unless it still holds exactly those bytes (`None`:
/// still does not exist): another program may have written it meanwhile.
fn replace_file(path: &Path, bytes: &[u8], expect: Option<Option<&[u8]>>) -> Result<(), Error> {
    let target =
        write_target(path).map_err(io_err(format!("cannot resolve {}", path.display())))?;
    let tmp = target.with_file_name(format!(
        ".{}.tmp-cc-hook-{}",
        file_name(&target),
        std::process::id()
    ));
    let original = std::fs::metadata(&target).ok();
    let _ = std::fs::remove_file(&tmp); // a leftover of a crashed run
    let result = (|| {
        let writing = || io_err(format!("cannot write {}", tmp.display()));
        let mut f = create_new_private(&tmp).map_err(writing())?;
        f.write_all(bytes)
            .and_then(|()| f.sync_all())
            .map_err(writing())?;
        drop(f);
        copy_permissions(original.as_ref(), &tmp).map_err(writing())?;
        if let Some(expected) = expect {
            let current = match std::fs::read(&target) {
                Ok(b) => Some(b),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(io_err(format!("cannot re-read {}", target.display()))(e)),
            };
            if current.as_deref() != expected {
                return Err(Error::Changed(path.to_path_buf()));
            }
        }
        rename_retrying(&tmp, &target).map_err(io_err(format!("cannot write {}", path.display())))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Backs up, writes, re-reads and verifies; restores the original on failure.
fn commit(
    path: &Path,
    loaded: &Loaded,
    doc: &Value,
    verify: impl Fn(&Value) -> Result<(), String>,
) -> Result<Option<PathBuf>, Error> {
    let bytes = render(doc, loaded.original.as_deref())?;
    let backup = match &loaded.original {
        Some(orig) => Some(write_backup(path, orig)?),
        None => {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)
                    .map_err(io_err(format!("cannot create {}", dir.display())))?;
            }
            None
        }
    };
    if let Err(e) = replace_file(path, &bytes, Some(loaded.original.as_deref())) {
        if matches!(e, Error::Changed(_)) {
            // Nothing was changed, so the backup is not needed.
            if let Some(b) = &backup {
                let _ = std::fs::remove_file(b);
            }
        }
        return Err(e);
    }

    let check = std::fs::read(path)
        .map_err(|e| format!("cannot re-read: {e}"))
        .and_then(|b| parse_settings(path, &b).map_err(|e| e.to_string()))
        .and_then(|reread| {
            if &reread != doc {
                return Err("content read back differs from what was written".into());
            }
            verify(&reread)
        });
    if let Err(problem) = check {
        let restore = match &loaded.original {
            Some(orig) => replace_file(path, orig, None).err().map(|e| e.to_string()),
            None => std::fs::remove_file(path).err().map(|e| e.to_string()),
        };
        return Err(Error::Verify {
            problem,
            restore_error: restore,
        });
    }
    Ok(backup)
}

// ---------------------------------------------------------------------------
// Status line chain
// ---------------------------------------------------------------------------

/// The name `install` and `uninstall` report the status line under.
pub const STATUS_LINE: &str = "statusLine";

/// What `install` does with `statusLine`, and `uninstall` undoes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLineSetup {
    /// cc-hook's status line command, see [`statusline_command`].
    pub command: String,
    /// The chain file that keeps the replaced `statusLine` values
    /// (`<data_dir>/statusline-chain.json` outside tests).
    pub chain_file: PathBuf,
}

/// cc-hook's status line command for a hook command: `<hook command>
/// statusline`, plus `--settings <path>` (quoted by [`quote_path`]) when the
/// settings file is not the default one ([`default_settings_path`], which
/// `cc-hook statusline` falls back to), so a test copy's chain never stands
/// in for the real file's.
pub fn statusline_command(
    hook_command: &str,
    settings_override: Option<&Path>,
) -> Result<String, Error> {
    let Some(path) = settings_override else {
        return Ok(format!("{hook_command} statusline"));
    };
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    Ok(format!(
        "{hook_command} statusline --settings {}",
        quote_path(&abs)?
    ))
}

/// Whether a `statusLine` value runs cc-hook.
pub fn is_our_statusline(value: &Value) -> bool {
    value
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(is_cc_hook_command)
}

fn root_mut(doc: &mut Value) -> Result<&mut Map<String, Value>, Error> {
    doc.as_object_mut()
        .ok_or_else(|| Error::Shape("the top level is not a JSON object".into()))
}

/// Points `statusLine.command` at `command`, keeping `type`, `padding` and
/// every other key. When the current value is not cc-hook's, it is returned as
/// the [`chain::Chain`] to record (verbatim, `null` when there was none) before
/// the file is written. When it already is cc-hook's, only the command path is
/// updated, and a record is returned only if `existing` is missing.
pub fn apply_statusline_install(
    doc: &mut Value,
    command: &str,
    existing: Option<&chain::Chain>,
    settings_path: &str,
    now_secs: u64,
) -> Result<(Action, Option<chain::Chain>), Error> {
    if !is_cc_hook_command(command) {
        return Err(Error::BadCommand(command.into()));
    }
    let root = root_mut(doc)?;
    let record = |had: bool, original: Value| chain::Chain {
        settings_path: settings_path.to_string(),
        had_status_line: had,
        original,
        installed_at: now_secs,
    };
    match root.get_mut(STATUS_LINE) {
        Some(current) if is_our_statusline(current) => {
            let mut changed = false;
            if current.get("type").and_then(Value::as_str) != Some("command") {
                current["type"] = Value::from("command");
                changed = true;
            }
            if current.get("command").and_then(Value::as_str) != Some(command) {
                current["command"] = Value::from(command);
                changed = true;
            }
            // Ours without a record: the original is unknown, chain nothing.
            let rec = existing.is_none().then(|| record(false, Value::Null));
            Ok((
                if changed {
                    Action::Updated
                } else {
                    Action::Unchanged
                },
                rec,
            ))
        }
        Some(current) => {
            let original = current.clone();
            match current.as_object_mut() {
                Some(obj) => {
                    if obj.get("type").and_then(Value::as_str) != Some("command") {
                        obj.insert("type".into(), Value::from("command"));
                    }
                    obj.insert("command".into(), Value::from(command));
                }
                None => *current = serde_json::json!({ "type": "command", "command": command }),
            }
            Ok((Action::Added, Some(record(true, original))))
        }
        None => {
            root.insert(
                STATUS_LINE.into(),
                serde_json::json!({ "type": "command", "command": command }),
            );
            Ok((Action::Added, Some(record(false, Value::Null))))
        }
    }
}

/// Puts back the `statusLine` cc-hook replaced: the recorded value verbatim,
/// in the same position, or no key when there was none (or no record). A
/// `statusLine` that no longer runs cc-hook (the user changed it) is left
/// alone and reported [`Action::Unchanged`].
pub fn apply_statusline_uninstall(
    doc: &mut Value,
    existing: Option<&chain::Chain>,
) -> Result<Action, Error> {
    let root = root_mut(doc)?;
    if !root.get(STATUS_LINE).is_some_and(is_our_statusline) {
        return Ok(Action::Unchanged);
    }
    match existing {
        Some(c) if c.had_status_line => {
            root.insert(STATUS_LINE.into(), c.original.clone());
        }
        _ => {
            root.shift_remove(STATUS_LINE);
        }
    }
    Ok(Action::Removed)
}

/// Checks that `statusLine.command` is `command`.
pub fn verify_statusline_installed(doc: &Value, command: &str) -> Result<(), String> {
    match doc
        .get(STATUS_LINE)
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
    {
        Some(c) if c == command => Ok(()),
        other => Err(format!(
            "statusLine.command is {other:?}, expected {command:?}"
        )),
    }
}

/// Checks that `statusLine` does not run cc-hook.
pub fn verify_statusline_uninstalled(doc: &Value) -> Result<(), String> {
    match doc.get(STATUS_LINE) {
        Some(s) if is_our_statusline(s) => Err("statusLine still runs cc-hook".into()),
        _ => Ok(()),
    }
}

/// The status line as `status` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusLineStatus {
    /// `statusLine.command`, when there is one.
    pub command: Option<String>,
    /// Whether that command runs cc-hook.
    pub ours: bool,
    /// The recorded chain for this settings file, if any.
    pub chain: Option<chain::Chain>,
}

/// The status line of the settings file at `path` and its chain record, or
/// `None` when the file does not exist.
pub fn statusline_status_file(
    path: &Path,
    chain_file: &Path,
) -> Result<Option<StatusLineStatus>, Error> {
    let loaded = load(path)?;
    if loaded.original.is_none() {
        return Ok(None);
    }
    let current = loaded.doc.get(STATUS_LINE);
    Ok(Some(StatusLineStatus {
        command: current
            .and_then(|s| s.get("command"))
            .and_then(Value::as_str)
            .map(str::to_string),
        ours: current.is_some_and(is_our_statusline),
        chain: chain::lookup(chain_file, path),
    }))
}

fn load_chains(chain_file: &Path) -> Result<chain::ChainFile, Error> {
    chain::load(chain_file).map_err(|e| {
        let what = if e.kind() == io::ErrorKind::InvalidData {
            "is not a valid chain file; refusing to touch the status line. Fix or remove it and try again"
        } else {
            "cannot be read"
        };
        Error::Io {
            context: format!("{} {what}", chain_file.display()),
            source: e,
        }
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Installs `command` into the settings file at `path` (created if missing).
/// Hooks only; see [`install_file_with`] for the status line.
pub fn install_file(path: &Path, command: &str) -> Result<Outcome, Error> {
    install_file_with(path, command, None)
}

/// Installs the hooks, and with `statusline` also chains cc-hook in front of
/// the status line. The replaced `statusLine` is recorded in the chain file
/// before the settings file is written, so cc-hook's status line never runs
/// without its record.
pub fn install_file_with(
    path: &Path,
    command: &str,
    statusline: Option<&StatusLineSetup>,
) -> Result<Outcome, Error> {
    let loaded = load(path)?;
    let mut doc = loaded.doc.clone();
    let mut report = apply_install(&mut doc, command)?;
    if let Some(sl) = statusline {
        let mut chains = load_chains(&sl.chain_file)?;
        let key = chain::settings_key(path);
        let (action, record) = apply_statusline_install(
            &mut doc,
            &sl.command,
            chains.chains.get(&key),
            &path.display().to_string(),
            unix_now(),
        )?;
        report.actions.push((STATUS_LINE.to_string(), action));
        if let Some(record) = record {
            chains.chains.insert(key, record);
            chain::save(&sl.chain_file, &chains)
                .map_err(io_err(format!("cannot write {}", sl.chain_file.display())))?;
        }
    }
    if !report.changed() && loaded.original.is_some() {
        return Ok(Outcome {
            report,
            written: false,
            backup: None,
            notes: Vec::new(),
        });
    }
    let backup = commit(path, &loaded, &doc, |d| {
        verify_installed(d, command)?;
        match statusline {
            Some(sl) => verify_statusline_installed(d, &sl.command),
            None => Ok(()),
        }
    })?;
    Ok(Outcome {
        report,
        written: true,
        backup,
        notes: Vec::new(),
    })
}

/// Removes every cc-hook entry from the settings file at `path`. A missing
/// file means nothing to do. Hooks only; see [`uninstall_file_with`].
pub fn uninstall_file(path: &Path) -> Result<Outcome, Error> {
    uninstall_file_with(path, None)
}

/// Removes the hooks, and with `chain_file` also puts back the `statusLine`
/// cc-hook replaced, if `statusLine` still runs cc-hook. The record is dropped
/// once the settings file no longer needs it.
pub fn uninstall_file_with(path: &Path, chain_file: Option<&Path>) -> Result<Outcome, Error> {
    let loaded = load(path)?;
    let mut doc = loaded.doc.clone();
    let mut report = apply_uninstall(&mut doc)?;
    let mut notes = Vec::new();
    let mut drop_record: Option<chain::ChainFile> = None;
    if let Some(chain_file) = chain_file {
        let mut chains = load_chains(chain_file)?;
        let key = chain::settings_key(path);
        let existing = chains.chains.get(&key).cloned();
        match apply_statusline_uninstall(&mut doc, existing.as_ref())? {
            Action::Removed => {
                report
                    .actions
                    .push((STATUS_LINE.to_string(), Action::Removed));
                if existing.is_none() {
                    notes.push(
                        "statusLine ran cc-hook but there was no record of the one it replaced; removed it \
                         (the settings backups hold earlier versions)"
                            .to_string(),
                    );
                }
            }
            _ => {
                if loaded.doc.get(STATUS_LINE).is_some() && existing.is_some() {
                    notes.push("statusLine no longer runs cc-hook (changed since install); left it as it is".into());
                }
            }
        }
        if existing.is_some() {
            chains.chains.remove(&key);
            drop_record = Some(chains);
        }
    }
    let mut outcome = Outcome {
        report,
        written: false,
        backup: None,
        notes,
    };
    if outcome.report.changed() && loaded.original.is_some() {
        outcome.backup = commit(path, &loaded, &doc, |d| {
            verify_uninstalled(d)?;
            match chain_file {
                Some(_) => verify_statusline_uninstalled(d),
                None => Ok(()),
            }
        })?;
        outcome.written = true;
    }
    if let (Some(chains), Some(chain_file)) = (drop_record, chain_file) {
        let result = if chains.chains.is_empty() {
            std::fs::remove_file(chain_file).or_else(|e| match e.kind() {
                io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            })
        } else {
            chain::save(chain_file, &chains)
        };
        result.map_err(io_err(format!("cannot update {}", chain_file.display())))?;
    }
    Ok(outcome)
}

/// cc-hook's registration in the settings file at `path`, or `None` when the
/// file does not exist.
pub fn status_file(path: &Path) -> Result<Option<Vec<EventStatus>>, Error> {
    let loaded = load(path)?;
    if loaded.original.is_none() {
        return Ok(None);
    }
    Ok(Some(hook_status(&loaded.doc)))
}

#[cfg(test)]
mod tests;
