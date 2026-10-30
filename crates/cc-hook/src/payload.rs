//! Reduces a Claude Code hook payload to a [`SpoolEvent`].
//!
//! Hook payloads differ per event and across Claude Code versions, so every
//! field is read leniently: a missing or oddly typed field is just absent from
//! the event. Free text (prompts, messages, errors) is flattened and truncated
//! with [`cc_core::events::truncate_text`]; identifiers and paths are kept as
//! they are.
//!
//! Everything here is written to disk, so the text is kept lean: a shell
//! tool's summary is its `description` or just the program it runs (never
//! the command line, which can carry tokens and passwords), and obvious
//! secrets in free text are masked by [`redact_secrets`] before truncation.

use cc_core::events::{truncate_text, SpoolEvent, MAX_TEXT_CHARS, MAX_TOOL_SUMMARY_CHARS};
use serde_json::Value;

/// What a masked secret is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Builds the spool event for one hook payload.
///
/// Returns `None` when the payload is not an object or lacks a non-empty
/// `hook_event_name` or `session_id`: such an event cannot be attributed to a
/// session, so it is not worth spooling. `pid` is left empty; the receiver
/// fills it in.
pub fn build_event(payload: &Value, ts: f64, term_program: Option<String>) -> Option<SpoolEvent> {
    let obj = payload.as_object()?;
    let event = non_empty(obj.get("hook_event_name"))?;
    let session_id = non_empty(obj.get("session_id"))?;
    let mut e = SpoolEvent::new(event, session_id, ts);

    let field = |key: &str| non_empty(obj.get(key));
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| text_of(obj.get(*k)))
            .and_then(|t| truncate_text(&redact_secrets(&t), MAX_TEXT_CHARS))
    };

    e.cwd = field("cwd");
    e.transcript_path = field("transcript_path");
    e.prompt_id = field("prompt_id");
    e.agent_id = field("agent_id");
    e.agent_type = field("agent_type");
    e.notification_type = field("notification_type");
    e.message = text(&["message"]);
    e.tool_name = field("tool_name");
    e.tool_use_id = field("tool_use_id");
    let tool_input = obj.get("tool_input").unwrap_or(&Value::Null);
    e.tool_summary = e
        .tool_name
        .as_deref()
        .and_then(|name| tool_summary(name, tool_input));
    // Pairs the call with its subagent's SubagentStart (`agent_type`).
    if matches!(e.tool_name.as_deref(), Some("Agent" | "Task")) {
        e.subagent_type = non_empty(tool_input.get("subagent_type"));
    }
    e.user_input = text(&["prompt", "user_input"]);
    e.last_message = text(&["last_assistant_message"]);
    e.error_message = text(&["error", "error_message"]);
    e.source = field("source");
    e.reason = field("reason");
    e.model = model_of(obj.get("model"));
    e.term_program = term_program.filter(|t| !t.trim().is_empty());
    Some(e)
}

/// The tool's most identifying argument, truncated to
/// [`MAX_TOOL_SUMMARY_CHARS`]:
///
/// | Tool | Summary |
/// |---|---|
/// | Bash, BashOutput, PowerShell | `description`, else the program `command` runs (see [`program_name`]) |
/// | Read, Write, Edit, MultiEdit, NotebookEdit | file name of `file_path` (or `notebook_path`) |
/// | Grep, Glob | `pattern` |
/// | WebFetch | host of `url` |
/// | WebSearch | `query` |
/// | Task, Agent | `description`, else `subagent_type` |
/// | Skill | `skill` |
/// | anything else | `description` |
pub fn tool_summary(tool_name: &str, input: &Value) -> Option<String> {
    let get = |key: &str| non_empty(input.get(key));
    let first = |keys: &[&str]| keys.iter().find_map(|k| get(k));
    let raw = match tool_name {
        "Bash" | "BashOutput" | "PowerShell" => {
            get("description").or_else(|| get("command").and_then(|c| program_name(&c)))
        }
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            first(&["file_path", "notebook_path"])
                .and_then(|p| file_name_of(&p).map(str::to_string))
        }
        "Grep" | "Glob" => get("pattern"),
        "WebFetch" => get("url").and_then(|u| url_host(&u).map(str::to_string)),
        "WebSearch" => get("query"),
        "Task" | "Agent" => first(&["description", "subagent_type"]),
        "Skill" => get("skill"),
        _ => get("description"),
    }?;
    truncate_text(&redact_secrets(&raw), MAX_TOOL_SUMMARY_CHARS)
}

/// The program a shell command line runs, without its folder: the first
/// word past `NAME=value` assignments and PowerShell's `&`, e.g. `git` for
/// `GIT_TOKEN=x /usr/bin/git push`. `None` when there is none.
pub fn program_name(command: &str) -> Option<String> {
    crate::shell::words(command, 16)
        .into_iter()
        .find(|w| w != "&" && !is_assignment(w))
        .and_then(|w| file_name_of(&w).map(str::to_string))
}

/// `NAME=...` with `NAME` a shell variable name.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        let mut c = name.chars();
        c.next()
            .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
            && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
    })
}

/// Words in a variable or option name that mark its value as secret.
const SECRET_NAMES: &[&str] = &[
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH",
];

/// Prefixes of well-known API token formats; a word starting with one,
/// followed by at least [`MIN_TOKEN_TAIL`] token characters, is masked.
const TOKEN_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
];

/// Token characters a known prefix must be followed by to count as a token.
const MIN_TOKEN_TAIL: usize = 16;

/// Masks obvious secrets in free text with [`REDACTED`]:
///
/// - the value of `NAME=value` (or `NAME = value`) when `NAME` contains
///   `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL` or `AUTH`
///   (any case; `--api-key=...` and `$env:API_KEY = ...` too);
/// - the word after `Bearer`;
/// - words that look like `sk-...`, `ghp_...`, `github_pat_...`, `xoxb-...`
///   or `AKIA...` tokens.
///
/// It is a safety net for the excerpts cc-hook writes to disk, not a
/// scanner: it works word by word and keeps everything else as it was.
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut mask_next = false;
    let mut rest = text;
    while !rest.is_empty() {
        let ws = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..ws]);
        rest = &rest[ws..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        rest = tail;
        if word.is_empty() {
            continue;
        }
        if mask_next {
            mask_next = false;
            out.push_str(REDACTED);
            continue;
        }
        let bare = word.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')'));
        if bare.eq_ignore_ascii_case("bearer") {
            out.push_str(word);
            mask_next = true;
            continue;
        }
        if let Some(i) = word.find('=') {
            let (name, value) = (&word[..i], &word[i + 1..]);
            if is_secret_name(name) {
                out.push_str(&word[..=i]);
                // `NAME= value`: the value is the next word.
                if value.trim_matches(['"', '\'']).is_empty() {
                    mask_next = true;
                } else {
                    out.push_str(REDACTED);
                }
                continue;
            }
        }
        // `$env:API_KEY = "x"`: the value is the word after a lone `=`.
        if word == "="
            && out
                .trim_end()
                .rsplit(char::is_whitespace)
                .next()
                .is_some_and(is_secret_name)
        {
            out.push_str(word);
            mask_next = true;
            continue;
        }
        out.push_str(&mask_tokens(word));
    }
    out
}

fn is_secret_name(name: &str) -> bool {
    let name = name.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    SECRET_NAMES.iter().any(|s| upper.contains(s))
}

/// Replaces each known-format token inside `word` with [`REDACTED`].
fn mask_tokens(word: &str) -> String {
    let is_token_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < word.len() {
        let at_boundary = word[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if at_boundary {
            for prefix in TOKEN_PREFIXES {
                if let Some(after) = word[i..].strip_prefix(prefix) {
                    let tail = after
                        .find(|c: char| !is_token_char(c))
                        .unwrap_or(after.len());
                    if tail >= MIN_TOKEN_TAIL {
                        out.push_str(REDACTED);
                        i += prefix.len() + tail;
                        continue 'outer;
                    }
                }
            }
        }
        let c = word[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Last component of a path written with either separator, so a Windows path
/// is split correctly on Linux too. `None` when the path ends in a separator.
pub fn file_name_of(path: &str) -> Option<&str> {
    let name = path.trim().rsplit(['/', '\\']).next()?;
    (!name.is_empty()).then_some(name)
}

/// Host part of a URL, without scheme, user info, port, path, query or
/// fragment. IPv6 literals keep their brackets.
pub fn url_host(url: &str) -> Option<&str> {
    let rest = url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host_port.starts_with('[') {
        host_port
            .find(']')
            .map_or(host_port, |end| &host_port[..=end])
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    (!host.is_empty()).then_some(host)
}

/// A string field that is not blank, kept verbatim (identifiers and paths).
fn non_empty(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Free text from a string, or, for a number, object or array (some error
/// payloads), its compact JSON.
fn text_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Null | Value::Bool(_) => None,
        other => Some(other.to_string()),
    }
}

/// `model` is a plain id in hook payloads, but accept the status-line shape
/// `{ "id": ..., "display_name": ... }` too.
fn model_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(_) => non_empty(value),
        Value::Object(m) => non_empty(m.get("id")).or_else(|| non_empty(m.get("display_name"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requires_event_name_and_session_id() {
        assert!(build_event(&json!({"session_id": "s"}), 1.0, None).is_none());
        assert!(build_event(&json!({"hook_event_name": "Stop"}), 1.0, None).is_none());
        assert!(build_event(
            &json!({"hook_event_name": "Stop", "session_id": ""}),
            1.0,
            None
        )
        .is_none());
        assert!(build_event(&json!([1, 2]), 1.0, None).is_none());
        assert!(build_event(&json!("Stop"), 1.0, None).is_none());
    }

    #[test]
    fn keeps_named_fields_of_a_pre_tool_use() {
        let p = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s-1",
            "cwd": "C:\\repo",
            "transcript_path": "C:\\t.jsonl",
            "prompt_id": "p-1",
            "agent_id": "a-1",
            "agent_type": "Explore",
            "permission_mode": "default",
            "tool_name": "Bash",
            "tool_use_id": "toolu_1",
            "tool_input": {"command": "npm test", "description": "Run the tests"}
        });
        let e = build_event(&p, 12.5, Some("vscode".into())).unwrap();
        assert_eq!(e.event, "PreToolUse");
        assert_eq!(e.session_id, "s-1");
        assert_eq!(e.ts, 12.5);
        assert_eq!(e.cwd.as_deref(), Some("C:\\repo"));
        assert_eq!(e.transcript_path.as_deref(), Some("C:\\t.jsonl"));
        assert_eq!(e.prompt_id.as_deref(), Some("p-1"));
        assert_eq!(e.agent_id.as_deref(), Some("a-1"));
        assert_eq!(e.agent_type.as_deref(), Some("Explore"));
        assert_eq!(e.tool_name.as_deref(), Some("Bash"));
        assert_eq!(e.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(e.tool_summary.as_deref(), Some("Run the tests"));
        assert_eq!(e.term_program.as_deref(), Some("vscode"));
        assert_eq!(e.pid, None);
        assert_eq!(e.subagent_type, None);
    }

    #[test]
    fn agent_calls_keep_their_subagent_type() {
        let call = |event: &str, tool: &str, input: Value| {
            let p = json!({
                "hook_event_name": event, "session_id": "s",
                "tool_name": tool, "tool_use_id": "toolu_9", "tool_input": input
            });
            build_event(&p, 1.0, None).unwrap()
        };
        let e = call(
            "PreToolUse",
            "Agent",
            json!({"description": "Find usages", "subagent_type": "Explore", "prompt": "secret"}),
        );
        assert_eq!(e.subagent_type.as_deref(), Some("Explore"));
        assert_eq!(e.tool_summary.as_deref(), Some("Find usages"));
        let e = call(
            "PostToolUseFailure",
            "Task",
            json!({"subagent_type": "Plan"}),
        );
        assert_eq!(e.subagent_type.as_deref(), Some("Plan"));
        assert_eq!(e.tool_use_id.as_deref(), Some("toolu_9"));
        // Only Agent calls; a blank or missing type is absent.
        assert_eq!(
            call("PreToolUse", "Bash", json!({"subagent_type": "Explore"})).subagent_type,
            None
        );
        assert_eq!(
            call("PreToolUse", "Agent", json!({"subagent_type": " "})).subagent_type,
            None
        );
        assert_eq!(
            call("PreToolUse", "Agent", json!({"description": "x"})).subagent_type,
            None
        );
    }

    #[test]
    fn texts_use_fallback_keys_and_are_truncated() {
        let long = "x".repeat(MAX_TEXT_CHARS + 50);
        let p = json!({
            "hook_event_name": "UserPromptSubmit", "session_id": "s",
            "user_input": "fallback\nprompt",
            "error_message": "boom",
            "last_assistant_message": long,
            "message": "Claude needs your permission",
            "notification_type": "permission_prompt"
        });
        let e = build_event(&p, 1.0, None).unwrap();
        assert_eq!(e.user_input.as_deref(), Some("fallback prompt"));
        assert_eq!(e.error_message.as_deref(), Some("boom"));
        assert_eq!(
            e.last_message.as_ref().unwrap().chars().count(),
            MAX_TEXT_CHARS + 1
        );
        assert!(e.last_message.as_ref().unwrap().ends_with('…'));
        assert_eq!(e.message.as_deref(), Some("Claude needs your permission"));
        assert_eq!(e.notification_type.as_deref(), Some("permission_prompt"));

        let p = json!({"hook_event_name": "UserPromptSubmit", "session_id": "s",
                       "prompt": "primary", "user_input": "fallback", "error": "e1", "error_message": "e2"});
        let e = build_event(&p, 1.0, None).unwrap();
        assert_eq!(e.user_input.as_deref(), Some("primary"));
        assert_eq!(e.error_message.as_deref(), Some("e1"));
    }

    #[test]
    fn session_fields_and_model_shapes() {
        let p = json!({"hook_event_name": "SessionStart", "session_id": "s", "source": "resume", "model": "claude-opus-5-5"});
        let e = build_event(&p, 1.0, Some("  ".into())).unwrap();
        assert_eq!(e.source.as_deref(), Some("resume"));
        assert_eq!(e.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(e.term_program, None);

        let p = json!({"hook_event_name": "SessionEnd", "session_id": "s", "reason": "logout",
                       "model": {"id": "m-1", "display_name": "Opus"}});
        let e = build_event(&p, 1.0, None).unwrap();
        assert_eq!(e.reason.as_deref(), Some("logout"));
        assert_eq!(e.model.as_deref(), Some("m-1"));
    }

    #[test]
    fn odd_field_types_are_ignored() {
        let p = json!({"hook_event_name": "Stop", "session_id": "s", "cwd": 5, "tool_name": ["x"],
                       "last_assistant_message": null, "error": {"code": 529}});
        let e = build_event(&p, 1.0, None).unwrap();
        assert_eq!(e.cwd, None);
        assert_eq!(e.tool_name, None);
        assert_eq!(e.last_message, None);
        assert_eq!(e.error_message.as_deref(), Some(r#"{"code":529}"#));
    }

    #[test]
    fn tool_summaries_per_tool() {
        let s = |tool: &str, input: Value| tool_summary(tool, &input);
        assert_eq!(
            s("Bash", json!({"command": "ls -la"})).as_deref(),
            Some("ls")
        );
        assert_eq!(
            s(
                "Bash",
                json!({"command": "API_TOKEN=abc123 /usr/local/bin/deploy --password hunter2"})
            )
            .as_deref(),
            Some("deploy"),
            "never the command line"
        );
        assert_eq!(
            s(
                "PowerShell",
                json!({"command": "& \"C:\\Program Files\\Git\\bin\\git.exe\" push"})
            )
            .as_deref(),
            Some("git.exe")
        );
        assert_eq!(s("Bash", json!({"command": "   "})), None);
        assert_eq!(
            s("Bash", json!({"command": "ls", "description": "List"})).as_deref(),
            Some("List")
        );
        assert_eq!(s("BashOutput", json!({"bash_id": "b1"})), None);
        assert_eq!(
            s("Read", json!({"file_path": "C:\\repo\\src\\main.rs"})).as_deref(),
            Some("main.rs")
        );
        assert_eq!(
            s("Write", json!({"file_path": "/home/u/a.txt"})).as_deref(),
            Some("a.txt")
        );
        assert_eq!(
            s("Edit", json!({"file_path": "C:/x/lib.rs"})).as_deref(),
            Some("lib.rs")
        );
        assert_eq!(
            s("NotebookEdit", json!({"notebook_path": "/n/a.ipynb"})).as_deref(),
            Some("a.ipynb")
        );
        assert_eq!(
            s("Grep", json!({"pattern": "fn main"})).as_deref(),
            Some("fn main")
        );
        assert_eq!(
            s("Glob", json!({"pattern": "**/*.rs"})).as_deref(),
            Some("**/*.rs")
        );
        assert_eq!(
            s(
                "WebFetch",
                json!({"url": "https://user:pw@docs.rs:443/serde?x=1"})
            )
            .as_deref(),
            Some("docs.rs")
        );
        assert_eq!(
            s("WebSearch", json!({"query": "tauri tray"})).as_deref(),
            Some("tauri tray")
        );
        assert_eq!(
            s("Task", json!({"subagent_type": "Explore"})).as_deref(),
            Some("Explore")
        );
        assert_eq!(
            s(
                "Agent",
                json!({"description": "Find usages", "subagent_type": "Explore"})
            )
            .as_deref(),
            Some("Find usages")
        );
        assert_eq!(s("Skill", json!({"skill": "pdf"})).as_deref(), Some("pdf"));
        assert_eq!(
            s("mcp__x__y", json!({"description": "Do it"})).as_deref(),
            Some("Do it")
        );
        assert_eq!(s("mcp__x__y", json!({"other": 1})), None);
        assert_eq!(s("Bash", Value::Null), None);
    }

    #[test]
    fn tool_summary_is_flattened_and_truncated() {
        let description = format!("echo a\necho {}", "b".repeat(300));
        let out = tool_summary("Bash", &json!({ "description": description })).unwrap();
        assert!(out.starts_with("echo a echo b"));
        assert_eq!(out.chars().count(), MAX_TOOL_SUMMARY_CHARS + 1);
    }

    #[test]
    fn obvious_secrets_are_masked() {
        let r = |t: &str| redact_secrets(t);
        assert_eq!(
            r("export GITHUB_TOKEN=ghx123 && run"),
            "export GITHUB_TOKEN=[redacted] && run"
        );
        assert_eq!(
            r("curl --api-key=abc -H x"),
            "curl --api-key=[redacted] -H x"
        );
        assert_eq!(r("PASSWORD= hunter2 next"), "PASSWORD= [redacted] next");
        assert_eq!(r("$env:API_KEY = \"x y\""), "$env:API_KEY = [redacted] y\"");
        assert_eq!(
            r("Authorization: Bearer eyJhbGciOi.x.y done"),
            "Authorization: Bearer [redacted] done"
        );
        assert_eq!(
            r("use sk-ant-api03-AAAAAAAAAAAAAAAAAAAA now"),
            "use [redacted] now"
        );
        assert_eq!(r("(ghp_0123456789abcdefABCDEF)"), "([redacted])");
        assert_eq!(r("AKIAABCDEFGHIJKLMNOP"), "[redacted]");
        // Ordinary text is left alone, short look-alikes included.
        for plain in [
            "sk-learn is a library",
            "task-1234 done",
            "x=1 y=2",
            "the key point",
            "a  b\tc",
        ] {
            assert_eq!(r(plain), plain);
        }
    }

    #[test]
    fn persisted_excerpts_are_redacted() {
        let p = json!({
            "hook_event_name": "UserPromptSubmit", "session_id": "s",
            "prompt": "deploy with DB_PASSWORD=s3cret please",
            "error": "401 for Bearer abc.def",
            "last_assistant_message": "set OPENAI_API_KEY=sk-proj-0123456789abcdef0123"
        });
        let e = build_event(&p, 1.0, None).unwrap();
        assert_eq!(
            e.user_input.as_deref(),
            Some("deploy with DB_PASSWORD=[redacted] please")
        );
        assert_eq!(
            e.error_message.as_deref(),
            Some("401 for Bearer [redacted]")
        );
        assert_eq!(
            e.last_message.as_deref(),
            Some("set OPENAI_API_KEY=[redacted]")
        );
    }

    #[test]
    fn file_names_and_hosts() {
        assert_eq!(file_name_of("C:\\a\\b.txt"), Some("b.txt"));
        assert_eq!(file_name_of("b.txt"), Some("b.txt"));
        assert_eq!(file_name_of("/a/b/"), None);
        assert_eq!(url_host("http://localhost:8080/x"), Some("localhost"));
        assert_eq!(url_host("https://[::1]:3000/"), Some("[::1]"));
        assert_eq!(url_host("example.com/path"), Some("example.com"));
        assert_eq!(url_host("https:///nohost"), None);
    }
}
