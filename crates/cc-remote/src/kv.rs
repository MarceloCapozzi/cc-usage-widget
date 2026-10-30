//! The two roles on the bucket: [`Publisher`] writes and deletes keys under
//! its own prefix, [`Viewer`] reads and watches everything.
//!
//! Values are opaque bytes; sealing and opening them is cc-envelope's job.
//!
//! The viewer's watch is an ordered push consumer created directly on the
//! bucket's stream (deliver policy *last per subject*) instead of
//! async-nats's `Store::watch_with_history`, so that this crate knows whether
//! the initial snapshot was empty and when it has been fully delivered. It
//! only uses JetStream subjects the viewer is allowed to publish to:
//! `$JS.API.STREAM.INFO.KV_<bucket>` to bind,
//! `$JS.API.CONSUMER.CREATE.KV_<bucket>[.>]` to watch,
//! `$JS.API.DIRECT.GET.KV_<bucket>.>` to get, and `$JS.FC.KV_<bucket>.>` for
//! flow control replies.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_nats::jetstream::{
    self,
    consumer::{
        push::{Ordered, OrderedConfig},
        DeliverPolicy, ReplayPolicy,
    },
    kv::Store,
};
use futures_util::{Stream, StreamExt};

use crate::client::Client;
use crate::error::{RemoteError, Result};

/// Deadline for each JetStream request (bind, put, delete, get, watch).
/// A request the server denies gets no reply, so it fails after this long.
pub const OP_TIMEOUT: Duration = Duration::from_secs(5);

/// Deadline for [`Viewer::snapshot`] to receive the whole bucket.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(15);

/// Consecutive watch errors, with no entry in between, after which a
/// [`KvWatch`] gives up and ends.
const MAX_CONSECUTIVE_WATCH_ERRORS: u32 = 8;

const KV_OPERATION: &str = "KV-Operation";
const NATS_MARKER_REASON: &str = "Nats-Marker-Reason";

/// One change seen on the bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KvChange {
    /// `key` now holds `bytes`. `revision` is the stream sequence of the write.
    Put {
        /// The key, without the `$KV.<bucket>.` prefix.
        key: String,
        /// The stored value (sealed bytes).
        bytes: Vec<u8>,
        /// Stream sequence of this write; grows with every write in the bucket.
        revision: u64,
    },
    /// `key` was deleted or purged, or the server removed it (TTL marker).
    Delete {
        /// The key, without the `$KV.<bucket>.` prefix.
        key: String,
    },
}

impl KvChange {
    /// The key this change is about.
    pub fn key(&self) -> &str {
        match self {
            KvChange::Put { key, .. } | KvChange::Delete { key } => key,
        }
    }
}

async fn bind_store(client: &Client, bucket: &str) -> Result<Store> {
    let mut js = jetstream::new(client.nats().clone());
    js.set_timeout(OP_TIMEOUT);
    js.get_key_value(bucket)
        .await
        .map_err(|err| RemoteError::nats("bind", err))
}

/// Writes keys to the bucket. A publisher may only write under its own
/// prefix (`<alias>.>`); anything else is denied and fails after
/// [`OP_TIMEOUT`].
#[derive(Debug, Clone)]
pub struct Publisher {
    store: Store,
}

impl Publisher {
    /// Binds to `bucket` (the [`Config::bucket`](crate::Config::bucket) the
    /// connection was made with); checks that it exists.
    pub async fn bind(client: &Client, bucket: &str) -> Result<Self> {
        Ok(Publisher {
            store: bind_store(client, bucket).await?,
        })
    }

    /// Stores `bytes` under `key` and returns the new revision once the
    /// server has acknowledged the write.
    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<u64> {
        self.store
            .put(key, bytes.into())
            .await
            .map_err(|err| RemoteError::nats("put", err))
    }

    /// Deletes `key` (writes a delete marker that watchers see).
    pub async fn delete(&self, key: &str) -> Result<()> {
        self.store
            .delete(key)
            .await
            .map_err(|err| RemoteError::nats("delete", err))
    }
}

/// Reads and watches the bucket. The viewer cannot write.
#[derive(Debug, Clone)]
pub struct Viewer {
    store: Store,
    client: async_nats::Client,
}

impl Viewer {
    /// Binds to `bucket` (the [`Config::bucket`](crate::Config::bucket) the
    /// connection was made with); checks that it exists.
    pub async fn bind(client: &Client, bucket: &str) -> Result<Self> {
        Ok(Viewer {
            store: bind_store(client, bucket).await?,
            client: client.nats().clone(),
        })
    }

    /// Watches every key: first the latest entry of each key (the initial
    /// snapshot), then live changes as they happen. See [`KvWatch`].
    pub async fn watch_all(&self) -> Result<KvWatch> {
        let consumer = self
            .store
            .stream
            .create_consumer(OrderedConfig {
                deliver_subject: self.client.new_inbox(),
                description: Some("cc-remote watch".to_string()),
                filter_subject: format!("{}>", self.store.prefix),
                replay_policy: ReplayPolicy::Instant,
                deliver_policy: DeliverPolicy::LastPerSubject,
                ..Default::default()
            })
            .await
            .map_err(|err| RemoteError::nats("watch", err))?;
        let initially_current = consumer.cached_info().num_pending == 0;
        let messages = consumer
            .messages()
            .await
            .map_err(|err| RemoteError::nats("watch", err))?;
        Ok(KvWatch {
            messages: Box::pin(messages),
            prefix: self.store.prefix.clone(),
            tracker: Tracker {
                current: initially_current,
            },
            consecutive_errors: 0,
            last_error: None,
            ended: false,
        })
    }

    /// The current value of every key, sorted by key. Opens a short-lived
    /// watch and stops once the initial snapshot is complete.
    pub async fn snapshot(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let mut watch = self.watch_all().await?;
        let mut values = BTreeMap::new();
        let collect = async {
            while !watch.is_current() {
                match watch.next().await {
                    Some(KvChange::Put { key, bytes, .. }) => {
                        values.insert(key, bytes);
                    }
                    Some(KvChange::Delete { key }) => {
                        values.remove(&key);
                    }
                    None => {
                        let reason = watch
                            .last_error()
                            .unwrap_or("watch ended before the snapshot was complete")
                            .to_string();
                        return Err(RemoteError::nats("snapshot", reason));
                    }
                }
            }
            Ok(())
        };
        tokio::time::timeout(SNAPSHOT_TIMEOUT, collect)
            .await
            .map_err(|_| RemoteError::Timeout {
                op: "snapshot",
                after: SNAPSHOT_TIMEOUT,
            })??;
        Ok(values.into_iter().collect())
    }

    /// The value of one key, or `None` if it does not exist or was deleted.
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.store
            .get(key)
            .await
            .map(|value| value.map(|bytes| bytes.to_vec()))
            .map_err(|err| RemoteError::nats("get", err))
    }
}

/// A live view of the bucket: a [`Stream`] of [`KvChange`].
///
/// It yields the initial snapshot first ([`KvWatch::is_current`] turns true
/// once its last entry has been yielded, or right away for an empty bucket),
/// then live changes. A key whose latest entry is a delete marker shows up
/// in the snapshot as a [`KvChange::Delete`], possibly for a key the caller
/// never saw, so treat a delete as "remove if present". Reconnects are handled underneath: after one, the watch
/// resumes from the last revision it delivered. Transient errors are logged
/// and skipped; the stream only ends when the connection is closed or
/// errors keep repeating, and then [`KvWatch::last_error`] says why.
///
/// Keys that expire through the bucket's TTL produce no event unless the
/// server writes TTL markers, so consumers should age out entries themselves.
pub struct KvWatch {
    messages: Pin<Box<Ordered>>,
    prefix: String,
    tracker: Tracker,
    consecutive_errors: u32,
    last_error: Option<String>,
    ended: bool,
}

impl KvWatch {
    /// True once the initial snapshot has been fully yielded; every change
    /// after that is live.
    pub fn is_current(&self) -> bool {
        self.tracker.current
    }

    /// The most recent error seen by the watch, if any.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn change_from(&mut self, message: &jetstream::Message) -> Result<KvChange, String> {
        let info = message
            .info()
            .map_err(|err| format!("bad message metadata: {err}"))?;
        self.tracker.observe(info.pending);
        let Some(key) = message.subject.as_str().strip_prefix(&self.prefix) else {
            return Err(format!("unexpected subject {}", message.subject));
        };
        let header = |name: &str| {
            message
                .headers
                .as_ref()
                .and_then(|h| h.get(name))
                .map(|v| v.as_str().to_string())
        };
        let op = operation(
            header(KV_OPERATION).as_deref(),
            header(NATS_MARKER_REASON).as_deref(),
        );
        Ok(change(op, key, &message.payload, info.stream_sequence))
    }
}

impl std::fmt::Debug for KvWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvWatch")
            .field("prefix", &self.prefix)
            .field("current", &self.tracker.current)
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl Stream for KvWatch {
    type Item = KvChange;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<KvChange>> {
        let this = &mut *self;
        if this.ended {
            return Poll::Ready(None);
        }
        loop {
            match this.messages.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.ended = true;
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Ok(message))) => match this.change_from(&message) {
                    Ok(change) => {
                        this.consecutive_errors = 0;
                        return Poll::Ready(Some(change));
                    }
                    Err(err) => this.record_error(err),
                },
                Poll::Ready(Some(Err(err))) => this.record_error(err.to_string()),
            }
            if this.consecutive_errors >= MAX_CONSECUTIVE_WATCH_ERRORS {
                tracing::warn!("watch gave up after repeated errors");
                this.ended = true;
                return Poll::Ready(None);
            }
        }
    }
}

impl KvWatch {
    fn record_error(&mut self, err: String) {
        tracing::warn!("watch error: {err}");
        self.consecutive_errors += 1;
        self.last_error = Some(err);
    }
}

/// What a stored message means for its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Put,
    Delete,
}

/// Reads the operation from the `KV-Operation` header (`DEL`, `PURGE`) or a
/// server marker (`Nats-Marker-Reason`: `MaxAge`, `Purge`, `Remove`).
fn operation(kv_operation: Option<&str>, marker_reason: Option<&str>) -> Operation {
    match kv_operation {
        Some("DEL") | Some("PURGE") => Operation::Delete,
        Some(_) => Operation::Put,
        None if marker_reason.is_some() => Operation::Delete,
        None => Operation::Put,
    }
}

/// Turns one stored message into the change it represents.
fn change(op: Operation, key: &str, payload: &[u8], revision: u64) -> KvChange {
    match op {
        Operation::Put => KvChange::Put {
            key: key.to_string(),
            bytes: payload.to_vec(),
            revision,
        },
        Operation::Delete => KvChange::Delete {
            key: key.to_string(),
        },
    }
}

/// Tracks whether the initial snapshot is over. Every message yields a
/// change, so a caller waiting for [`KvWatch::is_current`] is never left
/// blocked after the last snapshot entry.
#[derive(Debug, Clone)]
struct Tracker {
    current: bool,
}

impl Tracker {
    /// Records a delivered message with `pending` messages still to come.
    fn observe(&mut self, pending: u64) {
        if pending == 0 {
            self.current = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_comes_from_kv_header_or_marker() {
        assert_eq!(operation(None, None), Operation::Put);
        assert_eq!(operation(Some("DEL"), None), Operation::Delete);
        assert_eq!(operation(Some("PURGE"), None), Operation::Delete);
        assert_eq!(operation(None, Some("MaxAge")), Operation::Delete);
        assert_eq!(operation(None, Some("Remove")), Operation::Delete);
        assert_eq!(operation(Some("PUT"), None), Operation::Put);
    }

    #[test]
    fn messages_map_to_changes() {
        assert_eq!(
            change(Operation::Put, "a.s.2", b"x", 5),
            KvChange::Put {
                key: "a.s.2".into(),
                bytes: b"x".to_vec(),
                revision: 5
            }
        );
        assert_eq!(
            change(Operation::Delete, "a.s.2", b"", 7),
            KvChange::Delete {
                key: "a.s.2".into()
            }
        );
    }

    #[test]
    fn snapshot_is_current_after_last_pending_message() {
        let mut t = Tracker { current: false };
        t.observe(2);
        t.observe(1);
        assert!(!t.current);
        t.observe(0);
        assert!(t.current);
        // Live messages keep it current.
        t.observe(0);
        assert!(t.current);
    }

    #[test]
    fn empty_snapshot_is_current_from_the_start() {
        let mut t = Tracker { current: true };
        t.observe(0);
        assert!(t.current);
    }

    #[test]
    fn change_key_accessor() {
        assert_eq!(KvChange::Delete { key: "k".into() }.key(), "k");
        let put = KvChange::Put {
            key: "p".into(),
            bytes: vec![],
            revision: 1,
        };
        assert_eq!(put.key(), "p");
    }
}
