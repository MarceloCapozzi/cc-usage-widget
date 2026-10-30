//! Session state shared across the widget, the publisher and the wire.
//!
//! This file fixes the vocabulary. The state machine that drives it from spool
//! events and registry snapshots lives in `state.rs`.

use serde::{Deserialize, Serialize};

/// The five visible states, plus `Offline` for remote rows whose heartbeat expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    NeedsYou,
    Failed,
    Working,
    Done,
    Idle,
    Offline,
}

impl SessionState {
    /// Panel order: what needs the user first, then failures, work, finished, idle, offline.
    pub fn sort_rank(self) -> u8 {
        match self {
            SessionState::NeedsYou => 0,
            SessionState::Failed => 1,
            SessionState::Working => 2,
            SessionState::Done => 3,
            SessionState::Idle => 4,
            SessionState::Offline => 5,
        }
    }
}

/// Where a row comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "alias", rename_all = "camelCase")]
pub enum Origin {
    /// This machine, read from disk.
    Local,
    /// A publisher machine, identified by its alias (e.g. `lab-01`).
    Remote(String),
}

/// What Claude Code itself reports about a live session: one item of
/// `claude agents --json`, enriched from `~/.claude/sessions/<pid>.json`.
/// Produced by `cc-local`, consumed by the state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The registry's name for the session (a slug like `my-repo-35`, or one the user set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `interactive` or `background`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub status: RegistryStatus,
    /// What a waiting session waits for, when Claude Code says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
    /// From the registry file: `claude-vscode`, `cli`, ... Used to pick a focus strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    /// The background job id (`id` in `claude agents --json`), only on
    /// background sessions that also report a `sessionId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_id: Option<String>,
}

/// `status` as `claude agents --json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RegistryStatus {
    Busy,
    Waiting,
    Idle,
    /// A value this build does not know, kept verbatim.
    #[serde(untagged)]
    Other(String),
}

/// What a session's transcript records and nothing else does. The last record
/// of each kind wins. Produced by `cc-local`, consumed by the state machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptMarks {
    /// Set by the user with `/rename` or `--name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    /// Derived by Claude Code from the conversation (`ai-title`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Set with `/color`, e.g. `green`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Model of the newest main-thread assistant message, e.g. `claude-opus-5-5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tokens the newest main-thread assistant message was given as input
    /// (`input_tokens + cache_creation_input_tokens + cache_read_input_tokens`):
    /// how full the context is. The window size is not in the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
}

/// Longest session title a publisher sends, in characters (`…` added when cut).
pub const MAX_REMOTE_TITLE: usize = 80;
/// Longest folder name a publisher sends, in characters.
pub const MAX_REMOTE_DIR: usize = 64;
/// Longest second-line text a publisher sends, in characters.
pub const MAX_REMOTE_TEXT: usize = 120;
/// Longest tool name a remote row shows, in characters: the viewer cuts a
/// longer one (real tool names are far shorter).
pub const MAX_REMOTE_TOOL: usize = 64;
/// Longest model name a remote row shows, in characters: the viewer cuts a
/// longer one (real model ids are far shorter).
pub const MAX_REMOTE_MODEL: usize = 64;
/// Most subagents a publisher reports per session; the viewer caps at this too.
pub const MAX_REMOTE_AGENTS: u16 = 64;
/// Longest subagent type a publisher sends, in characters.
pub const MAX_REMOTE_AGENT_TYPE: usize = 24;
/// Longest subagent task description a publisher sends, in characters.
pub const MAX_REMOTE_AGENT_TASK: usize = 60;
/// Longest subagent tool name a publisher sends, in characters.
pub const MAX_REMOTE_AGENT_TOOL: usize = 32;
/// Longest subagent tool summary a publisher sends, in characters.
pub const MAX_REMOTE_AGENT_SUMMARY: usize = 60;
/// Most subagents a publisher describes one by one ([`RemotePayload::ags`]);
/// the rest are only counted. The viewer caps at this too.
pub const MAX_REMOTE_AGENT_ROWS: usize = 6;
/// Most bytes of JSON in a session payload; the subagent lines get what
/// the other fields leave (see [`RemotePayload::fit_agents`]), and agents
/// past it are only counted.
///
/// Sealed, a payload of this size takes about 3950 of the envelope's 4096
/// bytes (base64 adds a third, the envelope's own fields about 200), so the
/// limit holds with every field at its cap. A static share for the lines
/// could not do both: six lines at their caps take about 1400 bytes, while
/// the other fields at their caps in four-byte characters leave only about
/// 1000. With texts of up to three bytes per character (any script but
/// emoji) at their caps, six full lines still fit.
pub const MAX_REMOTE_PAYLOAD_BYTES: usize = 2800;

/// What a publisher sends for one session, before encryption (the envelope
/// opens only on the viewer): the state, tool name, model and context, and
/// what the row shows: the title, the folder name (never the path) and the
/// one second-line text the state uses (a question, a tool summary, an
/// error or the last assistant message), each cut short, and the subagent
/// counts with a short line for the first few (type, task description,
/// tool name and summary). Never the prompts, the conversation beyond those
/// short texts, or a path.
///
/// Field names are short because every byte is stored in the KV bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemotePayload {
    /// Publisher alias, same as the first token of the KV key (the viewer
    /// rejects a value where they differ).
    pub machine: String,
    /// Truncated SHA-256 of the session id, 8 hex characters, same as the
    /// last token of the KV key (the viewer goes by the key's).
    pub sid: String,
    /// Optional alias chosen by the user for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// The session's title: its custom title, else its AI title, else its
    /// registry name; at most [`MAX_REMOTE_TITLE`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The last component of the working directory, at most
    /// [`MAX_REMOTE_DIR`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// The row's second line for the state: the question (needs you), the
    /// tool summary (working), the error (failed) or the last message (done,
    /// idle); at most [`MAX_REMOTE_TEXT`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Subagents as `[running, total]` (total counts the recently finished
    /// ones too), at most [`MAX_REMOTE_AGENTS`]; absent when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<[u16; 2]>,
    /// What the first subagents do, running ones first, each in start
    /// order: at most [`MAX_REMOTE_AGENT_ROWS`], and only as many as keep
    /// the payload within [`MAX_REMOTE_PAYLOAD_BYTES`] (see
    /// [`Self::fit_agents`]). The others are only in [`Self::agents`].
    /// Absent without subagents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ags: Option<Vec<RemoteAgent>>,
    pub state: SessionState,
    /// Unix seconds of the last state transition.
    pub since: i64,
    /// Tool name only, never its arguments, at most [`MAX_REMOTE_TOOL`]
    /// characters (cut on the publisher and again on the viewer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The session's model, at most [`MAX_REMOTE_MODEL`] characters (cut on
    /// the publisher and again on the viewer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Context window used, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<u8>,
    /// Unix seconds of this write; the viewer marks the row offline when stale.
    pub hb: i64,
}

/// One subagent as it travels in [`RemotePayload::ags`]: a line for the
/// row's agents block. Field names are short for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteAgent {
    /// `agent_type`, e.g. `Explore`; at most [`MAX_REMOTE_AGENT_TYPE`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
    /// Its task: the `description` of the main agent's `Agent` call; at
    /// most [`MAX_REMOTE_AGENT_TASK`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<String>,
    /// The tool it runs (name only); at most [`MAX_REMOTE_AGENT_TOOL`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// That tool's summary, as the hook made it; at most
    /// [`MAX_REMOTE_AGENT_SUMMARY`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    /// Still running (else finished recently).
    pub run: bool,
}

impl RemoteAgent {
    /// A subagent line with every text through [`remote_text`] at its cap.
    /// Publishers build it this way, and the viewer again.
    pub fn new(
        agent_type: Option<&str>,
        task: Option<&str>,
        tool: Option<&str>,
        summary: Option<&str>,
        running: bool,
    ) -> Self {
        let cut = |v: Option<&str>, max| v.and_then(|v| remote_text(v, max));
        Self {
            t: cut(agent_type, MAX_REMOTE_AGENT_TYPE),
            d: cut(task, MAX_REMOTE_AGENT_TASK),
            tool: cut(tool, MAX_REMOTE_AGENT_TOOL),
            s: cut(summary, MAX_REMOTE_AGENT_SUMMARY),
            run: running,
        }
    }

    /// Bytes of its JSON, exactly as serde_json writes it (compact).
    pub fn json_len(&self) -> usize {
        let text = |key: &str, v: &Option<String>| {
            v.as_deref()
                .map_or(0, |v| key.len() + 3 + json_string_len(v))
        };
        let fields = [
            text("t", &self.t),
            text("d", &self.d),
            text("tool", &self.tool),
            text("s", &self.s),
        ];
        let present = fields.iter().filter(|&&n| n > 0).count();
        let run = if self.run { 10 } else { 11 };
        // Braces, the texts, `"run":...` and one comma per text before it.
        2 + fields.iter().sum::<usize>() + present + run
    }
}

/// Bytes of a JSON string literal for `v`, quotes and escapes included.
fn json_string_len(v: &str) -> usize {
    2 + v
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}

/// `,"ags":`, what [`RemotePayload::ags`] adds besides its list.
const AGS_FIELD_BYTES: usize = 7;

impl RemotePayload {
    /// Bytes of its JSON, exactly as serde_json writes it (compact).
    pub fn json_len(&self) -> usize {
        let field = |key: &str, value: usize| key.len() + 3 + value;
        let text = |key: &str, v: Option<&str>| v.map(|v| field(key, json_string_len(v)));
        let digits = |n: i64| n.to_string().len();
        let fields = [
            text("machine", Some(&self.machine)),
            text("sid", Some(&self.sid)),
            text("alias", self.alias.as_deref()),
            text("title", self.title.as_deref()),
            text("dir", self.dir.as_deref()),
            text("text", self.text.as_deref()),
            // `[running,total]`.
            self.agents
                .map(|[r, t]| field("agents", 3 + digits(r.into()) + digits(t.into()))),
            self.ags.as_deref().map(|a| field("ags", agent_list_len(a))),
            text("state", Some(state_name(self.state))),
            Some(field("since", digits(self.since))),
            text("tool", self.tool.as_deref()),
            text("model", self.model.as_deref()),
            self.ctx.map(|c| field("ctx", digits(c.into()))),
            Some(field("hb", digits(self.hb))),
        ];
        let present = fields.iter().flatten().count();
        // Braces, the fields and a comma between each two.
        2 + fields.iter().flatten().sum::<usize>() + present - 1
    }

    /// Sets [`Self::ags`] to the first lines of `agents`, in the order to
    /// show them: at most [`MAX_REMOTE_AGENT_ROWS`], up to the first one
    /// that would take the payload past [`MAX_REMOTE_PAYLOAD_BYTES`].
    /// `None` when that leaves none. Call it once the other fields are set.
    pub fn fit_agents(&mut self, agents: impl IntoIterator<Item = RemoteAgent>) {
        self.ags = None;
        let budget = MAX_REMOTE_PAYLOAD_BYTES.saturating_sub(self.json_len() + AGS_FIELD_BYTES);
        self.ags = fit_remote_agents(agents, budget);
    }
}

/// How [`SessionState`] serializes, without its quotes.
fn state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::NeedsYou => "needsYou",
        SessionState::Failed => "failed",
        SessionState::Working => "working",
        SessionState::Done => "done",
        SessionState::Idle => "idle",
        SessionState::Offline => "offline",
    }
}

/// Bytes of a JSON list of subagent lines.
fn agent_list_len(agents: &[RemoteAgent]) -> usize {
    2 + agents.iter().map(RemoteAgent::json_len).sum::<usize>() + agents.len().saturating_sub(1)
}

/// The first [`MAX_REMOTE_AGENT_ROWS`] of `agents`, up to the first one
/// that would take their JSON list past `budget` bytes. `None` when that
/// leaves none.
fn fit_remote_agents(
    agents: impl IntoIterator<Item = RemoteAgent>,
    budget: usize,
) -> Option<Vec<RemoteAgent>> {
    let mut out = Vec::new();
    // The brackets, then each line and the comma before it.
    let mut bytes = 2;
    for a in agents.into_iter().take(MAX_REMOTE_AGENT_ROWS) {
        let len = a.json_len() + usize::from(!out.is_empty());
        if bytes + len > budget {
            break;
        }
        bytes += len;
        out.push(a);
    }
    (!out.is_empty()).then_some(out)
}

/// A text as it travels in a [`RemotePayload`]: control characters and
/// runs of white space become one space, trimmed, cut to `max` characters
/// plus `…`. `None` when nothing is left. Publishers cut with it, and the
/// viewer again, so a value from a foreign publisher stays short.
pub fn remote_text(value: &str, max: usize) -> Option<String> {
    let mut out = String::new();
    let mut count = 0;
    let mut pending_space = false;
    for c in value.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            if count == max {
                out.push('…');
                return Some(out);
            }
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        if count == max {
            out.push('…');
            return Some(out);
        }
        out.push(c);
        count += 1;
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_text_flattens_trims_and_cuts() {
        assert_eq!(
            remote_text("  Fix\n the\ttray \u{7}icon  ", 80).as_deref(),
            Some("Fix the tray icon")
        );
        assert_eq!(remote_text(" \n\t ", 80), None);
        assert_eq!(remote_text("abcdef", 6).as_deref(), Some("abcdef"));
        assert_eq!(remote_text("abcdefg", 6).as_deref(), Some("abcdef…"));
        assert_eq!(remote_text("abc def", 3).as_deref(), Some("abc…"));
        assert_eq!(remote_text("abc ", 3).as_deref(), Some("abc"));
        assert_eq!(remote_text(&"é".repeat(200), 5).unwrap().chars().count(), 6);
    }

    #[test]
    fn states_serialize_as_camel_case() {
        assert_eq!(
            serde_json::to_string(&SessionState::NeedsYou).unwrap(),
            "\"needsYou\""
        );
    }

    #[test]
    fn sort_rank_puts_needs_you_first() {
        let mut v = [
            SessionState::Idle,
            SessionState::NeedsYou,
            SessionState::Working,
        ];
        v.sort_by_key(|s| s.sort_rank());
        assert_eq!(
            v,
            [
                SessionState::NeedsYou,
                SessionState::Working,
                SessionState::Idle
            ]
        );
    }

    #[test]
    fn registry_status_keeps_unknown_values() {
        let s: RegistryStatus = serde_json::from_str("\"busy\"").unwrap();
        assert_eq!(s, RegistryStatus::Busy);
        let s: RegistryStatus = serde_json::from_str("\"compacting\"").unwrap();
        assert_eq!(s, RegistryStatus::Other("compacting".into()));
    }

    #[test]
    fn remote_payload_omits_empty_optionals() {
        let p = RemotePayload {
            machine: "lab-01".into(),
            sid: "3f9a1c2e".into(),
            alias: None,
            title: None,
            dir: None,
            text: None,
            agents: None,
            ags: None,
            state: SessionState::Working,
            since: 1,
            tool: Some("Bash".into()),
            model: None,
            ctx: Some(42),
            hb: 2,
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("alias") && !json.contains("model") && !json.contains("title"));
        assert!(!json.contains("ags"));
        assert_eq!(serde_json::from_str::<RemotePayload>(&json).unwrap(), p);
    }

    #[test]
    fn remote_agent_cuts_its_texts_and_travels_with_short_names() {
        let a = RemoteAgent::new(
            Some("general-purpose-with-a-very-long-name"),
            Some("Find every   caller\nof remote_text"),
            Some("Grep"),
            None,
            true,
        );
        assert_eq!(a.t.as_deref(), Some("general-purpose-with-a-v…"));
        assert_eq!(a.d.as_deref(), Some("Find every caller of remote_text"));
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(
            json,
            r#"{"t":"general-purpose-with-a-v…","d":"Find every caller of remote_text","tool":"Grep","run":true}"#
        );
        assert_eq!(serde_json::from_str::<RemoteAgent>(&json).unwrap(), a);
        // Every text is optional.
        let bare: RemoteAgent = serde_json::from_str(r#"{"run":false}"#).unwrap();
        assert_eq!(bare, RemoteAgent::new(None, None, None, None, false));
    }

    #[test]
    fn remote_agent_json_len_matches_serde_json() {
        let cases = [
            RemoteAgent::new(None, None, None, None, false),
            RemoteAgent::new(Some("Explore"), None, None, None, true),
            RemoteAgent::new(
                None,
                Some("say \"hi\" \\ there"),
                Some("Bash"),
                Some("é😀"),
                false,
            ),
            RemoteAgent {
                t: Some("a\u{1}b\nc".into()),
                d: None,
                tool: Some("\u{7f}".into()),
                s: Some(String::new()),
                run: true,
            },
            RemoteAgent::new(
                Some(&"😀".repeat(99)),
                Some(&"😀".repeat(99)),
                Some(&"😀".repeat(99)),
                Some(&"😀".repeat(99)),
                true,
            ),
        ];
        for a in cases {
            assert_eq!(a.json_len(), serde_json::to_vec(&a).unwrap().len(), "{a:?}");
        }
    }

    /// A payload with every field set, each text `text` cut at its cap.
    fn payload_with(text: &str, tool_and_model: &str) -> RemotePayload {
        RemotePayload {
            machine: "m".repeat(31),
            sid: "ffffffff".into(),
            alias: None,
            title: remote_text(text, MAX_REMOTE_TITLE),
            dir: remote_text(text, MAX_REMOTE_DIR),
            text: remote_text(text, MAX_REMOTE_TEXT),
            agents: Some([MAX_REMOTE_AGENTS; 2]),
            ags: None,
            state: SessionState::NeedsYou,
            since: i64::MAX,
            tool: remote_text(tool_and_model, MAX_REMOTE_TOOL),
            model: remote_text(tool_and_model, MAX_REMOTE_MODEL),
            ctx: Some(100),
            hb: i64::MAX,
        }
    }

    /// A subagent line with every text `text` cut at its cap.
    fn full_agent(text: &str) -> RemoteAgent {
        RemoteAgent::new(Some(text), Some(text), Some(text), Some(text), false)
    }

    #[test]
    fn remote_payload_json_len_matches_serde_json() {
        let mut p = payload_with(&"é\"".repeat(99), "Bash");
        p.fit_agents((0..3).map(|_| full_agent("x")));
        let mut cases = vec![p.clone()];
        for state in [
            SessionState::NeedsYou,
            SessionState::Failed,
            SessionState::Working,
            SessionState::Done,
            SessionState::Idle,
            SessionState::Offline,
        ] {
            let mut bare = RemotePayload {
                machine: "devbox".into(),
                sid: "3f9a1c2e".into(),
                alias: Some("a".into()),
                title: None,
                dir: None,
                text: None,
                agents: Some([0, 7]),
                ags: Some(Vec::new()),
                state,
                since: -5,
                tool: None,
                model: None,
                ctx: Some(0),
                hb: 0,
            };
            cases.push(bare.clone());
            bare.alias = None;
            bare.agents = None;
            bare.ags = None;
            bare.ctx = None;
            cases.push(bare);
        }
        for p in cases {
            assert_eq!(p.json_len(), serde_json::to_vec(&p).unwrap().len(), "{p:?}");
        }
    }

    #[test]
    fn fit_agents_keeps_the_first_rows_within_the_payload_budget() {
        let mut p = payload_with("x", "Bash");
        p.fit_agents(Vec::new());
        assert_eq!(p.ags, None);
        let small = |i: usize| {
            RemoteAgent::new(
                Some("Explore"),
                Some(&format!("task {i}")),
                None,
                None,
                true,
            )
        };
        p.fit_agents((0..10).map(small));
        let ags = p.ags.as_ref().unwrap();
        assert_eq!(ags.len(), MAX_REMOTE_AGENT_ROWS);
        assert_eq!(ags[0].d.as_deref(), Some("task 0"));

        // Six lines at every cap fit next to the other fields at theirs, in
        // ASCII and in three-byte characters.
        let long = "x".repeat(200);
        let cjk = "漢".repeat(200);
        for text in [&long, &cjk] {
            let mut p = payload_with(text, &long);
            p.fit_agents((0..10).map(|_| full_agent(&long)));
            assert_eq!(p.ags.as_ref().map(Vec::len), Some(MAX_REMOTE_AGENT_ROWS));
            assert!(p.json_len() <= MAX_REMOTE_PAYLOAD_BYTES);
        }

        // Four-byte characters in every field: the budget stops the list
        // early, and the payload never exceeds it.
        let emoji = "😀".repeat(200);
        let mut p = payload_with(&emoji, &emoji);
        p.fit_agents((0..MAX_REMOTE_AGENT_ROWS).map(|_| full_agent(&emoji)));
        let kept = p.ags.as_ref().map_or(0, Vec::len);
        assert!((1..MAX_REMOTE_AGENT_ROWS).contains(&kept), "{kept}");
        assert!(p.json_len() <= MAX_REMOTE_PAYLOAD_BYTES);

        // A first line past the budget on its own leaves none.
        let over = RemoteAgent {
            t: Some("x".repeat(MAX_REMOTE_PAYLOAD_BYTES)),
            d: None,
            tool: None,
            s: None,
            run: true,
        };
        p.fit_agents([over]);
        assert_eq!(p.ags, None);
    }
}
