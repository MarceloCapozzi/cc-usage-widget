//! Connecting to Synadia Cloud with a user's `.creds` file.
//!
//! Every connection uses the inbox prefix `_INBOX.<user>`, the only inbox its
//! permissions allow, so JetStream replies actually reach it. The server list
//! may mix `tls://` and `wss://` URLs: async-nats cannot mix both in one pool,
//! so [`connect`] tries each transport in the order it first appears and keeps
//! the first one that connects. Only one connection is open at any time.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::{ConnectOptions, Event};
use tokio::sync::broadcast;

use crate::error::{RemoteError, Result};

/// The servers used when the configuration names none, for every client in
/// this repository (the widget, its Publisher role and cc-publish): Synadia
/// Cloud over WebSocket on port 443 first, since corporate networks often
/// block 4222, then native TLS.
pub const DEFAULT_SERVERS: [&str; 2] = ["wss://connect.ngs.global", "tls://connect.ngs.global"];

/// The bucket used when the configuration names none, [`cc_core::BUCKET`].
pub const DEFAULT_BUCKET: &str = cc_core::BUCKET;

/// [`DEFAULT_SERVERS`] as owned strings.
pub fn default_servers() -> Vec<String> {
    DEFAULT_SERVERS.iter().map(|s| s.to_string()).collect()
}

/// How long one connection attempt on one server may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound for the delay between reconnect attempts.
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// How long [`Client::close`] waits for buffered messages to flush.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Capacity of the connection event channel; slow listeners miss old events.
const EVENT_CAPACITY: usize = 64;

/// What [`connect`] needs to open a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Server URLs, e.g. [`DEFAULT_SERVERS`]: scheme, host and optional
    /// port. Only `tls://` and `wss://` are accepted (plaintext `nats://` and
    /// `ws://` are refused, see [`validate_servers`]); transports are tried
    /// in order.
    pub servers: Vec<String>,
    /// The KV bucket the caller binds, e.g. [`DEFAULT_BUCKET`] (see
    /// [`validate_bucket`]). [`connect`] only checks it; the caller passes it
    /// to [`Viewer::bind`](crate::Viewer::bind) or
    /// [`Publisher::bind`](crate::Publisher::bind).
    pub bucket: String,
    /// Path to the user's `.creds` file.
    pub creds_path: PathBuf,
    /// NATS user name, e.g. `cc-sessions.viewer`. Sets the inbox prefix.
    pub user: String,
    /// Connection name shown in the server's connection list.
    pub name: String,
}

impl Config {
    /// The standard config for `user`: [`DEFAULT_SERVERS`],
    /// [`DEFAULT_BUCKET`], the creds file in [`cc_core::paths::nats_dir`]
    /// and the name `cc-usage-<user>`.
    pub fn for_user(user: &str) -> Self {
        Config {
            servers: default_servers(),
            bucket: DEFAULT_BUCKET.to_string(),
            creds_path: cc_core::paths::nats_dir().join(format!("{user}.creds")),
            user: user.to_string(),
            name: format!("cc-usage-{user}"),
        }
    }

    /// The inbox prefix this connection must use, `_INBOX.<user>`.
    pub fn inbox_prefix(&self) -> String {
        cc_core::inbox_prefix(&self.user)
    }
}

/// Connection state, for a status indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Connected and usable.
    Connected,
    /// Lost; async-nats is reconnecting with backoff.
    Disconnected,
    /// Not connected yet.
    Pending,
}

/// A connection-level event, mirrored from async-nats without its types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionEvent {
    /// (Re)connected to a server.
    Connected,
    /// The connection dropped; reconnecting.
    Disconnected,
    /// The connection is closed for good.
    Closed,
    /// The server is shutting down and asked clients to move.
    LameDuckMode,
    /// The client is draining before closing.
    Draining,
    /// A subscription is not keeping up; messages were dropped.
    SlowConsumer,
    /// An `-ERR` from the server, e.g. a permissions violation.
    ServerError(String),
    /// An error inside the client.
    ClientError(String),
}

impl ConnectionEvent {
    fn from_nats(event: &Event) -> Self {
        match event {
            Event::Connected => ConnectionEvent::Connected,
            Event::Disconnected => ConnectionEvent::Disconnected,
            Event::Closed => ConnectionEvent::Closed,
            Event::LameDuckMode => ConnectionEvent::LameDuckMode,
            Event::Draining => ConnectionEvent::Draining,
            Event::SlowConsumer(_) => ConnectionEvent::SlowConsumer,
            Event::ServerError(err) => ConnectionEvent::ServerError(err.to_string()),
            Event::ClientError(err) => ConnectionEvent::ClientError(err.to_string()),
        }
    }

    /// True for a server error that reports a permissions violation.
    pub fn is_permission_violation(&self) -> bool {
        matches!(self, ConnectionEvent::ServerError(msg)
            if msg.to_ascii_lowercase().contains("permissions violation"))
    }
}

/// An open connection. Cheap to clone; all clones share one connection.
#[derive(Debug, Clone)]
pub struct Client {
    inner: async_nats::Client,
    user: String,
    server: String,
    events: broadcast::Sender<ConnectionEvent>,
}

impl Client {
    /// The NATS user this connection authenticated as.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The configured URL whose transport this connection uses.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// True when connected over WebSocket.
    pub fn is_websocket(&self) -> bool {
        is_websocket_url(&self.server)
    }

    /// Current connection state.
    pub fn state(&self) -> ConnectionState {
        match self.inner.connection_state() {
            async_nats::connection::State::Connected => ConnectionState::Connected,
            async_nats::connection::State::Disconnected => ConnectionState::Disconnected,
            async_nats::connection::State::Pending => ConnectionState::Pending,
        }
    }

    /// Subscribes to connection events from now on.
    pub fn events(&self) -> broadcast::Receiver<ConnectionEvent> {
        self.events.subscribe()
    }

    /// The underlying async-nats client, for checks this crate does not wrap.
    pub fn nats(&self) -> &async_nats::Client {
        &self.inner
    }

    /// Flushes pending messages and closes the connection, freeing one of the
    /// account's few connection slots. Other clones become unusable.
    pub async fn close(self) {
        match tokio::time::timeout(CLOSE_TIMEOUT, self.inner.drain()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::debug!(user = %self.user, "drain failed: {err}"),
            Err(_) => tracing::debug!(user = %self.user, "drain timed out"),
        }
    }
}

/// Opens a connection for `config.user`, trying each transport in turn.
///
/// After the first successful connect, async-nats reconnects on its own with
/// the backoff of [`reconnect_delay`], forever.
pub async fn connect(config: Config) -> Result<Client> {
    if config.user.is_empty() {
        return Err(RemoteError::Config("user is empty".into()));
    }
    validate_bucket(&config.bucket)?;
    let groups = transport_groups(&config.servers)?;
    let mut last_err = None;
    for group in groups {
        match connect_group(&config, &group).await {
            Ok(client) => return Ok(client),
            Err(err @ (RemoteError::Credentials { .. } | RemoteError::Config(_))) => {
                return Err(err)
            }
            Err(err) => {
                tracing::warn!(servers = ?group, "connect failed: {err}");
                last_err = Some(err);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| RemoteError::Config("no servers".into())))
}

async fn connect_group(config: &Config, servers: &[String]) -> Result<Client> {
    let addrs = servers
        .iter()
        .map(|s| s.parse::<async_nats::ServerAddr>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| RemoteError::Config(format!("bad server URL: {err}")))?;

    let (events, _) = broadcast::channel(EVENT_CAPACITY);
    let sender = events.clone();
    let user = config.user.clone();
    let options = ConnectOptions::with_credentials_file(&config.creds_path)
        .await
        .map_err(|source| RemoteError::Credentials {
            path: config.creds_path.clone(),
            source,
        })?
        .custom_inbox_prefix(config.inbox_prefix())
        .name(&config.name)
        .connection_timeout(CONNECT_TIMEOUT)
        .max_reconnects(None)
        .reconnect_delay_callback(reconnect_delay)
        .event_callback(move |event| {
            let sender = sender.clone();
            let user = user.clone();
            async move {
                match &event {
                    Event::ServerError(_) | Event::ClientError(_) => {
                        tracing::warn!(%user, "nats: {event}")
                    }
                    _ => tracing::info!(%user, "nats: {event}"),
                }
                let _ = sender.send(ConnectionEvent::from_nats(&event));
            }
        });

    // The connection timeout applies per server; bound the whole group too.
    let deadline = CONNECT_TIMEOUT * (servers.len() as u32) + Duration::from_secs(2);
    let inner = tokio::time::timeout(deadline, options.connect(addrs))
        .await
        .map_err(|_| RemoteError::Timeout {
            op: "connect",
            after: deadline,
        })?
        .map_err(|err| RemoteError::nats("connect", err))?;

    Ok(Client {
        inner,
        user: config.user.clone(),
        server: servers[0].clone(),
        events,
    })
}

fn is_websocket_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("ws://") || lower.starts_with("wss://")
}

/// Refuses anything but an encrypted transport with a host: `tls://` or
/// `wss://`, then a host and an optional port (`tls://host:4222`,
/// `wss://host`, which is port 443). A plaintext connection would carry the
/// account's traffic and the credentials' nonce signature in the clear; a
/// user or password in the URL would end up in logs and the status line.
fn check_transport(server: &str) -> Result<()> {
    let (shown, has_user) = redact_user(server);
    let lower = server.to_ascii_lowercase();
    if lower.starts_with("nats://") || lower.starts_with("ws://") {
        return Err(RemoteError::Config(format!(
            "{shown:?}: plaintext transports are refused; use tls:// or wss://"
        )));
    }
    if !["tls://", "wss://"].iter().any(|s| lower.starts_with(s)) {
        return Err(RemoteError::Config(format!(
            "{shown:?}: use tls:// or wss://"
        )));
    }
    if has_user {
        return Err(RemoteError::Config(format!(
            "{shown:?}: no user or password in the URL; the creds file authenticates"
        )));
    }
    let addr = server
        .parse::<async_nats::ServerAddr>()
        .map_err(|err| RemoteError::Config(format!("{shown:?}: {err}")))?;
    if addr.host().is_empty() {
        return Err(RemoteError::Config(format!("{shown:?}: no host")));
    }
    Ok(())
}

/// `server` with any `user[:password]@` of its authority replaced by
/// `***@`, for messages, and whether it had one.
fn redact_user(server: &str) -> (String, bool) {
    let Some((scheme, rest)) = server.split_once("://") else {
        return (server.to_string(), false);
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => (format!("{scheme}://***{}", &rest[at..]), true),
        None => (server.to_string(), false),
    }
}

/// Checks a server list the way [`connect`] does, without connecting: at
/// least one URL, every one `tls://` or `wss://` with a host and an
/// optional port. Blank entries are skipped. The error is a
/// [`RemoteError::Config`] naming the first bad URL.
pub fn validate_servers(servers: &[String]) -> Result<()> {
    transport_groups(servers).map(|_| ())
}

/// Checks a KV bucket name: 1 to 64 ASCII letters, digits, `-` or `_`
/// (what JetStream accepts, within a length that keeps the stream name
/// `KV_<bucket>` short). The error is a [`RemoteError::Config`].
pub fn validate_bucket(bucket: &str) -> Result<()> {
    let ok = !bucket.is_empty()
        && bucket.len() <= MAX_BUCKET_LEN
        && bucket
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(RemoteError::Config(format!(
            "bucket {bucket:?}: use 1 to {MAX_BUCKET_LEN} letters, digits, '-' or '_'"
        )))
    }
}

/// Longest bucket name [`validate_bucket`] accepts.
pub const MAX_BUCKET_LEN: usize = 64;

/// Splits servers into transport groups (WebSocket or not), ordered by the
/// first appearance of each transport; async-nats rejects mixed pools. Any
/// URL that is not `tls://` or `wss://` makes it a [`RemoteError::Config`].
pub(crate) fn transport_groups(servers: &[String]) -> Result<Vec<Vec<String>>> {
    let mut groups: Vec<(bool, Vec<String>)> = Vec::new();
    for server in servers.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        check_transport(server)?;
        let ws = is_websocket_url(server);
        match groups.iter_mut().find(|(kind, _)| *kind == ws) {
            Some((_, group)) => group.push(server.to_string()),
            None => groups.push((ws, vec![server.to_string()])),
        }
    }
    if groups.is_empty() {
        return Err(RemoteError::Config("no servers".into()));
    }
    Ok(groups.into_iter().map(|(_, group)| group).collect())
}

/// Delay before reconnect attempt number `attempts` (1-based): immediate
/// first, then exponential from 250 ms, capped at [`MAX_RECONNECT_DELAY`],
/// with up to 20% jitter so machines do not reconnect in lockstep.
pub fn reconnect_delay(attempts: usize) -> Duration {
    let base = base_reconnect_delay(attempts);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter = base.mul_f64(f64::from(nanos % 1000) / 1000.0 * 0.2);
    (base + jitter).min(MAX_RECONNECT_DELAY)
}

fn base_reconnect_delay(attempts: usize) -> Duration {
    if attempts <= 1 {
        return Duration::ZERO;
    }
    let exp = (attempts - 2).min(16) as u32;
    Duration::from_millis(250)
        .saturating_mul(1 << exp)
        .min(MAX_RECONNECT_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn groups_split_by_transport_in_first_seen_order() {
        let groups = transport_groups(&s(&["wss://a", "tls://b", "WSS://c", "TLS://d"])).unwrap();
        assert_eq!(
            groups,
            vec![s(&["wss://a", "WSS://c"]), s(&["tls://b", "TLS://d"])]
        );
    }

    #[test]
    fn plaintext_and_unknown_transports_are_refused() {
        for bad in [
            "nats://d:4222",
            "ws://e",
            "NATS://f",
            "https://g",
            "tls://",
            "wss://:443",
            "tls://host:notaport",
            "wss://user:secret@host",
            "connect.ngs.global",
        ] {
            let err = transport_groups(&s(&["wss://a", bad])).unwrap_err();
            assert!(matches!(err, RemoteError::Config(_)), "{bad}");
            if bad.to_ascii_lowercase().starts_with("nats") || bad.starts_with("ws:") {
                assert!(err.to_string().contains("plaintext"), "{err}");
            }
            assert!(!err.to_string().contains("secret"), "{err}");
            assert!(validate_servers(&s(&[bad])).is_err(), "{bad}");
        }
    }

    #[test]
    fn hosts_ports_and_paths_are_accepted() {
        for good in [
            "tls://nats.example:4222",
            "wss://nats.example",
            "wss://nats.example:8443/nats",
            "tls://10.0.0.5:4443",
            "tls://[::1]:4222",
            "TLS://Upper.Example",
        ] {
            assert!(validate_servers(&s(&[good])).is_ok(), "{good}");
        }
        assert!(validate_servers(&s(&[" ", ""])).is_err(), "no servers");
    }

    #[test]
    fn default_servers_try_websocket_then_tls() {
        let groups = transport_groups(&default_servers()).unwrap();
        assert_eq!(
            groups,
            vec![
                s(&["wss://connect.ngs.global"]),
                s(&["tls://connect.ngs.global"])
            ]
        );
    }

    #[test]
    fn bucket_names_follow_jetstream_rules() {
        for good in [
            DEFAULT_BUCKET,
            "cc_sessions",
            "A-1",
            &"b".repeat(MAX_BUCKET_LEN),
        ] {
            assert!(validate_bucket(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "cc.sessions",
            "cc sessions",
            "cc/s",
            "cc*",
            ">",
            "é",
            &"b".repeat(65),
        ] {
            assert!(
                matches!(validate_bucket(bad), Err(RemoteError::Config(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn empty_server_list_is_a_config_error() {
        assert!(matches!(
            transport_groups(&s(&[" ", ""])),
            Err(RemoteError::Config(_))
        ));
        assert!(matches!(transport_groups(&[]), Err(RemoteError::Config(_))));
    }

    #[test]
    fn reconnect_backoff_grows_and_is_capped() {
        assert_eq!(base_reconnect_delay(0), Duration::ZERO);
        assert_eq!(base_reconnect_delay(1), Duration::ZERO);
        assert_eq!(base_reconnect_delay(2), Duration::from_millis(250));
        assert_eq!(base_reconnect_delay(3), Duration::from_millis(500));
        assert_eq!(base_reconnect_delay(6), Duration::from_secs(4));
        assert_eq!(base_reconnect_delay(100), MAX_RECONNECT_DELAY);
        for attempt in 0..200 {
            let d = reconnect_delay(attempt);
            assert!(d <= MAX_RECONNECT_DELAY);
            assert!(d >= base_reconnect_delay(attempt));
        }
    }

    #[test]
    fn config_for_user_follows_conventions() {
        let config = Config::for_user("cc-sessions.viewer");
        assert_eq!(config.inbox_prefix(), "_INBOX.cc-sessions.viewer");
        assert_eq!(config.name, "cc-usage-cc-sessions.viewer");
        assert_eq!(
            config.creds_path,
            cc_core::paths::nats_dir().join("cc-sessions.viewer.creds")
        );
        assert_eq!(config.servers, s(&DEFAULT_SERVERS));
        assert_eq!(
            config.servers[0], "wss://connect.ngs.global",
            "WebSocket first"
        );
        assert_eq!(config.bucket, "cc-sessions");
    }

    #[test]
    fn permission_violations_are_recognised() {
        let ev = ConnectionEvent::ServerError(
            "Permissions Violation for Subscription to \"_INBOX.>\"".into(),
        );
        assert!(ev.is_permission_violation());
        assert!(!ConnectionEvent::ServerError("stale connection".into()).is_permission_violation());
        assert!(!ConnectionEvent::Connected.is_permission_violation());
    }

    #[tokio::test]
    async fn missing_creds_file_fails_before_any_network_io() {
        let config = Config {
            servers: s(&["tls://127.0.0.1:1"]),
            bucket: DEFAULT_BUCKET.into(),
            creds_path: PathBuf::from("this/file/does/not/exist.creds"),
            user: "cc-sessions.viewer".into(),
            name: "test".into(),
        };
        let err = connect(config).await.unwrap_err();
        assert!(matches!(err, RemoteError::Credentials { .. }), "{err}");
    }

    #[tokio::test]
    async fn empty_user_is_rejected() {
        let mut config = Config::for_user("x");
        config.user.clear();
        assert!(matches!(connect(config).await, Err(RemoteError::Config(_))));
    }

    #[tokio::test]
    async fn a_bad_bucket_or_server_fails_before_any_network_io() {
        let mut config = Config::for_user("cc-sessions.viewer");
        config.creds_path = PathBuf::from("this/file/does/not/exist.creds");
        config.bucket = "cc.sessions".into();
        assert!(matches!(
            connect(config.clone()).await,
            Err(RemoteError::Config(_))
        ));
        config.bucket = DEFAULT_BUCKET.into();
        config.servers = s(&["nats://127.0.0.1:1"]);
        assert!(matches!(connect(config).await, Err(RemoteError::Config(_))));
    }
}
