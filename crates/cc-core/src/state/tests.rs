//! Tests for the session state machine: one table or test per rule, plus a
//! long realistic sequence.

use super::*;
use crate::session::{RegistryEntry, RegistryStatus, SessionState as S, TranscriptMarks};

const T0: f64 = 1_800_000_000.0;
const SID: &str = "3f9a1c2e-0000-4000-8000-000000000001";

// ---------------------------------------------------------------- helpers

fn ev(event: &str, ts: f64) -> SpoolEvent {
    SpoolEvent::new(event, SID, T0 + ts)
}

fn start(ts: f64) -> SpoolEvent {
    let mut e = ev("SessionStart", ts);
    e.cwd = Some("C:\\Users\\me\\repo-a".into());
    e.source = Some("startup".into());
    e
}

fn prompt(ts: f64) -> SpoolEvent {
    let mut e = ev("UserPromptSubmit", ts);
    e.user_input = Some("fix the tests".into());
    e
}

fn tool(ts: f64, name: &str, summary: &str) -> SpoolEvent {
    let mut e = ev("PreToolUse", ts);
    e.tool_name = Some(name.into());
    e.tool_summary = Some(summary.into());
    e
}

fn notification(ts: f64, kind: &str, message: &str) -> SpoolEvent {
    let mut e = ev("Notification", ts);
    e.notification_type = Some(kind.into());
    e.message = Some(message.into());
    e
}

fn stop(ts: f64, message: &str) -> SpoolEvent {
    let mut e = ev("Stop", ts);
    e.last_message = Some(message.into());
    e
}

fn stop_failure(ts: f64, error: &str) -> SpoolEvent {
    let mut e = ev("StopFailure", ts);
    e.error_message = Some(error.into());
    e
}

fn tool_failure(ts: f64, error: &str) -> SpoolEvent {
    let mut e = ev("PostToolUseFailure", ts);
    e.tool_name = Some("Bash".into());
    e.error_message = Some(error.into());
    e
}

fn in_agent(mut e: SpoolEvent, agent_id: &str) -> SpoolEvent {
    e.agent_id = Some(agent_id.into());
    e.agent_type = Some("Explore".into());
    e
}

fn sub_start(ts: f64, agent_id: &str) -> SpoolEvent {
    in_agent(ev("SubagentStart", ts), agent_id)
}

fn sub_stop(ts: f64, agent_id: &str) -> SpoolEvent {
    in_agent(ev("SubagentStop", ts), agent_id)
}

fn reg(session_id: &str, status: RegistryStatus) -> RegistryEntry {
    RegistryEntry {
        session_id: session_id.into(),
        pid: Some(4242),
        cwd: Some("C:/work/registry-repo".into()),
        name: Some("registry-repo-35".into()),
        kind: Some("interactive".into()),
        status,
        waiting_for: Some("approve Bash".into()),
        entrypoint: Some("claude-vscode".into()),
        started_at_ms: Some(1_800_000_000_000),
        background_id: None,
    }
}

/// Applies each event with `now` equal to its own timestamp.
fn feed(events: &[SpoolEvent]) -> SessionTracker {
    let mut t = SessionTracker::new();
    for e in events {
        t.apply_event(e, e.ts);
    }
    t
}

/// Like [`feed`], then lists the session in a busy registry snapshot taken at
/// the last event, so it does not expire as an event-only session.
fn feed_registered(events: &[SpoolEvent]) -> SessionTracker {
    let mut t = feed(events);
    let last = events.last().map_or(T0, |e| e.ts);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], last);
    t
}

fn view(t: &SessionTracker) -> SessionView {
    t.session(SID).expect("session is tracked")
}

fn state(t: &SessionTracker) -> Option<S> {
    t.session(SID).map(|v| v.state)
}

/// A hooked session that the registry knows, currently needing the user
/// since `T0 + 10`, with the registry reporting busy since `T0 + 1`.
fn hooked_needs_you() -> SessionTracker {
    let mut t = SessionTracker::new();
    t.apply_event(&start(0.0), T0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 1.0);
    t.apply_event(&prompt(2.0), T0 + 2.0);
    t.apply_event(&tool(3.0, "Bash", "npm test"), T0 + 3.0);
    t.apply_event(
        &notification(10.0, "permission_prompt", "Allow Bash?"),
        T0 + 10.0,
    );
    assert_eq!(state(&t), Some(S::NeedsYou));
    t
}

// ---------------------------------------------------------------- hook events

#[test]
fn hook_events_drive_the_main_state() {
    struct Case {
        name: &'static str,
        events: Vec<SpoolEvent>,
        expected: Option<S>,
    }
    let cases = vec![
        Case {
            name: "SessionStart creates an idle session",
            events: vec![start(0.0)],
            expected: Some(S::Idle),
        },
        Case {
            name: "UserPromptSubmit starts working",
            events: vec![start(0.0), prompt(1.0)],
            expected: Some(S::Working),
        },
        Case {
            name: "PreToolUse is working",
            events: vec![start(0.0), tool(1.0, "Read", "main.rs")],
            expected: Some(S::Working),
        },
        Case {
            name: "permission_prompt needs the user",
            events: vec![
                start(0.0),
                prompt(1.0),
                notification(2.0, "permission_prompt", "Allow?"),
            ],
            expected: Some(S::NeedsYou),
        },
        Case {
            name: "idle_prompt needs the user",
            events: vec![
                start(0.0),
                prompt(1.0),
                stop(2.0, "ok"),
                notification(70.0, "idle_prompt", "Waiting"),
            ],
            expected: Some(S::NeedsYou),
        },
        Case {
            name: "other notification types do not change state",
            events: vec![
                start(0.0),
                prompt(1.0),
                notification(2.0, "auth_success", "Logged in"),
            ],
            expected: Some(S::Working),
        },
        Case {
            name: "notification without a type does not change state",
            events: vec![start(0.0), ev("Notification", 1.0)],
            expected: Some(S::Idle),
        },
        Case {
            name: "PreToolUse clears needsYou",
            events: vec![
                start(0.0),
                prompt(1.0),
                notification(2.0, "permission_prompt", "?"),
                tool(3.0, "Bash", "ls"),
            ],
            expected: Some(S::Working),
        },
        Case {
            name: "UserPromptSubmit clears needsYou",
            events: vec![
                start(0.0),
                notification(2.0, "idle_prompt", "?"),
                prompt(3.0),
            ],
            expected: Some(S::Working),
        },
        Case {
            name: "Stop clears needsYou",
            events: vec![
                start(0.0),
                prompt(1.0),
                notification(2.0, "permission_prompt", "?"),
                stop(3.0, "denied"),
            ],
            expected: Some(S::Done),
        },
        Case {
            name: "Stop is done",
            events: vec![start(0.0), prompt(1.0), stop(2.0, "All green")],
            expected: Some(S::Done),
        },
        Case {
            name: "StopFailure is failed",
            events: vec![start(0.0), prompt(1.0), stop_failure(2.0, "rate limited")],
            expected: Some(S::Failed),
        },
        Case {
            name: "PostToolUseFailure keeps working",
            events: vec![
                start(0.0),
                prompt(1.0),
                tool(2.0, "Bash", "npm test"),
                tool_failure(3.0, "exit 1"),
            ],
            expected: Some(S::Working),
        },
        Case {
            name: "SessionEnd removes the session",
            events: vec![start(0.0), prompt(1.0), ev("SessionEnd", 2.0)],
            expected: None,
        },
        Case {
            name: "an event before SessionStart creates the session",
            events: vec![prompt(1.0)],
            expected: Some(S::Working),
        },
        Case {
            name: "unknown events are ignored",
            events: vec![start(0.0), ev("PostToolUse", 1.0)],
            expected: Some(S::Idle),
        },
    ];
    for case in cases {
        let t = feed(&case.events);
        assert_eq!(state(&t), case.expected, "{}", case.name);
    }
}

#[test]
fn user_prompt_records_the_turn_start_and_clears_old_errors() {
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        stop_failure(2.0, "boom"),
        prompt(5.0),
    ]);
    let v = view(&t);
    assert_eq!(v.turn_started_at, Some(T0 + 5.0));
    assert_eq!(v.state_since, T0 + 5.0);
    assert_eq!(v.error_message, None);
}

#[test]
fn pre_tool_use_sets_the_current_tool() {
    let t = feed(&[start(0.0), prompt(1.0), tool(4.0, "Bash", "npm test")]);
    let v = view(&t);
    assert_eq!(v.current_tool.as_deref(), Some("Bash"));
    assert_eq!(v.tool_summary.as_deref(), Some("npm test"));
    assert_eq!(v.tool_started_at, Some(T0 + 4.0));
    assert_eq!(v.turn_started_at, Some(T0 + 1.0));
}

#[test]
fn question_is_shown_while_needs_you_and_cleared_after() {
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        notification(2.0, "permission_prompt", "Allow Bash: rm -rf target?"),
    ]);
    assert_eq!(
        view(&t).question.as_deref(),
        Some("Allow Bash: rm -rf target?")
    );
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        notification(2.0, "permission_prompt", "Allow?"),
        tool(3.0, "Bash", "ls"),
    ]);
    assert_eq!(view(&t).question, None);
}

#[test]
fn stop_records_the_last_message_and_clears_the_tool() {
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Edit", "lib.rs"),
        stop(3.0, "Done, tests pass."),
    ]);
    let v = view(&t);
    assert_eq!(v.last_message.as_deref(), Some("Done, tests pass."));
    assert_eq!(v.current_tool, None);
    assert_eq!(v.tool_started_at, None);
}

#[test]
fn stop_failure_records_the_error() {
    let t = feed(&[start(0.0), prompt(1.0), stop_failure(2.0, "API error 529")]);
    assert_eq!(view(&t).error_message.as_deref(), Some("API error 529"));
}

#[test]
fn a_failed_tool_records_the_error_but_not_a_failed_turn() {
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Bash", "cargo build"),
        tool_failure(3.0, "exit code 101"),
    ]);
    let v = view(&t);
    assert_eq!(v.state, S::Working);
    assert_eq!(v.error_message.as_deref(), Some("exit code 101"));
}

#[test]
fn events_with_another_schema_version_are_ignored() {
    let mut e = start(0.0);
    e.v = SPOOL_SCHEMA_VERSION + 1;
    let t = feed(&[e]);
    assert!(t.is_empty());

    let mut late = stop(2.0, "x");
    late.v = 0;
    let t = feed(&[start(0.0), prompt(1.0), late]);
    assert_eq!(state(&t), Some(S::Working));
}

#[test]
fn events_fill_cwd_pid_and_model_and_mark_has_hooks() {
    let mut e = start(0.0);
    e.pid = Some(77);
    e.model = Some("claude-opus-4-1".into());
    let t = feed(&[e]);
    let v = view(&t);
    assert!(v.has_hooks);
    assert_eq!(v.pid, Some(77));
    assert_eq!(v.cwd.as_deref(), Some("C:\\Users\\me\\repo-a"));
    assert_eq!(v.model.as_deref(), Some("claude-opus-4-1"));
    assert_eq!(v.display_name, "repo-a");
}

#[test]
fn event_time_is_capped_at_now() {
    let mut t = SessionTracker::new();
    t.apply_event(&prompt(50.0), T0 + 10.0);
    assert_eq!(view(&t).state_since, T0 + 10.0);
}

// ---------------------------------------------------------------- subagents

#[test]
fn subagent_lifecycle() {
    let t = feed(&[start(0.0), prompt(1.0), sub_start(2.0, "a1")]);
    let v = view(&t);
    assert_eq!(v.subagents.len(), 1);
    assert_eq!(v.subagents[0].state, SubagentState::Working);
    assert_eq!(v.subagents[0].agent_type.as_deref(), Some("Explore"));
    assert_eq!(v.subagents[0].started_at, T0 + 2.0);

    let t = feed(&[
        start(0.0),
        prompt(1.0),
        sub_start(2.0, "a1"),
        sub_stop(9.0, "a1"),
    ]);
    let a = &view(&t).subagents[0];
    assert_eq!(a.state, SubagentState::Done);
    assert_eq!(a.finished_at, Some(T0 + 9.0));
}

#[test]
fn subagent_tool_use_updates_the_subagent_not_the_main_row() {
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Task", "explore the repo"),
        sub_start(3.0, "a1"),
        in_agent(tool(4.0, "Grep", "fn main"), "a1"),
    ]);
    let v = view(&t);
    assert_eq!(v.current_tool.as_deref(), Some("Task"));
    assert_eq!(v.tool_summary.as_deref(), Some("explore the repo"));
    assert_eq!(v.subagents[0].current_tool.as_deref(), Some("Grep"));
    assert_eq!(v.subagents[0].tool_summary.as_deref(), Some("fn main"));
    assert_eq!(
        v.subagents[0].description.as_deref(),
        Some("explore the repo")
    );
}

#[test]
fn subagents_take_the_agent_call_descriptions_in_order() {
    let mut t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Agent", "Find usages"),
        tool(2.1, "Agent", "Plan the fix"),
        tool(2.2, "Read", "lib.rs"),
        sub_start(3.0, "a1"),
        sub_start(3.5, "a2"),
        // No call left: no task.
        sub_start(4.0, "a3"),
    ]);
    let tasks = |t: &SessionTracker| -> Vec<Option<String>> {
        view(t)
            .subagents
            .into_iter()
            .map(|a| a.description)
            .collect()
    };
    assert_eq!(
        tasks(&t),
        [
            Some("Find usages".into()),
            Some("Plan the fix".into()),
            None
        ]
    );

    // A repeated start keeps its task and leaves the next call for a new agent.
    t.apply_event(&tool(5.0, "Task", "Write the tests"), T0 + 5.0);
    t.apply_event(&sub_start(5.5, "a1"), T0 + 5.5);
    t.apply_event(&sub_start(6.0, "a4"), T0 + 6.0);
    let v = view(&t);
    assert_eq!(v.subagents[0].description.as_deref(), Some("Find usages"));
    assert_eq!(
        v.subagents[3].description.as_deref(),
        Some("Write the tests")
    );

    // Its own tool and summary go with it, and go when it stops.
    t.apply_event(
        &in_agent(tool(7.0, "Bash", "Run the tests"), "a4"),
        T0 + 7.0,
    );
    let a = &view(&t).subagents[3];
    assert_eq!(
        (a.current_tool.as_deref(), a.tool_summary.as_deref()),
        (Some("Bash"), Some("Run the tests"))
    );
    t.apply_event(&sub_stop(8.0, "a4"), T0 + 8.0);
    let a = &view(&t).subagents[3];
    assert!(a.current_tool.is_none() && a.tool_summary.is_none());
    assert_eq!(
        a.description.as_deref(),
        Some("Write the tests"),
        "the task stays"
    );
}

/// A main-agent `Agent` call `id` asking for a subagent of `kind`.
fn agent_call(ts: f64, id: &str, kind: &str, description: &str) -> SpoolEvent {
    let mut e = tool(ts, "Agent", description);
    e.tool_use_id = Some(id.into());
    e.subagent_type = Some(kind.into());
    e
}

/// `SubagentStart` of a subagent of `kind`.
fn sub_start_of(ts: f64, agent_id: &str, kind: &str) -> SpoolEvent {
    let mut e = sub_start(ts, agent_id);
    e.agent_type = Some(kind.into());
    e
}

fn tasks(t: &SessionTracker) -> Vec<(String, Option<String>)> {
    view(t)
        .subagents
        .into_iter()
        .map(|a| (a.id, a.description))
        .collect()
}

#[test]
fn parallel_agent_calls_match_their_subagents_by_type() {
    // Two parallel calls whose subagents start in reverse order.
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        agent_call(2.0, "toolu_1", "Explore", "Find usages"),
        agent_call(2.1, "toolu_2", "Plan", "Plan the fix"),
        sub_start_of(3.0, "a-plan", "Plan"),
        sub_start_of(3.1, "a-explore", "Explore"),
    ]);
    assert_eq!(
        tasks(&t),
        [
            ("a-plan".into(), Some("Plan the fix".into())),
            ("a-explore".into(), Some("Find usages".into())),
        ]
    );

    // Two of the same type go in call order; a start of a type no call
    // asked for takes an untyped call first, else the oldest one.
    let mut untyped = tool(2.2, "Task", "Untyped");
    untyped.tool_use_id = Some("toolu_3".into());
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        agent_call(2.0, "toolu_1", "Explore", "First"),
        agent_call(2.1, "toolu_2", "Explore", "Second"),
        untyped,
        agent_call(2.3, "toolu_4", "Plan", "Plan it"),
        sub_start_of(3.0, "b1", "Explore"),
        sub_start_of(3.1, "b2", "general-purpose"),
        sub_start_of(3.2, "b3", "general-purpose"),
        sub_start_of(3.3, "b4", "Plan"),
    ]);
    assert_eq!(
        tasks(&t),
        [
            ("b1".into(), Some("First".into())),
            ("b2".into(), Some("Untyped".into())),
            ("b3".into(), Some("Second".into())),
            ("b4".into(), Some("Plan it".into())),
        ]
    );
}

#[test]
fn a_failed_or_denied_agent_call_leaves_the_queue() {
    let mut denied = tool_failure(2.5, "The user doesn't want to proceed with this tool use.");
    denied.tool_name = Some("Agent".into());
    denied.tool_use_id = Some("toolu_1".into());
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        agent_call(2.0, "toolu_1", "Explore", "Denied"),
        denied,
        agent_call(3.0, "toolu_2", "Explore", "Allowed"),
        sub_start_of(4.0, "a1", "Explore"),
    ]);
    assert_eq!(tasks(&t), [("a1".into(), Some("Allowed".into()))]);
    let v = view(&t);
    assert_eq!(
        v.error_message.as_deref(),
        Some("The user doesn't want to proceed with this tool use.")
    );

    // A failure of another call leaves the queue alone.
    let mut other = tool_failure(2.5, "exit 1");
    other.tool_use_id = Some("toolu_9".into());
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        agent_call(2.0, "toolu_1", "Explore", "Kept"),
        other,
        sub_start_of(4.0, "a1", "Explore"),
    ]);
    assert_eq!(tasks(&t), [("a1".into(), Some("Kept".into()))]);
}

#[test]
fn pending_agent_calls_expire_are_capped_and_end_with_the_turn() {
    // Older than PENDING_TASK_SECS: dropped.
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Agent", "stale"),
        tool(2.0 + PENDING_TASK_SECS + 1.0, "Agent", "fresh"),
        sub_start(2.0 + PENDING_TASK_SECS + 2.0, "a1"),
    ]);
    assert_eq!(view(&t).subagents[0].description.as_deref(), Some("fresh"));
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Agent", "too late"),
        sub_start(3.0 + PENDING_TASK_SECS, "a1"),
    ]);
    assert_eq!(view(&t).subagents[0].description, None);

    // At most MAX_PENDING_TASKS: the oldest go first.
    let mut events = vec![start(0.0), prompt(1.0)];
    for i in 0..MAX_PENDING_TASKS + 2 {
        events.push(tool(2.0 + i as f64 * 0.01, "Agent", &format!("task {i}")));
    }
    events.push(sub_start(3.0, "a1"));
    let t = feed(&events);
    assert_eq!(view(&t).subagents[0].description.as_deref(), Some("task 2"));

    // A call still waiting when the next prompt arrives was denied or
    // interrupted: the next turn's agents do not take it.
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Agent", "denied"),
        prompt(3.0),
        sub_start(4.0, "a1"),
    ]);
    assert_eq!(view(&t).subagents[0].description, None);

    // A subagent's own Agent call is not a task for the next one.
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        sub_start(2.0, "a1"),
        in_agent(tool(3.0, "Agent", "nested"), "a1"),
        sub_start(4.0, "a2"),
    ]);
    assert_eq!(view(&t).subagents[1].description, None);
}

#[test]
fn subagent_events_without_start_create_the_subagent() {
    let t = feed(&[start(0.0), in_agent(tool(4.0, "Read", "a.rs"), "a9")]);
    let v = view(&t);
    assert_eq!(v.subagents.len(), 1);
    assert_eq!(v.subagents[0].id, "a9");
    // The session itself stays idle; it shows as working while that
    // subagent runs (background_agents_keep_the_session_working_after_its_turn).
    assert_eq!(v.state, S::Working, "shown working while its subagent runs");
    assert!(
        v.current_tool.is_none(),
        "a subagent tool is not the main row's"
    );
}

#[test]
fn finished_subagents_are_dropped_after_retention() {
    let cases = [(120.0, 1usize), (121.0, 0usize)];
    for (after_stop, expected) in cases {
        let mut t = feed(&[
            start(0.0),
            prompt(1.0),
            sub_start(2.0, "a1"),
            sub_start(3.0, "a2"),
            sub_stop(10.0, "a1"),
        ]);
        t.tick(T0 + 10.0 + after_stop);
        let v = view(&t);
        let done = v
            .subagents
            .iter()
            .filter(|a| a.state == SubagentState::Done)
            .count();
        assert_eq!(done, expected, "{after_stop} s after stop");
        assert!(
            v.subagents.iter().any(|a| a.id == "a2"),
            "working subagents stay"
        );
    }
}

#[test]
fn a_new_prompt_clears_finished_subagents() {
    let t = feed(&[
        start(0.0),
        prompt(1.0),
        sub_start(2.0, "a1"),
        sub_start(3.0, "a2"),
        sub_stop(4.0, "a1"),
        stop(5.0, "ok"),
        prompt(6.0),
    ]);
    let ids: Vec<String> = view(&t).subagents.into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["a2"]);
}

#[test]
fn a_subagent_question_is_cleared_only_by_that_subagent_moving_on() {
    let base = vec![
        start(0.0),
        prompt(1.0),
        sub_start(2.0, "a1"),
        sub_start(2.0, "a2"),
        in_agent(
            notification(3.0, "permission_prompt", "Allow WebFetch?"),
            "a1",
        ),
    ];
    let t = feed(&base);
    assert_eq!(state(&t), Some(S::NeedsYou));
    assert_eq!(view(&t).question.as_deref(), Some("Allow WebFetch?"));

    let mut other = base.clone();
    other.push(in_agent(tool(4.0, "Read", "x"), "a2"));
    assert_eq!(
        state(&feed(&other)),
        Some(S::NeedsYou),
        "another subagent's tool"
    );

    let mut same = base.clone();
    same.push(in_agent(tool(4.0, "WebFetch", "docs.rs"), "a1"));
    assert_eq!(
        state(&feed(&same)),
        Some(S::Working),
        "the asking subagent's tool"
    );

    let mut main = base;
    main.push(tool(4.0, "Bash", "ls"));
    assert_eq!(
        state(&feed(&main)),
        Some(S::Working),
        "main PreToolUse always clears"
    );
}

#[test]
fn other_subagent_events_are_activity_only() {
    let mut t = feed(&[start(0.0), prompt(1.0), sub_start(2.0, "a1")]);
    t.apply_event(&in_agent(tool_failure(5.0, "x"), "a1"), T0 + 5.0);
    let v = view(&t);
    assert_eq!(v.error_message, None);
    assert_eq!(v.last_activity, T0 + 5.0);
}

// ---------------------------------------------------------------- tick

#[test]
fn done_becomes_idle_after_twenty_seconds() {
    let cases = [(19.9, S::Done), (20.0, S::Idle), (300.0, S::Idle)];
    for (after, expected) in cases {
        let mut t = feed(&[start(0.0), prompt(1.0), stop(2.0, "ok")]);
        t.tick(T0 + 2.0 + after);
        assert_eq!(state(&t), Some(expected), "{after} s after Stop");
    }
}

#[test]
fn failed_and_needs_you_do_not_time_out_on_tick() {
    let mut t = feed(&[start(0.0), prompt(1.0), stop_failure(2.0, "x")]);
    t.tick(T0 + 500.0);
    assert_eq!(state(&t), Some(S::Failed));
    let mut t = feed(&[
        start(0.0),
        prompt(1.0),
        notification(2.0, "permission_prompt", "?"),
    ]);
    t.tick(T0 + 500.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
}

#[test]
fn stall_detection() {
    // (seconds since the last event, expected stalled)
    let cases = [(600.0, false), (600.5, true)];
    for (quiet, expected) in cases {
        let mut t = feed_registered(&[start(0.0), prompt(1.0), tool(2.0, "Bash", "npm run e2e")]);
        t.tick(T0 + 2.0 + quiet);
        let v = view(&t);
        assert_eq!(v.stalled, expected, "{quiet} s quiet");
        assert_eq!(v.state, S::Working, "stall never changes state");
    }

    let mut t = feed_registered(&[start(0.0), prompt(1.0), tool(2.0, "Bash", "npm run e2e")]);
    t.tick(T0 + 700.0);
    assert!(view(&t).stalled);
    t.apply_event(&tool(701.0, "Read", "log.txt"), T0 + 701.0);
    assert!(!view(&t).stalled, "the next event clears the stall");
}

#[test]
fn only_working_sessions_stall() {
    let mut t = feed_registered(&[
        start(0.0),
        prompt(1.0),
        notification(2.0, "permission_prompt", "?"),
    ]);
    t.tick(T0 + 900.0);
    assert!(!view(&t).stalled);
}

// ---------------------------------------------------------------- registry

#[test]
fn registry_creates_unknown_sessions() {
    let cases = [
        (RegistryStatus::Busy, S::Working, None),
        (RegistryStatus::Waiting, S::NeedsYou, Some("approve Bash")),
        (RegistryStatus::Idle, S::Idle, None),
        (RegistryStatus::Other("compacting".into()), S::Idle, None),
    ];
    for (status, expected, question) in cases {
        let mut t = SessionTracker::new();
        t.apply_registry(&[reg(SID, status.clone())], T0);
        let v = view(&t);
        assert_eq!(v.state, expected, "{status:?}");
        assert_eq!(v.question.as_deref(), question, "{status:?}");
        assert!(!v.has_hooks);
        assert_eq!(v.state_since, T0);
    }
}

#[test]
fn registry_fills_pid_cwd_name_and_entrypoint() {
    let mut t = feed(&[start(0.0)]);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 1.0);
    let v = view(&t);
    assert_eq!(v.pid, Some(4242));
    assert_eq!(v.cwd.as_deref(), Some("C:/work/registry-repo"));
    assert_eq!(v.entrypoint.as_deref(), Some("claude-vscode"));
    assert_eq!(v.display_name, "registry-repo-35");
}

#[test]
fn registry_drives_sessions_without_hooks() {
    let steps = [
        (RegistryStatus::Busy, S::Working),
        (RegistryStatus::Waiting, S::NeedsYou),
        (RegistryStatus::Busy, S::Working),
        (RegistryStatus::Idle, S::Idle),
        (RegistryStatus::Other("compacting".into()), S::Idle),
    ];
    let mut t = SessionTracker::new();
    for (i, (status, expected)) in steps.into_iter().enumerate() {
        t.apply_registry(&[reg(SID, status.clone())], T0 + i as f64 * 2.0);
        assert_eq!(state(&t), Some(expected), "step {i}: {status:?}");
    }
}

#[test]
fn registry_waiting_does_not_create_needs_you_for_hooked_sessions() {
    let mut t = feed(&[start(0.0), prompt(1.0)]);
    t.apply_registry(&[reg(SID, RegistryStatus::Waiting)], T0 + 5.0);
    assert_eq!(state(&t), Some(S::Working));
}

#[test]
fn registry_never_clears_needs_you_of_a_hooked_session() {
    // (status, seconds after the prompt appeared, expected)
    let cases = [
        (RegistryStatus::Busy, 5.0, S::NeedsYou),
        (RegistryStatus::Busy, 500.0, S::NeedsYou),
        (RegistryStatus::Idle, 30.0, S::NeedsYou),
    ];
    for (status, after, expected) in cases {
        let mut t = hooked_needs_you();
        t.apply_registry(&[reg(SID, status.clone())], T0 + 10.0 + after);
        assert_eq!(state(&t), Some(expected), "{status:?} after {after} s");
        assert_eq!(view(&t).question.as_deref(), Some("Allow Bash?"));
    }
}

#[test]
fn registry_waiting_then_busy_after_the_question_means_it_was_approved() {
    // Waiting seen after the question, then busy: approved, back to working.
    let mut t = hooked_needs_you();
    t.take_transitions();
    t.apply_registry(&[reg(SID, RegistryStatus::Waiting)], T0 + 11.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 14.0);
    let v = view(&t);
    assert_eq!(v.state, S::Working);
    assert_eq!(v.question, None);
    let edges: Vec<_> = t
        .take_transitions()
        .into_iter()
        .map(|e| (e.from, e.to))
        .collect();
    assert_eq!(edges, vec![(Some(S::NeedsYou), S::Working)]);

    // The registry already said waiting before the hook reported the question.
    let mut t = feed(&[start(0.0), prompt(1.0), tool(2.0, "Bash", "cargo test")]);
    t.apply_registry(&[reg(SID, RegistryStatus::Waiting)], T0 + 9.0);
    t.apply_event(
        &notification(10.0, "permission_prompt", "Allow Bash?"),
        T0 + 10.0,
    );
    assert_eq!(state(&t), Some(S::NeedsYou));
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 15.0);
    assert_eq!(state(&t), Some(S::Working));
}

#[test]
fn registry_busy_without_a_waiting_step_keeps_the_question() {
    // A waiting -> busy swap that happened before the question does not count.
    let mut t = feed(&[start(0.0), prompt(1.0), tool(2.0, "Bash", "cargo test")]);
    t.apply_registry(&[reg(SID, RegistryStatus::Waiting)], T0 + 5.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 8.0);
    t.apply_event(
        &notification(10.0, "permission_prompt", "Allow Bash?"),
        T0 + 10.0,
    );
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 15.0);
    assert_eq!(state(&t), Some(S::NeedsYou));

    // Idle -> busy is not an approval either.
    let mut t = hooked_needs_you();
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 11.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 14.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
}

#[test]
fn registry_idle_for_sixty_seconds_releases_needs_you() {
    let mut t = hooked_needs_you();
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 12.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 40.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 72.0);
    let v = view(&t);
    assert_eq!(v.state, S::Idle);
    assert_eq!(v.question, None);

    // The same release happens on tick, without a new snapshot.
    let mut t = hooked_needs_you();
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 12.0);
    t.tick(T0 + 71.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
    t.tick(T0 + 72.0);
    assert_eq!(state(&t), Some(S::Idle));
}

#[test]
fn an_interrupted_idle_run_restarts_the_sixty_seconds() {
    let mut t = hooked_needs_you();
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 12.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 50.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 52.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 100.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 112.0);
    assert_eq!(state(&t), Some(S::Idle));
}

#[test]
fn an_idle_run_older_than_the_question_counts_from_the_question() {
    // idle_prompt arrives while the registry has said idle for minutes.
    let mut t = feed(&[start(0.0)]);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 1.0);
    t.apply_event(
        &notification(200.0, "idle_prompt", "Waiting for input"),
        T0 + 200.0,
    );
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 202.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 260.0);
    assert_eq!(state(&t), Some(S::Idle));
}

#[test]
fn registry_moves_hooked_sessions_between_working_and_idle_when_newer() {
    // Hook says working; the registry later turns idle (e.g. a missed Stop).
    let mut t = feed(&[start(0.0), prompt(1.0)]);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 2.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 4.0);
    assert_eq!(state(&t), Some(S::Idle));

    // Hook says idle; the registry later reports busy.
    let mut t = feed(&[start(0.0)]);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 1.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 3.0);
    assert_eq!(state(&t), Some(S::Working));
}

#[test]
fn a_lagging_registry_does_not_override_a_newer_hook_event() {
    // The registry has said idle since before the prompt.
    let mut t = feed(&[start(0.0)]);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 1.0);
    t.apply_event(&prompt(5.0), T0 + 5.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 6.0);
    assert_eq!(state(&t), Some(S::Working));

    // The registry still says busy after Stop and the done timeout.
    let mut t = feed(&[start(0.0), prompt(1.0)]);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 2.0);
    t.apply_event(&stop(5.0, "ok"), T0 + 5.0);
    t.tick(T0 + 26.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 27.0);
    assert_eq!(state(&t), Some(S::Idle));
}

#[test]
fn registry_does_not_touch_done_or_failed_hooked_sessions() {
    let cases = [
        (vec![start(0.0), prompt(1.0), stop(2.0, "ok")], S::Done),
        (
            vec![start(0.0), prompt(1.0), stop_failure(2.0, "x")],
            S::Failed,
        ),
    ];
    for (events, expected) in cases {
        for status in [
            RegistryStatus::Busy,
            RegistryStatus::Idle,
            RegistryStatus::Waiting,
        ] {
            let mut t = feed(&events);
            t.apply_registry(&[reg(SID, status.clone())], T0 + 5.0);
            assert_eq!(state(&t), Some(expected), "{expected:?} with {status:?}");
        }
    }
}

#[test]
fn sessions_absent_from_the_registry_are_removed_after_the_grace_period() {
    // (seconds absent, expected present)
    let cases = [(0.0, true), (29.0, true), (30.0, false), (90.0, false)];
    for (absent, expected) in cases {
        let mut t = SessionTracker::new();
        t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0);
        t.apply_registry(&[], T0 + 2.0);
        t.apply_registry(&[], T0 + 2.0 + absent);
        assert_eq!(t.session(SID).is_some(), expected, "absent {absent} s");
    }

    // tick applies the same rule.
    let mut t = SessionTracker::new();
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0);
    t.apply_registry(&[], T0 + 2.0);
    t.tick(T0 + 32.0);
    assert!(t.is_empty());
}

#[test]
fn a_recent_spool_event_keeps_a_session_absent_from_the_registry() {
    let mut t = feed(&[start(0.0)]);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 1.0);
    t.apply_registry(&[], T0 + 3.0);
    t.apply_event(&prompt(20.0), T0 + 20.0);
    t.apply_registry(&[], T0 + 40.0);
    assert!(t.session(SID).is_some(), "event 20 s ago");
    t.apply_registry(&[], T0 + 51.0);
    assert!(t.session(SID).is_none(), "event 31 s ago");
}

#[test]
fn reappearing_in_the_registry_resets_the_grace_period() {
    let mut t = SessionTracker::new();
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0);
    t.apply_registry(&[], T0 + 20.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 25.0);
    t.apply_registry(&[], T0 + 40.0);
    t.apply_registry(&[], T0 + 60.0);
    assert!(t.session(SID).is_some());
}

#[test]
fn event_only_sessions_expire_after_ten_minutes_without_events() {
    let cases = [(599.0, true), (600.0, true), (601.0, false)];
    for (quiet, expected) in cases {
        let mut t = feed(&[start(0.0), prompt(1.0)]);
        // Registry snapshots that do not list it are not absences for it.
        t.apply_registry(&[], T0 + 30.0);
        t.apply_registry(&[], T0 + 100.0);
        assert!(t.session(SID).is_some());
        t.tick(T0 + 1.0 + quiet);
        assert_eq!(t.session(SID).is_some(), expected, "quiet {quiet} s");
    }
}

#[test]
fn registry_snapshots_handle_several_sessions() {
    let mut t = SessionTracker::new();
    t.apply_registry(
        &[
            reg("a", RegistryStatus::Busy),
            reg("b", RegistryStatus::Idle),
            reg("a", RegistryStatus::Idle),
        ],
        T0,
    );
    assert_eq!(t.len(), 2);
    assert_eq!(
        t.session("a").unwrap().state,
        S::Working,
        "first duplicate wins"
    );
    t.apply_registry(&[reg("b", RegistryStatus::Idle)], T0 + 1.0);
    t.apply_registry(&[reg("b", RegistryStatus::Idle)], T0 + 40.0);
    assert!(t.session("a").is_none());
    assert!(t.session("b").is_some());
}

// ---------------------------------------------------------------- marks, context, names

#[test]
fn display_name_precedence() {
    let full = TranscriptMarks {
        custom_title: Some("My rename".into()),
        title: Some("Fix flaky tests".into()),
        color: Some("green".into()),
        ..Default::default()
    };
    let ai = TranscriptMarks {
        title: Some("Fix flaky tests".into()),
        ..Default::default()
    };
    let blank = TranscriptMarks {
        custom_title: Some("  ".into()),
        ..Default::default()
    };
    let cases = [
        (
            "custom title wins",
            Some(full),
            true,
            Some("C:/x/repo"),
            "My rename",
        ),
        (
            "ai title next",
            Some(ai),
            true,
            Some("C:/x/repo"),
            "Fix flaky tests",
        ),
        (
            "blank titles are skipped",
            Some(blank),
            true,
            Some("C:/x/repo"),
            "registry-repo-35",
        ),
        ("registry name", None, true, None, "registry-repo-35"),
        (
            "cwd basename",
            None,
            false,
            Some("C:\\x\\my-repo\\"),
            "my-repo",
        ),
        (
            "posix cwd basename",
            None,
            false,
            Some("/home/me/proj"),
            "proj",
        ),
        ("short id", None, false, None, "3f9a1c2e"),
    ];
    for (name, marks, registry, cwd, expected) in cases {
        let mut t = SessionTracker::new();
        let mut e = ev("SessionStart", 0.0);
        e.cwd = cwd.map(str::to_string);
        t.apply_event(&e, T0);
        if registry {
            let mut entry = reg(SID, RegistryStatus::Idle);
            entry.cwd = None;
            t.apply_registry(&[entry], T0 + 1.0);
        }
        if let Some(m) = marks {
            t.apply_marks(SID, &m);
        }
        assert_eq!(view(&t).display_name, expected, "{name}");
    }
}

#[test]
fn marks_set_the_colour_and_replace_previous_marks() {
    let mut t = feed(&[start(0.0)]);
    t.apply_marks(
        SID,
        &TranscriptMarks {
            color: Some("green".into()),
            title: Some("A".into()),
            ..Default::default()
        },
    );
    assert_eq!(view(&t).color.as_deref(), Some("green"));
    t.apply_marks(
        SID,
        &TranscriptMarks {
            title: Some("B".into()),
            ..Default::default()
        },
    );
    let v = view(&t);
    assert_eq!(v.color, None);
    assert_eq!(v.display_name, "B");
    t.apply_marks("unknown", &TranscriptMarks::default());
    assert_eq!(t.len(), 1);
}

#[test]
fn set_context_enriches_the_view() {
    let mut t = feed(&[start(0.0)]);
    t.set_context(SID, Some(42), Some("claude-sonnet-4-5"));
    let v = view(&t);
    assert_eq!(v.context_percent, Some(42));
    assert_eq!(v.model.as_deref(), Some("claude-sonnet-4-5"));
    t.set_context(SID, Some(250), None);
    let v = view(&t);
    assert_eq!(v.context_percent, Some(100), "clamped");
    assert_eq!(
        v.model.as_deref(),
        Some("claude-sonnet-4-5"),
        "None keeps the model"
    );
    t.set_context("unknown", Some(1), None);
    assert_eq!(t.len(), 1);
}

// ---------------------------------------------------------------- outputs

#[test]
fn sessions_are_sorted_by_rank_then_recent_activity() {
    let mut t = SessionTracker::new();
    let at = |id: &str, event: &str, ts: f64| SpoolEvent::new(event, id, T0 + ts);
    t.apply_event(&at("idle-old", "SessionStart", 1.0), T0 + 1.0);
    t.apply_event(&at("work-old", "UserPromptSubmit", 2.0), T0 + 2.0);
    t.apply_event(&at("idle-new", "SessionStart", 3.0), T0 + 3.0);
    t.apply_event(&at("work-new", "UserPromptSubmit", 4.0), T0 + 4.0);
    let mut n = at("asks", "Notification", 5.0);
    n.notification_type = Some("permission_prompt".into());
    t.apply_event(&n, T0 + 5.0);
    t.apply_event(&at("failed", "StopFailure", 6.0), T0 + 6.0);
    t.apply_event(&at("done", "Stop", 7.0), T0 + 7.0);
    let ids: Vec<String> = t.sessions().into_iter().map(|v| v.id).collect();
    assert_eq!(
        ids,
        ["asks", "failed", "work-new", "work-old", "done", "idle-new", "idle-old"]
    );
}

#[test]
fn each_transition_is_reported_once() {
    let mut t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Bash", "a"),
        tool(3.0, "Bash", "b"),
    ]);
    let got = t.take_transitions();
    let edges: Vec<(Option<S>, S)> = got.iter().map(|x| (x.from, x.to)).collect();
    assert_eq!(edges, [(None, S::Idle), (Some(S::Idle), S::Working)]);
    assert_eq!(got[1].at, T0 + 1.0);
    assert_eq!(got[1].session_id, SID);
    assert!(t.take_transitions().is_empty(), "drained");

    t.apply_event(&notification(4.0, "permission_prompt", "?"), T0 + 4.0);
    t.apply_event(&notification(5.0, "permission_prompt", "again"), T0 + 5.0);
    t.apply_event(&tool(6.0, "Bash", "c"), T0 + 6.0);
    t.apply_event(&stop(7.0, "ok"), T0 + 7.0);
    t.tick(T0 + 30.0);
    t.tick(T0 + 31.0);
    let edges: Vec<(Option<S>, S)> = t
        .take_transitions()
        .iter()
        .map(|x| (x.from, x.to))
        .collect();
    assert_eq!(
        edges,
        [
            (Some(S::Working), S::NeedsYou),
            (Some(S::NeedsYou), S::Working),
            (Some(S::Working), S::Done),
            (Some(S::Done), S::Idle),
        ]
    );
}

#[test]
fn registry_creation_and_changes_are_transitions() {
    let mut t = SessionTracker::new();
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 2.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Waiting)], T0 + 4.0);
    let edges: Vec<(Option<S>, S, f64)> = t
        .take_transitions()
        .iter()
        .map(|x| (x.from, x.to, x.at))
        .collect();
    assert_eq!(
        edges,
        [
            (None, S::Working, T0),
            (Some(S::Working), S::NeedsYou, T0 + 4.0)
        ]
    );
}

#[test]
fn remote_sid_is_the_sha256_prefix() {
    // SHA-256("abc") = ba7816bf8f01cfea...
    assert_eq!(remote_sid("abc"), "ba7816bf");
    assert_eq!(remote_sid(SID).len(), 8);
    assert_ne!(remote_sid(SID), remote_sid("other"));
}

#[test]
fn remote_payload_carries_the_row_but_no_path() {
    let mut t = feed(&[
        start(0.0),
        prompt(1.5),
        tool(2.5, "Bash", "cargo test --workspace"),
    ]);
    t.set_context(SID, Some(37), Some("claude-opus-4-1"));
    let p = t.to_remote_payload(SID, "lab-01", T0 + 9.9).unwrap();
    assert_eq!(p.machine, "lab-01");
    assert_eq!(p.sid, remote_sid(SID));
    assert_eq!(p.alias, None);
    assert_eq!(p.state, S::Working);
    assert_eq!(p.since, (T0 + 1.5).floor() as i64);
    assert_eq!(p.tool.as_deref(), Some("Bash"));
    assert_eq!(p.model.as_deref(), Some("claude-opus-4-1"));
    assert_eq!(p.ctx, Some(37));
    assert_eq!(p.hb, (T0 + 9.9).floor() as i64);
    // The folder name and, while working, the tool summary; no title yet.
    assert_eq!(p.dir.as_deref(), Some("repo-a"));
    assert_eq!(p.text.as_deref(), Some("cargo test --workspace"));
    assert_eq!(p.title, None);
    let json = serde_json::to_string(&p).unwrap();
    assert!(
        !json.contains("Users") && !json.contains(SID),
        "no path, no session id: {json}"
    );
    assert!(t.to_remote_payload("unknown", "lab-01", T0).is_none());
}

#[test]
fn remote_payload_text_follows_the_state_and_titles_are_cut() {
    // Needs you: the question.
    let t = hooked_needs_you();
    let v = view(&t);
    let p = t.to_remote_payload(SID, "lab-01", T0 + 30.0).unwrap();
    assert_eq!(p.state, S::NeedsYou);
    assert_eq!(
        p.text,
        v.question
            .as_deref()
            .and_then(|q| remote_text(q, MAX_REMOTE_TEXT))
    );
    assert!(p.text.is_some());

    // Done: the last message, flattened; failed: the error.
    let t = feed(&[start(0.0), prompt(1.0), stop(2.0, "All done.\nTests pass.")]);
    let p = t.to_remote_payload(SID, "lab-01", T0 + 3.0).unwrap();
    assert_eq!(p.state, S::Done);
    assert_eq!(p.text.as_deref(), Some("All done. Tests pass."));
    let t = feed(&[start(0.0), prompt(1.0), stop_failure(2.0, "rate limited")]);
    let p = t.to_remote_payload(SID, "lab-01", T0 + 3.0).unwrap();
    assert_eq!(p.state, S::Failed);
    assert_eq!(p.text.as_deref(), Some("rate limited"));

    // A custom title wins over the AI title, and both are cut.
    let mut t = feed(&[start(0.0)]);
    t.apply_marks(
        SID,
        &TranscriptMarks {
            title: Some("t".repeat(300)),
            custom_title: Some("release prep".into()),
            ..TranscriptMarks::default()
        },
    );
    let p = t.to_remote_payload(SID, "lab-01", T0 + 1.0).unwrap();
    assert_eq!(p.title.as_deref(), Some("release prep"));
    t.apply_marks(
        SID,
        &TranscriptMarks {
            title: Some("t".repeat(300)),
            ..TranscriptMarks::default()
        },
    );
    let p = t.to_remote_payload(SID, "lab-01", T0 + 1.0).unwrap();
    assert_eq!(p.title.unwrap().chars().count(), MAX_REMOTE_TITLE + 1);
}

#[test]
fn views_serialize_in_camel_case() {
    let t = feed(&[start(0.0), prompt(1.0), sub_start(2.0, "a1")]);
    let json = serde_json::to_value(t.sessions()).unwrap();
    let row = &json[0];
    for key in [
        "id",
        "displayName",
        "state",
        "stateSince",
        "cwd",
        "pid",
        "entrypoint",
        "color",
        "currentTool",
        "toolSummary",
        "toolStartedAt",
        "turnStartedAt",
        "stalled",
        "subagents",
        "lastMessage",
        "question",
        "errorMessage",
        "model",
        "contextPercent",
        "hasHooks",
        "lastActivity",
    ] {
        assert!(row.get(key).is_some(), "missing {key}");
    }
    assert_eq!(row["state"], "working");
    assert_eq!(row["subagents"][0]["state"], "working");
    assert_eq!(row["subagents"][0]["agentType"], "Explore");
    assert!(row["subagents"][0].get("finishedAt").is_some());
    assert!(row["subagents"][0].get("description").is_some());
    assert!(row["subagents"][0].get("toolSummary").is_some());
    let back: Vec<SessionView> = serde_json::from_value(json).unwrap();
    assert_eq!(back, t.sessions());
}

// ---------------------------------------------------------------- a whole session

#[test]
fn a_long_realistic_session() {
    let mut t = SessionTracker::new();
    let mut now = T0;
    let mut step = |t: &mut SessionTracker, e: SpoolEvent| {
        now = e.ts;
        t.apply_event(&e, now);
        t.tick(now);
    };

    // The widget sees the registry entry a moment after startup.
    let mut s = start(0.0);
    s.model = Some("claude-opus-4-1".into());
    s.pid = Some(4242);
    step(&mut t, s);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 1.0);
    t.apply_marks(
        SID,
        &TranscriptMarks {
            title: Some("Fix the flaky e2e test".into()),
            color: Some("blue".into()),
            ..Default::default()
        },
    );

    step(&mut t, prompt(10.0));
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 11.0);
    step(&mut t, tool(12.0, "Read", "e2e.spec.ts"));
    step(&mut t, tool(14.0, "Grep", "waitFor"));
    step(&mut t, tool(16.0, "Bash", "npm run e2e"));
    step(&mut t, tool_failure(40.0, "exit code 1"));
    step(&mut t, tool(42.0, "Edit", "e2e.spec.ts"));
    step(&mut t, tool(44.0, "Bash", "git push"));
    step(
        &mut t,
        notification(
            45.0,
            "permission_prompt",
            "Claude needs your permission to use Bash",
        ),
    );
    // The registry keeps saying busy (and later idle) while the prompt is open.
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 47.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 49.0);
    t.tick(T0 + 80.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
    assert_eq!(
        view(&t).question.as_deref(),
        Some("Claude needs your permission to use Bash")
    );

    // Approved: the next tool clears the prompt.
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 90.0);
    step(&mut t, tool(91.0, "Task", "review the change"));
    step(&mut t, sub_start(92.0, "rev-1"));
    step(&mut t, sub_start(92.5, "rev-2"));
    step(&mut t, in_agent(tool(93.0, "Read", "e2e.spec.ts"), "rev-1"));
    step(&mut t, in_agent(tool(94.0, "Grep", "timeout"), "rev-2"));
    step(&mut t, sub_stop(110.0, "rev-1"));
    step(&mut t, sub_stop(115.0, "rev-2"));
    step(
        &mut t,
        stop(120.0, "Fixed the race in e2e.spec.ts and pushed."),
    );
    t.set_context(SID, Some(58), None);

    let v = view(&t);
    assert_eq!(v.state, S::Done);
    assert_eq!(v.state_since, T0 + 120.0);
    assert_eq!(v.display_name, "Fix the flaky e2e test");
    assert_eq!(v.color.as_deref(), Some("blue"));
    assert_eq!(v.pid, Some(4242));
    assert_eq!(v.entrypoint.as_deref(), Some("claude-vscode"));
    assert_eq!(v.current_tool, None);
    assert_eq!(v.turn_started_at, Some(T0 + 10.0));
    assert_eq!(v.question, None);
    assert_eq!(v.error_message.as_deref(), Some("exit code 1"));
    assert_eq!(
        v.last_message.as_deref(),
        Some("Fixed the race in e2e.spec.ts and pushed.")
    );
    assert_eq!(v.model.as_deref(), Some("claude-opus-4-1"));
    assert_eq!(v.context_percent, Some(58));
    assert!(v.has_hooks && !v.stalled);
    assert_eq!(v.last_activity, T0 + 120.0);
    let subs: Vec<(&str, SubagentState, Option<&str>, Option<f64>)> = v
        .subagents
        .iter()
        .map(|a| {
            (
                a.id.as_str(),
                a.state,
                a.agent_type.as_deref(),
                a.finished_at,
            )
        })
        .collect();
    assert_eq!(
        subs,
        [
            (
                "rev-1",
                SubagentState::Done,
                Some("Explore"),
                Some(T0 + 110.0)
            ),
            (
                "rev-2",
                SubagentState::Done,
                Some("Explore"),
                Some(T0 + 115.0)
            ),
        ]
    );

    let edges: Vec<(Option<S>, S)> = t
        .take_transitions()
        .iter()
        .map(|x| (x.from, x.to))
        .collect();
    assert_eq!(
        edges,
        [
            (None, S::Idle),
            (Some(S::Idle), S::Working),
            (Some(S::Working), S::NeedsYou),
            (Some(S::NeedsYou), S::Working),
            (Some(S::Working), S::Done),
        ]
    );

    // Twenty seconds later it is idle; two minutes after that the subagents go.
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 122.0);
    t.tick(T0 + 140.0);
    assert_eq!(state(&t), Some(S::Idle));
    assert_eq!(view(&t).subagents.len(), 2);
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 236.0);
    t.tick(T0 + 236.0);
    assert!(view(&t).subagents.is_empty());

    // The user closes the terminal: no SessionEnd, the registry drops it.
    t.apply_registry(&[], T0 + 300.0);
    assert!(t.session(SID).is_some());
    t.apply_registry(&[], T0 + 330.0);
    assert!(t.is_empty());
}

#[test]
fn an_ended_session_is_not_resurrected_by_a_lagging_registry() {
    let mut t = feed_registered(&[start(0.0), prompt(1.0)]);
    t.apply_event(&ev("SessionEnd", 5.0), T0 + 5.0);
    assert!(t.is_empty());
    t.take_transitions();

    // The process is still exiting: its registry file lists it for a pass.
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 7.0);
    assert!(t.is_empty(), "ghost row re-created from the registry");
    // A straggler written before SessionEnd does not bring it back either.
    t.apply_event(&stop(4.0, "bye"), T0 + 8.0);
    assert!(t.is_empty(), "straggler re-created the row");
    assert!(t.take_transitions().is_empty());

    // Once a snapshot drops it, a resume of the same id shows up again.
    t.apply_registry(&[], T0 + 9.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 20.0);
    assert_eq!(state(&t), Some(S::Working));
}

#[test]
fn a_resumed_session_clears_its_tombstone_through_hooks() {
    let mut t = feed_registered(&[start(0.0)]);
    t.apply_event(&ev("SessionEnd", 5.0), T0 + 5.0);
    t.apply_event(&start(10.0), T0 + 10.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 11.0);
    assert!(t.session(SID).is_some_and(|v| v.has_hooks));
}

// ---------------------------------------------------------------- clear

const OTHER: &str = "3f9a1c2e-0000-4000-8000-0000000000ff";

#[test]
fn clear_empties_the_list_and_the_registry_brings_live_sessions_back() {
    let mut t = hooked_needs_you();
    t.apply_registry(
        &[
            reg(SID, RegistryStatus::Busy),
            reg(OTHER, RegistryStatus::Idle),
        ],
        T0 + 11.0,
    );
    t.take_transitions();
    let before = view(&t);

    assert_eq!(t.clear(T0 + 12.0), 2);
    assert!(t.is_empty());
    assert!(t.is_cleared(SID) && t.is_cleared(OTHER));

    // Only SID is still alive: the next snapshot lists it, not OTHER.
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 14.0);
    assert_eq!(t.len(), 1);
    assert!(
        t.session(OTHER).is_none(),
        "a dead session does not come back"
    );
    assert!(!t.is_cleared(OTHER), "and is forgotten");

    let after = view(&t);
    assert_eq!(after.state, S::NeedsYou, "the question survives the clear");
    assert_eq!(after.question.as_deref(), Some("Allow Bash?"));
    assert_eq!(after.state_since, before.state_since);
    assert!(after.has_hooks, "the hook flag survives");
    assert_eq!(after.cwd, before.cwd);
    assert!(
        after.current_tool.is_none(),
        "the tool returns with the next hook event"
    );
    assert!(
        t.take_transitions().is_empty(),
        "no creation transition, so no second toast"
    );
}

#[test]
fn clear_drops_the_tool_and_subagents_until_the_next_hook_event() {
    let mut t = feed_registered(&[
        start(0.0),
        prompt(1.0),
        sub_start(2.0, "agent-1"),
        tool(3.0, "Bash", "npm test"),
    ]);
    assert_eq!(view(&t).subagents.len(), 1);
    t.clear(T0 + 4.0);
    t.apply_registry(&[reg(SID, RegistryStatus::Busy)], T0 + 5.0);
    let v = view(&t);
    assert_eq!(v.state, S::Working);
    assert!(v.current_tool.is_none() && v.subagents.is_empty());
    assert_eq!(
        v.turn_started_at,
        Some(T0 + 1.0),
        "the turn timer keeps counting"
    );

    t.apply_event(&tool(6.0, "Edit", "src/lib.rs"), T0 + 6.0);
    assert_eq!(view(&t).current_tool.as_deref(), Some("Edit"));
}

#[test]
fn a_cleared_session_also_comes_back_through_a_hook_event() {
    let mut t = feed_registered(&[start(0.0), prompt(1.0), stop(2.0, "done")]);
    t.clear(T0 + 3.0);
    t.take_transitions();
    t.apply_event(&prompt(4.0), T0 + 4.0);
    let v = view(&t);
    assert_eq!(v.state, S::Working);
    assert_eq!(
        v.cwd.as_deref(),
        Some("C:/work/registry-repo"),
        "restored, not re-created"
    );
    let tr = t.take_transitions();
    assert_eq!(tr.len(), 1);
    assert_eq!(
        tr[0].from,
        Some(S::Done),
        "an edge from the old state, not a creation"
    );
}

#[test]
fn an_unlisted_cleared_session_is_forgotten_and_a_later_event_starts_afresh() {
    // An event-only session (never in the registry), cleared.
    let mut t = feed(&[start(0.0), prompt(1.0)]);
    t.clear(T0 + 2.0);
    t.apply_registry(&[], T0 + 3.0);
    assert!(!t.is_cleared(SID));
    t.apply_event(&tool(4.0, "Read", "a.rs"), T0 + 4.0);
    let v = view(&t);
    assert_eq!(v.state, S::Working);
    assert_eq!(v.turn_started_at, None, "a new row, nothing carried over");
}

#[test]
fn cleared_sessions_expire_without_a_registry() {
    let mut t = feed(&[start(0.0)]);
    t.clear(T0 + 1.0);
    t.tick(T0 + 1.0 + UNREGISTERED_TIMEOUT_SECS);
    assert!(t.is_cleared(SID));
    t.tick(T0 + 2.0 + UNREGISTERED_TIMEOUT_SECS);
    assert!(!t.is_cleared(SID));
}

#[test]
fn an_event_only_session_that_needs_you_comes_back_after_a_clear() {
    // Hooked, never listed by the registry, waiting for an answer: no hook
    // fires until the user answers, so it must not wait for one to return.
    let mut t = feed(&[
        start(0.0),
        prompt(1.0),
        notification(2.0, "permission_prompt", "Allow Bash?"),
    ]);
    t.take_transitions();
    t.clear(T0 + 3.0);
    t.apply_registry(&[reg(OTHER, RegistryStatus::Idle)], T0 + 4.0);
    let v = view(&t);
    assert_eq!(v.state, S::NeedsYou);
    assert_eq!(v.question.as_deref(), Some("Allow Bash?"));
    assert!(
        !t.take_transitions().iter().any(|tr| tr.session_id == SID),
        "restored, so it does not toast again"
    );

    // A session the registry did list and no longer does is gone, waiting or not.
    let mut t = SessionTracker::new();
    t.apply_registry(&[reg(SID, RegistryStatus::Waiting)], T0);
    t.clear(T0 + 1.0);
    t.apply_registry(&[], T0 + 2.0);
    assert!(t.is_empty() && !t.is_cleared(SID));
}

#[test]
fn a_cleared_session_that_needs_you_does_not_wait_for_a_snapshot() {
    // The CLI is failing: no registry snapshot on the next passes.
    let mut t = hooked_needs_you();
    t.take_transitions();
    t.clear(T0 + 11.0);
    t.tick(T0 + 13.0);
    assert_eq!(state(&t), Some(S::NeedsYou));
    assert!(t.take_transitions().is_empty(), "no second toast");

    // Rows that do not need the user still wait for the registry or a hook.
    let mut t = feed_registered(&[start(0.0), prompt(1.0)]);
    t.clear(T0 + 2.0);
    t.tick(T0 + 4.0);
    assert!(t.is_empty() && t.is_cleared(SID));
}

#[test]
fn a_session_that_ends_after_the_clear_does_not_come_back() {
    let mut t = feed_registered(&[start(0.0), prompt(1.0)]);
    t.clear(T0 + 2.0);
    t.apply_event(&ev("SessionEnd", 3.0), T0 + 3.0);
    assert!(!t.is_cleared(SID));
    t.apply_registry(&[reg(SID, RegistryStatus::Idle)], T0 + 4.0);
    assert!(t.is_empty(), "the tombstone still holds");
}

#[test]
fn background_agents_keep_the_session_working_after_its_turn() {
    // The main turn ends while two subagents still run.
    let mut t = feed(&[
        start(0.0),
        prompt(1.0),
        sub_start(2.0, "a1"),
        sub_start(2.5, "a2"),
        stop(3.0, "Started two agents."),
    ]);
    t.tick(T0 + 30.0);
    let v = view(&t);
    assert_eq!(v.state, S::Working, "shown working while agents run");
    let p = t.to_remote_payload(SID, "lab-01", T0 + 30.0).unwrap();
    assert_eq!(p.state, S::Working);
    assert_eq!(p.agents, Some([2, 2]));
    assert_eq!(p.text, None, "no tool: the viewer says the agents run");

    // One finishes: still working, 1 of 2 running.
    t.apply_event(&sub_stop(31.0, "a1"), T0 + 31.0);
    assert_eq!(view(&t).state, S::Working);
    assert_eq!(
        t.to_remote_payload(SID, "lab-01", T0 + 31.0)
            .unwrap()
            .agents,
        Some([1, 2])
    );

    // The last one finishes: the session's own state shows again.
    t.apply_event(&sub_stop(32.0, "a2"), T0 + 32.0);
    let v = view(&t);
    assert_eq!(v.state, S::Idle);
    let p = t.to_remote_payload(SID, "lab-01", T0 + 32.0).unwrap();
    assert_eq!(p.state, S::Idle);
    assert_eq!(p.text.as_deref(), Some("Started two agents."));
    assert_eq!(p.agents, Some([0, 2]));
    assert_eq!(p.ags.map(|a| a.len()), Some(2));
}

#[test]
fn the_payload_describes_the_first_subagents_running_ones_first() {
    let long = "x".repeat(200);
    let mut t = feed(&[
        start(0.0),
        prompt(1.0),
        tool(2.0, "Agent", "Find usages"),
        sub_start(2.5, "a1"),
        tool(3.0, "Agent", &long),
        sub_start(3.5, "a2"),
        in_agent(tool(4.0, "Grep", "remote_text"), "a2"),
        sub_stop(5.0, "a1"),
    ]);
    let p = t.to_remote_payload(SID, "lab-01", T0 + 6.0).unwrap();
    assert_eq!(p.agents, Some([1, 2]));
    let ags = p.ags.unwrap();
    assert_eq!(ags.len(), 2);
    // a2 runs, so it comes first; its texts are cut.
    assert!(ags[0].run);
    assert_eq!(ags[0].t.as_deref(), Some("Explore"));
    assert_eq!(
        ags[0].d.as_ref().map(|d| d.chars().count()),
        Some(crate::session::MAX_REMOTE_AGENT_TASK + 1)
    );
    assert_eq!(
        (ags[0].tool.as_deref(), ags[0].s.as_deref()),
        (Some("Grep"), Some("remote_text"))
    );
    assert_eq!(
        ags[1],
        RemoteAgent {
            t: Some("Explore".into()),
            d: Some("Find usages".into()),
            tool: None,
            s: None,
            run: false
        }
    );

    // At most MAX_REMOTE_AGENT_ROWS lines; the counts still cover them all.
    for i in 0..10 {
        t.apply_event(
            &sub_start(7.0 + f64::from(i), &format!("b{i}")),
            T0 + 7.0 + f64::from(i),
        );
    }
    let p = t.to_remote_payload(SID, "lab-01", T0 + 20.0).unwrap();
    assert_eq!(p.agents, Some([11, 12]));
    let ags = p.ags.unwrap();
    assert_eq!(ags.len(), crate::session::MAX_REMOTE_AGENT_ROWS);
    assert!(ags.iter().all(|a| a.run));
}

#[test]
fn a_silent_subagent_stops_counting_and_comes_back_with_an_event() {
    let mut t = feed(&[
        start(0.0),
        prompt(1.0),
        sub_start(2.0, "a1"),
        stop(3.0, "ok"),
    ]);
    t.tick(T0 + 2.0 + SUBAGENT_SILENT_SECS + 1.0);
    assert_eq!(
        view(&t).state,
        S::Idle,
        "a missed SubagentStop does not keep it working"
    );
    let mut e = tool(SUBAGENT_SILENT_SECS + 10.0, "Read", "x");
    e = in_agent(e, "a1");
    t.apply_event(&e, T0 + SUBAGENT_SILENT_SECS + 10.0);
    assert_eq!(view(&t).state, S::Working, "its next event revives it");
}
