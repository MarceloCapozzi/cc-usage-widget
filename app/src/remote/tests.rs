//! Tests for the viewer side: the map's rules, the rows and their order,
//! the hub's status, the setup, the pins, the keys waiting for review and
//! the projects file.

use std::time::{Duration, Instant};

use cc_core::projects::{ActivityPayload, ProjectPayload};
use cc_core::session::{RemotePayload, SessionState};
use cc_core::state::SessionView;
use cc_envelope::{
    host_key, session_key, EnvelopeError, HostPayload, OpenKeys, Opened, Sealer, SecretKeys,
    ViewerKeys,
};
use cc_remote::KvChange;
use serde::Serialize;
use serde_json::{json, Value};

use super::hub::lock;
use super::task::retry_delay;
use super::*;
use crate::sessions::IdleFilter;

const T0: i64 = 1_800_000_000;

/// A publisher sealing to a viewer, and that viewer's keys (pins in
/// memory): what most tests seal and open with.
struct Ring {
    sealer: Sealer,
    keys: ViewerKeys,
}

fn ring() -> Ring {
    let viewer = SecretKeys::generate("k1").unwrap();
    Ring {
        sealer: Sealer::new(SecretKeys::generate("p1").unwrap(), viewer.public_keys()),
        keys: ViewerKeys::new(viewer, None),
    }
}

impl Ring {
    fn seal<T: Serialize>(&self, value: &T, key: &str) -> Vec<u8> {
        self.sealer.seal(value, key).unwrap()
    }

    /// The same publisher sealing to another viewer key, `kid`: what it
    /// seals does not open with this ring's viewer keys.
    fn other_viewer(&self, kid: &str) -> Ring {
        let viewer = SecretKeys::generate(kid).unwrap();
        Ring {
            sealer: Sealer::new(self.sealer.sender.clone(), viewer.public_keys()),
            keys: ViewerKeys::new(viewer, None),
        }
    }
}

impl OpenKeys for Ring {
    fn open_any<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
        kv_key: &str,
    ) -> Result<Opened<T>, EnvelopeError> {
        self.keys.open_any(bytes, kv_key)
    }
}

fn payload(alias: &str, sid: &str, state: SessionState, since: i64, hb: i64) -> RemotePayload {
    RemotePayload {
        machine: alias.into(),
        sid: sid.into(),
        alias: None,
        title: None,
        dir: None,
        text: None,
        agents: None,
        ags: None,
        state,
        since,
        tool: Some("Bash".into()),
        model: Some("claude-opus-5-5".into()),
        ctx: Some(42),
        hb,
    }
}

fn put_session(ring: &Ring, p: &RemotePayload) -> KvChange {
    let key = session_key(&p.machine, &p.sid);
    KvChange::Put {
        bytes: ring.seal(p, &key),
        key,
        revision: 1,
    }
}

fn put_host(ring: &Ring, alias: &str, hb: i64) -> KvChange {
    let key = host_key(alias);
    let host = HostPayload {
        machine: alias.into(),
        version: "0.1.0".into(),
        os: "linux".into(),
        hb,
    };
    KvChange::Put {
        bytes: ring.seal(&host, &key),
        key,
        revision: 1,
    }
}

#[test]
fn a_put_adds_a_row_and_an_update_replaces_it() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let p = payload("lab-01", "3f9a1c2e", SessionState::Working, T0 - 30, T0);
    assert!(m.apply(&put_session(&ring, &p), &ring, T0, true));
    let rows = m.rows(T0 + 5);
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.alias, "lab-01");
    assert_eq!(r.view.id, "remote:lab-01.s.3f9a1c2e");
    assert_eq!(r.view.display_name, "3f9a1c2e", "no session alias: the sid");
    assert_eq!(r.view.state, SessionState::Working);
    assert_eq!(r.view.current_tool.as_deref(), Some("Bash"));
    assert_eq!(r.view.context_percent, Some(42));
    assert!(r.view.pid.is_none() && r.view.subagents.is_empty());
    assert_eq!(r.view.state_since, (T0 - 30) as f64);
    assert_eq!(r.view.last_activity, T0 as f64);

    let mut done = payload("lab-01", "3f9a1c2e", SessionState::Done, T0 + 10, T0 + 10);
    done.tool = None;
    done.alias = Some("deploy-nightly".into());
    assert!(m.apply(&put_session(&ring, &done), &ring, T0 + 10, true));
    let rows = m.rows(T0 + 11);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].view.state, SessionState::Done);
    assert_eq!(rows[0].view.display_name, "deploy-nightly");
    assert!(rows[0].view.current_tool.is_none());
}

#[test]
fn extreme_timestamps_from_a_publisher_do_not_overflow() {
    let ring = ring();
    let mut m = RemoteMap::new();
    for (since, hb) in [
        (i64::MAX, i64::MIN),
        (i64::MIN, i64::MAX),
        (i64::MIN, i64::MIN),
    ] {
        let p = payload("lab-01", "3f9a1c2e", SessionState::Working, since, hb);
        m.apply(&put_session(&ring, &p), &ring, T0, false);
        let rows = m.rows(T0 + 1);
        assert!(rows.len() <= 1);
        m.prune(T0 + 2);
    }
}

#[test]
fn the_title_folder_and_text_fill_the_row_where_the_state_shows_them() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let mut p = payload("devbox", "47c6aacf", SessionState::NeedsYou, T0 - 5, T0);
    p.title = Some("Port the formula".into());
    p.dir = Some("tap/../homebrew-tap".into());
    p.text = Some(format!("Edit release-notes.sh?\n{}", "x".repeat(500)));
    assert!(m.apply(&put_session(&ring, &p), &ring, T0, true));
    let v = &m.rows(T0 + 1)[0].view;
    assert_eq!(v.display_name, "Port the formula");
    assert_eq!(
        v.cwd.as_deref(),
        Some("homebrew-tap"),
        "a folder name, never a path"
    );
    let q = v.question.as_deref().unwrap();
    assert!(q.starts_with("Edit release-notes.sh? x"), "flattened: {q}");
    assert_eq!(
        q.chars().count(),
        cc_core::session::MAX_REMOTE_TEXT + 1,
        "cut again here"
    );
    assert!(v.last_message.is_none() && v.error_message.is_none() && v.tool_summary.is_none());

    // Working: the text is the tool summary; idle: the last message.
    p.state = SessionState::Working;
    p.text = Some("cargo test".into());
    p.hb = T0 + 2;
    assert!(m.apply(&put_session(&ring, &p), &ring, T0 + 2, true));
    let v = &m.rows(T0 + 3)[0].view;
    assert_eq!(v.tool_summary.as_deref(), Some("cargo test"));
    assert!(v.question.is_none());
    p.state = SessionState::Idle;
    p.tool = None;
    p.text = Some("All tests pass.".into());
    p.hb = T0 + 4;
    assert!(m.apply(&put_session(&ring, &p), &ring, T0 + 4, true));
    let v = &m.rows(T0 + 5)[0].view;
    assert_eq!(v.last_message.as_deref(), Some("All tests pass."));
    assert!(v.tool_summary.is_none());

    // Agent counts become placeholder rows for the tag, capped.
    p.agents = Some([2, 5]);
    p.hb = T0 + 5;
    assert!(m.apply(&put_session(&ring, &p), &ring, T0 + 5, true));
    let agents = &m.rows(T0 + 5)[0].view.subagents;
    assert_eq!(agents.len(), 5);
    assert_eq!(
        agents
            .iter()
            .filter(|a| a.state == cc_core::state::SubagentState::Working)
            .count(),
        2
    );
    p.agents = Some([900, 3]);
    p.hb = T0 + 5;
    assert!(m.apply(&put_session(&ring, &p), &ring, T0 + 5, true));
    let agents = &m.rows(T0 + 5)[0].view.subagents;
    assert_eq!(agents.len(), 3, "running never exceeds total");
    assert!(agents
        .iter()
        .all(|a| a.state == cc_core::state::SubagentState::Working));

    // Described subagents come first, cut again; placeholders fill the
    // rest of the counts, running ones before finished ones.
    use cc_core::session::{RemoteAgent, MAX_REMOTE_AGENT_ROWS, MAX_REMOTE_AGENT_TASK};
    let line = |t: &str, d: &str, tool: Option<&str>, run: bool| RemoteAgent {
        t: Some(t.into()),
        d: Some(d.into()),
        tool: tool.map(Into::into),
        s: tool.map(|_| "lib.rs".into()),
        run,
    };
    p.agents = Some([2, 5]);
    p.ags = Some(vec![
        line("Plan", "Plan\nthe fix", Some("Bash"), false),
        line("Explore", &"x".repeat(300), Some("Read"), true),
    ]);
    p.hb = T0 + 5;
    assert!(m.apply(&put_session(&ring, &p), &ring, T0 + 5, true));
    let agents = &m.rows(T0 + 5)[0].view.subagents;
    let summary: Vec<(Option<&str>, bool)> = agents
        .iter()
        .map(|a| {
            (
                a.agent_type.as_deref(),
                a.state == cc_core::state::SubagentState::Working,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (Some("Explore"), true),
            (None, true),
            (Some("Plan"), false),
            (None, false),
            (None, false)
        ]
    );
    assert_eq!(
        agents[0].description.as_ref().map(|d| d.chars().count()),
        Some(MAX_REMOTE_AGENT_TASK + 1)
    );
    assert_eq!(
        (
            agents[0].current_tool.as_deref(),
            agents[0].tool_summary.as_deref()
        ),
        (Some("Read"), Some("lib.rs"))
    );
    assert_eq!(agents[2].description.as_deref(), Some("Plan the fix"));
    assert!(
        agents[2].current_tool.is_none() && agents[2].tool_summary.is_none(),
        "a finished one runs nothing"
    );
    // More lines than the viewer takes, and than the counts say: the lines
    // still show, at most MAX_REMOTE_AGENT_ROWS of them.
    p.agents = Some([1, 1]);
    p.ags = Some(
        (0..10)
            .map(|i| line("Explore", &format!("task {i}"), None, true))
            .collect(),
    );
    p.hb = T0 + 5;
    assert!(m.apply(&put_session(&ring, &p), &ring, T0 + 5, true));
    let agents = &m.rows(T0 + 5)[0].view.subagents;
    assert_eq!(agents.len(), MAX_REMOTE_AGENT_ROWS);
    assert_eq!(agents[5].description.as_deref(), Some("task 5"));
    p.agents = None;
    p.ags = None;

    // No title: the folder name; neither: the sid.
    p.title = None;
    p.hb = T0 + 6;
    assert!(m.apply(&put_session(&ring, &p), &ring, T0 + 6, true));
    assert_eq!(m.rows(T0 + 7)[0].view.display_name, "homebrew-tap");
    p.dir = None;
    p.hb = T0 + 8;
    assert!(m.apply(&put_session(&ring, &p), &ring, T0 + 8, true));
    assert_eq!(m.rows(T0 + 9)[0].view.display_name, "47c6aacf");
}

#[test]
fn a_delete_removes_the_row_at_once() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let p = payload("lab-01", "3f9a1c2e", SessionState::Idle, T0, T0);
    m.apply(&put_session(&ring, &p), &ring, T0, true);
    let del = KvChange::Delete {
        key: "lab-01.s.3f9a1c2e".into(),
    };
    assert!(m.apply(&del, &ring, T0 + 1, true));
    assert!(m.rows(T0 + 1).is_empty());
    assert!(
        !m.apply(&del, &ring, T0 + 2, true),
        "deleting twice changes nothing"
    );
    // A delete marker in the snapshot for a key never seen is harmless.
    let unseen = KvChange::Delete {
        key: "other.s.0000aaaa".into(),
    };
    assert!(!m.apply(&unseen, &ring, T0, false));
}

#[test]
fn a_silent_row_goes_offline_after_150_s_and_is_dropped_after_10_min() {
    let ring = ring();
    const DROP_AFTER_SECS: i64 = 600;
    let mut m = RemoteMap::new();
    m.set_drop_after(Some(DROP_AFTER_SECS));
    let p = payload("wsl", "0badf00d", SessionState::Working, T0 - 60, T0);
    m.apply(&put_session(&ring, &p), &ring, T0, true);
    assert!(m.tick(T0), "first tick records the shape");
    assert!(
        !m.tick(T0 + OFFLINE_AFTER_SECS),
        "exactly 150 s is still fresh"
    );
    assert_eq!(
        m.rows(T0 + OFFLINE_AFTER_SECS)[0].view.state,
        SessionState::Working
    );

    assert!(m.tick(T0 + OFFLINE_AFTER_SECS + 1), "went offline");
    let row = &m.rows(T0 + OFFLINE_AFTER_SECS + 1)[0];
    assert_eq!(row.view.state, SessionState::Offline);
    assert_eq!(
        row.view.state_since, T0 as f64,
        "counted from the last heartbeat"
    );
    assert_eq!(row.view.last_activity, T0 as f64, "no heartbeat since then");
    assert!(row.view.current_tool.is_none());
    assert_eq!(
        row.view.context_percent,
        Some(42),
        "the last known context stays"
    );
    assert!(!m.tick(T0 + 300), "nothing new");

    assert!(!m.tick(T0 + DROP_AFTER_SECS));
    assert_eq!(m.len(), 1);
    assert!(m.tick(T0 + DROP_AFTER_SECS + 1), "dropped");
    assert!(m.is_empty());
}

#[test]
fn a_heartbeat_brings_an_offline_row_back() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let p = payload("wsl", "0badf00d", SessionState::Idle, T0, T0);
    m.apply(&put_session(&ring, &p), &ring, T0, true);
    let later = T0 + 200;
    assert_eq!(m.rows(later)[0].view.state, SessionState::Offline);
    let beat = payload("wsl", "0badf00d", SessionState::Idle, T0, later);
    m.apply(&put_session(&ring, &beat), &ring, later, true);
    assert_eq!(m.rows(later)[0].view.state, SessionState::Idle);
}

#[test]
fn snapshot_values_age_from_their_heartbeat_and_live_ones_from_arrival() {
    let ring = ring();
    let mut m = RemoteMap::new();
    // Written 100 s before the viewer connected.
    let old = payload("a", "0000aaaa", SessionState::Working, T0 - 200, T0 - 100);
    m.apply(&put_session(&ring, &old), &ring, T0, false);
    assert_eq!(m.rows(T0 + 51)[0].view.state, SessionState::Offline);

    // A publisher whose clock runs 10 minutes ahead: in the snapshot its
    // hb is capped at the local time, live writes count from arrival.
    let ahead = payload("b", "0000bbbb", SessionState::Working, T0, T0 + 600);
    m.apply(&put_session(&ring, &ahead), &ring, T0, false);
    let row = |m: &RemoteMap, now| {
        m.rows(now)
            .into_iter()
            .find(|r| r.alias == "b")
            .map(|r| r.view.state)
    };
    assert_eq!(row(&m, T0 + 151), Some(SessionState::Offline));
    m.apply(&put_session(&ring, &ahead), &ring, T0 + 200, true);
    assert_eq!(row(&m, T0 + 300), Some(SessionState::Working));
    assert_eq!(row(&m, T0 + 351), Some(SessionState::Offline));
}

#[test]
fn undecryptable_values_are_counted_and_never_shown() {
    let ring = ring();
    let other = ring.other_viewer("k1");
    let mut m = RemoteMap::new();
    let p = payload("lab-01", "3f9a1c2e", SessionState::Working, T0, T0);
    // Sealed with another key under the same kid.
    assert!(!m.apply(&put_session(&other, &p), &ring, T0, true));
    // Sealed with an unknown kid.
    let rotated = ring.other_viewer("k2");
    assert!(!m.apply(&put_session(&rotated, &p), &ring, T0, true));
    // Not an envelope at all.
    let junk = KvChange::Put {
        key: "lab-01.s.3f9a1c2e".into(),
        bytes: b"{not json".to_vec(),
        revision: 3,
    };
    assert!(!m.apply(&junk, &ring, T0, true));
    // A valid value moved to another key fails authentication.
    let moved = match put_session(&ring, &p) {
        KvChange::Put { bytes, .. } => KvChange::Put {
            key: "lab-01.s.0000ffff".into(),
            bytes,
            revision: 4,
        },
        other => other,
    };
    assert!(!m.apply(&moved, &ring, T0, true));
    // A bad host record too.
    assert!(!m.apply(&put_host(&other, "lab-01", T0), &ring, T0, true));
    assert_eq!(m.undecryptable(), 5);
    assert!(m.rows(T0).is_empty());
    assert_eq!(m.machines(T0), 0);

    // A readable row whose key then holds an unreadable value goes away.
    assert!(m.apply(&put_session(&ring, &p), &ring, T0, true));
    assert!(m.apply(&put_session(&other, &p), &ring, T0 + 1, true));
    assert!(m.rows(T0 + 1).is_empty());
    assert_eq!(m.undecryptable(), 6);
}

#[test]
fn keys_outside_the_layout_are_ignored_without_counting() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let stray = KvChange::Put {
        key: "Lab.whatever".into(),
        bytes: vec![1, 2, 3],
        revision: 1,
    };
    assert!(!m.apply(&stray, &ring, T0, true));
    assert_eq!(m.undecryptable(), 0);
    assert!(m.rows(T0).is_empty());
}

#[test]
fn machines_count_fresh_hosts_and_sessions_once_per_alias() {
    let ring = ring();
    let mut m = RemoteMap::new();
    m.apply(&put_host(&ring, "lab-01", T0), &ring, T0, true);
    m.apply(
        &put_session(
            &ring,
            &payload("lab-01", "0000aaaa", SessionState::Idle, T0, T0),
        ),
        &ring,
        T0,
        true,
    );
    m.apply(
        &put_session(
            &ring,
            &payload("lab-01", "0000bbbb", SessionState::Idle, T0, T0),
        ),
        &ring,
        T0,
        true,
    );
    // A machine with only a host record, no session.
    m.apply(&put_host(&ring, "wsl", T0 + 100), &ring, T0 + 100, true);
    // A machine with only a session.
    m.apply(
        &put_session(
            &ring,
            &payload("home", "0000cccc", SessionState::Done, T0, T0 + 100),
        ),
        &ring,
        T0 + 100,
        true,
    );
    assert_eq!(m.machines(T0 + 100), 3);
    // lab-01 goes silent: only wsl and home stay fresh.
    assert_eq!(m.machines(T0 + 200), 2);
    // A host delete removes its presence.
    m.apply(
        &KvChange::Delete {
            key: "wsl.host".into(),
        },
        &ring,
        T0 + 200,
        true,
    );
    assert_eq!(m.machines(T0 + 200), 1);
    assert_eq!(m.machines(T0 + 300), 0);
}

#[test]
fn remote_rows_sort_by_rank_then_latest_state_change() {
    let ring = ring();
    let mut m = RemoteMap::new();
    for (sid, state, since) in [
        ("0000000a", SessionState::Working, T0 - 900),
        ("0000000b", SessionState::Working, T0 - 300),
        ("0000000c", SessionState::NeedsYou, T0 - 5),
        ("0000000d", SessionState::Idle, T0 - 1),
    ] {
        let p = payload("lab", sid, state, since, T0);
        m.apply(&put_session(&ring, &p), &ring, T0, true);
    }
    let order: Vec<_> = m
        .rows(T0)
        .into_iter()
        .map(|r| r.view.id.trim_start_matches("remote:lab.s.").to_string())
        .collect();
    assert_eq!(order, ["0000000c", "0000000b", "0000000a", "0000000d"]);
}

fn local(id: &str, state: SessionState) -> SessionView {
    let mut row = bare(id, state);
    row.pid = Some(4242);
    row
}

/// A bare local row; only the fields the merge looks at matter.
fn bare(id: &str, state: SessionState) -> SessionView {
    SessionView {
        id: id.into(),
        display_name: id.into(),
        state,
        state_since: T0 as f64,
        cwd: Some("C:/work/repo".into()),
        pid: None,
        entrypoint: Some("cli".into()),
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
        last_activity: T0 as f64,
    }
}

#[test]
fn merged_rows_follow_the_demo_order_and_shape() {
    // The demo fixture: needs you (local), two remote working rows, done
    // (local), one remote offline row.
    let ring = ring();
    let mut m = RemoteMap::new();
    let mut wsl = payload(
        "wsl-ubuntu",
        "0000aaaa",
        SessionState::Working,
        T0 - 300,
        T0 - 48,
    );
    wsl.alias = Some("Port the formula to the new tap".into());
    let mut lab = payload(
        "lab-01",
        "0000bbbb",
        SessionState::Working,
        T0 - 900,
        T0 - 20,
    );
    lab.alias = Some("deploy-nightly".into());
    lab.tool = Some("Edit".into());
    let mut off = payload(
        "home-ubuntu",
        "3f9a1c2e",
        SessionState::Working,
        T0 - 400,
        T0 - 290,
    );
    off.tool = None;
    off.model = None;
    off.ctx = Some(31);
    for p in [&wsl, &lab, &off] {
        m.apply(&put_session(&ring, p), &ring, T0, false);
    }
    let rows = merge_rows(
        &[
            local("needs", SessionState::NeedsYou),
            local("done", SessionState::Done),
        ],
        &m.rows(T0),
    );
    let names: Vec<_> = rows
        .iter()
        .map(|r| r["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "needs",
            "Port the formula to the new tap",
            "deploy-nightly",
            "done",
            "3f9a1c2e"
        ]
    );

    // The shape ui/scripts/sessions.js reads, same as demo::sessions_json.
    assert_eq!(rows[0]["origin"], json!({ "kind": "local" }));
    assert_eq!(rows[0]["origin"], origin_json(None));
    assert_eq!(
        rows[1]["origin"],
        json!({ "kind": "remote", "alias": "wsl-ubuntu" })
    );
    assert_eq!(rows[2]["origin"], origin_json(Some("lab-01")));
    assert_eq!(rows[4]["origin"]["alias"], "home-ubuntu");

    // Local rows get this machine's name; remote ones keep their alias.
    let mut named = rows.clone();
    name_local_rows(&mut named, "office-pc-07");
    assert_eq!(
        named[0]["origin"],
        json!({ "kind": "local", "alias": "office-pc-07" })
    );
    assert_eq!(named[3]["origin"]["alias"], "office-pc-07");
    assert_eq!(named[1]["origin"], rows[1]["origin"]);
    assert_eq!(named[4]["origin"], rows[4]["origin"]);
    name_local_rows(&mut named, "office-pc");
    assert_eq!(
        named[0]["origin"]["alias"], "office-pc",
        "a rename replaces it"
    );
    assert_eq!(rows[1]["state"], "working");
    assert_eq!(rows[1]["currentTool"], "Bash");
    assert_eq!(rows[2]["currentTool"], "Edit");
    assert!(
        rows[1]["toolSummary"].is_null(),
        "only the tool name travels"
    );
    assert_eq!(rows[1]["subagents"], json!([]));
    assert!(rows[1]["pid"].is_null(), "remote rows cannot be clicked");
    assert_eq!(rows[1]["lastActivity"], json!((T0 - 48) as f64), "48s ago");
    assert_eq!(rows[1]["contextPercent"], 42);
    assert_eq!(rows[1]["model"], "claude-opus-5-5");
    assert_eq!(rows[1]["hasHooks"], true);
    assert_eq!(rows[4]["state"], "offline");
    assert_eq!(rows[4]["contextPercent"], 31);
    assert_eq!(rows[4]["lastActivity"], json!((T0 - 290) as f64));
    assert_eq!(rows[0]["pid"], 4242);
    for key in [
        "id",
        "displayName",
        "state",
        "stateSince",
        "lastActivity",
        "subagents",
    ] {
        assert!(rows.iter().all(|r| !r[key].is_null()), "{key} missing");
    }
    let mut ids: Vec<_> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), rows.len());
}

#[test]
fn merged_rows_keep_local_order_within_a_rank() {
    let rows = merge_rows(
        &[
            local("b", SessionState::Working),
            local("a", SessionState::Working),
        ],
        &[],
    );
    let ids: Vec<_> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["b", "a"]);
}

#[test]
fn status_text_matches_the_footer_wording() {
    assert_eq!(status_text(ConnState::NotConfigured, 0), "not configured");
    assert_eq!(status_text(ConnState::Connecting, 0), "connecting…");
    assert_eq!(
        status_text(ConnState::Connected, 1),
        "1 machine · connected"
    );
    assert_eq!(
        status_text(ConnState::Connected, 3),
        "3 machines · connected"
    );
    assert_eq!(
        status_text(ConnState::Connected, 0),
        "0 machines · connected"
    );
    assert_eq!(
        status_text(ConnState::Disconnected, 2),
        "disconnected · retrying"
    );
    assert_eq!(demo_status().text, "3 machines · connected");
}

#[test]
fn hub_status_counts_machines_and_reports_unreadable_values() {
    let ring = ring();
    let mut hub = RemoteHub::new();
    assert_eq!(hub.status(T0).state, ConnState::NotConfigured);
    assert!(hub.set_state(ConnState::Connected, Some("Watching".into())));
    assert!(!hub.set_state(ConnState::Connected, Some("Watching".into())));
    hub.map_mut()
        .apply(&put_host(&ring, "test", T0), &ring, T0, true);
    let other = ring.other_viewer("v9");
    hub.map_mut()
        .apply(&put_host(&other, "x", T0), &ring, T0, true);
    let s = hub.status(T0);
    assert_eq!(s.text, "1 machine · connected");
    assert_eq!(s.undecryptable, 1);
    let detail = s.detail.unwrap();
    assert!(detail.starts_with("Watching"));
    assert!(detail.contains("could not be opened"), "{detail}");
    let json = serde_json::to_value(hub.status(T0)).unwrap();
    assert_eq!(json["state"], "connected");
    assert_eq!(json["machines"], 1);
}

fn temp_setup(dir: &std::path::Path) -> Setup {
    let keys = dir.join("keys");
    Setup {
        creds_path: dir.join("nats").join("cc-sessions.viewer.creds"),
        viewer_key_path: keys.join("viewer.key"),
        viewer_pub_path: keys.join("viewer.pub"),
        publishers_dir: keys.join("publishers"),
        settings_dir: Some(dir.to_path_buf()),
    }
}

#[test]
fn setup_is_not_configured_without_creds_and_creates_the_viewer_keys() {
    let dir = tempfile::tempdir().unwrap();
    let setup = temp_setup(dir.path());
    assert!(!setup.is_configured());
    let err = setup.check().unwrap_err();
    assert!(err.contains("cc-sessions.viewer.creds"), "{err}");

    std::fs::create_dir_all(dir.path().join("nats")).unwrap();
    std::fs::write(&setup.creds_path, "placeholder").unwrap();
    // The viewer key pair is created and is all it takes.
    let keys = setup.check().unwrap();
    assert!(setup.is_configured());
    let made = keys.secrets().current().public_key();
    assert!(made.kid().starts_with('v'), "{made:?}");
    assert_eq!(
        cc_envelope::PublicKeys::load(&setup.viewer_pub_path)
            .unwrap()
            .current(),
        &made
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&setup.viewer_key_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Never replaced on the next start.
    let again = setup.check().unwrap();
    assert_eq!(again.secrets().current().public_key(), made);

    // A broken viewer key: not configured, and the file is never echoed.
    std::fs::write(&setup.viewer_key_path, "not a viewer key").unwrap();
    let err = setup.check().unwrap_err();
    assert!(err.contains("Unusable viewer key file"), "{err}");
    assert!(!err.contains("not a viewer key"));
    assert_eq!(
        std::fs::read_to_string(&setup.viewer_key_path).unwrap(),
        "not a viewer key",
        "an unreadable viewer key is never replaced"
    );
}

#[test]
fn default_setup_uses_the_viewer_creds_and_the_app_settings() {
    let setup = Setup::from_data_dir();
    assert!(setup.creds_path.ends_with("cc-sessions.viewer.creds"));
    assert_eq!(setup.viewer_key_path, cc_core::paths::viewer_key_file());
    assert_eq!(setup.viewer_pub_path, cc_core::paths::viewer_pub_file());
    assert_eq!(setup.publishers_dir, cc_core::paths::publishers_dir());
    assert_eq!(setup.settings_dir, Some(crate::settings::default_dir()));
    // Nothing to read: cc-remote's defaults, WebSocket first.
    let none = Setup {
        settings_dir: None,
        ..setup
    };
    let nats = none.nats();
    assert!(nats.servers[0].starts_with("wss://"));
    assert!(nats.servers[1].starts_with("tls://"));
    assert_eq!(nats.bucket, "cc-sessions");
}

/// The viewer reads its servers and bucket from `settings.json`; a change
/// there is one the running watch reconnects for, a new problem alone is
/// not.
#[test]
fn the_viewer_takes_its_nats_settings_from_settings_json() {
    let dir = tempfile::tempdir().unwrap();
    let setup = temp_setup(dir.path());
    let before = setup.nats();
    assert!(before.is_default() && before.problems.is_empty());
    std::fs::write(
        crate::settings::path(dir.path()),
        r#"{"nats":{"servers":["tls://nats.example:4222"],"bucket":"team"}}"#,
    )
    .unwrap();
    let after = setup.nats();
    assert_eq!(after.servers, ["tls://nats.example:4222"]);
    assert_eq!(after.bucket, "team");
    assert!(super::task::nats_moved(&before, &after));
    std::fs::write(
        crate::settings::path(dir.path()),
        r#"{"nats":{"servers":["tls://nats.example:4222"],"bucket":"team","extra":1}}"#,
    )
    .unwrap();
    let noted = setup.nats();
    assert_eq!(noted.problems.len(), 1);
    assert!(!super::task::nats_moved(&after, &noted));
}

/// Ignored NATS settings show in the footer's tooltip, under the state.
#[test]
fn ignored_nats_settings_are_named_in_the_status() {
    let mut hub = RemoteHub::new();
    hub.set_state(
        ConnState::Connected,
        Some("Watching cc-sessions over wss://x".into()),
    );
    let problem = "settings.json: nats.servers ignored (no servers); using the default servers";
    assert!(hub.set_config_problems(vec![problem.into()]));
    assert!(!hub.set_config_problems(vec![problem.into()]), "unchanged");
    let detail = hub.status(T0).detail.unwrap();
    assert_eq!(
        detail,
        format!("Watching cc-sessions over wss://x\n{problem}")
    );
    assert!(hub.set_config_problems(Vec::new()));
    assert_eq!(
        hub.status(T0).detail.as_deref(),
        Some("Watching cc-sessions over wss://x")
    );
}

/// The project file's `max_age` (how long a key lives without a re-put)
/// against the viewer's timing: a row turns offline before its key
/// expires, and a dismissal outlives every value it hides. The publisher's
/// side is `bucket_limits_match_the_code` in cc-publish.
#[test]
fn the_bucket_outlives_the_offline_rule() {
    let text = include_str!("../../../infra/nats/cc-sessions.example.yaml");
    let max_age = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("max_age:"))
        .map(str::trim)
        .expect("max_age in the example project file");
    let secs: i64 = match max_age.split_at(max_age.len() - 1) {
        (n, "s") => n.parse().unwrap(),
        (n, "m") => n.parse::<i64>().unwrap() * 60,
        (n, "h") => n.parse::<i64>().unwrap() * 3600,
        _ => panic!("max_age {max_age:?}"),
    };
    assert!(
        OFFLINE_AFTER_SECS < secs,
        "offline {OFFLINE_AFTER_SECS} s, max_age {secs} s"
    );
    assert!(DISMISS_RETENTION_SECS > secs);
}

/// `toolctl check` writes its probe value under a key that is no session
/// key: the viewer ignores it unopened, shows nothing and counts nothing.
#[test]
fn the_toolctl_probe_is_ignored() {
    let text = include_str!("../../../infra/nats/cc-sessions.example.yaml");
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(name))
            .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
            .unwrap_or_else(|| panic!("{name} in the example project file"))
    };
    let key = field("probe_key:").replace("{alias}", "test");
    let value = field("probe_value:");
    let ring = ring();
    let mut m = RemoteMap::new();
    let change = KvChange::Put {
        key,
        bytes: value.into_bytes(),
        revision: 1,
    };
    assert!(!m.apply(&change, &ring, T0, true));
    assert!(m.rows(T0).is_empty());
    assert_eq!(m.undecryptable(), 0);
    assert_eq!(m.machines(T0), 0);
}

/// A viewer with its own key pair and its pin folder in `dir`, and two
/// publishers with key pairs of their own.
struct KeyWorld {
    keys: std::sync::Arc<ViewerKeys>,
    pins: std::path::PathBuf,
    viewer: cc_envelope::SecretKeys,
    devbox: cc_envelope::SecretKeys,
    lab: cc_envelope::SecretKeys,
}

fn key_world(dir: &std::path::Path) -> KeyWorld {
    let viewer = cc_envelope::SecretKeys::generate("k1").unwrap();
    let pins = dir.join("publishers");
    KeyWorld {
        keys: std::sync::Arc::new(ViewerKeys::new(viewer.clone(), Some(pins.clone()))),
        pins,
        viewer,
        devbox: cc_envelope::SecretKeys::generate("p-devbox").unwrap(),
        lab: cc_envelope::SecretKeys::generate("p-lab").unwrap(),
    }
}

impl KeyWorld {
    fn sealer(&self, publisher: &cc_envelope::SecretKeys) -> cc_envelope::Sealer {
        cc_envelope::Sealer::new(publisher.clone(), self.viewer.public_keys())
    }

    fn put(&self, publisher: &cc_envelope::SecretKeys, p: &RemotePayload) -> KvChange {
        let key = session_key(&p.machine, &p.sid);
        KvChange::Put {
            bytes: self.sealer(publisher).seal(p, &key).unwrap(),
            key,
            revision: 1,
        }
    }

    /// A hub that keeps the waiting keys in the pin folder and answers
    /// about them with these keys, as the app's viewer does.
    fn hub(&self) -> SharedHub {
        let hub = RemoteHub::shared();
        {
            let mut h = lock(&hub);
            h.keep_pending_in(self.pins.clone());
            h.set_keys(Some(self.keys.clone()));
        }
        hub
    }

    fn apply(&self, hub: &SharedHub, change: &KvChange, now: i64) -> bool {
        lock(hub).map_mut().apply(change, &*self.keys, now, true)
    }

    /// A pin file written before (by an Accept or by hand).
    fn prepin(&self, alias: &str, publisher: &cc_envelope::SecretKeys) {
        cc_envelope::pin_key(&self.pins, alias, &publisher.current().public_key()).unwrap();
        self.keys.refresh_alias(alias);
    }
}

fn line_of(publisher: &cc_envelope::SecretKeys) -> String {
    publisher.current().public_key().to_line()
}

/// Writes the `.pending` files if a write is due.
fn save_pending(hub: &SharedHub, force: bool) -> bool {
    let write = lock(hub).pending_to_save(Instant::now(), force);
    let due = write.is_some();
    if let Some(w) = write {
        w.run();
    }
    due
}

#[test]
fn a_new_machine_waits_for_review_and_shows_once_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let w = key_world(dir.path());
    let hub = w.hub();
    let p = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0);
    // Noted, not shown, not pinned, not counted as unreadable.
    assert!(
        w.apply(&hub, &w.put(&w.devbox, &p), T0),
        "the notice changed"
    );
    assert!(lock(&hub).rows(T0).is_empty());
    assert!(!w.pins.join("devbox.pub").exists());
    let s = status_of(&hub, T0);
    assert_eq!(s.undecryptable, 0);
    assert_eq!(s.pending.len(), 1);
    let waiting = &s.pending[0];
    assert_eq!(
        (waiting.alias.as_str(), waiting.known, waiting.count),
        ("devbox", false, 1)
    );
    assert_eq!(waiting.kid, "p-devbox");
    assert_eq!(waiting.key, line_of(&w.devbox));
    assert_eq!(waiting.fingerprint, &waiting.key["p-devbox:".len()..][..8]);
    assert_eq!((waiting.first_seen, waiting.last_seen), (T0, T0));
    assert!(s.detail.unwrap().contains("1 key to review"));
    let json = serde_json::to_value(&s.pending).unwrap();
    assert_eq!(json[0]["firstSeen"], T0);
    assert_eq!(json[0]["fingerprint"].as_str().unwrap().len(), 8);

    // Its next values are counted, and still not shown.
    let later = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0 + 60);
    assert!(
        !w.apply(&hub, &w.put(&w.devbox, &later), T0 + 60),
        "only the count moved"
    );
    let waiting = pending_keys_of(&hub);
    assert_eq!((waiting[0].count, waiting[0].last_seen), (2, T0 + 60));
    assert!(lock(&hub).rows(T0 + 60).is_empty());
    assert!(save_pending(&hub, false));
    assert!(w.pins.join("devbox.pending").is_file());

    // Accept: pinned, off the list, and the next value opens.
    accept_key_of(&hub, "devbox", &line_of(&w.devbox)).unwrap();
    assert_eq!(
        cc_envelope::PublicKeys::load(w.pins.join("devbox.pub"))
            .unwrap()
            .current(),
        &w.devbox.current().public_key()
    );
    assert!(pending_keys_of(&hub).is_empty());
    assert!(!w.pins.join("devbox.pending").exists(), "written at once");
    let next = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0 + 120);
    assert!(w.apply(&hub, &w.put(&w.devbox, &next), T0 + 120));
    assert_eq!(lock(&hub).rows(T0 + 120)[0].alias, "devbox");
    // The answer cannot be given twice, nor about a key never offered.
    assert!(accept_key_of(&hub, "devbox", &line_of(&w.devbox)).is_err());
    assert!(reject_key_of(&hub, "lab", &line_of(&w.lab)).is_err());
}

#[test]
fn a_new_key_of_a_known_machine_waits_and_the_pinned_one_keeps_working() {
    let dir = tempfile::tempdir().unwrap();
    let w = key_world(dir.path());
    w.prepin("devbox", &w.devbox);
    let hub = w.hub();
    lock(&hub).set_state(ConnState::Connected, Some("Watching".into()));
    let p = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0);
    assert!(w.apply(&hub, &w.put(&w.devbox, &p), T0));
    assert_eq!(
        lock(&hub).rows(T0).len(),
        1,
        "an existing pin file keeps working"
    );

    // The (valid) key of lab under devbox's alias: a new key of a known
    // machine, held back; the row its value replaced goes.
    let other = payload("devbox", "3f9a1c2e", SessionState::NeedsYou, T0, T0 + 5);
    assert!(w.apply(&hub, &w.put(&w.lab, &other), T0 + 5));
    assert!(lock(&hub).rows(T0 + 5).is_empty());
    let waiting = pending_keys_of(&hub);
    assert_eq!((waiting.len(), waiting[0].known), (1, true));
    let s = status_of(&hub, T0 + 5);
    assert_eq!(s.undecryptable, 0);
    assert!(!s.detail.unwrap().contains("unknown sender"));
    assert_eq!(
        cc_envelope::PublicKeys::load(w.pins.join("devbox.pub"))
            .unwrap()
            .keys()
            .len(),
        1
    );

    // devbox's own key still opens.
    let back = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0 + 60);
    assert!(w.apply(&hub, &w.put(&w.devbox, &back), T0 + 60));

    // Reject: off the list, and its values are dropped and counted.
    reject_key_of(&hub, "devbox", &line_of(&w.lab)).unwrap();
    assert!(pending_keys_of(&hub).is_empty());
    assert!(w.pins.join("devbox.rejected").is_file());
    let again = payload("devbox", "3f9a1c2e", SessionState::NeedsYou, T0, T0 + 90);
    assert!(
        w.apply(&hub, &w.put(&w.lab, &again), T0 + 90),
        "the row it replaced goes"
    );
    assert!(pending_keys_of(&hub).is_empty(), "never asked again");
    let s = status_of(&hub, T0 + 90);
    assert_eq!(s.undecryptable, 0);
    assert!(s
        .detail
        .unwrap()
        .contains("devbox: 1 value(s) from a rejected key dropped"));
    assert_eq!(
        lock(&hub).map().rejected_values(),
        [("devbox".to_string(), 1)]
    );

    // After a restart the rejection holds and the pin is unchanged.
    let fresh = ViewerKeys::new(w.viewer.clone(), Some(w.pins.clone()));
    let mut m = RemoteMap::new();
    assert!(!m.apply(&w.put(&w.lab, &again), &fresh, T0 + 100, true));
    assert!(m.pending_keys().is_empty());
    assert!(m.apply(&w.put(&w.devbox, &back), &fresh, T0 + 100, true));
}

#[test]
fn waiting_keys_survive_a_restart_and_can_be_answered_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let w = key_world(dir.path());
    let hub = w.hub();
    w.apply(&hub, &put_host(&ring_of(&w, &w.devbox), "devbox", T0), T0);
    w.apply(
        &hub,
        &w.put(
            &w.lab,
            &payload("lab", "3f9a1c2e", SessionState::Idle, T0, T0),
        ),
        T0 + 7,
    );
    assert!(save_pending(&hub, false));
    let before = pending_keys_of(&hub);
    assert_eq!(before.len(), 2);

    // Read back as they were, by another run.
    let again = w.hub();
    assert_eq!(pending_keys_of(&again), before);
    assert_eq!(
        lock(&again).map().aliases(),
        ["devbox", "lab"],
        "their machines are held"
    );
    // A planted file under an invalid name, or one that does not parse,
    // is not read.
    std::fs::write(w.pins.join("Not An Alias.pending"), "[]").unwrap();
    std::fs::write(w.pins.join("junk.pending"), "not json").unwrap();
    assert_eq!(read_pending(&w.pins).len(), 2);

    reject_key_of(&again, "lab", &line_of(&w.lab)).unwrap();
    accept_key_of(&again, "devbox", &line_of(&w.devbox)).unwrap();
    assert!(pending_keys_of(&again).is_empty());
    assert!(w.pins.join("devbox.pub").is_file() && w.pins.join("lab.rejected").is_file());
    assert!(!w.pins.join("devbox.pending").exists() && !w.pins.join("lab.pending").exists());
    assert!(
        !w.pins.join("junk.pending").exists(),
        "the files follow what is held"
    );

    // A third run finds nothing to ask, and opens devbox.
    let third = w.hub();
    assert!(pending_keys_of(&third).is_empty());
    let p = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0 + 60);
    assert!(w.apply(&third, &w.put(&w.devbox, &p), T0 + 60));
}

/// A ring whose publisher is `publisher` and whose viewer is `w`'s.
fn ring_of(w: &KeyWorld, publisher: &cc_envelope::SecretKeys) -> Ring {
    Ring {
        sealer: w.sealer(publisher),
        keys: ViewerKeys::new(w.viewer.clone(), None),
    }
}

#[test]
fn waiting_keys_are_capped_per_machine_and_in_machines() {
    let dir = tempfile::tempdir().unwrap();
    let w = key_world(dir.path());
    let hub = w.hub();
    let keys: Vec<cc_envelope::SecretKeys> = (0..=MAX_PENDING_PER_ALIAS)
        .map(|i| cc_envelope::SecretKeys::generate(&format!("p{i}")).unwrap())
        .collect();
    let p = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0);
    for (i, k) in keys.iter().take(MAX_PENDING_PER_ALIAS).enumerate() {
        w.apply(&hub, &w.put(k, &p), T0 + i as i64);
    }
    // The first key keeps publishing: the one seen least recently goes.
    w.apply(&hub, &w.put(&keys[0], &p), T0 + 30);
    assert!(w.apply(&hub, &w.put(&keys[MAX_PENDING_PER_ALIAS], &p), T0 + 31));
    let kids: Vec<String> = pending_keys_of(&hub).into_iter().map(|k| k.kid).collect();
    assert_eq!(kids.len(), MAX_PENDING_PER_ALIAS);
    assert!(
        kids.contains(&"p0".to_string()) && !kids.contains(&"p1".to_string()),
        "{kids:?}"
    );

    // Machines: one past the cap is not noted, only counted.
    for i in 1..MAX_PENDING_ALIASES {
        let alias = format!("m{i:02}");
        w.apply(
            &hub,
            &w.put(
                &w.lab,
                &payload(&alias, "3f9a1c2e", SessionState::Idle, T0, T0),
            ),
            T0,
        );
    }
    let late = payload("late", "3f9a1c2e", SessionState::Idle, T0, T0);
    assert!(!w.apply(&hub, &w.put(&w.lab, &late), T0));
    let s = status_of(&hub, T0);
    assert!(s.pending.iter().all(|k| k.alias != "late"));
    assert_eq!(lock(&hub).map().pending.over_cap(), 1);
    assert!(s.detail.unwrap().contains(&format!(
        "more than {MAX_PENDING_ALIASES} machines wait for review"
    )));
    assert_eq!(s.undecryptable, 0);
}

#[test]
fn waiting_keys_are_written_when_they_change_and_their_counts_rarely() {
    let dir = tempfile::tempdir().unwrap();
    let w = key_world(dir.path());
    let hub = w.hub();
    assert!(!save_pending(&hub, true), "nothing to write");
    let p = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0);
    w.apply(&hub, &w.put(&w.devbox, &p), T0);
    assert!(save_pending(&hub, false), "a new key is written soon");
    w.apply(&hub, &w.put(&w.devbox, &p), T0 + 60);
    let start = Instant::now();
    assert!(
        lock(&hub).pending_to_save(start, false).is_none(),
        "a count waits"
    );
    assert!(lock(&hub)
        .pending_to_save(start + PENDING_SAVE_INTERVAL, false)
        .is_some());
    w.apply(&hub, &w.put(&w.devbox, &p), T0 + 120);
    assert!(save_pending(&hub, true), "the exit writes it");
    let stored = read_pending(&w.pins);
    assert_eq!(stored[0].count, 3);
}

#[test]
fn the_viewer_never_asks_about_its_own_alias() {
    let dir = tempfile::tempdir().unwrap();
    let w = key_world(dir.path());
    let hub = w.hub();
    w.apply(
        &hub,
        &w.put(
            &w.devbox,
            &payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0),
        ),
        T0,
    );
    assert_eq!(pending_keys_of(&hub).len(), 1);
    lock(&hub).set_own_alias(Some("devbox".into()));
    assert!(pending_keys_of(&hub).is_empty());
    assert!(!w.apply(
        &hub,
        &w.put(
            &w.devbox,
            &payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0 + 5)
        ),
        T0 + 5
    ));
    assert!(pending_keys_of(&hub).is_empty());
}

#[test]
fn a_revoked_machine_with_an_empty_pin_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let w = key_world(dir.path());
    let pins = dir.path().join("publishers");
    std::fs::create_dir_all(&pins).unwrap();
    std::fs::write(pins.join("devbox.pub"), "# revoked 2026-10-14\n").unwrap();
    let mut hub = RemoteHub::new();
    let p = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0);
    assert!(!hub
        .map_mut()
        .apply(&w.put(&w.devbox, &p), &*w.keys, T0, true));
    assert!(hub.rows(T0).is_empty());
    assert!(hub.map().pending_keys().is_empty(), "not even asked about");
    let detail = hub.status(T0).detail.unwrap();
    assert!(
        detail.contains("devbox: unknown sender key \u{2014} values rejected"),
        "{detail}"
    );
    assert_eq!(
        std::fs::read_to_string(pins.join("devbox.pub")).unwrap(),
        "# revoked 2026-10-14\n",
        "never re-pinned"
    );
}

#[test]
fn values_of_an_unsupported_envelope_version_are_rejected_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let w = key_world(dir.path());
    let hub = w.hub();
    // A value naming another envelope version: never opened.
    let other = |key: String| KvChange::Put {
        key,
        bytes: br#"{"v":3,"x":"y"}"#.to_vec(),
        revision: 1,
    };
    assert!(!w.apply(&hub, &other(session_key("devbox", "3f9a1c2e")), T0));
    assert!(!w.apply(&hub, &other(host_key("lab")), T0));
    assert!(lock(&hub).rows(T0).is_empty());
    let s = status_of(&hub, T0);
    assert_eq!((s.undecryptable, s.machines), (2, 0));
    let detail = s.detail.unwrap();
    assert!(
        detail.contains("2 value(s) could not be opened"),
        "{detail}"
    );
    assert!(detail.contains("unsupported envelope version"), "{detail}");
    assert!(s.pending.is_empty(), "nothing to ask about");

    // A value devbox seals: asked about, then shown once accepted.
    let new = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0 + 60);
    assert!(w.apply(&hub, &w.put(&w.devbox, &new), T0 + 60));
    accept_key_of(&hub, "devbox", &line_of(&w.devbox)).unwrap();
    let next = payload("devbox", "3f9a1c2e", SessionState::Working, T0, T0 + 120);
    assert!(w.apply(&hub, &w.put(&w.devbox, &next), T0 + 120));
    assert_eq!(lock(&hub).rows(T0 + 120).len(), 1);
}

fn put_rev(ring: &Ring, p: &RemotePayload, revision: u64) -> KvChange {
    match put_session(ring, p) {
        KvChange::Put { key, bytes, .. } => KvChange::Put {
            key,
            bytes,
            revision,
        },
        other => other,
    }
}

#[test]
fn dismissed_rows_stay_hidden_until_a_newer_revision_arrives() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let live = payload("lab", "0000aaaa", SessionState::Working, T0, T0);
    let dead = payload("lab", "0000bbbb", SessionState::Idle, T0, T0);
    m.apply(&put_rev(&ring, &live, 10), &ring, T0, true);
    m.apply(&put_rev(&ring, &dead, 11), &ring, T0, true);
    assert_eq!(m.rows(T0).len(), 2);

    assert_eq!(m.dismiss_all(T0 + 1), 2);
    assert!(m.rows(T0 + 1).is_empty());

    // A reconnect replays the bucket: the same revisions stay hidden.
    assert!(!m.apply(&put_rev(&ring, &live, 10), &ring, T0 + 2, false));
    assert!(!m.apply(&put_rev(&ring, &dead, 11), &ring, T0 + 2, false));
    assert!(m.rows(T0 + 2).is_empty());

    // The live session's next heartbeat is a newer write: it is back.
    let beat = payload("lab", "0000aaaa", SessionState::Working, T0, T0 + 60);
    assert!(m.apply(&put_rev(&ring, &beat, 12), &ring, T0 + 60, true));
    let rows = m.rows(T0 + 60);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].view.id, "remote:lab.s.0000aaaa");
    // And it is no longer dismissed: a later clear starts from it again.
    assert_eq!(m.dismiss_all(T0 + 61), 1);
}

#[test]
fn a_live_move_into_needs_you_toasts_once() {
    let ring = ring();
    let mut m = RemoteMap::new();
    // The initial snapshot: a session already waiting does not toast.
    let waiting = payload("lab", "0000aaaa", SessionState::NeedsYou, T0, T0);
    m.apply(&put_rev(&ring, &waiting, 1), &ring, T0, false);
    let working = payload("lab", "0000bbbb", SessionState::Working, T0, T0);
    m.apply(&put_rev(&ring, &working, 2), &ring, T0, false);
    assert!(m.take_needs_you(T0).is_empty());

    // Its heartbeat, live and still waiting, does not either.
    let beat = payload("lab", "0000aaaa", SessionState::NeedsYou, T0, T0 + 60);
    m.apply(&put_rev(&ring, &beat, 3), &ring, T0 + 60, true);
    assert!(m.take_needs_you(T0 + 60).is_empty());

    // The working one asks, live: one toast, naming the machine.
    let mut asks = payload("lab", "0000bbbb", SessionState::NeedsYou, T0 + 61, T0 + 61);
    asks.text = Some("Allow Bash?".into());
    asks.alias = Some("deploy".into());
    m.apply(&put_rev(&ring, &asks, 4), &ring, T0 + 61, true);
    let rows = m.take_needs_you(T0 + 61);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].view.id, "remote:lab.s.0000bbbb");
    assert_eq!(
        needs_you_toast(&rows[0]),
        (
            "deploy on lab needs you".to_string(),
            "Allow Bash?".to_string()
        )
    );
    assert!(m.take_needs_you(T0 + 61).is_empty(), "once");

    // A brand-new session that starts out waiting toasts too.
    let new = payload("lab", "0000cccc", SessionState::NeedsYou, T0 + 62, T0 + 62);
    m.apply(&put_rev(&ring, &new, 5), &ring, T0 + 62, true);
    assert_eq!(m.take_needs_you(T0 + 62).len(), 1);
}

#[test]
fn a_session_back_after_a_clear_does_not_toast_again() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let asks = payload("lab", "0000aaaa", SessionState::NeedsYou, T0, T0);
    m.apply(&put_rev(&ring, &asks, 1), &ring, T0, true);
    assert_eq!(m.take_needs_you(T0).len(), 1);

    m.dismiss_all(T0 + 1);
    // Its next heartbeat brings the row back, still waiting: no second toast.
    let beat = payload("lab", "0000aaaa", SessionState::NeedsYou, T0, T0 + 60);
    m.apply(&put_rev(&ring, &beat, 2), &ring, T0 + 60, true);
    assert_eq!(m.rows(T0 + 60).len(), 1);
    assert!(m.take_needs_you(T0 + 60).is_empty());

    // Answered, then a new question: that one toasts.
    let answered = payload("lab", "0000aaaa", SessionState::Working, T0 + 70, T0 + 70);
    m.apply(&put_rev(&ring, &answered, 3), &ring, T0 + 70, true);
    let again = payload("lab", "0000aaaa", SessionState::NeedsYou, T0 + 80, T0 + 80);
    m.apply(&put_rev(&ring, &again, 4), &ring, T0 + 80, true);
    assert_eq!(m.take_needs_you(T0 + 80).len(), 1);
}

#[test]
fn a_toast_not_taken_goes_when_the_session_moves_on_or_is_cleared() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let asks = payload("lab", "0000aaaa", SessionState::NeedsYou, T0, T0);
    m.apply(&put_rev(&ring, &asks, 1), &ring, T0, true);
    let answered = payload("lab", "0000aaaa", SessionState::Working, T0 + 1, T0 + 1);
    m.apply(&put_rev(&ring, &answered, 2), &ring, T0 + 1, true);
    assert!(m.take_needs_you(T0 + 1).is_empty());

    let again = payload("lab", "0000aaaa", SessionState::NeedsYou, T0 + 2, T0 + 2);
    m.apply(&put_rev(&ring, &again, 3), &ring, T0 + 2, true);
    m.dismiss_all(T0 + 3);
    assert!(m.take_needs_you(T0 + 3).is_empty());
}

#[test]
fn a_delete_forgets_the_dismissal_so_a_resumed_key_shows() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let p = payload("lab", "0000aaaa", SessionState::Working, T0, T0);
    m.apply(&put_rev(&ring, &p, 40), &ring, T0, true);
    m.dismiss_all(T0);
    m.apply(
        &KvChange::Delete {
            key: "lab.s.0000aaaa".into(),
        },
        &ring,
        T0 + 1,
        true,
    );
    // Revisions only grow, but a dismissal must not outlive its key.
    assert!(m.dismissed.is_empty());
    assert!(m.apply(&put_rev(&ring, &p, 41), &ring, T0 + 2, true));
    assert_eq!(m.rows(T0 + 2).len(), 1);
}

#[test]
fn dismissals_are_forgotten_after_an_hour() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let p = payload("lab", "0000aaaa", SessionState::Working, T0, T0);
    m.apply(&put_rev(&ring, &p, 7), &ring, T0, true);
    m.dismiss_all(T0);
    m.prune(T0 + DISMISS_RETENTION_SECS);
    assert_eq!(m.dismissed.len(), 1);
    m.prune(T0 + DISMISS_RETENTION_SECS + 1);
    assert!(m.dismissed.is_empty());
}

#[test]
fn the_offline_drop_delay_is_configurable() {
    let ring = ring();
    let mut m = RemoteMap::new();
    assert_eq!(m.drop_after(), None, "kept until cleared by default");
    let p = payload("wsl", "0badf00d", SessionState::Working, T0, T0);
    m.apply(&put_session(&ring, &p), &ring, T0, true);
    m.tick(T0);
    m.tick(T0 + 7 * 24 * 3600);
    assert_eq!(m.len(), 1, "a week later: still listed");
    assert_eq!(
        m.rows(T0 + 7 * 24 * 3600)[0].view.state,
        SessionState::Offline
    );
    assert_eq!(
        m.machines(T0 + 7 * 24 * 3600),
        0,
        "an offline machine is not counted"
    );
    m.dismiss_all(T0 + 7 * 24 * 3600);
    assert!(
        m.rows(T0 + 7 * 24 * 3600).is_empty(),
        "the clear button removes it"
    );

    let mut m = RemoteMap::new();
    m.set_drop_after(Some(5 * 60));
    m.apply(&put_session(&ring, &p), &ring, T0, true);
    m.tick(T0);
    assert_eq!(m.rows(T0 + 200)[0].view.state, SessionState::Offline);
    m.tick(T0 + 300);
    assert_eq!(m.len(), 1, "exactly 5 minutes: still listed");
    assert!(m.tick(T0 + 301), "dropped after 5 minutes");
    assert!(m.is_empty());

    // An hour keeps it that long.
    m.set_drop_after(Some(60 * 60));
    m.apply(&put_session(&ring, &p), &ring, T0, true);
    m.tick(T0 + 3000);
    assert_eq!(m.len(), 1);
    m.tick(T0 + 3601);
    assert!(m.is_empty());

    // Never below the offline threshold: a row always shows offline first.
    m.set_drop_after(Some(10));
    assert_eq!(m.drop_after(), Some(OFFLINE_AFTER_SECS));
}

#[test]
fn the_idle_filter_marks_long_idle_rows_hidden_until_they_change() {
    let ring = ring();
    let mut m = RemoteMap::new();
    // Remote: idle for 20 minutes, and one working.
    let idle = payload("lab", "0000aaaa", SessionState::Idle, T0 - 1200, T0);
    let busy = payload("lab", "0000bbbb", SessionState::Working, T0 - 1200, T0);
    m.apply(&put_session(&ring, &idle), &ring, T0, true);
    m.apply(&put_session(&ring, &busy), &ring, T0, true);
    // Local: idle for 16 minutes, idle for 5, and one needing you for an hour.
    let mut old_idle = local("old-idle", SessionState::Idle);
    old_idle.state_since = (T0 - 960) as f64;
    let mut new_idle = local("new-idle", SessionState::Idle);
    new_idle.state_since = (T0 - 300) as f64;
    let mut waiting = local("waiting", SessionState::NeedsYou);
    waiting.state_since = (T0 - 3600) as f64;

    let filter = IdleFilter::after_minutes(15);
    let hidden_of = |rows: &[Value]| -> Vec<String> {
        rows.iter()
            .filter(|r| r["hidden"] == true)
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    };
    let rows = merge_rows_with(
        &[waiting.clone(), new_idle.clone(), old_idle.clone()],
        &m.rows(T0),
        filter,
        T0 as f64,
    );
    assert_eq!(rows.len(), 5, "hidden rows stay in the list");
    assert_eq!(hidden_of(&rows), ["old-idle", "remote:lab.s.0000aaaa"]);
    assert!(rows.iter().all(|r| r["hidden"].is_boolean()));

    // Never: nothing hidden.
    let rows = merge_rows_with(
        &[old_idle.clone()],
        &m.rows(T0),
        IdleFilter::after_minutes(0),
        T0 as f64,
    );
    assert!(hidden_of(&rows).is_empty());

    // The remote session starts working: shown again at once.
    let woke = payload("lab", "0000aaaa", SessionState::Working, T0 + 5, T0 + 5);
    m.apply(&put_session(&ring, &woke), &ring, T0 + 5, true);
    let rows = merge_rows_with(&[old_idle], &m.rows(T0 + 5), filter, (T0 + 5) as f64);
    assert_eq!(hidden_of(&rows), ["old-idle"]);

    // An offline row is not idle: the filter leaves it alone.
    let rows = merge_rows_with(&[], &m.rows(T0 + 1000), filter, (T0 + 1000) as f64);
    assert!(rows.iter().all(|r| r["state"] == "offline"));
    assert!(hidden_of(&rows).is_empty());
}

#[test]
fn the_idle_time_of_a_remote_row_ignores_the_publishers_clock_offset() {
    let ring = ring();
    let filter = IdleFilter::after_minutes(15);
    let skew = 30 * 60;
    for offset in [-skew, skew] {
        // Idle for one minute, on a publisher clock 30 minutes off ours.
        let mut m = RemoteMap::new();
        let p = payload(
            "wsl",
            "0000aaaa",
            SessionState::Idle,
            T0 + offset - 60,
            T0 + offset,
        );
        m.apply(&put_session(&ring, &p), &ring, T0, true);
        let row = &m.rows(T0)[0];
        assert_eq!(row.view.state_since, (T0 - 60) as f64, "offset {offset}");
        assert!(
            !filter.hides(&row.view, T0 as f64),
            "offset {offset}: idle 1 min"
        );

        // A heartbeat 15 minutes later, still idle: 16 minutes in, hidden.
        let beat = payload(
            "wsl",
            "0000aaaa",
            SessionState::Idle,
            T0 + offset - 60,
            T0 + offset + 900,
        );
        m.apply(&put_session(&ring, &beat), &ring, T0 + 900, true);
        let row = &m.rows(T0 + 900)[0];
        assert_eq!(row.view.state_since, (T0 - 60) as f64, "offset {offset}");
        assert!(
            filter.hides(&row.view, (T0 + 900) as f64),
            "offset {offset}: idle 16 min"
        );
    }
}

#[test]
fn retries_back_off_from_two_seconds_to_the_cap() {
    assert_eq!(retry_delay(1), MIN_RETRY_DELAY);
    assert!(retry_delay(50) <= cc_remote::MAX_RECONNECT_DELAY);
    assert!(retry_delay(50) >= Duration::from_secs(20));
}

fn project(alias: &str, name: &str, input: u64, last: i64, hb: i64) -> ProjectPayload {
    let mut by_model = std::collections::HashMap::new();
    by_model.insert(
        "claude-opus-5-5".to_string(),
        cc_core::projects::Tokens {
            input,
            ..Default::default()
        },
    );
    ProjectPayload::new(alias, name, 2, &by_model, Some(last), hb)
}

fn put_project(ring: &Ring, p: &ProjectPayload, folder: &str) -> KvChange {
    let key = cc_envelope::project_key(&p.machine, &cc_envelope::pid_for(folder));
    KvChange::Put {
        bytes: ring.seal(p, &key),
        key,
        revision: 1,
    }
}

/// A project value feeds the usage view, not the rows; a delete (or the
/// TTL) leaves it, and a re-put that only moves `hb` is not a change.
#[test]
fn projects_are_kept_apart_from_rows_and_through_deletes() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let put = put_project(
        &ring,
        &project("devbox", "app", 100, T0 - 60, T0),
        "c--home-k-app",
    );
    assert!(
        !m.apply(&put, &ring, T0, true),
        "the session list does not change"
    );
    assert!(m.rows(T0).is_empty());
    assert!(m.take_projects_changed());
    let held = m.projects();
    assert_eq!(held.len(), 1);
    assert_eq!(
        (held[0].alias.as_str(), held[0].payload.name.as_str()),
        ("devbox", "app")
    );

    let again = put_project(
        &ring,
        &project("devbox", "app", 100, T0 - 60, T0 + 120),
        "c--home-k-app",
    );
    m.apply(&again, &ring, T0 + 120, true);
    assert!(!m.take_projects_changed(), "only the heartbeat moved");
    let more = put_project(
        &ring,
        &project("devbox", "app", 900, T0 + 100, T0 + 240),
        "c--home-k-app",
    );
    m.apply(&more, &ring, T0 + 240, true);
    assert!(m.take_projects_changed());

    let KvChange::Put { key, .. } = &more else {
        unreachable!()
    };
    m.apply(
        &KvChange::Delete { key: key.clone() },
        &ring,
        T0 + 400,
        true,
    );
    assert_eq!(
        m.projects().len(),
        1,
        "a switched-off machine still used what it used"
    );

    // Past the window after its last activity, it goes.
    m.prune(T0 + 100 + PROJECT_KEEP_SECS + 1);
    assert!(m.projects().is_empty());
    assert!(m.take_projects_changed());
}

/// Saved by one run, loaded by the next; a value of another key or one
/// past the window is not loaded, and a newer value heard first wins.
#[test]
fn projects_survive_a_restart() {
    let ring = ring();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REMOTE_PROJECTS_FILE);
    let hub = RemoteHub::shared();
    lock(&hub).keep_projects_in(path.clone(), T0);
    let put = put_project(
        &ring,
        &project("devbox", "app", 100, T0 - 60, T0),
        "c--home-k-app",
    );
    lock(&hub).map_mut().apply(&put, &ring, T0, true);
    flush_projects_of(&hub);
    assert!(path.is_file());

    let next = RemoteHub::shared();
    lock(&next).keep_projects_in(path.clone(), T0 + 3600);
    let loaded = projects_of(&next);
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].payload.tokens().input, 100);

    // A forged entry whose key names another machine is ignored, and so
    // is an expired one.
    let mut stored = loaded.clone();
    stored[0].alias = "other".into();
    let mut old = loaded[0].clone();
    old.key = cc_envelope::project_key("devbox", &cc_envelope::pid_for("c--old"));
    old.payload.last = Some(T0 - PROJECT_KEEP_SECS - 10);
    stored.push(old);
    let mut m = RemoteMap::new();
    m.load_projects(stored, T0);
    assert!(m.projects().is_empty());
}

/// A machine's activity is held apart from the rows and through a delete.
#[test]
fn activity_is_held_per_machine_through_deletes() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let key = cc_envelope::activity_key("devbox");
    let payload = ActivityPayload {
        machine: "devbox".into(),
        days: vec![cc_core::projects::DayActivity {
            day: "2026-10-13".into(),
            tokens: 42,
            peak_hour: 9,
        }],
        hb: T0,
    };
    let put = KvChange::Put {
        bytes: ring.seal(&payload, &key),
        key: key.clone(),
        revision: 1,
    };
    assert!(
        !m.apply(&put, &ring, T0, true),
        "the session list does not change"
    );
    m.apply(&KvChange::Delete { key }, &ring, T0 + 400, true);
    assert_eq!(m.activity(), vec![("devbox".to_string(), payload)]);
}

/// A viewer that published before finds its own values in the bucket:
/// its sessions, host record, projects and activity are ignored (they
/// are shown as local), and those held already go once its name is set.
#[test]
fn the_viewer_ignores_its_own_alias() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let other = payload("lab-01", "0000aaaa", SessionState::Working, T0, T0);
    let mine = payload("wk-01", "0000bbbb", SessionState::Working, T0, T0);
    let activity = |alias: &str| {
        let key = cc_envelope::activity_key(alias);
        let payload = ActivityPayload {
            machine: alias.into(),
            days: vec![cc_core::projects::DayActivity {
                day: "2026-10-13".into(),
                tokens: 42,
                peak_hour: 9,
            }],
            hb: T0,
        };
        KvChange::Put {
            bytes: ring.seal(&payload, &key),
            key,
            revision: 1,
        }
    };
    for change in [
        put_session(&ring, &other),
        put_session(&ring, &mine),
        put_host(&ring, "wk-01", T0),
        put_project(&ring, &project("wk-01", "app", 100, T0 - 60, T0), "c--app"),
        put_project(&ring, &project("lab-01", "web", 10, T0 - 60, T0), "c--web"),
        activity("wk-01"),
    ] {
        m.apply(&change, &ring, T0, true);
    }
    assert_eq!(m.rows(T0).len(), 2, "held before the name is known");
    assert!(m.set_own_alias(Some("wk-01".into())));
    let rows = m.rows(T0);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].alias, "lab-01");
    assert_eq!(m.machines(T0), 1, "its own host record does not count");
    assert_eq!(
        m.projects()
            .iter()
            .map(|p| p.alias.as_str())
            .collect::<Vec<_>>(),
        ["lab-01"]
    );
    assert!(m.activity().is_empty());
    assert!(m.take_projects_changed(), "saved without its own projects");

    // New values under its name are ignored on arrival; others are not.
    let later = payload("wk-01", "0000cccc", SessionState::Idle, T0, T0 + 5);
    assert!(!m.apply(&put_session(&ring, &later), &ring, T0 + 5, true));
    assert!(!m.apply(&put_host(&ring, "wk-01", T0 + 5), &ring, T0 + 5, true));
    m.apply(&activity("wk-01"), &ring, T0 + 5, true);
    assert!(m.activity().is_empty());
    assert_eq!(m.rows(T0 + 5).len(), 1);
    assert_eq!(m.undecryptable(), 0, "ignored, not rejected");

    // Nor are they loaded from the projects file.
    let stored = StoredProject {
        key: cc_envelope::project_key("wk-01", &cc_envelope::pid_for("c--app")),
        alias: "wk-01".into(),
        payload: project("wk-01", "app", 100, T0 - 60, T0),
        heard: T0,
    };
    assert_eq!(m.load_projects(vec![stored], T0), (0, 0));

    // A new name stops ignoring the old one.
    assert!(!m.set_own_alias(Some("renamed".into())));
    assert!(m.apply(&put_session(&ring, &later), &ring, T0 + 6, true));
}

/// An old value put again does not replace the current one; the same
/// value again (a reconnect's snapshot) and a newer one do.
#[test]
fn an_older_heartbeat_is_a_replay_and_ignored() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let old = payload("lab", "0000aaaa", SessionState::Working, T0 - 60, T0);
    let new = payload("lab", "0000aaaa", SessionState::Done, T0 + 50, T0 + 60);
    assert!(m.apply(&put_rev(&ring, &new, 2), &ring, T0 + 60, true));
    assert!(
        !m.apply(&put_rev(&ring, &old, 3), &ring, T0 + 61, true),
        "replayed"
    );
    assert_eq!(m.rows(T0 + 61)[0].view.state, SessionState::Done);
    // The snapshot after a reconnect repeats the value held.
    m.apply(&put_rev(&ring, &new, 2), &ring, T0 + 70, false);
    assert_eq!(m.rows(T0 + 70)[0].view.state, SessionState::Done);
    let newer = payload("lab", "0000aaaa", SessionState::Idle, T0 + 100, T0 + 120);
    assert!(m.apply(&put_rev(&ring, &newer, 4), &ring, T0 + 120, true));
    assert_eq!(m.rows(T0 + 120)[0].view.state, SessionState::Idle);

    // Hosts, projects and activity follow the same rule.
    m.apply(&put_host(&ring, "lab", T0 + 120), &ring, T0 + 120, true);
    assert!(!m.apply(&put_host(&ring, "lab", T0), &ring, T0 + 121, true));
    assert_eq!(m.hosts["lab"].payload.hb, T0 + 120);
    let p_new = put_project(&ring, &project("lab", "app", 900, T0, T0 + 120), "c--app");
    let p_old = put_project(&ring, &project("lab", "app", 100, T0 - 600, T0), "c--app");
    m.apply(&p_new, &ring, T0 + 120, true);
    m.apply(&p_old, &ring, T0 + 121, true);
    assert_eq!(m.projects()[0].payload.tokens().input, 900);
}

/// A value dated in the future blocks newer ones for a day at most, and
/// a future project cannot outlive the window.
#[test]
fn future_timestamps_are_believed_a_day_ahead_at_most() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let far = T0 + 365 * 24 * 3600;
    let put = put_project(&ring, &project("devbox", "app", 100, far, far), "c--app");
    m.apply(&put, &ring, T0, true);
    let held = &m.projects()[0];
    assert_eq!(held.payload.last, Some(T0 + MAX_CLOCK_SKEW_SECS));
    assert_eq!(held.payload.hb, T0 + MAX_CLOCK_SKEW_SECS);
    m.prune(T0 + MAX_CLOCK_SKEW_SECS + PROJECT_KEEP_SECS + 1);
    assert!(m.projects().is_empty(), "expired like any other");

    // A session heartbeat from next year blocks the key for a day only.
    let ahead = payload("devbox", "0000aaaa", SessionState::Working, T0, far);
    m.apply(&put_session(&ring, &ahead), &ring, T0, true);
    let honest = payload("devbox", "0000aaaa", SessionState::Done, T0, T0 + 60);
    assert!(!m.apply(&put_session(&ring, &honest), &ring, T0 + 60, true));
    let later = T0 + MAX_CLOCK_SKEW_SECS + 10;
    let honest = payload("devbox", "0000aaaa", SessionState::Done, later, later);
    assert!(m.apply(&put_session(&ring, &honest), &ring, later, true));
    assert_eq!(m.rows(later)[0].view.state, SessionState::Done);

    // The same rules apply to the file a previous run saved.
    let mut stored = StoredProject {
        key: cc_envelope::project_key("devbox", &cc_envelope::pid_for("c--x")),
        alias: "devbox".into(),
        payload: project("devbox", "x", 1, far, far),
        heard: far,
    };
    stored.payload.hb = far;
    let mut m = RemoteMap::new();
    m.load_projects(vec![stored], T0);
    let p = &m.projects()[0];
    assert_eq!(
        (p.payload.last, p.heard),
        (Some(T0 + MAX_CLOCK_SKEW_SECS), T0)
    );
}

/// One machine cannot fill the flyout or the memory: keys past the cap
/// are ignored, the ones held keep updating, other machines are not
/// affected.
#[test]
fn a_machine_gets_a_bounded_number_of_sessions_and_projects() {
    let ring = ring();
    let mut m = RemoteMap::new();
    for i in 0..MAX_SESSIONS_PER_ALIAS + 5 {
        let p = payload("noisy", &format!("{i:08x}"), SessionState::Idle, T0, T0);
        m.apply(&put_session(&ring, &p), &ring, T0, true);
    }
    assert_eq!(m.len(), MAX_SESSIONS_PER_ALIAS);
    let beat = payload("noisy", "00000000", SessionState::Working, T0, T0 + 60);
    assert!(
        m.apply(&put_session(&ring, &beat), &ring, T0 + 60, true),
        "a held key still updates"
    );
    let other = payload("calm", "0000aaaa", SessionState::Idle, T0, T0);
    assert!(m.apply(&put_session(&ring, &other), &ring, T0, true));
    assert_eq!(m.len(), MAX_SESSIONS_PER_ALIAS + 1);

    for i in 0..MAX_PROJECTS_PER_ALIAS + 5 {
        let put = put_project(
            &ring,
            &project("noisy", "p", 1, T0, T0),
            &format!("c--p{i}"),
        );
        m.apply(&put, &ring, T0, true);
    }
    assert_eq!(m.projects().len(), MAX_PROJECTS_PER_ALIAS);
    let put = put_project(&ring, &project("calm", "p", 1, T0, T0), "c--p");
    m.apply(&put, &ring, T0, true);
    assert_eq!(m.projects().len(), MAX_PROJECTS_PER_ALIAS + 1);
}

/// The viewer holds a bounded number of machines: the values of one
/// more are ignored unopened (no pin), counted and named in the tooltip,
/// and a place frees once a machine holds nothing any more.
#[test]
fn the_number_of_machines_held_is_capped() {
    let ring = ring();
    let mut hub = RemoteHub::new();
    hub.set_drop_after(Some(OFFLINE_AFTER_SECS));
    for i in 0..MAX_ALIASES {
        hub.map_mut()
            .apply(&put_host(&ring, &format!("m{i:02}"), T0), &ring, T0, true);
    }
    assert_eq!(hub.map().aliases().len(), MAX_ALIASES);
    assert!(hub.map().admits("m00") && !hub.map().admits("late"));

    let late = payload("late", "3f9a1c2e", SessionState::Working, T0, T0);
    assert!(!hub
        .map_mut()
        .apply(&put_session(&ring, &late), &ring, T0, true));
    hub.map_mut()
        .apply(&put_host(&ring, "late", T0), &ring, T0, true);
    let put = put_project(&ring, &project("late", "p", 1, T0, T0), "c--p");
    hub.map_mut().apply(&put, &ring, T0, true);
    assert!(hub.map().is_empty() && hub.map().projects().is_empty());
    assert!(
        ring.keys.pinned("late").unwrap().is_none(),
        "not even opened"
    );
    assert_eq!(hub.map().over_alias_cap(), 3);
    assert_eq!(hub.map().undecryptable(), 0);
    let detail = hub.status(T0).detail.unwrap();
    assert!(
        detail.contains(&format!("more than {MAX_ALIASES} machines: 3 value(s)")),
        "{detail}"
    );
    assert_eq!(hub.map().machines(T0), MAX_ALIASES);

    // A held machine keeps updating.
    let beat = put_host(&ring, "m00", T0 + 60);
    assert!(hub.map_mut().apply(&beat, &ring, T0 + 60, true));

    // The others go silent and are dropped: their places free.
    hub.tick(T0 + 200);
    assert_eq!(hub.map().aliases(), ["m00"]);
    assert!(hub
        .map_mut()
        .apply(&put_session(&ring, &late), &ring, T0 + 200, true));
    assert_eq!(hub.map().aliases(), ["late", "m00"]);
}

/// Projects loaded from the file count against the cap too.
#[test]
fn projects_loaded_from_the_file_respect_the_machine_cap() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let stored: Vec<StoredProject> = (0..MAX_ALIASES + 3)
        .map(|i| {
            let alias = format!("m{i:02}");
            let p = project(&alias, "p", 1, T0, T0);
            let KvChange::Put { key, .. } = put_project(&ring, &p, "c--p") else {
                unreachable!()
            };
            StoredProject {
                key,
                alias,
                payload: p,
                heard: T0,
            }
        })
        .collect();
    assert_eq!(m.load_projects(stored, T0), (MAX_ALIASES, 0));
    assert_eq!(m.aliases().len(), MAX_ALIASES);
    assert_eq!(m.over_alias_cap(), 0, "the file is not the bucket");
}

/// The memory of logged keys stays bounded.
#[test]
fn logged_keys_are_bounded() {
    let mut m = RemoteMap::new();
    for i in 0..MAX_LOGGED_KEYS + 10 {
        m.log_once(&format!("k{i}"), "test");
        assert!(m.logged.len() <= MAX_LOGGED_KEYS);
    }
    assert!(m.logged.contains(&format!("k{}", MAX_LOGGED_KEYS + 9)));
    m.log_once("k0", "test");
    assert!(m.logged.contains("k0"));
}

/// The watch and the ticker write the projects file at most every
/// [`PROJECTS_SAVE_INTERVAL`]; a change held back stays pending, and a
/// forced write (restore, exit) goes at once.
#[test]
fn projects_writes_are_spaced_and_a_held_back_change_stays_pending() {
    let ring = ring();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REMOTE_PROJECTS_FILE);
    let mut hub = RemoteHub::new();
    hub.keep_projects_in(path.clone(), T0);
    let start = Instant::now();
    assert!(
        hub.projects_to_save(start, false).is_none(),
        "nothing changed"
    );

    let put = put_project(
        &ring,
        &project("devbox", "app", 100, T0 - 60, T0),
        "c--home-k-app",
    );
    hub.map_mut().apply(&put, &ring, T0, true);
    hub.projects_to_save(start, false)
        .expect("the first write goes")
        .run();
    assert!(path.is_file());

    let put = put_project(
        &ring,
        &project("devbox", "app", 200, T0, T0 + 1),
        "c--home-k-app",
    );
    hub.map_mut().apply(&put, &ring, T0 + 1, true);
    assert!(hub
        .projects_to_save(start + Duration::from_secs(1), false)
        .is_none());
    let due = hub.projects_to_save(start + PROJECTS_SAVE_INTERVAL, false);
    due.expect("pending, written once the interval passed")
        .run();
    let saved: Vec<StoredProject> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved[0].payload.tokens().input, 200);

    let put = put_project(
        &ring,
        &project("devbox", "app", 300, T0 + 2, T0 + 2),
        "c--home-k-app",
    );
    hub.map_mut().apply(&put, &ring, T0 + 2, true);
    let soon = start + PROJECTS_SAVE_INTERVAL + Duration::from_secs(1);
    assert!(hub.projects_to_save(soon, true).is_some(), "forced");
}

/// Of two writes handed out, the older one finishing last does not win.
#[test]
fn an_older_projects_write_finishing_last_is_skipped() {
    let ring = ring();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REMOTE_PROJECTS_FILE);
    let mut hub = RemoteHub::new();
    hub.keep_projects_in(path.clone(), T0);
    let now = Instant::now();
    let put = put_project(
        &ring,
        &project("devbox", "app", 100, T0 - 60, T0),
        "c--home-k-app",
    );
    hub.map_mut().apply(&put, &ring, T0, true);
    let older = hub.projects_to_save(now, true).unwrap();
    let put = put_project(
        &ring,
        &project("devbox", "app", 200, T0, T0 + 1),
        "c--home-k-app",
    );
    hub.map_mut().apply(&put, &ring, T0 + 1, true);
    let newer = hub.projects_to_save(now, true).unwrap();
    newer.run();
    older.run();
    let saved: Vec<StoredProject> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved[0].payload.tokens().input, 200);
}

/// The whole flow of a shared hub: [`flush_projects_of`] writes what is
/// pending at once, and nothing when nothing is.
#[test]
fn flushing_writes_only_what_is_pending() {
    let ring = ring();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REMOTE_PROJECTS_FILE);
    let hub = RemoteHub::shared();
    lock(&hub).keep_projects_in(path.clone(), T0);
    flush_projects_of(&hub);
    assert!(!path.exists());
    let put = put_project(
        &ring,
        &project("devbox", "app", 100, T0 - 60, T0),
        "c--home-k-app",
    );
    lock(&hub).map_mut().apply(&put, &ring, T0, true);
    assert!(lock(&hub).projects_to_save(Instant::now(), false).is_some());
    // Within the interval of the write just handed out: flushed anyway.
    let put = put_project(&ring, &project("devbox", "app", 5, T0, T0 + 1), "c--x");
    lock(&hub).map_mut().apply(&put, &ring, T0, true);
    flush_projects_of(&hub);
    assert!(path.is_file());
}

/// A put of `value` sealed under `key`, as a publisher would write it.
fn put_at<T: Serialize>(ring: &Ring, key: &str, value: &T) -> KvChange {
    KvChange::Put {
        bytes: ring.seal(value, key),
        key: key.to_string(),
        revision: 1,
    }
}

/// A project as a broken publisher might send it: a long name and `models`
/// models with long names, `n` input tokens each.
fn oversized_project(alias: &str, models: usize, n: u64) -> ProjectPayload {
    use cc_core::projects::ModelTokens;
    ProjectPayload {
        machine: alias.into(),
        name: "é".repeat(200),
        sessions: 1,
        models: (0..models)
            .map(|i| ModelTokens {
                model: format!("{i:02}{}", "m".repeat(70)),
                input: n,
                output: 0,
                cache_creation: 0,
                cache_read: 0,
            })
            .collect(),
        last: Some(T0),
        days: cc_core::projects::PROJECT_WINDOW_DAYS,
        hb: T0,
    }
}

/// What a project of [`oversized_project`] looks like once held.
fn assert_project_bounded(p: &ProjectPayload, models: usize, n: u64) {
    use cc_core::projects::{MAX_PROJECT_MODELS, MAX_PROJECT_NAME, OTHER_MODELS};
    assert_eq!(p.name.chars().count(), MAX_PROJECT_NAME);
    assert_eq!(p.models.len(), MAX_PROJECT_MODELS + 1);
    assert!(p.models[..MAX_PROJECT_MODELS]
        .iter()
        .all(|m| m.model.chars().count() == cc_core::session::MAX_REMOTE_MODEL));
    assert!(
        p.models[0].model.starts_with("00"),
        "the order sent is kept"
    );
    let other = p.models.last().unwrap();
    assert_eq!(other.model, OTHER_MODELS);
    assert_eq!(
        other.input,
        (models - MAX_PROJECT_MODELS) as u64 * n,
        "the rest is summed"
    );
    assert_eq!(p.tokens().input, models as u64 * n, "nothing is lost");
}

/// The tool and the model are cut like the texts, so a publisher sending
/// long ones still fits the row.
#[test]
fn a_long_tool_or_model_is_cut() {
    use cc_core::session::{MAX_REMOTE_MODEL, MAX_REMOTE_TOOL};
    let ring = ring();
    let mut m = RemoteMap::new();
    let mut p = payload("lab", "3f9a1c2e", SessionState::Working, T0, T0);
    p.tool = Some("T".repeat(500));
    p.model = Some(format!("claude-{}", "x".repeat(500)));
    p.text = Some("ls".into());
    m.apply(&put_session(&ring, &p), &ring, T0, true);
    let view = &m.rows(T0)[0].view;
    let tool = view.current_tool.as_deref().unwrap();
    assert_eq!(tool.chars().count(), MAX_REMOTE_TOOL + 1);
    assert!(tool.ends_with('…'));
    let model = view.model.as_deref().unwrap();
    assert_eq!(model.chars().count(), MAX_REMOTE_MODEL + 1);
    assert!(model.starts_with("claude-") && model.ends_with('…'));
    assert_eq!(view.tool_summary.as_deref(), Some("ls"));

    // A tool of white space only is no tool, and so has no summary.
    p.tool = Some(" \n ".into());
    p.hb = T0 + 1;
    m.apply(&put_session(&ring, &p), &ring, T0 + 1, true);
    let view = &m.rows(T0 + 1)[0].view;
    assert!(view.current_tool.is_none() && view.tool_summary.is_none());
}

/// The row goes by the sid of its key: a payload naming another sid
/// changes neither its id nor its name of last resort.
#[test]
fn the_row_goes_by_the_sid_of_its_key() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let mut p = payload("lab", "3f9a1c2e", SessionState::Idle, T0, T0);
    p.sid = "x".repeat(500);
    let key = session_key("lab", "3f9a1c2e");
    m.apply(&put_at(&ring, &key, &p), &ring, T0, true);
    let view = &m.rows(T0)[0].view;
    assert_eq!(view.id, "remote:lab.s.3f9a1c2e");
    assert_eq!(view.display_name, "3f9a1c2e");
}

/// A value that opens but names another machine than its key is rejected
/// like one that does not open: counted, not held, and a row or host it
/// would replace goes.
#[test]
fn a_value_naming_another_machine_is_rejected() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let session = session_key("lab", "3f9a1c2e");
    let good = payload("lab", "3f9a1c2e", SessionState::Working, T0, T0);
    m.apply(&put_session(&ring, &good), &ring, T0, true);
    m.apply(&put_host(&ring, "lab", T0), &ring, T0, true);
    assert_eq!((m.len(), m.machines(T0)), (1, 1));

    let forged = payload("devbox", "3f9a1c2e", SessionState::NeedsYou, T0, T0 + 1);
    assert!(m.apply(&put_at(&ring, &session, &forged), &ring, T0 + 1, true));
    assert!(m.is_empty(), "the row it would replace goes");
    let host = HostPayload {
        machine: "devbox".into(),
        version: "0.1.0".into(),
        os: "linux".into(),
        hb: T0 + 1,
    };
    assert!(m.apply(&put_at(&ring, &host_key("lab"), &host), &ring, T0 + 1, true));
    let project_key = cc_envelope::project_key("lab", &cc_envelope::pid_for("c--app"));
    let forged_project = project("devbox", "app", 1, T0, T0);
    m.apply(
        &put_at(&ring, &project_key, &forged_project),
        &ring,
        T0 + 1,
        true,
    );
    let activity = ActivityPayload {
        machine: "devbox".into(),
        days: Vec::new(),
        hb: T0,
    };
    m.apply(
        &put_at(&ring, &cc_envelope::activity_key("lab"), &activity),
        &ring,
        T0 + 1,
        true,
    );

    assert_eq!(m.undecryptable(), 4);
    assert_eq!(m.machines(T0 + 1), 0);
    assert!(m.projects().is_empty() && m.activity().is_empty());
    assert!(m.unknown_senders().is_empty(), "the sender key is fine");
}

/// A project heard live is held to what a publisher sends.
#[test]
fn a_live_project_is_cut_to_what_a_publisher_sends() {
    let ring = ring();
    let mut m = RemoteMap::new();
    let p = oversized_project("devbox", 16, 7);
    m.apply(&put_project(&ring, &p, "c--app"), &ring, T0, true);
    assert_project_bounded(&m.projects()[0].payload, 16, 7);

    // A real publisher's value comes out as it went in.
    let mut by_model = std::collections::HashMap::new();
    for i in 0..15u64 {
        by_model.insert(
            format!("claude-{i:02}"),
            cc_core::projects::Tokens {
                input: i + 1,
                ..Default::default()
            },
        );
    }
    let real = ProjectPayload::new("devbox", "web", 2, &by_model, Some(T0), T0);
    m.apply(&put_project(&ring, &real, "c--web"), &ring, T0, true);
    let held = m.projects();
    let held = held.iter().find(|p| p.payload.name == "web").unwrap();
    assert_eq!(held.payload, real);
}

/// The projects file is checked like the bucket: names and models cut,
/// and an entry whose payload names another machine is not loaded.
#[test]
fn projects_loaded_from_the_file_are_cut_like_live_ones() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REMOTE_PROJECTS_FILE);
    let entry = |alias: &str, payload: ProjectPayload| StoredProject {
        key: cc_envelope::project_key(alias, &cc_envelope::pid_for("c--app")),
        alias: alias.into(),
        payload,
        heard: T0,
    };
    let stored = vec![
        entry("devbox", oversized_project("devbox", 30, 3)),
        entry("lab", oversized_project("devbox", 1, 1)),
    ];
    std::fs::write(&path, serde_json::to_string(&stored).unwrap()).unwrap();
    let hub = RemoteHub::shared();
    lock(&hub).keep_projects_in(path, T0);
    let loaded = projects_of(&hub);
    assert_eq!(loaded.len(), 1, "the payload of lab names devbox");
    assert_eq!(loaded[0].alias, "devbox");
    assert_project_bounded(&loaded[0].payload, 30, 3);
}

/// A projects file over the limit is not parsed: it is kept aside, and
/// the next save writes a new one.
#[test]
fn an_oversized_projects_file_is_kept_aside() {
    let ring = ring();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REMOTE_PROJECTS_FILE);
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(super::store::MAX_PROJECTS_FILE_BYTES + 1)
        .unwrap();
    drop(file);
    let hub = RemoteHub::shared();
    lock(&hub).keep_projects_in(path.clone(), T0);
    assert!(projects_of(&hub).is_empty());
    assert!(!path.exists(), "moved out of the way");
    let aside: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("remote-projects.json.corrupt-"))
        .collect();
    assert_eq!(aside.len(), 1, "{aside:?}");

    let put = put_project(&ring, &project("devbox", "app", 100, T0 - 60, T0), "c--app");
    lock(&hub).map_mut().apply(&put, &ring, T0, true);
    flush_projects_of(&hub);
    assert!(path.is_file());

    // At the limit a file is still read: this one does not parse, so no
    // projects, and it stays where it is.
    std::fs::write(
        &path,
        vec![b' '; super::store::MAX_PROJECTS_FILE_BYTES as usize],
    )
    .unwrap();
    let next = RemoteHub::shared();
    lock(&next).keep_projects_in(path.clone(), T0);
    assert!(projects_of(&next).is_empty() && path.is_file());
}
