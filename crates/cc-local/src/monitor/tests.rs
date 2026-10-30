//! Tests for the shared monitor: the registry source (CLI cadence, stale,
//! new and dead sessions), the pass over the tracker (context, marks,
//! pruning, payloads) and a whole pass over temporary folders.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use cc_core::session::{RegistryStatus, SessionState};

const A_PID: u32 = 666;

fn reg(id: &str, status: RegistryStatus) -> RegistryEntry {
    RegistryEntry {
        session_id: id.into(),
        pid: Some(4242),
        cwd: Some("C:/work/demo-repo".into()),
        name: Some("demo-repo-1".into()),
        kind: Some("interactive".into()),
        status,
        waiting_for: None,
        entrypoint: Some("cli".into()),
        started_at_ms: None,
        background_id: None,
    }
}

fn reg_pid(id: &str, pid: u32) -> RegistryEntry {
    RegistryEntry {
        pid: Some(pid),
        ..reg(id, RegistryStatus::Idle)
    }
}

fn file(id: &str, status: RegistryStatus) -> SessionFile {
    SessionFile {
        session_id: id.into(),
        pid: Some(7),
        cwd: Some("C:/work/file-repo".into()),
        name: Some("file-repo-2".into()),
        kind: Some("interactive".into()),
        entrypoint: Some("claude-vscode".into()),
        status: Some(status),
        started_at_ms: None,
        updated_at_ms: None,
        version: None,
    }
}

fn file_pid(id: &str, status: RegistryStatus, pid: u32) -> SessionFile {
    SessionFile {
        pid: Some(pid),
        ..file(id, status)
    }
}

/// A source where every process is alive, so only the CLI and the files
/// decide (the behaviour before the process probe existed).
fn src() -> RegistrySource {
    RegistrySource::default().with_probe(|_| Some(true))
}

/// A source where [`A_PID`] is alive while `alive` says so, and every other
/// process is alive.
fn src_with(alive: &Arc<AtomicBool>) -> RegistrySource {
    let alive = Arc::clone(alive);
    RegistrySource::default()
        .with_probe(move |pid| Some(pid != A_PID || alive.load(Ordering::SeqCst)))
}

fn ids(entries: &Option<Vec<RegistryEntry>>) -> Vec<&str> {
    entries
        .as_ref()
        .map(|v| v.iter().map(|e| e.session_id.as_str()).collect())
        .unwrap_or_default()
}

fn monitor() -> (SessionMonitor, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reader = TranscriptReader::with_projects_dir(dir.path());
    (SessionMonitor::with_reader(reader), dir)
}

fn ev(event: &str, id: &str, ts: f64) -> SpoolEvent {
    SpoolEvent::new(event, id, ts)
}

// ---------------------------------------------------------------- cadence

#[test]
fn the_cli_runs_first_then_only_on_changes_or_every_refresh() {
    let mut src = src();
    let files = vec![file("a", RegistryStatus::Idle)];
    assert!(src.cli_due(&files, 0.0), "first pass");
    src.resolve(&files, Some(Ok(vec![reg("a", RegistryStatus::Idle)])), 0.0);
    assert!(!src.cli_due(&files, 2.0), "same files, too soon");
    let more = vec![
        file("a", RegistryStatus::Idle),
        file("b", RegistryStatus::Busy),
    ];
    assert!(src.cli_due(&more, 2.0), "a new session file");
    assert!(src.cli_due(&[], 2.0), "a session file went away");
    assert!(
        src.cli_due(&files, AGENTS_REFRESH.as_secs_f64()),
        "periodic refresh"
    );
}

#[test]
fn an_idle_machine_runs_the_cli_less_often_with_the_idle_backoff() {
    let mut src = RegistrySource::new(Cadence::IDLE_BACKOFF).with_probe(|_| Some(true));
    src.resolve(&[], Some(Ok(vec![])), 0.0);
    assert!(!src.cli_due(&[], AGENTS_REFRESH.as_secs_f64()));
    assert!(src.cli_due(&[], AGENTS_REFRESH_IDLE.as_secs_f64()));
    assert!(
        src.cli_due(&[file("new", RegistryStatus::Busy)], 4.0),
        "a new session"
    );

    // The widget's steady cadence does not back off.
    let mut steady = src_steady();
    steady.resolve(&[], Some(Ok(vec![])), 0.0);
    assert!(steady.cli_due(&[], AGENTS_REFRESH.as_secs_f64()));
}

fn src_steady() -> RegistrySource {
    RegistrySource::new(Cadence::STEADY).with_probe(|_| Some(true))
}

// ---------------------------------------------------------------- stale files and failures

#[test]
fn between_cli_runs_files_give_the_status_and_stale_files_stay_out() {
    let mut src = src();
    let files = vec![
        file("a", RegistryStatus::Idle),
        file("crashed", RegistryStatus::Busy),
    ];
    // The CLI knows the crashed process is gone (its pid is even alive here:
    // reused by another process).
    let first = src.resolve(&files, Some(Ok(vec![reg("a", RegistryStatus::Idle)])), 0.0);
    assert_eq!(ids(&first.registry), ["a"]);
    assert_eq!(
        first.registry.as_ref().unwrap()[0].entrypoint.as_deref(),
        Some("cli")
    );

    let busy = vec![
        file("a", RegistryStatus::Busy),
        file("crashed", RegistryStatus::Busy),
    ];
    let next = src.resolve(&busy, None, 2.0);
    assert!(next.error.is_none());
    assert_eq!(ids(&next.registry), ["a"]);
    assert_eq!(
        next.registry.as_ref().unwrap()[0].status,
        RegistryStatus::Busy
    );
    let later = src.resolve(&busy, None, 30.0);
    assert_eq!(ids(&later.registry), ["a"], "the stale file never gets in");
}

#[test]
fn cli_only_sessions_survive_between_runs() {
    let mut src = src();
    let mut bg = reg("bg", RegistryStatus::Busy);
    bg.background_id = Some("bg_01".into());
    src.resolve(&[], Some(Ok(vec![bg])), 0.0);
    let next = src.resolve(&[], None, 2.0);
    assert_eq!(ids(&next.registry), ["bg"]);
    assert_eq!(
        next.registry.unwrap()[0].background_id.as_deref(),
        Some("bg_01")
    );
}

#[test]
fn a_failing_cli_keeps_the_last_list_then_falls_back_to_the_files() {
    let mut src = self::src();
    let files = vec![file("a", RegistryStatus::Idle)];
    let none = src.resolve(&files, Some(Err("timeout".into())), 0.0);
    assert!(
        none.registry.is_none(),
        "nothing confirmed yet: skip the registry"
    );
    assert_eq!(none.error.as_deref(), Some("timeout"));

    let mut src = self::src();
    src.resolve(&files, Some(Ok(vec![reg("a", RegistryStatus::Idle)])), 0.0);
    let kept = src.resolve(&files, Some(Err("timeout".into())), 30.0);
    assert_eq!(ids(&kept.registry), ["a"]);
    src.resolve(&files, Some(Err("timeout".into())), 60.0);
    let both = vec![
        file("a", RegistryStatus::Idle),
        file("b", RegistryStatus::Busy),
    ];
    let fallback = src.resolve(&both, Some(Err("timeout".into())), 62.0);
    assert_eq!(
        ids(&fallback.registry),
        ["a", "b"],
        "after three failures, files alone"
    );
    assert_eq!(src.failures(), 3);
}

#[test]
fn stale_files_stay_out_and_a_failing_cli_falls_back_to_files() {
    let mut src = self::src();
    let files = vec![
        file("a", RegistryStatus::Idle),
        file("crashed", RegistryStatus::Busy),
    ];
    let first = src.resolve(&files, Some(Ok(vec![reg("a", RegistryStatus::Idle)])), 0.0);
    assert_eq!(ids(&first.registry), ["a"]);
    let next = src.resolve(&files, None, 2.0);
    assert_eq!(ids(&next.registry), ["a"]);

    let mut src = self::src();
    let none = src.resolve(&files, Some(Err("timeout".into())), 0.0);
    assert!(none.registry.is_none());
    assert_eq!(none.error.as_deref(), Some("timeout"));
    src.resolve(&files, Some(Err("timeout".into())), 30.0);
    let fallback = src.resolve(&files, Some(Err("timeout".into())), 60.0);
    assert_eq!(
        ids(&fallback.registry),
        ["a", "crashed"],
        "after three failures, files alone"
    );
    assert_eq!(src.failures(), 3);
}

// ---------------------------------------------------------------- dead sessions

#[test]
fn a_dead_session_whose_file_is_gone_drops_at_once_whatever_the_cli_does() {
    let alive = Arc::new(AtomicBool::new(true));
    let mut src = src_with(&alive);
    let files = vec![
        file_pid("a", RegistryStatus::Busy, A_PID),
        file("b", RegistryStatus::Idle),
    ];
    let cli = || Ok(vec![reg_pid("a", A_PID), reg("b", RegistryStatus::Idle)]);
    assert_eq!(
        ids(&src.resolve(&files, Some(cli()), 0.0).registry),
        ["a", "b"]
    );

    // "a" exits and its file goes. The CLI fails: before, "a" stayed listed
    // until the third failure in a row.
    alive.store(false, Ordering::SeqCst);
    let only_b = vec![file("b", RegistryStatus::Idle)];
    let mut failing = src.clone();
    let out = failing.resolve(&only_b, Some(Err("timeout".into())), 2.0);
    assert_eq!(ids(&out.registry), ["b"]);
    assert_eq!(out.dead, ["a"]);
    assert_eq!(out.error.as_deref(), Some("timeout"));
    let next = failing.resolve(&only_b, None, 4.0);
    assert_eq!(ids(&next.registry), ["b"], "and it stays out");
    assert!(next.dead.is_empty(), "reported once");

    // The CLI lags and still lists it: dropped all the same.
    let mut lagging = src.clone();
    let out = lagging.resolve(&only_b, Some(cli()), 2.0);
    assert_eq!(ids(&out.registry), ["b"]);
    assert_eq!(out.dead, ["a"]);

    // No CLI run on this pass at all (it is not due): dropped too.
    let mut quiet = src.clone();
    let out = quiet.resolve(&only_b, None, 2.0);
    assert_eq!(
        (ids(&out.registry), out.dead),
        (vec!["b"], vec!["a".to_string()])
    );
}

#[test]
fn a_session_is_only_dropped_when_its_process_is_known_to_have_exited() {
    let alive = Arc::new(AtomicBool::new(true));
    let mut src = src_with(&alive);
    let files = vec![file_pid("a", RegistryStatus::Busy, A_PID)];
    src.resolve(&files, Some(Ok(vec![reg_pid("a", A_PID)])), 0.0);

    // File gone, process alive (e.g. /clear gave it another id): the CLI decides.
    let out = src.clone().resolve(&[], Some(Err("timeout".into())), 2.0);
    assert_eq!(ids(&out.registry), ["a"]);
    assert!(out.dead.is_empty());

    // The OS cannot tell: the CLI decides.
    let mut unknown = RegistrySource::default().with_probe(|_| None);
    unknown.resolve(&files, Some(Ok(vec![reg_pid("a", A_PID)])), 0.0);
    let out = unknown.resolve(&[], Some(Err("timeout".into())), 2.0);
    assert_eq!(ids(&out.registry), ["a"]);
    assert!(out.dead.is_empty());

    // A session only the CLI knows (no file ever): only the CLI can drop it.
    alive.store(false, Ordering::SeqCst);
    let mut cli_only = src_with(&alive);
    cli_only.resolve(&[], Some(Ok(vec![reg_pid("bg", A_PID)])), 0.0);
    let out = cli_only.resolve(&[], None, 2.0);
    assert_eq!(ids(&out.registry), ["bg"]);
    assert!(out.dead.is_empty());
}

#[test]
fn a_dead_session_leaves_the_files_fallback_at_once_too() {
    let alive = Arc::new(AtomicBool::new(true));
    let mut src = src_with(&alive);
    let files = vec![
        file_pid("a", RegistryStatus::Busy, A_PID),
        file("b", RegistryStatus::Idle),
    ];
    for t in [0.0, 30.0, 60.0] {
        src.resolve(&files, Some(Err("not found".into())), t);
    }
    assert_eq!(src.failures(), 3);
    alive.store(false, Ordering::SeqCst);
    let out = src.resolve(&files[1..], Some(Err("not found".into())), 62.0);
    assert_eq!(ids(&out.registry), ["b"]);
    assert_eq!(out.dead, ["a"]);
}

#[test]
fn the_files_fallback_leaves_out_files_of_exited_processes() {
    // "crashed" left its file behind; its process is gone. "unknown" cannot
    // be probed, so it stays.
    let mut src = RegistrySource::default().with_probe(|pid| match pid {
        A_PID => Some(false),
        7 => Some(true),
        _ => None,
    });
    let files = vec![
        file("a", RegistryStatus::Idle),
        file_pid("crashed", RegistryStatus::Busy, A_PID),
        file_pid("unknown", RegistryStatus::Idle, 99),
    ];
    for t in [0.0, 30.0] {
        src.resolve(&files, Some(Err("not found".into())), t);
    }
    let out = src.resolve(&files, Some(Err("not found".into())), 60.0);
    assert_eq!(src.failures(), AGENTS_FAILURES_BEFORE_FALLBACK);
    assert_eq!(ids(&out.registry), ["a", "unknown"]);
    // Every later pass too, while the file is still there.
    let later = src.resolve(&files, None, 62.0);
    assert_eq!(ids(&later.registry), ["a", "unknown"]);
}

#[test]
fn a_file_without_a_status_keeps_the_one_the_cli_reported() {
    let mut src = src();
    let no_status = || {
        vec![SessionFile {
            status: None,
            ..file("a", RegistryStatus::Idle)
        }]
    };
    let mut waiting = reg("a", RegistryStatus::Waiting);
    waiting.waiting_for = Some("permission".into());
    let cli = src.resolve(&no_status(), Some(Ok(vec![waiting])), 0.0);
    assert_eq!(
        cli.registry.as_ref().unwrap()[0].status,
        RegistryStatus::Waiting
    );

    // A file-only pass: the status and what it waits for stay the CLI's.
    let next = src.resolve(&no_status(), None, 2.0);
    let entry = &next.registry.as_ref().unwrap()[0];
    assert_eq!(entry.status, RegistryStatus::Waiting);
    assert_eq!(entry.waiting_for.as_deref(), Some("permission"));
    // The file's own fields still win.
    assert_eq!(entry.entrypoint.as_deref(), Some("claude-vscode"));

    // A file that has a status gives it.
    let busy = vec![file("a", RegistryStatus::Busy)];
    let with_status = src.resolve(&busy, None, 4.0);
    let entry = &with_status.registry.as_ref().unwrap()[0];
    assert_eq!(entry.status, RegistryStatus::Busy);
    assert_eq!(entry.waiting_for, None);
}

#[test]
fn a_dead_session_leaves_the_rows_at_once() {
    let (mut m, _dir) = monitor();
    let both = || {
        Some(vec![
            reg("a", RegistryStatus::Busy),
            reg("b", RegistryStatus::Idle),
        ])
    };
    m.step(
        Gathered {
            registry: both(),
            registry_at: 1.0,
            ..Default::default()
        },
        1.0,
    );
    assert!(m.view("a").is_some());

    // Without `dead`, "a" would stay for the registry's grace period.
    let mut without = monitor().0;
    without.step(
        Gathered {
            registry: both(),
            registry_at: 1.0,
            ..Default::default()
        },
        1.0,
    );
    let only_b = || Some(vec![reg("b", RegistryStatus::Idle)]);
    without.step(
        Gathered {
            registry: only_b(),
            registry_at: 3.0,
            ..Default::default()
        },
        3.0,
    );
    assert!(without.view("a").is_some(), "grace period");

    // A straggler drained on the same pass, written before it died, does not
    // bring it back (nor toast).
    let mut late = ev("Notification", "a", 2.5);
    late.notification_type = Some("permission_prompt".into());
    let out = m.step(
        Gathered {
            registry: only_b(),
            registry_at: 3.0,
            dead: vec!["a".into()],
            events: vec![late],
            ..Default::default()
        },
        3.0,
    );
    assert!(out.changed);
    assert!(m.view("a").is_none());
    assert!(m.view("b").is_some());
    assert!(
        out.transitions.iter().all(|t| t.session_id != "a"),
        "{:?}",
        out.transitions
    );
}

// ---------------------------------------------------------------- new sessions

#[test]
fn a_new_session_file_shows_at_once_and_waits_for_the_cli_to_confirm_it() {
    let mut src = src();
    let a = vec![file("a", RegistryStatus::Idle)];
    src.resolve(&a, Some(Ok(vec![reg("a", RegistryStatus::Idle)])), 0.0);

    // "b" starts; the CLI runs on that pass but lags behind its file.
    let ab = vec![
        file("a", RegistryStatus::Idle),
        file("b", RegistryStatus::Busy),
    ];
    assert!(src.cli_due(&ab, 2.0));
    let out = src.resolve(&ab, Some(Ok(vec![reg("a", RegistryStatus::Idle)])), 2.0);
    assert_eq!(
        ids(&out.registry),
        ["a", "b"],
        "shown from its file at once"
    );
    let b = &out.registry.as_ref().unwrap()[1];
    assert_eq!(b.status, RegistryStatus::Busy, "status from the file");
    assert_eq!(b.entrypoint.as_deref(), Some("claude-vscode"));

    // Between CLI runs it stays, with the file's status.
    assert!(!src.cli_due(&ab, 4.0));
    let mut idle = ab.clone();
    idle[1].status = Some(RegistryStatus::Idle);
    let out = src.resolve(&idle, None, 4.0);
    assert_eq!(ids(&out.registry), ["a", "b"]);
    assert_eq!(
        out.registry.as_ref().unwrap()[1].status,
        RegistryStatus::Idle
    );

    // The next CLI run comes early, to confirm it.
    let confirm_at = 2.0 + NEW_SESSION_CONFIRM_SECS;
    assert!(!src.cli_due(&ab, confirm_at - 0.5));
    assert!(src.cli_due(&ab, confirm_at));

    let mut confirmed = src.clone();
    let out = confirmed.resolve(
        &ab,
        Some(Ok(vec![
            reg("a", RegistryStatus::Idle),
            reg("b", RegistryStatus::Busy),
        ])),
        confirm_at,
    );
    assert_eq!(ids(&out.registry), ["a", "b"]);
    assert!(
        !confirmed.cli_due(&ab, confirm_at + 2.0),
        "back to the normal cadence"
    );
    assert_eq!(
        ids(&confirmed.resolve(&ab, None, confirm_at + 2.0).registry),
        ["a", "b"]
    );

    // Still not listed when the CLI had its chance: dropped.
    let out = src.resolve(
        &ab,
        Some(Ok(vec![reg("a", RegistryStatus::Idle)])),
        confirm_at,
    );
    assert_eq!(ids(&out.registry), ["a"]);
    assert_eq!(
        ids(&src.resolve(&ab, None, confirm_at + 2.0).registry),
        ["a"]
    );
    assert!(!src.cli_due(&ab, confirm_at + 4.0));
}

#[test]
fn a_new_session_file_shows_at_once_while_the_cli_fails() {
    let mut src = src();
    src.resolve(
        &[file("a", RegistryStatus::Idle)],
        Some(Ok(vec![reg("a", RegistryStatus::Idle)])),
        0.0,
    );
    let ab = vec![
        file("a", RegistryStatus::Idle),
        file("b", RegistryStatus::Waiting),
    ];
    let out = src.resolve(&ab, Some(Err("timeout".into())), 2.0);
    assert_eq!(ids(&out.registry), ["a", "b"]);
    assert_eq!(out.registry.unwrap()[1].status, RegistryStatus::Waiting);
}

#[test]
fn a_new_file_of_a_dead_process_does_not_show() {
    let alive = Arc::new(AtomicBool::new(false));
    let mut src = src_with(&alive);
    src.resolve(&[], Some(Ok(vec![])), 0.0);
    let files = vec![file_pid("ghost", RegistryStatus::Busy, A_PID)];
    let out = src.resolve(&files, Some(Ok(vec![])), 2.0);
    assert_eq!(ids(&out.registry), Vec::<&str>::new());
    assert!(out.dead.is_empty());
}

// ---------------------------------------------------------------- the tracker side

#[test]
fn registry_only_sessions_show_up_and_change_is_reported_once() {
    let (mut m, _dir) = monitor();
    let input = Gathered {
        registry: Some(vec![reg("a", RegistryStatus::Busy)]),
        registry_at: 100.0,
        ..Default::default()
    };
    let out = m.step(input, 100.5);
    assert!(out.changed);
    assert_eq!(m.views().len(), 1);
    assert_eq!(m.views()[0].state, SessionState::Working);
    assert_eq!(m.views()[0].display_name, "demo-repo-1");

    let again = Gathered {
        registry: Some(vec![reg("a", RegistryStatus::Busy)]),
        registry_at: 102.0,
        ..Default::default()
    };
    assert!(
        !m.step(again, 102.5).changed,
        "nothing new, nothing to emit"
    );
}

#[test]
fn a_missed_snapshot_keeps_the_rows() {
    let (mut m, _dir) = monitor();
    m.step(
        Gathered {
            registry: Some(vec![reg("a", RegistryStatus::Idle)]),
            registry_at: 100.0,
            ..Default::default()
        },
        100.0,
    );
    m.step(Gathered::default(), 102.0);
    assert_eq!(m.views().len(), 1);
}

#[test]
fn transcript_marks_come_from_the_event_path() {
    let (mut m, dir) = monitor();
    let path = dir.path().join("s1.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"user\",\"message\":\"hi\"}\n{\"type\":\"custom-title\",\"customTitle\":\"Deploy nightly\",\"sessionId\":\"s1\"}\n{\"type\":\"agent-color\",\"agentColor\":\"green\",\"sessionId\":\"s1\"}\n",
    )
    .unwrap();
    let mut start = ev("SessionStart", "s1", 10.0);
    start.transcript_path = Some(path.to_string_lossy().into_owned());
    m.step(
        Gathered {
            events: vec![start],
            ..Default::default()
        },
        10.0,
    );
    let v = &m.views()[0];
    assert_eq!(v.display_name, "Deploy nightly");
    assert_eq!(v.color.as_deref(), Some("green"));
}

fn session_live(id: &str, pct: Option<f64>, model: &str) -> SessionLive {
    SessionLive {
        session_id: id.into(),
        written_at: crate::now_secs(),
        context_percent: pct,
        context_window_size: Some(1_000_000),
        context_used_tokens: None,
        model_id: Some(model.into()),
        model_display_name: Some("Opus".into()),
        five_hour_pct: None,
        seven_day_pct: None,
        cost_usd: None,
    }
}

#[test]
fn each_session_gets_its_own_context_from_its_status_line_file() {
    let (mut m, _dir) = monitor();
    let mut start = ev("SessionStart", "a", 1.0);
    start.model = Some("claude-sonnet-5".into());
    m.step(
        Gathered {
            registry: Some(vec![
                reg("a", RegistryStatus::Idle),
                reg("b", RegistryStatus::Busy),
            ]),
            registry_at: 1.0,
            events: vec![start],
            live_sessions: vec![
                session_live("a", Some(41.6), "claude-opus-5-5"),
                session_live("b", Some(7.2), "claude-haiku-5"),
                session_live("not-tracked", Some(99.0), "x"),
            ],
            ..Default::default()
        },
        1.0,
    );
    let a = m.view("a").unwrap();
    assert_eq!(a.context_percent, Some(42));
    assert_eq!(
        a.model.as_deref(),
        Some("claude-opus-5-5"),
        "the status line's model is the current one"
    );
    assert_eq!(m.view("b").unwrap().context_percent, Some(7));
    assert!(m.view("not-tracked").is_none());

    // A null percentage (early, or right after /compact) keeps the last one.
    m.step(
        Gathered {
            registry: Some(vec![
                reg("a", RegistryStatus::Idle),
                reg("b", RegistryStatus::Busy),
            ]),
            registry_at: 3.0,
            live_sessions: vec![session_live("a", None, "claude-opus-5-5")],
            ..Default::default()
        },
        3.0,
    );
    assert_eq!(m.view("a").unwrap().context_percent, Some(42));
}

#[test]
fn status_line_files_of_ended_sessions_are_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live");
    std::fs::create_dir_all(&live).unwrap();
    for id in ["a", "b", "c", "d"] {
        std::fs::write(live.join(format!("{id}.json")), "{}").unwrap();
    }
    let reader = TranscriptReader::with_projects_dir(dir.path());
    let mut m = SessionMonitor::with_reader(reader).with_live_dir(live.clone());
    let regs = |ids: &[&str]| Some(ids.iter().map(|id| reg(id, RegistryStatus::Idle)).collect());
    m.step(
        Gathered {
            registry: regs(&["a", "b", "d"]),
            registry_at: 1.0,
            ..Default::default()
        },
        1.0,
    );
    assert!(live.join("a.json").exists() && live.join("b.json").exists());

    // "a" ends with a hook event, "b" drops out of the registry, "d" is dead.
    m.step(
        Gathered {
            registry: regs(&["c"]),
            registry_at: 3.0,
            events: vec![ev("SessionEnd", "a", 2.5)],
            dead: vec!["d".into()],
            ..Default::default()
        },
        3.0,
    );
    assert!(!live.join("a.json").exists());
    assert!(!live.join("d.json").exists());
    assert!(
        live.join("b.json").exists(),
        "still within the registry grace period"
    );
    let later = 3.0 + cc_core::state::REGISTRY_ABSENT_GRACE_SECS + 1.0;
    m.step(
        Gathered {
            registry: regs(&["c"]),
            registry_at: later,
            ..Default::default()
        },
        later,
    );
    assert!(m.view("b").is_none());
    assert!(!live.join("b.json").exists());
    assert!(live.join("c.json").exists());
}

#[test]
fn payloads_carry_the_row_but_never_the_path() {
    let (mut m, _dir) = monitor();
    let mut start = SpoolEvent::new("SessionStart", "s1", 10.0);
    start.cwd = Some("/work/secret-project".into());
    start.model = Some("claude-opus-5-5".into());
    let mut tool = SpoolEvent::new("PreToolUse", "s1", 11.0);
    tool.tool_name = Some("Bash".into());
    tool.tool_summary = Some("rm -rf /tmp/secret".into());
    m.step(
        Gathered {
            registry: Some(vec![reg("s1", RegistryStatus::Busy)]),
            registry_at: 10.0,
            events: vec![start, SpoolEvent::new("UserPromptSubmit", "s1", 10.5), tool],
            live_sessions: vec![SessionLive {
                model_display_name: None,
                ..session_live("s1", Some(33.4), "claude-opus-5-5")
            }],
            ..Default::default()
        },
        12.0,
    );
    let p = m.payloads("lab-01", 12.0);
    assert_eq!(p.len(), 1);
    let p = &p[0];
    assert_eq!(p.machine, "lab-01");
    assert_eq!(p.sid, cc_core::state::remote_sid("s1"));
    assert_eq!(p.state, SessionState::Working);
    assert_eq!(p.tool.as_deref(), Some("Bash"));
    assert_eq!(p.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(p.ctx, Some(33));
    assert_eq!(p.hb, 12);
    // The registry's name and folder name (its cwd, C:/work/demo-repo, wins
    // over the hook's), the tool summary as the second line; never the path.
    assert_eq!(p.title.as_deref(), Some("demo-repo-1"));
    assert_eq!(p.dir.as_deref(), Some("demo-repo"));
    assert_eq!(p.text.as_deref(), Some("rm -rf /tmp/secret"));
    let json = serde_json::to_string(p).unwrap();
    assert!(!json.contains("/work"), "no path: {json}");
}

#[test]
fn ended_sessions_leave_the_payloads_and_their_live_file() {
    let dir = tempfile::tempdir().unwrap();
    let live_dir = dir.path().join("live");
    std::fs::create_dir_all(&live_dir).unwrap();
    std::fs::write(live_dir.join("s1.json"), "{}").unwrap();
    let mut m = SessionMonitor::with_reader(TranscriptReader::with_projects_dir(dir.path()))
        .with_live_dir(live_dir.clone());
    m.step(
        Gathered {
            events: vec![SpoolEvent::new("SessionStart", "s1", 1.0)],
            ..Default::default()
        },
        1.0,
    );
    assert_eq!(m.payloads("a", 1.0).len(), 1);
    m.step(
        Gathered {
            events: vec![SpoolEvent::new("SessionEnd", "s1", 2.0)],
            ..Default::default()
        },
        2.0,
    );
    assert!(m.payloads("a", 2.0).is_empty());
    assert!(!live_dir.join("s1.json").exists());
    assert_eq!(m.passes(), 2);
}

// ---------------------------------------------------------------- whole passes over real folders

struct World {
    _tmp: tempfile::TempDir,
    paths: Paths,
    projects: PathBuf,
    clock: Arc<Mutex<f64>>,
    cli: Arc<Mutex<Result<Vec<RegistryEntry>, String>>>,
    cli_runs: Arc<Mutex<u32>>,
    alive: Arc<AtomicBool>,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            sessions_dir: tmp.path().join("sessions"),
            spool_dir: tmp.path().join("spool"),
            live_dir: tmp.path().join("live"),
        };
        for dir in [&paths.sessions_dir, &paths.spool_dir, &paths.live_dir] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let projects = tmp.path().join("projects");
        World {
            _tmp: tmp,
            paths,
            projects,
            // Real time: file modification times are compared with it.
            clock: Arc::new(Mutex::new(crate::now_secs())),
            cli: Arc::new(Mutex::new(Ok(Vec::new()))),
            cli_runs: Arc::new(Mutex::new(0)),
            alive: Arc::new(AtomicBool::new(true)),
        }
    }

    fn sources(&self, cadence: Cadence) -> Sources {
        let (clock, cli, runs, alive) = (
            Arc::clone(&self.clock),
            Arc::clone(&self.cli),
            Arc::clone(&self.cli_runs),
            Arc::clone(&self.alive),
        );
        Sources::new(self.paths.clone(), cadence)
            .with_clock(move || *clock.lock().unwrap())
            .with_agents(move || {
                *runs.lock().unwrap() += 1;
                cli.lock().unwrap().clone()
            })
            .with_probe(move |pid| Some(pid != A_PID || alive.load(Ordering::SeqCst)))
    }

    fn monitor(&self) -> SessionMonitor {
        SessionMonitor::with_reader(TranscriptReader::with_projects_dir(&self.projects))
            .with_live_dir(self.paths.live_dir.clone())
    }

    fn now(&self) -> f64 {
        *self.clock.lock().unwrap()
    }

    fn advance(&self, secs: f64) {
        *self.clock.lock().unwrap() += secs;
    }

    fn set_cli(&self, result: Result<Vec<RegistryEntry>, String>) {
        *self.cli.lock().unwrap() = result;
    }

    fn cli_runs(&self) -> u32 {
        *self.cli_runs.lock().unwrap()
    }

    fn write_registry(&self, id: &str, pid: u32, status: &str) {
        std::fs::write(
            self.paths.sessions_dir.join(format!("{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{id}","cwd":"C:/w/{id}","kind":"interactive","entrypoint":"cli","name":"{id}-1","status":"{status}"}}"#
            ),
        )
        .unwrap();
    }

    fn remove_registry(&self, pid: u32) {
        std::fs::remove_file(self.paths.sessions_dir.join(format!("{pid}.json"))).unwrap();
    }
}

fn pass(w: &World, sources: &mut Sources, m: &mut SessionMonitor) -> StepOutcome {
    let gathered = sources.gather();
    m.step(gathered, w.now())
}

#[test]
fn a_pass_reads_every_source_from_its_paths() {
    let w = World::new();
    w.write_registry("a", A_PID, "busy");
    w.set_cli(Ok(vec![reg_pid("a", A_PID)]));
    let now = w.now();
    std::fs::write(
        w.paths.spool_dir.join("000000000000001-x.json"),
        serde_json::to_string(&ev("SessionStart", "a", now - 1.0)).unwrap(),
    )
    .unwrap();
    std::fs::write(
        w.paths.live_dir.join("a.json"),
        format!(r#"{{"ts":{now},"sessionId":"a","contextUsedPercent":12.4}}"#),
    )
    .unwrap();

    let mut sources = w.sources(Cadence::STEADY);
    let g = sources.gather();
    assert_eq!(ids(&g.registry), ["a"]);
    assert_eq!(
        g.registry.as_ref().unwrap()[0].entrypoint.as_deref(),
        Some("cli")
    );
    assert_eq!(g.registry_at, now);
    assert_eq!(g.events.len(), 1);
    assert_eq!(
        std::fs::read_dir(&w.paths.spool_dir).unwrap().count(),
        0,
        "drained"
    );
    assert_eq!(g.live_sessions.len(), 1);
    assert!(g.agents_error.is_none());

    let mut m = w.monitor();
    m.step(g, now);
    let a = m.view("a").unwrap();
    assert_eq!(a.context_percent, Some(12));
    assert!(a.has_hooks);
}

#[test]
fn the_cli_runs_on_start_on_file_changes_and_every_refresh() {
    let w = World::new();
    let mut sources = w.sources(Cadence::STEADY);
    let mut m = w.monitor();
    pass(&w, &mut sources, &mut m);
    assert_eq!(w.cli_runs(), 1, "on start");
    for _ in 0..5 {
        w.advance(2.0);
        pass(&w, &mut sources, &mut m);
    }
    assert_eq!(w.cli_runs(), 1, "nothing changed");
    w.write_registry("a", 100, "idle");
    w.set_cli(Ok(vec![reg_pid("a", 100)]));
    w.advance(2.0);
    pass(&w, &mut sources, &mut m);
    assert_eq!(w.cli_runs(), 2, "a registry file appeared");
    w.advance(AGENTS_REFRESH.as_secs_f64());
    pass(&w, &mut sources, &mut m);
    assert_eq!(w.cli_runs(), 3, "periodic");
}

#[test]
fn a_session_that_dies_while_the_cli_fails_leaves_on_the_next_pass() {
    let w = World::new();
    w.write_registry("a", A_PID, "busy");
    w.write_registry("b", 100, "idle");
    w.set_cli(Ok(vec![reg_pid("a", A_PID), reg_pid("b", 100)]));
    let mut sources = w.sources(Cadence::STEADY);
    let mut m = w.monitor();
    pass(&w, &mut sources, &mut m);
    assert!(m.view("a").is_some() && m.view("b").is_some());

    // The process exits, its file goes, and the CLI starts failing.
    w.alive.store(false, Ordering::SeqCst);
    w.remove_registry(A_PID);
    w.set_cli(Err("claude did not answer within 5000 ms".into()));
    w.advance(2.0);
    let out = pass(&w, &mut sources, &mut m);
    assert!(out.changed);
    assert!(m.view("a").is_none(), "gone at once: {:?}", m.views());
    assert!(m.view("b").is_some());
    assert_eq!(sources.registry().failures(), 1);
}

#[test]
fn a_new_session_appears_on_the_pass_its_file_does() {
    let w = World::new();
    let mut sources = w.sources(Cadence::STEADY);
    let mut m = w.monitor();
    pass(&w, &mut sources, &mut m);
    assert!(m.views().is_empty());

    // The CLI lags: it does not list the new session yet.
    w.write_registry("new", 100, "busy");
    w.advance(2.0);
    pass(&w, &mut sources, &mut m);
    let v = m.view("new").expect("shown from its file at once");
    assert_eq!(v.state, SessionState::Working, "status from the file");
    assert_eq!(v.display_name, "new-1");
    let runs = w.cli_runs();

    // The CLI catches up on the confirmation run.
    w.set_cli(Ok(vec![reg_pid("new", 100)]));
    let mut t = 0.0;
    while t < NEW_SESSION_CONFIRM_SECS {
        w.advance(2.0);
        t += 2.0;
        pass(&w, &mut sources, &mut m);
    }
    assert_eq!(w.cli_runs(), runs + 1, "one confirmation run");
    assert!(m.view("new").is_some());
}

#[test]
fn clear_removes_every_row_and_only_live_sessions_come_back() {
    let w = World::new();
    w.write_registry("a", 100, "busy");
    w.set_cli(Ok(vec![reg_pid("a", 100)]));
    let now = w.now();
    // "a" is live and asks for permission; "b" only ever existed through
    // hook events (a finished headless run) and lingers.
    let mut ask = ev("Notification", "a", now - 1.0);
    ask.notification_type = Some("permission_prompt".into());
    ask.message = Some("Allow Bash?".into());
    for (i, e) in [
        ev("SessionStart", "a", now - 3.0),
        ev("SessionStart", "b", now - 2.0),
        ask,
    ]
    .iter()
    .enumerate()
    {
        std::fs::write(
            w.paths.spool_dir.join(format!("00000000000000{i}-x.json")),
            serde_json::to_string(e).unwrap(),
        )
        .unwrap();
    }
    for id in ["a", "b"] {
        std::fs::write(
            w.paths.live_dir.join(format!("{id}.json")),
            format!(r#"{{"ts":{now},"sessionId":"{id}","contextUsedPercent":12.4}}"#),
        )
        .unwrap();
    }
    let mut sources = w.sources(Cadence::STEADY);
    let mut m = w.monitor();
    pass(&w, &mut sources, &mut m);
    assert_eq!(m.views().len(), 2);
    assert_eq!(m.view("a").unwrap().state, SessionState::NeedsYou);

    assert_eq!(m.clear(w.now()), 2);
    assert!(
        m.views().is_empty(),
        "cleared at once, before the next pass"
    );

    w.advance(2.0);
    let out = pass(&w, &mut sources, &mut m);
    assert!(out.changed);
    let ids: Vec<_> = m.views().iter().map(|v| v.id.as_str()).collect();
    assert_eq!(
        ids,
        ["a"],
        "the live session is back, the lingering one is not"
    );
    let a = m.view("a").unwrap();
    assert_eq!(a.state, SessionState::NeedsYou, "still asking");
    assert_eq!(a.question.as_deref(), Some("Allow Bash?"));
    assert!(a.has_hooks);
    assert_eq!(a.context_percent, Some(12));
    assert!(
        !out.transitions
            .iter()
            .any(|t| t.to == SessionState::NeedsYou),
        "no second toast: {:?}",
        out.transitions
    );
    assert!(w.paths.live_dir.join("a.json").exists());
    assert!(
        !w.paths.live_dir.join("b.json").exists(),
        "the forgotten session's file goes"
    );

    // Nor on a later pass.
    w.advance(2.0);
    pass(&w, &mut sources, &mut m);
    assert!(m.view("b").is_none());
}
