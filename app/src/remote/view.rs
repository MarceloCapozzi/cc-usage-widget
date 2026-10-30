//! The rows the flyout shows: a remote session as a [`SessionView`], and
//! local and remote rows merged in panel order.

use cc_core::session::{Origin, SessionState};
use cc_core::state::SessionView;
use serde_json::{json, Value};

use super::map::{effective_state, SessionEntry};
use super::REMOTE_ID_PREFIX;
use crate::sessions::IdleFilter;

/// A remote session as a [`SessionView`]: only what travels (state, tool
/// name, model, context, title, folder name, the second line's text and
/// the subagents' counts and first lines), no pid or path. The texts, the
/// tool and the model are cut again here, so a publisher that sends longer
/// ones still fits the row. The row
/// id is `key`, and the name of last resort is the sid of `key`, never the
/// payload's.
pub(super) fn remote_view(key: &str, e: &SessionEntry, now: i64) -> SessionView {
    use cc_core::session::{
        remote_text, MAX_REMOTE_DIR, MAX_REMOTE_MODEL, MAX_REMOTE_TEXT, MAX_REMOTE_TITLE,
        MAX_REMOTE_TOOL,
    };
    let p = &e.payload;
    let tool = p
        .tool
        .as_deref()
        .and_then(|t| remote_text(t, MAX_REMOTE_TOOL));
    let model = p
        .model
        .as_deref()
        .and_then(|m| remote_text(m, MAX_REMOTE_MODEL));
    let state = effective_state(e, now);
    let offline = state == SessionState::Offline;
    let title = p
        .title
        .as_deref()
        .and_then(|t| remote_text(t, MAX_REMOTE_TITLE));
    // A folder name only: anything after a separator is dropped.
    let dir = p
        .dir
        .as_deref()
        .and_then(|d| d.rsplit(['/', '\\']).next())
        .and_then(|d| remote_text(d, MAX_REMOTE_DIR));
    let display_name = p
        .alias
        .as_deref()
        .and_then(|a| remote_text(a, MAX_REMOTE_TITLE))
        .or_else(|| title.clone())
        .or_else(|| dir.clone())
        .unwrap_or_else(|| e.sid.clone());
    // The text goes where the frontend looks for it in this state.
    let text = p
        .text
        .as_deref()
        .and_then(|t| remote_text(t, MAX_REMOTE_TEXT));
    let text_if = |s: SessionState| if state == s { text.clone() } else { None };
    // `since` and `hb` are on the publisher's clock, which may differ from
    // this one by minutes (a WSL clock after sleep, say). How long it has
    // been in its state is measured there (`hb - since`) and placed on this
    // clock from when the write was heard, so the elapsed time and the idle
    // filter do not move by the difference between the clocks.
    let in_state = p.hb.saturating_sub(p.since).max(0);
    let since_local = e.heard.saturating_sub(in_state).min(now);
    SessionView {
        id: format!("{REMOTE_ID_PREFIX}{key}"),
        display_name,
        state,
        // Offline: counted from the last heartbeat ("4m" in the mockup).
        state_since: if offline { e.heard } else { since_local } as f64,
        // The folder name stands in for the path: the frontend shows its
        // last component.
        cwd: dir,
        pid: None,
        entrypoint: None,
        color: None,
        current_tool: if offline { None } else { tool.clone() },
        tool_summary: if tool.is_some() {
            text_if(SessionState::Working)
        } else {
            None
        },
        tool_started_at: None,
        turn_started_at: None,
        stalled: false,
        subagents: if offline {
            Vec::new()
        } else {
            remote_agents(p.agents, p.ags.as_deref(), since_local as f64)
        },
        last_message: text_if(SessionState::Done).or_else(|| text_if(SessionState::Idle)),
        question: text_if(SessionState::NeedsYou),
        error_message: text_if(SessionState::Failed),
        model,
        context_percent: p.ctx.map(|c| c.min(100)),
        // The publisher sends a number either way; whether it was exact stays there.
        context_estimated: false,
        context_tokens: None,
        // Remote sessions are hook-driven on their machine; this also keeps
        // the frontend from showing the local "restart the session" hint.
        has_hooks: true,
        // "48s ago" and "no heartbeat since HH:MM" count from here.
        last_activity: e.heard as f64,
    }
}

/// A remote session's subagents as rows for the agents block and the "2/5
/// agents" tag: one per line in `ags` (at most [`MAX_REMOTE_AGENT_ROWS`],
/// each text cut again), then placeholders (no type, no tool) for the rest
/// of the `[running, total]` counts, running ones first. The counts are
/// capped at [`MAX_REMOTE_AGENTS`], running never exceeds total, and a line
/// beyond the counts (a foreign publisher) still shows. The times are not
/// sent: every row counts from `since`, the session's state change.
///
/// [`MAX_REMOTE_AGENTS`]: cc_core::session::MAX_REMOTE_AGENTS
/// [`MAX_REMOTE_AGENT_ROWS`]: cc_core::session::MAX_REMOTE_AGENT_ROWS
fn remote_agents(
    agents: Option<[u16; 2]>,
    ags: Option<&[cc_core::session::RemoteAgent]>,
    since: f64,
) -> Vec<cc_core::state::SubagentView> {
    use cc_core::session::{RemoteAgent, MAX_REMOTE_AGENTS, MAX_REMOTE_AGENT_ROWS};
    use cc_core::state::{SubagentState, SubagentView};
    let row = |i: usize, running: bool, line: Option<RemoteAgent>| {
        let line = line.unwrap_or_else(|| RemoteAgent::new(None, None, None, None, running));
        SubagentView {
            id: format!("remote-agent-{i}"),
            agent_type: line.t,
            description: line.d,
            state: if running {
                SubagentState::Working
            } else {
                SubagentState::Done
            },
            // A finished one runs nothing.
            current_tool: line.tool.filter(|_| running),
            tool_summary: line.s.filter(|_| running),
            started_at: since,
            finished_at: (!running).then_some(since),
        }
    };
    let (run_lines, done_lines): (Vec<RemoteAgent>, Vec<RemoteAgent>) = ags
        .unwrap_or_default()
        .iter()
        .take(MAX_REMOTE_AGENT_ROWS)
        .map(|a| {
            RemoteAgent::new(
                a.t.as_deref(),
                a.d.as_deref(),
                a.tool.as_deref(),
                a.s.as_deref(),
                a.run,
            )
        })
        .partition(|a| a.run);
    let [running, total] = agents.unwrap_or_default();
    let total = usize::from(total.min(MAX_REMOTE_AGENTS));
    let running = usize::from(running).min(total);
    let more_running = running.saturating_sub(run_lines.len());
    let more_done = (total - running).saturating_sub(done_lines.len());

    let mut out = Vec::with_capacity(run_lines.len() + more_running + done_lines.len() + more_done);
    for line in run_lines {
        out.push(row(out.len(), true, Some(line)));
    }
    for _ in 0..more_running {
        out.push(row(out.len(), true, None));
    }
    for line in done_lines {
        out.push(row(out.len(), false, Some(line)));
    }
    for _ in 0..more_done {
        out.push(row(out.len(), false, None));
    }
    out
}

pub(super) fn remote_order(a: &SessionView, b: &SessionView) -> std::cmp::Ordering {
    a.state
        .sort_rank()
        .cmp(&b.state.sort_rank())
        .then(b.state_since.total_cmp(&a.state_since))
        .then_with(|| a.id.cmp(&b.id))
}

/// One remote row: the machine alias (the row's origin tag) and its view.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteRow {
    /// Publisher machine alias.
    pub alias: String,
    /// The row, shaped like a local one.
    pub view: SessionView,
}

/// Local and remote rows in panel order, as the frontend renders them: each
/// [`SessionView`] (camelCase) plus `origin`, `{"kind":"local"}` or
/// `{"kind":"remote","alias":...}`, and `hidden` (always false here; see
/// [`merge_rows_with`]).
///
/// Rows are ordered by state rank. Within a rank the local rows come first,
/// in the order the tracker gave them, then the remote ones as
/// [`RemoteMap::rows`] sorted them (most recent state change first), which
/// is the order of the demo fixture in `demo.rs`.
///
/// [`RemoteMap::rows`]: super::RemoteMap::rows
pub fn merge_rows(local: &[SessionView], remote: &[RemoteRow]) -> Vec<Value> {
    merge_rows_with(local, remote, IdleFilter::default(), 0.0)
}

/// [`merge_rows`], with every row also carrying `hidden`: true when `filter`
/// hides it at `now` (Unix seconds). Hidden rows stay in the list, in their
/// place, so the flyout can say how many it is not showing.
pub fn merge_rows_with(
    local: &[SessionView],
    remote: &[RemoteRow],
    filter: IdleFilter,
    now: f64,
) -> Vec<Value> {
    let local_origin = serde_json::to_value(Origin::Local).unwrap_or(Value::Null);
    let mut tagged: Vec<(u8, bool, Value)> = Vec::with_capacity(local.len() + remote.len());
    for v in local {
        let hidden = filter.hides(v, now);
        tagged.push((
            v.state.sort_rank(),
            false,
            row_json(v, local_origin.clone(), hidden),
        ));
    }
    for r in remote {
        let origin = serde_json::to_value(Origin::Remote(r.alias.clone())).unwrap_or(Value::Null);
        let hidden = filter.hides(&r.view, now);
        tagged.push((
            r.view.state.sort_rank(),
            true,
            row_json(&r.view, origin, hidden),
        ));
    }
    // Stable: equal keys keep the order they were pushed in.
    tagged.sort_by_key(|(rank, remote, _)| (*rank, *remote));
    tagged.into_iter().map(|(_, _, v)| v).collect()
}

/// Gives every local row of `rows` (as [`merge_rows`] builds them) this
/// machine's name, `origin: {"kind":"local","alias":name}`, so the flyout
/// tags it like a remote one. Remote rows are left as they are.
pub fn name_local_rows(rows: &mut [Value], name: &str) {
    for row in rows {
        if let Some(Value::Object(origin)) = row.get_mut("origin") {
            if origin.get("kind").and_then(Value::as_str) == Some("local") {
                origin.insert("alias".into(), Value::String(name.to_string()));
            }
        }
    }
}

fn row_json(view: &SessionView, origin: Value, hidden: bool) -> Value {
    let mut v = serde_json::to_value(view).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut v {
        map.insert("origin".into(), origin);
        map.insert("hidden".into(), Value::Bool(hidden));
    }
    v
}

/// The JSON `origin` of a row, for tests and callers outside [`merge_rows`].
pub fn origin_json(alias: Option<&str>) -> Value {
    match alias {
        Some(alias) => json!({ "kind": "remote", "alias": alias }),
        None => json!({ "kind": "local" }),
    }
}
