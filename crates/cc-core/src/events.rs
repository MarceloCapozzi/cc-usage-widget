//! The spool event: what `cc-hook` writes for every Claude Code hook call and
//! what the widget and `cc-publish` read back.
//!
//! One JSON object per file in the spool directory (see [`crate::paths`]),
//! written to a dotted `.tmp` name and renamed into place, so a reader never
//! sees a partial file. File names start with a 15-digit millisecond timestamp,
//! so a plain directory sort is chronological:
//!
//! ```text
//! 001791856373644-3f9a1c2e-....json
//! ```
//!
//! This is the LOCAL schema. It may carry prompt text, tool arguments and paths,
//! and it never leaves the machine; what goes to NATS is a separate, minimal
//! payload built by `cc-publish`.

use serde::{Deserialize, Serialize};

/// Bumped only for incompatible changes. Readers skip versions they do not know.
pub const SPOOL_SCHEMA_VERSION: u32 = 1;

/// Text fields are truncated to this many characters, with `…` appended.
pub const MAX_TEXT_CHARS: usize = 400;

/// Tool summaries ("npm test", "main.rs") are truncated to this many characters.
pub const MAX_TOOL_SUMMARY_CHARS: usize = 120;

/// Hook events cc-hook is registered for. `PostToolUse` is deliberately absent:
/// its payload carries the tool output, which can be megabytes.
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "Notification",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "StopFailure",
    "PostToolUseFailure",
    "SessionEnd",
];

/// One hook call, reduced to the fields the widget needs.
///
/// Every field except `v`, `ts`, `event` and `session_id` is optional: hook
/// payloads differ per event and across Claude Code versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpoolEvent {
    /// Schema version, [`SPOOL_SCHEMA_VERSION`].
    pub v: u32,
    /// When cc-hook ran, Unix seconds with fractional part.
    pub ts: f64,
    /// `hook_event_name` from the payload, e.g. `PreToolUse`.
    pub event: String,
    /// `session_id` from the payload (a UUID).
    pub session_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,

    /// Subagent identity, on events fired inside a subagent and on SubagentStart/Stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,

    /// Notification only: `permission_prompt`, `idle_prompt`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
    /// Notification only: the text Claude Code shows the user. Truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// PreToolUse / PostToolUseFailure: tool name, e.g. `Bash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// The tool's most identifying argument: Bash description or command,
    /// file name for Read/Write/Edit, pattern for Grep/Glob, host for WebFetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_summary: Option<String>,
    /// PreToolUse / PostToolUseFailure of an `Agent` (or `Task`) call: the
    /// subagent type it asks for (`tool_input.subagent_type`), which the
    /// subagent's `SubagentStart` reports as `agent_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,

    /// UserPromptSubmit: the prompt (`prompt` in the payload). Truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input: Option<String>,
    /// Stop / SubagentStop: `last_assistant_message`. Truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    /// StopFailure / PostToolUseFailure: `error` (or `error_message`). Truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,

    /// SessionStart: `source` (startup, resume, clear, compact).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// SessionEnd: `reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Process id of the Claude Code process that ran the hook, when cc-hook
    /// could resolve it by walking up its parent chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The terminal program from the hook's environment (`TERM_PROGRAM`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term_program: Option<String>,
}

impl SpoolEvent {
    /// A minimal event; callers fill the optional fields they have.
    pub fn new(event: impl Into<String>, session_id: impl Into<String>, ts: f64) -> Self {
        Self {
            v: SPOOL_SCHEMA_VERSION,
            ts,
            event: event.into(),
            session_id: session_id.into(),
            cwd: None,
            transcript_path: None,
            prompt_id: None,
            agent_id: None,
            agent_type: None,
            notification_type: None,
            message: None,
            tool_name: None,
            tool_use_id: None,
            tool_summary: None,
            subagent_type: None,
            user_input: None,
            last_message: None,
            error_message: None,
            source: None,
            reason: None,
            model: None,
            pid: None,
            term_program: None,
        }
    }

    /// File name for this event in the spool: `<15-digit ms>-<unique>.json`.
    pub fn file_name(&self, unique: &str) -> String {
        format!("{:015.0}-{unique}.json", self.ts * 1000.0)
    }
}

/// Flattens newlines, trims, and truncates to `max` characters with `…`.
/// Returns `None` for empty input.
pub fn truncate_text(value: &str, max: usize) -> Option<String> {
    let flat = value.replace(['\r', '\n'], " ");
    let flat = flat.trim();
    if flat.is_empty() {
        return None;
    }
    if flat.chars().count() <= max {
        return Some(flat.to_string());
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_camel_case_keys_and_skips_empty_fields() {
        let mut e = SpoolEvent::new("PreToolUse", "s-1", 1_791_856_373.644);
        e.tool_name = Some("Bash".into());
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"sessionId\":\"s-1\""));
        assert!(json.contains("\"toolName\":\"Bash\""));
        assert!(!json.contains("cwd"));
        assert_eq!(serde_json::from_str::<SpoolEvent>(&json).unwrap(), e);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{"v":1,"ts":1.5,"event":"Stop","sessionId":"s","future":true}"#;
        assert_eq!(
            serde_json::from_str::<SpoolEvent>(json).unwrap().event,
            "Stop"
        );
    }

    #[test]
    fn file_names_sort_chronologically() {
        let a = SpoolEvent::new("Stop", "s", 999.0).file_name("x");
        let b = SpoolEvent::new("Stop", "s", 1000.0).file_name("x");
        assert_eq!(a, "000000000999000-x.json");
        assert!(a < b);
    }

    #[test]
    fn truncation_flattens_and_marks_cuts() {
        assert_eq!(truncate_text("  a\nb ", 10).as_deref(), Some("a b"));
        assert_eq!(truncate_text("abcdef", 3).as_deref(), Some("abc…"));
        assert_eq!(truncate_text("   ", 3), None);
    }
}
