//! DEMO FIXTURES. Only reachable through the `--demo` / `--demo-empty` flags
//! of the tray app; nothing in a normal run calls into this module.
//!
//! The five rows of the "The panel" mockup in the design document (names,
//! states, origin tags, context, the two subagents, the offline row) and its
//! footer usage, so a screenshot of the app can be compared like for like
//! with `docs/reference/flyout-mockup.html`.
//!
//! Remote rows do not exist yet outside this module: [`sessions_json`] adds
//! the fields the frontend reads for them (`origin`, a subagent's
//! `toolSummary`) to the serialized [`SessionView`]s, so `cc-core` does not
//! carry demo-only fields.
//!
//! Times are relative to `now`, so the elapsed counters in the flyout start
//! from the mockup's values and keep ticking.

use cc_core::session::SessionState;
use cc_core::state::{SessionView, SubagentState, SubagentView};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use crate::model::{Limit, LimitKind, Severity, UsageSnapshot, LABEL_SESSION, LABEL_WEEKLY};

/// A row with every optional field empty; the fixtures fill in what they need.
fn base(id: &str, name: &str, state: SessionState, since: f64, now: f64) -> SessionView {
    SessionView {
        id: id.into(),
        display_name: name.into(),
        state,
        state_since: since,
        cwd: None,
        pid: None,
        entrypoint: None,
        color: None,
        current_tool: None,
        tool_summary: None,
        tool_started_at: None,
        turn_started_at: None,
        stalled: false,
        subagents: Vec::new(),
        last_message: None,
        question: None,
        error_message: None,
        model: None,
        context_percent: None,
        context_estimated: false,
        context_tokens: None,
        has_hooks: true,
        last_activity: now,
    }
}

/// Where a demo row comes from: `None` is this machine, `Some(alias)` a
/// remote machine publishing through the KV bucket.
pub struct DemoRow {
    pub view: SessionView,
    pub remote: Option<&'static str>,
}

/// The mockup's rows, already in panel order (needs you, working, done,
/// offline).
pub fn sessions(now: f64) -> Vec<DemoRow> {
    let needs_you = SessionView {
        cwd: Some(r"C:\Users\dev\Projects\cc-usage".into()),
        pid: Some(41_200),
        entrypoint: Some("claude-vscode".into()),
        color: Some("blue".into()),
        question: Some("Edit release-notes.sh?".into()),
        turn_started_at: Some(now - 312.0),
        model: Some("claude-opus-5-5".into()),
        context_percent: Some(42),
        subagents: vec![
            SubagentView {
                id: "demo-agent-explore".into(),
                agent_type: Some("explore".into()),
                description: None,
                state: SubagentState::Working,
                current_tool: Some("Grep".into()),
                tool_summary: Some("Capabilities".into()),
                started_at: now - 3.0,
                finished_at: None,
            },
            SubagentView {
                id: "demo-agent-plan".into(),
                agent_type: Some("plan".into()),
                description: None,
                state: SubagentState::Done,
                current_tool: None,
                tool_summary: None,
                started_at: now - 190.0,
                finished_at: Some(now - 128.0),
            },
        ],
        ..base(
            "demo-needs-you",
            "Wire Codex sessions into the panel",
            SessionState::NeedsYou,
            now - 12.0,
            now,
        )
    };

    // Remote rows carry no pid, cwd or subagents: those do not travel.
    let wsl = SessionView {
        current_tool: Some("Bash".into()),
        model: Some("claude-sonnet-5-5".into()),
        context_percent: Some(17),
        last_activity: now - 48.0,
        ..base(
            "demo-remote-wsl",
            "Port the formula to the new tap",
            SessionState::Working,
            now - 300.0,
            now,
        )
    };

    let lab = SessionView {
        color: Some("green".into()),
        current_tool: Some("Edit".into()),
        model: Some("claude-opus-5-5".into()),
        context_percent: Some(63),
        last_activity: now - 20.0,
        ..base(
            "demo-remote-lab",
            "deploy-nightly",
            SessionState::Working,
            now - 900.0,
            now,
        )
    };

    let done = SessionView {
        cwd: Some(r"C:\Users\dev\Projects\release-tools".into()),
        pid: Some(41_777),
        entrypoint: Some("cli".into()),
        last_message: Some("Done. The script is in place and the tests pass.".into()),
        model: Some("claude-sonnet-5-5".into()),
        context_percent: Some(9),
        ..base(
            "demo-done",
            "Rebuild the release notes script",
            SessionState::Done,
            now - 64.0,
            now,
        )
    };

    // A remote machine whose heartbeat expired: only what it last published.
    let offline = SessionView {
        context_percent: Some(31),
        last_activity: now - 290.0,
        ..base(
            "demo-remote-offline",
            "3f9a1c2e",
            SessionState::Offline,
            now - 240.0,
            now,
        )
    };

    vec![
        DemoRow {
            view: needs_you,
            remote: None,
        },
        DemoRow {
            view: wsl,
            remote: Some("wsl-ubuntu"),
        },
        DemoRow {
            view: lab,
            remote: Some("lab-01"),
        },
        DemoRow {
            view: done,
            remote: None,
        },
        DemoRow {
            view: offline,
            remote: Some("home-ubuntu"),
        },
    ]
}

/// The demo rows as the frontend receives them: each [`SessionView`] plus
/// `origin` (`{"kind":"local"}` or `{"kind":"remote","alias":...}`) and the
/// explore subagent's `toolSummary`, the mockup's "explore · Grep · Capabilities".
pub fn sessions_json(now: f64) -> Vec<Value> {
    sessions(now)
        .into_iter()
        .map(|row| {
            let mut v = serde_json::to_value(&row.view).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut v {
                let origin = match row.remote {
                    Some(alias) => json!({ "kind": "remote", "alias": alias }),
                    None => json!({ "kind": "local" }),
                };
                map.insert("origin".into(), origin);
                if let Some(Value::Array(agents)) = map.get_mut("subagents") {
                    for a in agents.iter_mut() {
                        if a["id"] == "demo-agent-explore" {
                            a["toolSummary"] = json!("Capabilities");
                        }
                    }
                }
            }
            v
        })
        .collect()
}

/// The mockup's plan usage, used for the footer in demo mode, plus the
/// per-model weekly line a Team account gets (Fable).
///
/// The reset times carry 50 extra seconds so the minute-granular countdown
/// reads exactly "2h24m" and "4d23h" when the flyout paints.
pub fn usage(now: DateTime<Utc>) -> UsageSnapshot {
    let limit = |kind, label: &str, percent, resets_in: Duration, active| Limit {
        kind,
        label: label.into(),
        percent,
        severity: Severity::Normal,
        resets_at: Some(now + resets_in + Duration::seconds(50)),
        is_active: active,
    };
    UsageSnapshot {
        limits: vec![
            limit(
                LimitKind::Session,
                LABEL_SESSION,
                61.0,
                Duration::minutes(144),
                true,
            ),
            limit(
                LimitKind::WeeklyAll,
                LABEL_WEEKLY,
                34.0,
                Duration::hours(4 * 24 + 23),
                false,
            ),
            limit(
                LimitKind::WeeklyScoped,
                "Fable · weekly",
                12.0,
                Duration::hours(4 * 24 + 23),
                false,
            ),
        ],
        spend: None,
        fetched_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_reproduces_the_mockup_rows_in_panel_order() {
        let list = sessions(1_000_000.0);
        let names: Vec<_> = list.iter().map(|r| r.view.display_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Wire Codex sessions into the panel",
                "Port the formula to the new tap",
                "deploy-nightly",
                "Rebuild the release notes script",
                "3f9a1c2e",
            ]
        );
        let states: Vec<_> = list.iter().map(|r| r.view.state).collect();
        assert_eq!(
            states,
            [
                SessionState::NeedsYou,
                SessionState::Working,
                SessionState::Working,
                SessionState::Done,
                SessionState::Offline,
            ]
        );
        let ranks: Vec<_> = states.iter().map(|s| s.sort_rank()).collect();
        assert!(ranks.windows(2).all(|w| w[0] <= w[1]), "not in panel order");
        let agents = &list[0].view.subagents;
        assert_eq!(agents.len(), 2);
        assert!(agents.iter().any(|a| a.state == SubagentState::Done));
        assert!(agents.iter().any(|a| a.state == SubagentState::Working));
        // Remote rows cannot be clicked: no pid.
        assert!(list
            .iter()
            .filter(|r| r.remote.is_some())
            .all(|r| r.view.pid.is_none()));
    }

    #[test]
    fn demo_json_carries_origin_and_the_subagent_summary() {
        let rows = sessions_json(1_000_000.0);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["origin"]["kind"], "local");
        assert_eq!(rows[1]["origin"]["alias"], "wsl-ubuntu");
        assert_eq!(rows[2]["origin"]["alias"], "lab-01");
        assert_eq!(rows[4]["origin"]["alias"], "home-ubuntu");
        assert_eq!(rows[4]["state"], "offline");
        assert_eq!(rows[0]["contextPercent"], 42);
        assert_eq!(rows[0]["subagents"][0]["toolSummary"], "Capabilities");
        // Ids are unique, or the frontend would mix up focus hints.
        let mut ids: Vec<_> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), rows.len());
    }

    #[test]
    fn demo_usage_matches_the_mockup_footer() {
        let u = usage(Utc::now());
        assert_eq!(u.session().unwrap().percent, 61.0);
        assert_eq!(u.weekly().unwrap().percent, 34.0);
        let json = serde_json::to_value(&u).unwrap();
        assert_eq!(json["limits"][0]["label"], "5-hour session");
        assert_eq!(json["limits"][1]["label"], "All models · weekly");
        assert_eq!(json["limits"][2]["label"], "Fable · weekly");
        assert!(json["limits"][0]["resets_at"].is_string());
    }
}
