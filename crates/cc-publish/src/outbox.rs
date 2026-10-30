//! The hand-off between the gathering thread and the network task.
//!
//! The [`Outbox`] holds at most one pending operation per KV key, and the
//! newest one wins: a put that has not gone out yet is replaced by the next
//! put or by a delete of the same key. The gathering loop only ever takes a
//! short lock to record a decision, so a slow or broken network never holds
//! it up, and the memory used is bounded by the number of sessions.
//!
//! [`flush`] sends everything pending. Payloads are sealed at send time with
//! `hb` set to that moment, so a value delayed by an outage never claims to be
//! older or newer than it is. On the first failure the failed operation and
//! everything not yet sent go back into the outbox (unless a newer operation
//! for the same key arrived meanwhile), and the flush stops: the network is
//! probably down, and every further request would only wait for its timeout.

use std::collections::BTreeMap;
use std::sync::Mutex;

use cc_core::projects::{ActivityPayload, ProjectPayload};
use cc_core::session::RemotePayload;
use cc_envelope::{HostPayload, Sealer};

use crate::planner::Sink;
use crate::projects::ProjectSink;

/// One pending operation on a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Put a session payload (sealed at send time).
    Put(RemotePayload),
    /// Delete the key.
    Delete,
    /// Put a fresh host record.
    Host,
    /// Put one project's usage (sealed at send time).
    Project(ProjectPayload),
    /// Put the machine's activity (sealed at send time).
    Activity(ActivityPayload),
}

/// Pending operations, newest per key. See the module docs.
#[derive(Debug, Default)]
pub struct Outbox {
    ops: Mutex<BTreeMap<String, Op>>,
    notify: tokio::sync::Notify,
}

impl Outbox {
    /// An empty outbox.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Op>> {
        // A panic while holding the lock cannot leave the map inconsistent.
        self.ops.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records `op` for `key`, replacing any pending one, and wakes the sender.
    pub fn push(&self, key: &str, op: Op) {
        self.lock().insert(key.to_string(), op);
        self.notify.notify_one();
    }

    /// Takes every pending operation, in key order.
    pub fn take(&self) -> Vec<(String, Op)> {
        std::mem::take(&mut *self.lock()).into_iter().collect()
    }

    /// Puts back an operation that could not be sent, unless a newer one for
    /// the same key arrived meanwhile.
    pub fn requeue(&self, key: String, op: Op) {
        self.lock().entry(key).or_insert(op);
    }

    /// Keys with a pending delete: still in the bucket as far as this run
    /// knows, so they belong in the persisted list of published keys.
    pub fn pending_deletes(&self) -> Vec<String> {
        self.lock()
            .iter()
            .filter(|(_, op)| **op == Op::Delete)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Number of pending operations.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// True when nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Resolves after the next [`Outbox::push`] (or at once if one happened
    /// since the last wait).
    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

impl Sink for &Outbox {
    fn put(&mut self, key: &str, payload: &RemotePayload) {
        self.push(key, Op::Put(payload.clone()));
    }

    fn delete(&mut self, key: &str) {
        self.push(key, Op::Delete);
    }

    fn put_host(&mut self, key: &str) {
        self.push(key, Op::Host);
    }
}

impl ProjectSink for &Outbox {
    fn put_project(&mut self, key: &str, payload: &ProjectPayload) {
        self.push(key, Op::Project(payload.clone()));
    }

    fn delete_project(&mut self, key: &str) {
        self.push(key, Op::Delete);
    }

    fn put_activity(&mut self, key: &str, payload: &ActivityPayload) {
        self.push(key, Op::Activity(payload.clone()));
    }
}

/// The two bucket operations a flush needs; implemented by
/// [`cc_remote::Publisher`] and by fakes in tests.
#[allow(async_fn_in_trait)]
pub trait Transport {
    /// Stores `bytes` under `key`.
    async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), String>;
    /// Deletes `key`.
    async fn delete(&self, key: &str) -> Result<(), String>;
}

impl Transport for cc_remote::Publisher {
    async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), String> {
        cc_remote::Publisher::put(self, key, bytes)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        cc_remote::Publisher::delete(self, key)
            .await
            .map_err(|e| e.to_string())
    }
}

/// What one [`flush`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FlushReport {
    /// Session puts acknowledged.
    pub puts: u64,
    /// Deletes acknowledged.
    pub deletes: u64,
    /// Host puts acknowledged.
    pub host_puts: u64,
    /// Project and activity puts acknowledged.
    pub project_puts: u64,
    /// Operations dropped because they could not be sealed (never retried).
    pub dropped: u64,
    /// The error that stopped the flush; everything unsent was requeued.
    pub error: Option<String>,
}

impl FlushReport {
    /// Total operations acknowledged by the server.
    pub fn sent(&self) -> u64 {
        self.puts + self.deletes + self.host_puts + self.project_puts
    }
}

/// The host record for `alias` at `now`.
pub fn host_payload(alias: &str, now: f64) -> HostPayload {
    HostPayload {
        machine: alias.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        hb: now.floor() as i64,
    }
}

/// Seals what an operation stores, or `None` for a delete.
fn seal_op(
    key: &str,
    op: &Op,
    alias: &str,
    sealer: &Sealer,
    now: f64,
) -> Option<Result<Vec<u8>, cc_envelope::EnvelopeError>> {
    match op {
        Op::Put(payload) => {
            let mut payload = payload.clone();
            payload.hb = now.floor() as i64;
            Some(sealer.seal(&payload, key))
        }
        Op::Host => Some(sealer.seal(&host_payload(alias, now), key)),
        Op::Project(payload) => {
            let mut payload = payload.clone();
            payload.hb = now.floor() as i64;
            Some(sealer.seal(&payload, key))
        }
        Op::Activity(payload) => {
            let mut payload = payload.clone();
            payload.hb = now.floor() as i64;
            Some(sealer.seal(&payload, key))
        }
        Op::Delete => None,
    }
}

/// Sends every pending operation. See the module docs for the failure rules.
/// `now` gives the Unix time used for each `hb`.
pub async fn flush<T: Transport>(
    outbox: &Outbox,
    transport: &T,
    sealer: &Sealer,
    alias: &str,
    now: impl Fn() -> f64,
) -> FlushReport {
    let mut report = FlushReport::default();
    let mut pending = outbox.take().into_iter();
    while let Some((key, op)) = pending.next() {
        let result = match seal_op(&key, &op, alias, sealer, now()) {
            Some(Ok(bytes)) => transport.put(&key, bytes).await,
            Some(Err(err)) => {
                tracing::error!(%key, "cannot seal value, dropped: {err}");
                report.dropped += 1;
                continue;
            }
            None => transport.delete(&key).await,
        };
        match result {
            Ok(()) => match op {
                Op::Put(_) => report.puts += 1,
                Op::Delete => report.deletes += 1,
                Op::Host => report.host_puts += 1,
                Op::Project(_) | Op::Activity(_) => report.project_puts += 1,
            },
            Err(err) => {
                outbox.requeue(key, op);
                for (key, op) in pending.by_ref() {
                    outbox.requeue(key, op);
                }
                report.error = Some(err);
                break;
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::session::SessionState;
    use std::cell::RefCell;

    /// Records calls; fails every call once `fail_after` calls succeeded.
    #[derive(Default)]
    struct FakeTransport {
        calls: RefCell<Vec<(String, Option<Vec<u8>>)>>,
        fail_after: Option<usize>,
    }

    impl FakeTransport {
        fn record(&self, key: &str, bytes: Option<Vec<u8>>) -> Result<(), String> {
            let mut calls = self.calls.borrow_mut();
            if self.fail_after.is_some_and(|n| calls.len() >= n) {
                return Err("put timed out after 5s".into());
            }
            calls.push((key.to_string(), bytes));
            Ok(())
        }
    }

    impl Transport for FakeTransport {
        async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), String> {
            self.record(key, Some(bytes))
        }
        async fn delete(&self, key: &str) -> Result<(), String> {
            self.record(key, None)
        }
    }

    fn payload(state: SessionState) -> RemotePayload {
        RemotePayload {
            machine: "lab-01".into(),
            sid: "3f9a1c2e".into(),
            alias: None,
            title: None,
            dir: None,
            text: None,
            agents: None,
            ags: None,
            state,
            since: 5,
            tool: None,
            model: None,
            ctx: None,
            hb: 1,
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn the_newest_operation_per_key_wins() {
        let outbox = Outbox::new();
        outbox.push("lab-01.s.3f9a1c2e", Op::Put(payload(SessionState::Working)));
        outbox.push("lab-01.s.3f9a1c2e", Op::Put(payload(SessionState::Done)));
        outbox.push("lab-01.host", Op::Host);
        assert_eq!(outbox.len(), 2);
        assert!(outbox.pending_deletes().is_empty());
        outbox.push("lab-01.s.3f9a1c2e", Op::Delete);
        assert_eq!(outbox.pending_deletes(), ["lab-01.s.3f9a1c2e"]);
        let ops = outbox.take();
        assert_eq!(
            ops,
            vec![
                ("lab-01.host".to_string(), Op::Host),
                ("lab-01.s.3f9a1c2e".to_string(), Op::Delete)
            ]
        );
        assert!(outbox.is_empty());
    }

    /// A publisher's sealer and the viewer keys that open what it seals.
    fn sealer() -> (Sealer, cc_envelope::ViewerKeys) {
        let viewer = cc_envelope::SecretKeys::generate("k1").unwrap();
        let sealer = Sealer::new(
            cc_envelope::SecretKeys::generate("p1").unwrap(),
            viewer.public_keys(),
        );
        (sealer, cc_envelope::ViewerKeys::new(viewer, None))
    }

    #[test]
    fn flush_seals_with_a_fresh_heartbeat() {
        use cc_envelope::OpenKeys;
        let (sealer, keys) = sealer();
        let outbox = Outbox::new();
        let key = "lab-01.s.3f9a1c2e";
        outbox.push(key, Op::Put(payload(SessionState::Working)));
        outbox.push("lab-01.host", Op::Host);
        let t = FakeTransport::default();
        let report = block_on(flush(&outbox, &t, &sealer, "lab-01", || 1000.5));
        assert_eq!(report.puts, 1);
        assert_eq!(report.host_puts, 1);
        assert_eq!(report.error, None);
        assert!(outbox.is_empty());
        let calls = t.calls.borrow();
        let host: HostPayload = keys
            .open_any(calls[0].1.as_ref().unwrap(), "lab-01.host")
            .unwrap()
            .value;
        assert_eq!(host.hb, 1000);
        assert_eq!(host.machine, "lab-01");
        assert_eq!(host.os, std::env::consts::OS);
        let session: RemotePayload = keys
            .open_any(calls[1].1.as_ref().unwrap(), key)
            .unwrap()
            .value;
        assert_eq!(
            session.hb, 1000,
            "hb is the send time, not the decision time"
        );
        assert_eq!(session.state, SessionState::Working);
    }

    #[test]
    fn flush_seals_version_2_that_only_the_viewer_opens() {
        use cc_envelope::{OpenKeys, Opened, SecretKeys, ViewerKeys};
        let (sealer, keys) = sealer();
        let outbox = Outbox::new();
        let key = "lab-01.s.3f9a1c2e";
        outbox.push(key, Op::Put(payload(SessionState::Working)));
        outbox.push("lab-01.host", Op::Host);
        let t = FakeTransport::default();
        let report = block_on(flush(&outbox, &t, &sealer, "lab-01", || 1000.5));
        assert_eq!(report.sent(), 2);
        let calls = t.calls.borrow();
        let host: Opened<HostPayload> = keys
            .open_any(calls[0].1.as_ref().unwrap(), "lab-01.host")
            .unwrap();
        assert_eq!(host.value.hb, 1000);
        assert!(host.pinned.is_some(), "first value of lab-01 pins its key");
        let session: Opened<RemotePayload> =
            keys.open_any(calls[1].1.as_ref().unwrap(), key).unwrap();
        assert_eq!(session.value.state, SessionState::Working);
        assert!(session.pinned.is_none());
        // Another viewer key, even the publisher's own, opens nothing.
        let elsewhere = ViewerKeys::new(SecretKeys::generate("k1").unwrap(), None);
        assert!(elsewhere
            .open_any::<RemotePayload>(calls[1].1.as_ref().unwrap(), key)
            .is_err());
        let own = ViewerKeys::new(sealer.sender.clone(), None);
        assert!(own
            .open_any::<RemotePayload>(calls[1].1.as_ref().unwrap(), key)
            .is_err());
    }

    #[test]
    fn a_failure_requeues_the_rest_and_newer_ops_win() {
        let (sealer, _) = sealer();
        let outbox = Outbox::new();
        outbox.push("a.s.00000001", Op::Put(payload(SessionState::Idle)));
        outbox.push("a.s.00000002", Op::Delete);
        outbox.push("a.s.00000003", Op::Put(payload(SessionState::Idle)));
        let t = FakeTransport {
            fail_after: Some(1),
            ..Default::default()
        };
        let report = block_on(flush(&outbox, &t, &sealer, "a", || 1.0));
        assert_eq!(report.sent(), 1);
        assert!(report.error.as_deref().unwrap().contains("timed out"));
        assert_eq!(outbox.len(), 2, "the failed op and the unsent one are back");

        // A newer decision made during the outage replaces the requeued one.
        outbox.requeue("a.s.00000003".into(), Op::Delete);
        let ops = outbox.take();
        assert_eq!(ops[0], ("a.s.00000002".to_string(), Op::Delete));
        assert_eq!(
            ops[1].1,
            Op::Put(payload(SessionState::Idle)),
            "requeue never overrides"
        );

        outbox.push("a.s.00000002", Op::Put(payload(SessionState::Working)));
        outbox.requeue("a.s.00000002".into(), Op::Delete);
        assert_eq!(outbox.take()[0].1, Op::Put(payload(SessionState::Working)));
    }

    #[test]
    fn the_outbox_works_as_the_planners_sink() {
        let outbox = Outbox::new();
        let mut planner = crate::planner::Planner::new("lab-01");
        let mut p = payload(SessionState::Working);
        p.sid = cc_envelope::sid_for("s1");
        planner.plan(&[p], 0.0, &mut &outbox);
        assert_eq!(outbox.len(), 2, "session and host");
        outbox.take();
        planner.plan(&[], 2.0, &mut &outbox);
        assert_eq!(
            outbox.take(),
            vec![(
                format!("lab-01.s.{}", cc_envelope::sid_for("s1")),
                Op::Delete
            )]
        );
    }
}
