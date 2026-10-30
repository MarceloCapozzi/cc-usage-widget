//! Local Claude Code sessions for the widget.
//!
//! The pass itself is shared with `cc-publish` and lives in
//! [`cc_local::monitor`]: [`Sources`] does the I/O (registry files every
//! pass, `claude agents --json` on start, every 30 s and when
//! `~/.claude/sessions` changes, the spool, the status line files) and
//! [`SessionMonitor`] feeds the `cc-core` state machine. This module only
//! configures them for the widget ([`sources`]: the steady CLI cadence) and
//! adds what only the widget needs:
//! toasts and the tray tooltip.
//!
//! Nothing here knows about Tauri; `poller.rs` runs a pass every
//! [`POLL_INTERVAL`] and publishes the result.

use cc_core::session::SessionState;
use cc_core::state::{SessionView, Transition};

pub use cc_local::monitor::{
    Cadence, Gathered, Paths, RegistrySource, SessionMonitor, Sources, StepOutcome,
    AGENTS_FAILURES_BEFORE_FALLBACK, AGENTS_REFRESH, LIVE_PRUNE_EVERY, POLL_INTERVAL,
};
pub use cc_local::now_secs;

/// The widget's sources: the real folders, and the CLI every
/// [`AGENTS_REFRESH`] whether or not any session is open.
pub fn sources() -> Sources {
    Sources::new(Paths::real(), Cadence::STEADY)
}

/// The ⚙ view's "Hide idle sessions after": a row that has been idle for
/// longer than the delay is hidden, local or remote. Idle sessions are
/// alive, so this is a view filter, not a removal: the row shows again as
/// soon as its state changes (it starts working, needs you, finishes), since
/// that resets the time it has been idle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct IdleFilter {
    /// Seconds of idle before a row is hidden; `None` never hides.
    pub after_secs: Option<f64>,
}

impl IdleFilter {
    /// Hides rows idle for longer than `minutes`; 0 never hides.
    pub fn after_minutes(minutes: u32) -> Self {
        Self {
            after_secs: (minutes > 0).then(|| f64::from(minutes) * 60.0),
        }
    }

    /// Whether `view` is hidden at `now` (Unix seconds).
    pub fn hides(&self, view: &SessionView, now: f64) -> bool {
        view.state == SessionState::Idle
            && self
                .after_secs
                .is_some_and(|after| now - view.state_since > after)
    }
}

/// Sessions that are waiting for the user.
pub fn needs_you_count(views: &[SessionView]) -> usize {
    views
        .iter()
        .filter(|v| v.state == SessionState::NeedsYou)
        .count()
}

/// The toast for a session that moved to *needs you*, as `(title, body)`.
/// User-facing.
pub fn needs_you_toast(view: &SessionView) -> (String, String) {
    let title = format!("{} needs you", view.display_name);
    let body = view
        .question
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .unwrap_or("Claude Code is waiting for your answer")
        .to_string();
    (title, body)
}

/// Transitions that deserve a toast: a move into *needs you* from another
/// state. Nothing toasts on the first pass, so the sessions already waiting
/// when the widget starts do not all fire at once. That covers creation
/// (`from == None`) and also hooked sessions: the first pass replays the
/// spool backlog, whose old `Notification` moves a just-created row from
/// working or idle into *needs you*.
pub fn toast_worthy(transitions: &[Transition], first_pass: bool) -> Vec<&Transition> {
    if first_pass {
        return Vec::new();
    }
    transitions
        .iter()
        .filter(|t| t.to == SessionState::NeedsYou)
        .filter(|t| t.from != Some(SessionState::NeedsYou))
        .collect()
}

/// One line for the tray tooltip, `None` when there are no sessions.
/// User-facing, the flyout header's wording.
pub fn tooltip_line(views: &[SessionView]) -> Option<String> {
    if views.is_empty() {
        return None;
    }
    let n = views.len();
    let mut line = format!("{n} {}", if n == 1 { "session" } else { "sessions" });
    match needs_you_count(views) {
        0 => {}
        1 => line.push_str(" · 1 needs you"),
        k => line.push_str(&format!(" · {k} need you")),
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::events::SpoolEvent;
    use cc_core::session::{RegistryEntry, RegistryStatus};
    use cc_local::transcript::TranscriptReader;

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

    fn monitor() -> (SessionMonitor, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let reader = TranscriptReader::with_projects_dir(dir.path());
        (SessionMonitor::with_reader(reader), dir)
    }

    fn ev(event: &str, id: &str, ts: f64) -> SpoolEvent {
        SpoolEvent::new(event, id, ts)
    }

    #[test]
    fn the_widget_reads_the_real_folders_with_the_steady_cadence() {
        let s = sources();
        assert_eq!(s.registry().cadence(), Cadence::STEADY);
        assert_eq!(*s.paths(), Paths::real());
    }

    #[test]
    fn hook_events_drive_needs_you_and_the_toast() {
        let (mut m, _dir) = monitor();
        let mut start = ev("SessionStart", "s1", 10.0);
        start.cwd = Some("C:/work/hooked".into());
        let mut ask = ev("Notification", "s1", 11.0);
        ask.notification_type = Some("permission_prompt".into());
        ask.message = Some("Claude needs your permission to use Bash".into());
        m.step(
            Gathered {
                events: vec![start, ev("UserPromptSubmit", "s1", 10.5), ask],
                ..Default::default()
            },
            12.0,
        );
        let out_views = m.views().to_vec();
        assert_eq!(out_views[0].state, SessionState::NeedsYou);
        assert!(out_views[0].has_hooks);
        let (title, body) = needs_you_toast(&out_views[0]);
        assert_eq!(title, "hooked needs you");
        assert_eq!(body, "Claude needs your permission to use Bash");
        assert_eq!(needs_you_count(&out_views), 1);
        assert_eq!(
            tooltip_line(&out_views).as_deref(),
            Some("1 session · 1 needs you")
        );
    }

    #[test]
    fn the_idle_filter_hides_only_rows_idle_for_longer_than_the_delay() {
        // An injected clock: the monitor runs at `now`, the filter is asked at `now`.
        let (mut m, _dir) = monitor();
        let t0 = 1_800_000_000.0;
        m.step(
            Gathered {
                registry: Some(vec![reg("a", RegistryStatus::Idle), {
                    let mut b = reg("b", RegistryStatus::Busy);
                    b.pid = Some(4243);
                    b
                }]),
                registry_at: t0,
                ..Default::default()
            },
            t0,
        );
        let filter = IdleFilter::after_minutes(15);
        let view = |m: &SessionMonitor, id: &str| m.view(id).unwrap().clone();
        let at = |secs: f64| t0 + secs;
        assert!(
            !filter.hides(&view(&m, "a"), at(900.0)),
            "exactly 15 minutes: shown"
        );
        assert!(
            filter.hides(&view(&m, "a"), at(901.0)),
            "idle past 15 minutes: hidden"
        );
        assert!(
            !filter.hides(&view(&m, "b"), at(5000.0)),
            "working is never hidden"
        );
        assert!(
            !IdleFilter::after_minutes(0).hides(&view(&m, "a"), at(1e6)),
            "Never"
        );
        assert!(IdleFilter::default().after_secs.is_none());

        // It starts working, then goes idle again: shown, and the clock restarts.
        m.step(
            Gathered {
                registry: Some(vec![reg("a", RegistryStatus::Busy)]),
                registry_at: at(1000.0),
                ..Default::default()
            },
            at(1000.0),
        );
        assert!(!filter.hides(&view(&m, "a"), at(1000.0)));
        m.step(
            Gathered {
                registry: Some(vec![reg("a", RegistryStatus::Idle)]),
                registry_at: at(1010.0),
                ..Default::default()
            },
            at(1010.0),
        );
        assert!(!filter.hides(&view(&m, "a"), at(1900.0)));
        assert!(filter.hides(&view(&m, "a"), at(1911.0)));
    }

    #[test]
    fn toasts_skip_sessions_already_waiting_at_startup() {
        let created = Transition {
            session_id: "a".into(),
            from: None,
            to: SessionState::NeedsYou,
            at: 1.0,
        };
        let moved = Transition {
            session_id: "b".into(),
            from: Some(SessionState::Working),
            to: SessionState::NeedsYou,
            at: 1.0,
        };
        let done = Transition {
            session_id: "c".into(),
            from: Some(SessionState::Working),
            to: SessionState::Done,
            at: 1.0,
        };
        let all = [created.clone(), moved.clone(), done];
        assert!(toast_worthy(&all, true).is_empty());
        let later: Vec<_> = toast_worthy(&all, false).into_iter().cloned().collect();
        assert_eq!(later, vec![created, moved]);
    }

    #[test]
    fn a_question_in_the_startup_backlog_does_not_toast() {
        // The widget starts while a hooked session is waiting: the registry
        // lists it as busy and the spool still holds its old Notification.
        let (mut m, _dir) = monitor();
        let mut ask = ev("Notification", "a", 50.0);
        ask.notification_type = Some("permission_prompt".into());
        ask.message = Some("Claude needs your permission to use Bash".into());
        let out = m.step(
            Gathered {
                registry: Some(vec![reg("a", RegistryStatus::Busy)]),
                registry_at: 100.0,
                events: vec![ev("UserPromptSubmit", "a", 40.0), ask],
                ..Default::default()
            },
            100.0,
        );
        assert_eq!(m.views()[0].state, SessionState::NeedsYou);
        assert!(
            toast_worthy(&out.transitions, true).is_empty(),
            "already waiting at startup: {:?}",
            out.transitions
        );

        // A question asked after startup still toasts.
        let mut answered = ev("PreToolUse", "a", 101.0);
        answered.tool_name = Some("Bash".into());
        m.step(
            Gathered {
                events: vec![answered],
                ..Default::default()
            },
            102.0,
        );
        let mut again = ev("Notification", "a", 103.0);
        again.notification_type = Some("permission_prompt".into());
        let out = m.step(
            Gathered {
                events: vec![again],
                ..Default::default()
            },
            104.0,
        );
        assert_eq!(toast_worthy(&out.transitions, false).len(), 1);
    }
}
