//! What to write to the bucket, decided once per gathering pass.
//!
//! The [`Planner`] remembers the last payload it handed to the [`Sink`] for
//! every session key and when, and on each pass:
//!
//! - puts a session whose payload changed (any field but `hb`: state,
//!   since, tool, model, ctx, title, folder, text, agent counts),
//! - re-puts a session whose last put is [`HEARTBEAT_SECS`] old, so its `hb`
//!   stays fresh and the bucket's 3-minute TTL never removes a live session,
//! - deletes the key of a session that left the tracker,
//! - puts the host record (`<alias>.host`) on the first pass and every
//!   [`HEARTBEAT_SECS`].
//!
//! The planner never touches the network: the sink is the [`crate::outbox`]
//! in production and a recording fake in tests.

use std::collections::BTreeMap;

use cc_core::session::RemotePayload;

/// Seconds between heartbeats of a session and of the host record.
pub const HEARTBEAT_SECS: f64 = 60.0;

/// Where the planner's decisions go.
pub trait Sink {
    /// Store `payload` (to be sealed) under the session key `key`.
    fn put(&mut self, key: &str, payload: &RemotePayload);
    /// Delete the session key `key`.
    fn delete(&mut self, key: &str);
    /// Store a fresh host record under `key`.
    fn put_host(&mut self, key: &str);
}

/// How many decisions one pass produced.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Puts because the payload changed (or the session is new).
    pub changed: usize,
    /// Puts because the heartbeat was due.
    pub heartbeats: usize,
    /// Deletes of sessions that ended.
    pub deletes: usize,
    /// Whether the host record was put.
    pub host: bool,
}

impl Plan {
    /// True when the pass produced nothing to send.
    pub fn is_empty(&self) -> bool {
        self.changed == 0 && self.heartbeats == 0 && self.deletes == 0 && !self.host
    }
}

#[derive(Debug, Clone)]
struct Sent {
    /// The payload with `hb` zeroed, to compare content only; `None` for a
    /// key adopted from a previous run, whose content is unknown.
    content: Option<RemotePayload>,
    /// Unix seconds of the decision.
    at: f64,
}

/// Decides puts, heartbeats and deletes. See the module docs.
#[derive(Debug, Clone)]
pub struct Planner {
    alias: String,
    sent: BTreeMap<String, Sent>,
    host_at: Option<f64>,
}

impl Planner {
    /// A planner for the machine `alias` that has sent nothing yet.
    pub fn new(alias: &str) -> Self {
        Planner {
            alias: alias.to_string(),
            sent: BTreeMap::new(),
            host_at: None,
        }
    }

    /// The session keys currently believed to be in the bucket.
    pub fn keys(&self) -> Vec<String> {
        self.sent.keys().cloned().collect()
    }

    /// Takes over session keys a previous run published (from its state
    /// file), which may still be in the bucket: the next [`Planner::plan`]
    /// deletes those that are not among its payloads, so the sessions that
    /// ended while no publisher ran leave the viewer at once instead of
    /// after the bucket's TTL, and re-puts the others. Keys this planner
    /// already tracks are left alone.
    pub fn adopt(&mut self, keys: impl IntoIterator<Item = String>) {
        for key in keys {
            self.sent.entry(key).or_insert(Sent {
                content: None,
                at: f64::NEG_INFINITY,
            });
        }
    }

    /// Makes every session and the host record due on the next pass, e.g.
    /// after a reconnect, when the bucket may have expired them meanwhile.
    pub fn force_resync(&mut self) {
        for sent in self.sent.values_mut() {
            sent.at = f64::NEG_INFINITY;
        }
        self.host_at = None;
    }

    /// One pass over the current payloads (as produced by
    /// [`cc_local::monitor::SessionMonitor::payloads`]) at `now`.
    pub fn plan(&mut self, current: &[RemotePayload], now: f64, sink: &mut impl Sink) -> Plan {
        let mut plan = Plan::default();
        let mut seen = BTreeMap::new();
        for payload in current {
            let key = cc_envelope::session_key(&self.alias, &payload.sid);
            let mut content = payload.clone();
            content.hb = 0;
            seen.insert(key.clone(), ());
            let due = match self.sent.get(&key) {
                None => Some(true),
                Some(prev) if prev.content.as_ref() != Some(&content) => Some(true),
                Some(prev) if now - prev.at >= HEARTBEAT_SECS => Some(false),
                Some(_) => None,
            };
            if let Some(changed) = due {
                sink.put(&key, payload);
                self.sent.insert(
                    key,
                    Sent {
                        content: Some(content),
                        at: now,
                    },
                );
                if changed {
                    plan.changed += 1;
                } else {
                    plan.heartbeats += 1;
                }
            }
        }
        let gone: Vec<String> = self
            .sent
            .keys()
            .filter(|k| !seen.contains_key(*k))
            .cloned()
            .collect();
        for key in gone {
            sink.delete(&key);
            self.sent.remove(&key);
            plan.deletes += 1;
        }
        if self.host_at.is_none_or(|t| now - t >= HEARTBEAT_SECS) {
            sink.put_host(&cc_envelope::host_key(&self.alias));
            self.host_at = Some(now);
            plan.host = true;
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::session::SessionState;

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Put(String, SessionState),
        Delete(String),
        Host(String),
    }

    #[derive(Default)]
    struct FakeSink(Vec<Call>);

    impl Sink for FakeSink {
        fn put(&mut self, key: &str, payload: &RemotePayload) {
            self.0.push(Call::Put(key.into(), payload.state));
        }
        fn delete(&mut self, key: &str) {
            self.0.push(Call::Delete(key.into()));
        }
        fn put_host(&mut self, key: &str) {
            self.0.push(Call::Host(key.into()));
        }
    }

    impl FakeSink {
        fn take(&mut self) -> Vec<Call> {
            std::mem::take(&mut self.0)
        }
    }

    fn payload(session: &str, state: SessionState, now: f64) -> RemotePayload {
        RemotePayload {
            machine: "lab-01".into(),
            sid: cc_envelope::sid_for(session),
            alias: None,
            title: None,
            dir: None,
            text: None,
            agents: None,
            ags: None,
            state,
            since: 100,
            tool: None,
            model: Some("claude-opus-5-5".into()),
            ctx: Some(10),
            hb: now as i64,
        }
    }

    fn key(session: &str) -> String {
        format!("lab-01.s.{}", cc_envelope::sid_for(session))
    }

    #[test]
    fn first_pass_puts_every_session_and_the_host() {
        let mut p = Planner::new("lab-01");
        let mut sink = FakeSink::default();
        let plan = p.plan(
            &[
                payload("a", SessionState::Working, 0.0),
                payload("b", SessionState::Idle, 0.0),
            ],
            0.0,
            &mut sink,
        );
        assert_eq!(
            plan,
            Plan {
                changed: 2,
                heartbeats: 0,
                deletes: 0,
                host: true
            }
        );
        let calls = sink.take();
        assert!(calls.contains(&Call::Put(key("a"), SessionState::Working)));
        assert!(calls.contains(&Call::Put(key("b"), SessionState::Idle)));
        assert!(calls.contains(&Call::Host("lab-01.host".into())));
        assert_eq!(p.keys().len(), 2);
    }

    #[test]
    fn unchanged_payloads_wait_for_the_heartbeat() {
        let mut p = Planner::new("lab-01");
        let mut sink = FakeSink::default();
        p.plan(&[payload("a", SessionState::Idle, 0.0)], 0.0, &mut sink);
        sink.take();
        // Only hb moves: nothing to send for a minute.
        for t in [2.0, 4.0, 30.0, 59.9] {
            let plan = p.plan(&[payload("a", SessionState::Idle, t)], t, &mut sink);
            assert!(plan.is_empty(), "at {t}: {plan:?}");
        }
        assert!(sink.take().is_empty());
        let plan = p.plan(&[payload("a", SessionState::Idle, 60.0)], 60.0, &mut sink);
        assert_eq!(
            plan,
            Plan {
                changed: 0,
                heartbeats: 1,
                deletes: 0,
                host: true
            }
        );
        assert_eq!(
            sink.take(),
            vec![
                Call::Put(key("a"), SessionState::Idle),
                Call::Host("lab-01.host".into())
            ]
        );
        // The heartbeat restarts the clock.
        assert!(p
            .plan(&[payload("a", SessionState::Idle, 62.0)], 62.0, &mut sink)
            .is_empty());
    }

    #[test]
    fn a_change_is_put_at_once_and_resets_the_heartbeat() {
        let mut p = Planner::new("lab-01");
        let mut sink = FakeSink::default();
        p.plan(&[payload("a", SessionState::Idle, 0.0)], 0.0, &mut sink);
        sink.take();
        let plan = p.plan(&[payload("a", SessionState::Working, 2.0)], 2.0, &mut sink);
        assert_eq!(plan.changed, 1);
        assert_eq!(
            sink.take(),
            vec![Call::Put(key("a"), SessionState::Working)]
        );

        for change in [
            |x: &mut RemotePayload| x.tool = Some("Bash".into()),
            |x: &mut RemotePayload| x.model = Some("claude-haiku-5".into()),
            |x: &mut RemotePayload| x.ctx = Some(11),
            |x: &mut RemotePayload| x.since = 101,
        ] {
            let mut next = payload("a", SessionState::Working, 4.0);
            change(&mut next);
            // Apply on a fresh copy of the planner so each field is tested alone.
            let mut q = p.clone();
            assert_eq!(q.plan(&[next], 4.0, &mut sink).changed, 1);
        }
        sink.take();
        // Heartbeat counts from the change at t=2, not from t=0.
        assert!(
            p.plan(
                &[payload("a", SessionState::Working, 61.0)],
                61.0,
                &mut sink
            )
            .heartbeats
                == 0
        );
        assert_eq!(
            p.plan(
                &[payload("a", SessionState::Working, 62.0)],
                62.0,
                &mut sink
            )
            .heartbeats,
            1
        );
    }

    #[test]
    fn a_session_that_leaves_is_deleted_once() {
        let mut p = Planner::new("lab-01");
        let mut sink = FakeSink::default();
        p.plan(
            &[
                payload("a", SessionState::Idle, 0.0),
                payload("b", SessionState::Idle, 0.0),
            ],
            0.0,
            &mut sink,
        );
        sink.take();
        let plan = p.plan(&[payload("b", SessionState::Idle, 2.0)], 2.0, &mut sink);
        assert_eq!(plan.deletes, 1);
        assert_eq!(sink.take(), vec![Call::Delete(key("a"))]);
        assert!(p
            .plan(&[payload("b", SessionState::Idle, 4.0)], 4.0, &mut sink)
            .is_empty());
        assert_eq!(p.keys(), vec![key("b")]);
        // Coming back is a new put.
        let plan = p.plan(
            &[
                payload("a", SessionState::Idle, 6.0),
                payload("b", SessionState::Idle, 6.0),
            ],
            6.0,
            &mut sink,
        );
        assert_eq!(plan.changed, 1);
    }

    #[test]
    fn the_host_heartbeats_every_minute_even_without_sessions() {
        let mut p = Planner::new("lab-01");
        let mut sink = FakeSink::default();
        let mut hosts = 0;
        let mut t = 0.0;
        while t <= 300.0 {
            if p.plan(&[], t, &mut sink).host {
                hosts += 1;
            }
            t += 2.0;
        }
        assert_eq!(hosts, 6, "t = 0, 60, 120, 180, 240, 300");
        assert!(sink
            .take()
            .iter()
            .all(|c| *c == Call::Host("lab-01.host".into())));
    }

    #[test]
    fn keys_of_a_previous_run_that_are_no_longer_live_are_deleted() {
        let mut p = Planner::new("lab-01");
        let mut sink = FakeSink::default();
        // The previous run published "a" and "gone"; only "a" still runs.
        p.adopt([key("a"), key("gone")]);
        let plan = p.plan(
            &[
                payload("a", SessionState::Idle, 0.0),
                payload("new", SessionState::Working, 0.0),
            ],
            0.0,
            &mut sink,
        );
        assert_eq!(
            plan,
            Plan {
                changed: 2,
                heartbeats: 0,
                deletes: 1,
                host: true
            }
        );
        let calls = sink.take();
        assert!(calls.contains(&Call::Delete(key("gone"))), "{calls:?}");
        assert!(
            calls.contains(&Call::Put(key("a"), SessionState::Idle)),
            "re-put"
        );
        assert!(calls.contains(&Call::Put(key("new"), SessionState::Working)));
        let mut keys = p.keys();
        keys.sort();
        let mut want = vec![key("a"), key("new")];
        want.sort();
        assert_eq!(keys, want);

        // Adopting a key it already tracks changes nothing.
        p.adopt([key("a")]);
        assert!(p
            .plan(
                &[
                    payload("a", SessionState::Idle, 2.0),
                    payload("new", SessionState::Working, 2.0),
                ],
                2.0,
                &mut sink
            )
            .is_empty());
    }

    #[test]
    fn resync_makes_everything_due() {
        let mut p = Planner::new("lab-01");
        let mut sink = FakeSink::default();
        p.plan(&[payload("a", SessionState::Idle, 0.0)], 0.0, &mut sink);
        sink.take();
        p.force_resync();
        let plan = p.plan(&[payload("a", SessionState::Idle, 2.0)], 2.0, &mut sink);
        assert_eq!(
            plan,
            Plan {
                changed: 0,
                heartbeats: 1,
                deletes: 0,
                host: true
            }
        );
    }
}
