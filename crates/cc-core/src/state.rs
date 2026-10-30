//! The session state machine.
//!
//! [`SessionTracker`] turns the three local sources into the rows the widget
//! shows:
//!
//! - spool events written by `cc-hook` ([`SessionTracker::apply_event`]),
//! - full snapshots of `claude agents --json` ([`SessionTracker::apply_registry`]),
//! - transcript marks such as titles and colours ([`SessionTracker::apply_marks`]),
//!
//! plus [`SessionTracker::tick`] for time-based transitions. It is pure and
//! deterministic: it never reads the clock or touches the disk, and every
//! method that depends on time takes `now` (Unix seconds) from the caller.
//!
//! # Rules
//!
//! Hook events are precise, the registry is coarse but always available, so:
//!
//! - Hooks drive the fine-grained states (`needsYou`, `failed`, `done`, the
//!   current tool, subagents).
//! - For a session that has hooks, the registry only moves `working` and
//!   `idle` into each other, and only when its status changed after the last
//!   hook event (whichever source observed more recently wins). It never
//!   clears `needsYou`, unless it has reported `idle` continuously for
//!   [`REGISTRY_IDLE_CLEARS_NEEDS_YOU_SECS`] since the session started
//!   needing the user.
//! - For a session without hooks, the registry status is the state.
//! - `SubagentStart` carries no task, so each main-agent `Agent` (or `Task`)
//!   call's description waits in a queue with the subagent type it asked
//!   for (see [`PENDING_TASK_SECS`] and [`MAX_PENDING_TASKS`]). The
//!   session's next `SubagentStart` takes the oldest call of its
//!   `agent_type`, else the oldest call without a type, else the oldest
//!   call. A call's `PostToolUseFailure` (it failed or was denied, so no
//!   subagent starts) takes it out, and a new prompt empties the queue.
//!
//! Time constants are public so the UI and tests can refer to them.

use std::collections::{BTreeMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::events::{SpoolEvent, SPOOL_SCHEMA_VERSION};
use crate::session::{
    remote_text, RegistryEntry, RegistryStatus, RemoteAgent, RemotePayload, SessionState,
    TranscriptMarks, MAX_REMOTE_AGENTS, MAX_REMOTE_DIR, MAX_REMOTE_MODEL, MAX_REMOTE_TEXT,
    MAX_REMOTE_TITLE, MAX_REMOTE_TOOL,
};

/// A `done` session becomes `idle` after this many seconds.
pub const DONE_TO_IDLE_SECS: f64 = 20.0;

/// A `working` session with no hook event for longer than this is marked stalled.
pub const STALL_SECS: f64 = 600.0;

/// Finished subagents are dropped once they finished longer ago than this.
pub const SUBAGENT_RETENTION_SECS: f64 = 120.0;

/// A running subagent with no event for longer than this is taken as
/// finished: its `SubagentStop` was missed (a crash, a killed session), and
/// it would otherwise keep the session working for good. Any later event
/// from it brings it back.
pub const SUBAGENT_SILENT_SECS: f64 = STALL_SECS;

/// The main agent's `Agent` (or `Task`) calls whose subagent has not
/// started yet are kept this long; the next `SubagentStart` of the session
/// takes the description of the oldest one of its type as its task.
pub const PENDING_TASK_SECS: f64 = 120.0;

/// Most `Agent` calls waiting for their `SubagentStart` per session; the
/// oldest goes first when another arrives.
pub const MAX_PENDING_TASKS: usize = 16;

/// Main-agent tools whose call starts a subagent.
const AGENT_TOOLS: &[&str] = &["Agent", "Task"];

/// How long the registry must report `idle` continuously, while a hooked
/// session needs the user, before the session is moved to `idle`.
pub const REGISTRY_IDLE_CLEARS_NEEDS_YOU_SECS: f64 = 60.0;

/// A session missing from the registry snapshot is removed after this grace
/// period, unless a hook event for it arrived within the same window.
pub const REGISTRY_ABSENT_GRACE_SECS: f64 = 30.0;

/// A session never seen in the registry is removed after this long without events.
pub const UNREGISTERED_TIMEOUT_SECS: f64 = 600.0;

/// Notification types that mean the session is waiting for the user.
const ATTENTION_NOTIFICATIONS: &[&str] = &["permission_prompt", "idle_prompt"];

/// Whether a subagent is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubagentState {
    /// Started and not yet stopped.
    Working,
    /// `SubagentStop` arrived.
    Done,
}

/// One subagent of a session, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentView {
    /// `agent_id` from the hook payload.
    pub id: String,
    /// `agent_type`, e.g. `Explore`, when the hook reported it.
    pub agent_type: Option<String>,
    /// Its task: the `description` of the main agent's `Agent` call that
    /// started it (the hook's tool summary of that call), when it was seen.
    pub description: Option<String>,
    /// Running or finished.
    pub state: SubagentState,
    /// The tool the subagent ran last (name only); cleared when it stops.
    pub current_tool: Option<String>,
    /// That tool's most identifying argument, as for the main agent's.
    pub tool_summary: Option<String>,
    /// Unix seconds of `SubagentStart` (or of the first event seen for it).
    pub started_at: f64,
    /// Unix seconds of `SubagentStop`.
    pub finished_at: Option<f64>,
}

/// One session row, ready for the Tauri frontend (serialized in camelCase;
/// missing values are `null`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    /// The Claude Code session id.
    pub id: String,
    /// Custom title, else AI title, else registry name, else cwd basename, else short id.
    pub display_name: String,
    /// Current state.
    pub state: SessionState,
    /// Unix seconds of the last state change.
    pub state_since: f64,
    /// Working directory.
    pub cwd: Option<String>,
    /// Process id of the Claude Code process.
    pub pid: Option<u32>,
    /// Registry entrypoint (`claude-vscode`, `cli`, ...), used to pick a focus strategy.
    pub entrypoint: Option<String>,
    /// Colour set with `/color`.
    pub color: Option<String>,
    /// Tool the main agent is running.
    pub current_tool: Option<String>,
    /// The tool's most identifying argument.
    pub tool_summary: Option<String>,
    /// Unix seconds when the current tool started.
    pub tool_started_at: Option<f64>,
    /// Unix seconds of the last `UserPromptSubmit`.
    pub turn_started_at: Option<f64>,
    /// Working, but no hook event for longer than [`STALL_SECS`].
    pub stalled: bool,
    /// Running subagents and recently finished ones, in start order.
    pub subagents: Vec<SubagentView>,
    /// Last assistant message, from `Stop`.
    pub last_message: Option<String>,
    /// What the session asks the user, while `needsYou`.
    pub question: Option<String>,
    /// Error from `StopFailure` or from the last failed tool of this turn.
    pub error_message: Option<String>,
    /// Model id, from hooks or [`SessionTracker::set_context`].
    pub model: Option<String>,
    /// Context window used, percent: exact from the status line, otherwise
    /// estimated from the transcript's tokens and the model's window (see
    /// [`Self::context_estimated`]).
    pub context_percent: Option<u8>,
    /// True when [`Self::context_percent`] is an estimate, not the status
    /// line's figure. VS Code panel sessions never run a status line.
    pub context_estimated: bool,
    /// Tokens in the context, from the transcript. Known even when the
    /// window size, and so [`Self::context_percent`], is not.
    pub context_tokens: Option<u64>,
    /// True once any spool event arrived for this session.
    pub has_hooks: bool,
    /// Unix seconds of the last hook event or registry status change.
    pub last_activity: f64,
}

/// A state change, reported once through [`SessionTracker::take_transitions`]
/// so the app can play a sound or show a toast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transition {
    /// The session that changed.
    pub session_id: String,
    /// The previous state; `None` when the session was just created.
    pub from: Option<SessionState>,
    /// The new state.
    pub to: SessionState,
    /// Unix seconds of the change.
    pub at: f64,
}

/// The remote session id: the first 8 hex characters of the SHA-256 of the
/// session id. Stable, and does not reveal the id itself.
pub fn remote_sid(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone)]
struct Subagent {
    id: String,
    agent_type: Option<String>,
    description: Option<String>,
    state: SubagentState,
    current_tool: Option<String>,
    tool_summary: Option<String>,
    started_at: f64,
    finished_at: Option<f64>,
    /// Unix seconds of its last event, for [`SUBAGENT_SILENT_SECS`].
    last_event_at: f64,
}

impl Subagent {
    fn new(id: &str, agent_type: Option<String>, at: f64) -> Self {
        Self {
            id: id.to_string(),
            agent_type,
            description: None,
            state: SubagentState::Working,
            current_tool: None,
            tool_summary: None,
            started_at: at,
            finished_at: None,
            last_event_at: at,
        }
    }

    /// Any event but `SubagentStop` means it still runs, even after
    /// [`SUBAGENT_SILENT_SECS`] took it as finished.
    fn revive(&mut self, at: f64) {
        self.last_event_at = at;
        if self.state == SubagentState::Done && self.finished_at.is_some_and(|f| f < at) {
            self.state = SubagentState::Working;
            self.finished_at = None;
        }
    }

    fn clear_tool(&mut self) {
        self.current_tool = None;
        self.tool_summary = None;
    }

    fn view(&self) -> SubagentView {
        SubagentView {
            id: self.id.clone(),
            agent_type: self.agent_type.clone(),
            description: self.description.clone(),
            state: self.state,
            current_tool: self.current_tool.clone(),
            tool_summary: self.tool_summary.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
        }
    }

    /// Its line in a [`RemotePayload`], each text cut.
    fn remote(&self) -> RemoteAgent {
        RemoteAgent::new(
            self.agent_type.as_deref(),
            self.description.as_deref(),
            self.current_tool.as_deref(),
            self.current_tool.as_ref().and(self.tool_summary.as_deref()),
            self.state == SubagentState::Working,
        )
    }
}

/// An `Agent` call of the main agent whose subagent has not started yet.
#[derive(Debug, Clone)]
struct PendingTask {
    description: String,
    /// The `subagent_type` the call asked for, matched against the
    /// `agent_type` of `SubagentStart`.
    subagent_type: Option<String>,
    /// The call's `tool_use_id`, so its `PostToolUseFailure` can drop it.
    tool_use_id: Option<String>,
    /// Unix seconds of the call's `PreToolUse`.
    at: f64,
}

#[derive(Debug, Clone)]
struct Session {
    id: String,
    state: SessionState,
    state_since: f64,

    cwd: Option<String>,
    pid: Option<u32>,
    entrypoint: Option<String>,
    registry_name: Option<String>,
    custom_title: Option<String>,
    title: Option<String>,
    color: Option<String>,

    current_tool: Option<String>,
    tool_summary: Option<String>,
    tool_started_at: Option<f64>,
    turn_started_at: Option<f64>,
    stalled: bool,
    subagents: Vec<Subagent>,
    /// `Agent` calls waiting for their `SubagentStart`, oldest first (see
    /// [`PENDING_TASK_SECS`] and [`MAX_PENDING_TASKS`]).
    pending_tasks: VecDeque<PendingTask>,

    last_message: Option<String>,
    question: Option<String>,
    /// The subagent that asked the question; `None` for the main agent.
    question_agent: Option<String>,
    error_message: Option<String>,
    model: Option<String>,
    context_percent: Option<u8>,
    context_tokens: Option<u64>,
    /// The status line reported a percentage: never overwrite it with an estimate.
    context_exact: bool,

    has_hooks: bool,
    last_activity: f64,
    last_event_at: Option<f64>,

    seen_in_registry: bool,
    /// The status in the latest snapshot; `None` when absent from it.
    registry_status: Option<RegistryStatus>,
    /// When `registry_status` was first observed with its current value.
    registry_status_since: f64,
    /// When the registry started reporting `idle` without interruption.
    registry_idle_since: Option<f64>,
    /// When the session first went missing from the registry snapshot.
    registry_absent_since: Option<f64>,
}

impl Session {
    fn new(id: &str, state: SessionState, at: f64) -> Self {
        Self {
            id: id.to_string(),
            state,
            state_since: at,
            cwd: None,
            pid: None,
            entrypoint: None,
            registry_name: None,
            custom_title: None,
            title: None,
            color: None,
            current_tool: None,
            tool_summary: None,
            tool_started_at: None,
            turn_started_at: None,
            stalled: false,
            subagents: Vec::new(),
            pending_tasks: VecDeque::new(),
            last_message: None,
            question: None,
            question_agent: None,
            error_message: None,
            model: None,
            context_percent: None,
            context_tokens: None,
            context_exact: false,
            has_hooks: false,
            last_activity: at,
            last_event_at: None,
            seen_in_registry: false,
            registry_status: None,
            registry_status_since: at,
            registry_idle_since: None,
            registry_absent_since: None,
        }
    }

    /// Moves to `to`, recording the edge. Leaving `needsYou` clears the question.
    fn set_state(&mut self, to: SessionState, at: f64, out: &mut Vec<Transition>) {
        if self.state == to {
            return;
        }
        if self.state == SessionState::NeedsYou {
            self.question = None;
            self.question_agent = None;
        }
        if to != SessionState::Working {
            self.stalled = false;
        }
        out.push(Transition {
            session_id: self.id.clone(),
            from: Some(self.state),
            to,
            at,
        });
        self.state = to;
        self.state_since = at;
    }

    fn clear_tool(&mut self) {
        self.current_tool = None;
        self.tool_summary = None;
        self.tool_started_at = None;
    }

    /// The subagent with this id, created as working if it is not known yet.
    fn subagent_mut(&mut self, id: &str, agent_type: Option<&String>, at: f64) -> &mut Subagent {
        let index = match self.subagents.iter().position(|a| a.id == id) {
            Some(i) => i,
            None => {
                self.subagents
                    .push(Subagent::new(id, agent_type.cloned(), at));
                self.subagents.len() - 1
            }
        };
        let agent = &mut self.subagents[index];
        if agent.agent_type.is_none() {
            agent.agent_type = agent_type.cloned();
        }
        agent
    }

    /// Remembers a main-agent `Agent` call made at `at`.
    fn push_task(&mut self, e: &SpoolEvent, description: &str, at: f64) {
        self.drop_stale_tasks(at);
        if self.pending_tasks.len() == MAX_PENDING_TASKS {
            self.pending_tasks.pop_front();
        }
        self.pending_tasks.push_back(PendingTask {
            description: description.to_string(),
            subagent_type: e.subagent_type.clone(),
            tool_use_id: e.tool_use_id.clone(),
            at,
        });
    }

    /// The description of the pending `Agent` call that started a subagent
    /// of `agent_type` at `at`: the oldest call of that type, else the
    /// oldest without a type, else the oldest. Parallel calls can start out
    /// of order, so the type keeps one call's task off another's subagent.
    fn take_task(&mut self, agent_type: Option<&str>, at: f64) -> Option<String> {
        self.drop_stale_tasks(at);
        let tasks = &self.pending_tasks;
        let index = agent_type
            .and_then(|kind| {
                tasks
                    .iter()
                    .position(|t| t.subagent_type.as_deref() == Some(kind))
            })
            .or_else(|| tasks.iter().position(|t| t.subagent_type.is_none()))
            .unwrap_or(0);
        self.pending_tasks.remove(index).map(|t| t.description)
    }

    /// Forgets the pending `Agent` call `tool_use_id`: it failed or was
    /// denied, so no subagent starts for it.
    fn drop_task(&mut self, tool_use_id: &str) {
        self.pending_tasks
            .retain(|t| t.tool_use_id.as_deref() != Some(tool_use_id));
    }

    fn drop_stale_tasks(&mut self, now: f64) {
        self.pending_tasks
            .retain(|t| now - t.at <= PENDING_TASK_SECS);
    }

    fn display_name(&self) -> String {
        let non_empty = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        };
        non_empty(&self.custom_title)
            .or_else(|| non_empty(&self.title))
            .or_else(|| non_empty(&self.registry_name))
            .or_else(|| self.cwd.as_deref().and_then(basename))
            .unwrap_or_else(|| self.id.chars().take(8).collect())
    }

    /// The state the panel and the publishers show: `working` while a
    /// subagent runs after the main turn ended (background agents), else
    /// the session's own state.
    fn shown_state(&self) -> SessionState {
        let agents_running = self
            .subagents
            .iter()
            .any(|a| a.state == SubagentState::Working);
        match self.state {
            SessionState::Done | SessionState::Idle if agents_running => SessionState::Working,
            state => state,
        }
    }

    /// Subagents as `[running, total]`, `None` without any.
    fn agent_counts(&self) -> Option<[u16; 2]> {
        let cap = |n: usize| u16::try_from(n).unwrap_or(u16::MAX).min(MAX_REMOTE_AGENTS);
        let running = self
            .subagents
            .iter()
            .filter(|a| a.state == SubagentState::Working)
            .count();
        (!self.subagents.is_empty()).then(|| [cap(running), cap(self.subagents.len())])
    }

    /// The subagent lines in the order a payload carries them: running ones
    /// first, each in start order (as many as
    /// [`RemotePayload::fit_agents`] keeps travel).
    fn remote_agents(&self) -> impl Iterator<Item = RemoteAgent> + '_ {
        let mut ordered: Vec<&Subagent> = self.subagents.iter().collect();
        ordered.sort_by(|a, b| {
            (a.state != SubagentState::Working)
                .cmp(&(b.state != SubagentState::Working))
                .then(a.started_at.total_cmp(&b.started_at))
        });
        ordered.into_iter().map(Subagent::remote)
    }

    fn view(&self) -> SessionView {
        SessionView {
            id: self.id.clone(),
            display_name: self.display_name(),
            state: self.shown_state(),
            state_since: self.state_since,
            cwd: self.cwd.clone(),
            pid: self.pid,
            entrypoint: self.entrypoint.clone(),
            color: self.color.clone(),
            current_tool: self.current_tool.clone(),
            tool_summary: self.tool_summary.clone(),
            tool_started_at: self.tool_started_at,
            turn_started_at: self.turn_started_at,
            stalled: self.stalled,
            subagents: self.subagents.iter().map(Subagent::view).collect(),
            last_message: self.last_message.clone(),
            question: self.question.clone(),
            error_message: self.error_message.clone(),
            model: self.model.clone(),
            context_percent: self.context_percent,
            context_estimated: self.context_percent.is_some() && !self.context_exact,
            context_tokens: self.context_tokens,
            has_hooks: self.has_hooks,
            last_activity: self.last_activity,
        }
    }

    /// Whether a hook event arrived within `window` seconds before `now`.
    fn event_within(&self, window: f64, now: f64) -> bool {
        self.last_event_at.is_some_and(|t| now - t <= window)
    }
}

/// Last path component of a Windows or POSIX path, ignoring trailing separators.
fn basename(path: &str) -> Option<String> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|b| !b.is_empty())
        .map(str::to_string)
}

/// The state a registry status implies, if it implies one.
fn registry_state(status: &RegistryStatus) -> Option<SessionState> {
    match status {
        RegistryStatus::Busy => Some(SessionState::Working),
        RegistryStatus::Waiting => Some(SessionState::NeedsYou),
        RegistryStatus::Idle => Some(SessionState::Idle),
        RegistryStatus::Other(_) => None,
    }
}

/// Tracks every local session. See the module docs for the rules.
#[derive(Debug, Clone, Default)]
pub struct SessionTracker {
    sessions: BTreeMap<String, Session>,
    transitions: Vec<Transition>,
    /// Sessions removed by `SessionEnd`, with the time it fired. The registry
    /// can still list such a session for a pass or two (the process is still
    /// exiting, or its file still names the id it had before `/clear`); the
    /// tombstone keeps that from re-creating it as a hookless row. It goes
    /// when a snapshot no longer lists the id, when a newer hook event for it
    /// arrives (a resume), or after [`UNREGISTERED_TIMEOUT_SECS`].
    ended: BTreeMap<String, f64>,
    /// Sessions taken off the list by [`SessionTracker::clear`], with the
    /// time it ran. One that the registry lists again (or that sends a hook
    /// event) comes back from here with its state, question, titles and
    /// hook flag, so a live session loses nothing that matters and does not
    /// toast twice; one that nothing reports again is forgotten at the next
    /// registry snapshot.
    cleared: BTreeMap<String, (Session, f64)>,
}

impl SessionTracker {
    /// An empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of tracked sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// True when no session is tracked.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// The view of one session, if it is tracked.
    pub fn session(&self, session_id: &str) -> Option<SessionView> {
        self.sessions.get(session_id).map(Session::view)
    }

    /// Takes every session off the list (the widget's "clear" button).
    /// Returns how many were listed.
    ///
    /// Nothing is lost for a session that is still alive: the next registry
    /// snapshot that lists it (or its next hook event) puts it back as it
    /// was, with its state, question, titles, context and hook flag, and
    /// without a creation transition, so a session that needs you does not
    /// toast again. Only the current tool and the subagents are dropped;
    /// they come back with the session's next hook event. A session the next
    /// snapshot does not list (it ended, or it only ever existed through
    /// hook events) is forgotten; an event-only session that is still
    /// running reappears with its next hook event. The exception is a
    /// session that needs the user: it is back on the next pass unless a
    /// snapshot that could list it does not, or it ended, because no hook
    /// fires until the user answers.
    pub fn clear(&mut self, now: f64) -> usize {
        let n = self.sessions.len();
        for (id, mut s) in std::mem::take(&mut self.sessions) {
            s.clear_tool();
            s.subagents.clear();
            s.pending_tasks.clear();
            self.cleared.insert(id, (s, now));
        }
        n
    }

    /// True while a session cleared by [`Self::clear`] can still come back.
    pub fn is_cleared(&self, session_id: &str) -> bool {
        self.cleared.contains_key(session_id)
    }

    /// Puts a cleared session back on the list, if there is one by that id.
    fn restore(&mut self, session_id: &str) {
        if let Some((s, _)) = self.cleared.remove(session_id) {
            self.sessions.insert(session_id.to_string(), s);
        }
    }

    /// Applies one spool event. Events with an unknown schema version are
    /// ignored. An event for an unknown session creates it (idle, then the
    /// event applies), since hooks can fire before `SessionStart` is seen,
    /// e.g. when the widget starts in the middle of a session.
    ///
    /// The event's own `ts` is the time of the change, capped at `now`.
    pub fn apply_event(&mut self, e: &SpoolEvent, now: f64) {
        if e.v != SPOOL_SCHEMA_VERSION || e.session_id.is_empty() {
            return;
        }
        let ts = if e.ts.is_finite() { e.ts.min(now) } else { now };

        if e.event == "SessionEnd" {
            self.sessions.remove(&e.session_id);
            self.cleared.remove(&e.session_id);
            let at = self.ended.entry(e.session_id.clone()).or_insert(ts);
            *at = at.max(ts);
            return;
        }
        if let Some(&ended_at) = self.ended.get(&e.session_id) {
            if ts <= ended_at {
                // A straggler written before the session ended.
                return;
            }
            self.ended.remove(&e.session_id);
        }
        if !self.sessions.contains_key(&e.session_id) {
            self.restore(&e.session_id);
        }

        let out = &mut self.transitions;
        let s = self
            .sessions
            .entry(e.session_id.clone())
            .or_insert_with(|| {
                out.push(Transition {
                    session_id: e.session_id.clone(),
                    from: None,
                    to: SessionState::Idle,
                    at: ts,
                });
                Session::new(&e.session_id, SessionState::Idle, ts)
            });

        s.has_hooks = true;
        s.stalled = false;
        s.last_event_at = Some(s.last_event_at.map_or(ts, |t| t.max(ts)));
        s.last_activity = s.last_activity.max(ts);
        if s.cwd.is_none() {
            s.cwd.clone_from(&e.cwd);
        }
        if s.pid.is_none() {
            s.pid = e.pid;
        }
        if e.model.is_some() {
            s.model.clone_from(&e.model);
        }

        if let Some(agent_id) = e.agent_id.as_deref() {
            match e.event.as_str() {
                "SubagentStart" => {
                    // Its task is the oldest `Agent` call of its type still
                    // waiting for its subagent; one that already has a task
                    // (a repeated start) keeps it and leaves the queue alone.
                    let known = s
                        .subagents
                        .iter()
                        .any(|a| a.id == agent_id && a.description.is_some());
                    let task = if known {
                        None
                    } else {
                        s.take_task(e.agent_type.as_deref(), ts)
                    };
                    let agent = s.subagent_mut(agent_id, e.agent_type.as_ref(), ts);
                    agent.state = SubagentState::Working;
                    agent.started_at = ts;
                    agent.finished_at = None;
                    agent.clear_tool();
                    agent.last_event_at = ts;
                    if task.is_some() {
                        agent.description = task;
                    }
                    return;
                }
                "SubagentStop" => {
                    let agent = s.subagent_mut(agent_id, e.agent_type.as_ref(), ts);
                    agent.state = SubagentState::Done;
                    agent.finished_at = Some(ts);
                    agent.clear_tool();
                    agent.last_event_at = ts;
                    return;
                }
                "PreToolUse" => {
                    let agent = s.subagent_mut(agent_id, e.agent_type.as_ref(), ts);
                    agent.revive(ts);
                    agent.current_tool.clone_from(&e.tool_name);
                    agent.tool_summary.clone_from(&e.tool_summary);
                    // The subagent that asked moved on, so the user answered.
                    if s.state == SessionState::NeedsYou
                        && s.question_agent.as_deref() == Some(agent_id)
                    {
                        s.set_state(SessionState::Working, ts, out);
                    }
                    return;
                }
                // A subagent asking for permission needs the user as much as
                // the main agent does: handled below with the main row.
                "Notification" => {}
                // Anything else from inside a subagent only counts as
                // activity (its own too, see SUBAGENT_SILENT_SECS).
                _ => {
                    if let Some(agent) = s.subagents.iter_mut().find(|a| a.id == agent_id) {
                        agent.revive(ts);
                    }
                    return;
                }
            }
        }

        match e.event.as_str() {
            "UserPromptSubmit" => {
                s.set_state(SessionState::Working, ts, out);
                s.turn_started_at = Some(ts);
                s.clear_tool();
                s.error_message = None;
                s.subagents.retain(|a| a.state == SubagentState::Working);
                // An `Agent` call starts its subagent within its turn: one
                // still waiting was denied or interrupted.
                s.pending_tasks.clear();
            }
            "PreToolUse" => {
                s.set_state(SessionState::Working, ts, out);
                s.current_tool.clone_from(&e.tool_name);
                s.tool_summary.clone_from(&e.tool_summary);
                s.tool_started_at = Some(ts);
                // The call's description becomes the task of the subagent
                // it starts (SubagentStart carries no description).
                if let (Some(tool), Some(summary)) =
                    (e.tool_name.as_deref(), e.tool_summary.as_deref())
                {
                    if AGENT_TOOLS.contains(&tool) {
                        s.push_task(e, summary, ts);
                    }
                }
            }
            "Notification" => {
                let kind = e.notification_type.as_deref().unwrap_or_default();
                if ATTENTION_NOTIFICATIONS.contains(&kind) {
                    s.set_state(SessionState::NeedsYou, ts, out);
                    s.question.clone_from(&e.message);
                    s.question_agent.clone_from(&e.agent_id);
                }
            }
            "Stop" => {
                s.set_state(SessionState::Done, ts, out);
                s.last_message.clone_from(&e.last_message);
                s.clear_tool();
            }
            "StopFailure" => {
                s.set_state(SessionState::Failed, ts, out);
                s.error_message.clone_from(&e.error_message);
                s.clear_tool();
            }
            "PostToolUseFailure" => {
                // A failed or denied `Agent` call starts no subagent.
                if let Some(id) = e.tool_use_id.as_deref() {
                    s.drop_task(id);
                }
                if e.error_message.is_some() {
                    s.error_message.clone_from(&e.error_message);
                }
            }
            // SessionStart (creation is handled above) and unknown events.
            _ => {}
        }
    }

    /// Applies a full snapshot of the live sessions from `claude agents --json`.
    ///
    /// Unknown sessions are created (busy as working, waiting as needs-you,
    /// idle as idle); pid, cwd, name and entrypoint are filled from the entry;
    /// sessions missing from the snapshot start their grace period (see
    /// [`REGISTRY_ABSENT_GRACE_SECS`]).
    pub fn apply_registry(&mut self, entries: &[RegistryEntry], now: f64) {
        // Cleared sessions the snapshot still lists come back as they were;
        // the rest are forgotten below.
        for entry in entries {
            if !self.ended.contains_key(&entry.session_id)
                && !self.sessions.contains_key(&entry.session_id)
            {
                self.restore(&entry.session_id);
            }
        }
        // A session that needs the user and only ever reported through hooks
        // is not the registry's to judge: no hook fires until the user
        // answers, so forgetting it would hide the question indefinitely.
        let unjudged: Vec<String> = self
            .cleared
            .iter()
            .filter(|(_, (s, _))| {
                s.state == SessionState::NeedsYou
                    && !s.seen_in_registry
                    && s.event_within(UNREGISTERED_TIMEOUT_SECS, now)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in unjudged {
            self.restore(&id);
        }
        self.cleared.clear();

        let out = &mut self.transitions;
        let mut present = HashSet::new();

        for entry in entries {
            if entry.session_id.is_empty() || !present.insert(entry.session_id.as_str()) {
                continue;
            }
            if self.ended.contains_key(&entry.session_id) {
                continue;
            }
            let s = self
                .sessions
                .entry(entry.session_id.clone())
                .or_insert_with(|| {
                    let state = registry_state(&entry.status).unwrap_or(SessionState::Idle);
                    out.push(Transition {
                        session_id: entry.session_id.clone(),
                        from: None,
                        to: state,
                        at: now,
                    });
                    let mut s = Session::new(&entry.session_id, state, now);
                    if state == SessionState::NeedsYou {
                        s.question.clone_from(&entry.waiting_for);
                    }
                    s
                });

            s.seen_in_registry = true;
            s.registry_absent_since = None;
            if entry.pid.is_some() {
                s.pid = entry.pid;
            }
            if entry.cwd.is_some() {
                s.cwd.clone_from(&entry.cwd);
            }
            if entry.name.is_some() {
                s.registry_name.clone_from(&entry.name);
            }
            if entry.entrypoint.is_some() {
                s.entrypoint.clone_from(&entry.entrypoint);
            }

            let mut previous_status = None;
            if s.registry_status.as_ref() != Some(&entry.status) {
                previous_status = s.registry_status.replace(entry.status.clone());
                s.registry_status_since = now;
                s.last_activity = s.last_activity.max(now);
            }

            // The one case where the registry may clear a hooked needsYou:
            // Claude Code moved the session from waiting to busy after the
            // question was asked, which is what approving a permission prompt
            // looks like. No hook fires between the approval and the tool
            // finishing, so without this the row would keep asking for the
            // whole of a long approved command.
            if s.has_hooks
                && s.state == SessionState::NeedsYou
                && previous_status == Some(RegistryStatus::Waiting)
                && entry.status == RegistryStatus::Busy
                && now >= s.state_since
            {
                s.question = None;
                s.set_state(SessionState::Working, now, out);
                continue;
            }
            if entry.status == RegistryStatus::Idle {
                s.registry_idle_since.get_or_insert(now);
            } else {
                s.registry_idle_since = None;
            }

            let Some(target) = registry_state(&entry.status) else {
                continue;
            };
            if !s.has_hooks {
                // Without hooks the registry is the only source of truth.
                s.set_state(target, now, out);
                if target == SessionState::NeedsYou {
                    s.question.clone_from(&entry.waiting_for);
                }
                continue;
            }
            // With hooks: only working <-> idle, and only when the registry
            // changed after the last hook event.
            let registry_newer = s.last_event_at.is_none_or(|t| s.registry_status_since > t);
            let allowed = matches!(
                (s.state, target),
                (SessionState::Working, SessionState::Idle)
                    | (SessionState::Idle, SessionState::Working)
            );
            if registry_newer && allowed {
                s.set_state(target, now, out);
                if target == SessionState::Idle {
                    s.clear_tool();
                }
            }
        }

        for s in self.sessions.values_mut() {
            if present.contains(s.id.as_str()) {
                continue;
            }
            s.registry_status = None;
            s.registry_idle_since = None;
            if s.seen_in_registry {
                s.registry_absent_since.get_or_insert(now);
            }
        }

        self.ended.retain(|id, _| present.contains(id.as_str()));

        self.release_needs_you(now);
        self.expire(now);
    }

    /// Applies the transcript marks of a session (custom title, AI title,
    /// colour), replacing the previous ones. Unknown sessions are ignored.
    pub fn apply_marks(&mut self, session_id: &str, marks: &TranscriptMarks) {
        if let Some(s) = self.sessions.get_mut(session_id) {
            s.custom_title.clone_from(&marks.custom_title);
            s.title.clone_from(&marks.title);
            s.color.clone_from(&marks.color);
            // The transcript names the model of the latest reply, so it also
            // follows a /model switch; hooks only report it at SessionStart.
            if marks.model.is_some() {
                s.model.clone_from(&marks.model);
            }
            if marks.context_tokens.is_some() {
                s.context_tokens = marks.context_tokens;
            }
            if !s.context_exact {
                if let (Some(tokens), Some(model)) = (s.context_tokens, s.model.as_deref()) {
                    if let Some(p) = crate::models::estimated_percent(tokens, model) {
                        s.context_percent = Some(p);
                    }
                }
            }
        }
    }

    /// Optional enrichment: context window use (clamped to 100) and model.
    /// `None` leaves the current value. Unknown sessions are ignored.
    pub fn set_context(&mut self, session_id: &str, percent: Option<u8>, model: Option<&str>) {
        if let Some(s) = self.sessions.get_mut(session_id) {
            if let Some(p) = percent {
                s.context_percent = Some(p.min(100));
                s.context_exact = true;
            }
            if let Some(m) = model {
                s.model = Some(m.to_string());
            }
        }
    }

    /// Time-based transitions: `done` to `idle`, stall detection, dropping
    /// finished subagents, the registry's delayed release of `needsYou`, and
    /// removal of sessions that are gone.
    pub fn tick(&mut self, now: f64) {
        let out = &mut self.transitions;
        for s in self.sessions.values_mut() {
            if s.state == SessionState::Done && now - s.state_since >= DONE_TO_IDLE_SECS {
                s.set_state(SessionState::Idle, now, out);
            }
            if s.state == SessionState::Working
                && s.has_hooks
                && s.last_event_at.is_some_and(|t| now - t > STALL_SECS)
            {
                s.stalled = true;
            }
            for a in &mut s.subagents {
                if a.state == SubagentState::Working && now - a.last_event_at > SUBAGENT_SILENT_SECS
                {
                    a.state = SubagentState::Done;
                    a.finished_at = Some(now);
                    a.clear_tool();
                }
            }
            s.drop_stale_tasks(now);
            s.subagents.retain(|a| {
                !(a.state == SubagentState::Done
                    && a.finished_at
                        .is_some_and(|f| now - f > SUBAGENT_RETENTION_SECS))
            });
        }
        self.ended
            .retain(|_, at| now - *at <= UNREGISTERED_TIMEOUT_SECS);
        // Without a registry snapshot (the CLI is failing), a cleared session
        // waits for its next hook event no longer than an event-only one.
        self.cleared
            .retain(|_, (_, at)| now - *at <= UNREGISTERED_TIMEOUT_SECS);
        // A cleared session that needs the user never waits for a snapshot
        // (with the CLI failing there may be none for a while): it is back
        // on the next pass, like a live session the registry lists.
        let waiting: Vec<String> = self
            .cleared
            .iter()
            .filter(|(_, (s, _))| s.state == SessionState::NeedsYou)
            .map(|(id, _)| id.clone())
            .collect();
        for id in waiting {
            self.restore(&id);
        }
        self.release_needs_you(now);
        self.expire(now);
    }

    /// All sessions: by [`SessionState::sort_rank`], then most recent activity
    /// first, then by id so the order is stable.
    pub fn sessions(&self) -> Vec<SessionView> {
        let mut views: Vec<SessionView> = self.sessions.values().map(Session::view).collect();
        views.sort_by(|a, b| {
            a.state
                .sort_rank()
                .cmp(&b.state.sort_rank())
                .then(b.last_activity.total_cmp(&a.last_activity))
                .then_with(|| a.id.cmp(&b.id))
        });
        views
    }

    /// State changes since the last call, oldest first. Each is reported once.
    /// Removed sessions produce no transition.
    pub fn take_transitions(&mut self) -> Vec<Transition> {
        std::mem::take(&mut self.transitions)
    }

    /// The payload a publisher sends for one session: state, tool name,
    /// model and context, plus the title, the folder name, the second line
    /// of the row and a line for the first subagents, each cut by
    /// [`remote_text`] (see [`RemotePayload`]).
    pub fn to_remote_payload(
        &self,
        session_id: &str,
        machine_alias: &str,
        now: f64,
    ) -> Option<RemotePayload> {
        let s = self.sessions.get(session_id)?;
        let title = [&s.custom_title, &s.title, &s.registry_name]
            .into_iter()
            .flatten()
            .find_map(|t| remote_text(t, MAX_REMOTE_TITLE));
        let dir = s
            .cwd
            .as_deref()
            .and_then(basename)
            .and_then(|d| remote_text(&d, MAX_REMOTE_DIR));
        // The one text the row's second line shows in this state.
        let state = s.shown_state();
        let text = match state {
            SessionState::NeedsYou => s.question.as_deref(),
            SessionState::Working => s.current_tool.as_ref().and(s.tool_summary.as_deref()),
            SessionState::Failed => s.error_message.as_deref(),
            SessionState::Done | SessionState::Idle => s.last_message.as_deref(),
            SessionState::Offline => None,
        }
        .and_then(|t| remote_text(t, MAX_REMOTE_TEXT));
        let mut payload = RemotePayload {
            machine: machine_alias.to_string(),
            sid: remote_sid(session_id),
            alias: None,
            title,
            dir,
            text,
            agents: s.agent_counts(),
            ags: None,
            state,
            since: s.state_since.floor() as i64,
            tool: s
                .current_tool
                .as_deref()
                .and_then(|t| remote_text(t, MAX_REMOTE_TOOL)),
            model: s
                .model
                .as_deref()
                .and_then(|m| remote_text(m, MAX_REMOTE_MODEL)),
            ctx: s.context_percent,
            hb: now.floor() as i64,
        };
        payload.fit_agents(s.remote_agents());
        Some(payload)
    }

    /// A hooked session that needs the user goes `idle` once the registry has
    /// reported `idle` continuously for the threshold, counted from the later
    /// of the start of that idle run and the start of `needsYou` (so an idle
    /// run that began before the question cannot clear it at once).
    fn release_needs_you(&mut self, now: f64) {
        let out = &mut self.transitions;
        for s in self.sessions.values_mut() {
            if s.state != SessionState::NeedsYou || !s.has_hooks {
                continue;
            }
            if let Some(idle_since) = s.registry_idle_since {
                if now - idle_since.max(s.state_since) >= REGISTRY_IDLE_CLEARS_NEEDS_YOU_SECS {
                    s.set_state(SessionState::Idle, now, out);
                    s.clear_tool();
                }
            }
        }
    }

    /// Removes sessions that left the registry (after the grace period) and
    /// event-only sessions that went quiet.
    fn expire(&mut self, now: f64) {
        self.sessions.retain(|_, s| {
            if s.seen_in_registry {
                match s.registry_absent_since {
                    Some(absent) => {
                        now - absent < REGISTRY_ABSENT_GRACE_SECS
                            || s.event_within(REGISTRY_ABSENT_GRACE_SECS, now)
                    }
                    None => true,
                }
            } else {
                s.event_within(UNREGISTERED_TIMEOUT_SECS, now)
            }
        });
    }
}

#[cfg(test)]
mod tests;
