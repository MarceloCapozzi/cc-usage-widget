//! Transport for the `cc-sessions` KV bucket on Synadia Cloud, built on
//! async-nats.
//!
//! This crate moves bytes only: values are sealed and opened by cc-envelope,
//! and the caller passes and receives the sealed bytes.
//!
//! - [`connect`] opens a connection from a [`Config`] (servers, bucket,
//!   `.creds` file, user, connection name). It always uses the inbox prefix
//!   `_INBOX.<user>`, the only inbox the account's permissions allow, tries
//!   each transport in the order the server list names it, and reconnects
//!   with capped exponential backoff. Only encrypted transports are
//!   accepted: a `nats://` or `ws://` URL is a [`RemoteError::Config`]
//!   error, and so is a URL without a host or with a user in it
//!   ([`validate_servers`]) or a bad bucket name ([`validate_bucket`]).
//! - [`DEFAULT_SERVERS`] (`wss://connect.ngs.global` first, since WebSocket
//!   on 443 gets through corporate networks, then `tls://`) and
//!   [`DEFAULT_BUCKET`] are the one default of every client in this
//!   repository; the widget's `settings.json` and cc-publish's
//!   `publish.json` can name others.
//! - [`Publisher`] (users `cc-sessions.pub-<alias>`) puts and deletes keys
//!   under its own prefix.
//! - [`Viewer`] (user `cc-sessions.viewer`) watches every key, snapshot
//!   first and then live changes, and can read single keys.
//!
//! The account allows only five connections in total, shared with other
//! projects: open one [`Client`] per role, share it by cloning, and call
//! [`Client::close`] when done.
//!
//! `examples/remote_check.rs` runs the 14 live permission checks of the
//! account over TLS and WebSocket; it is the way to validate the account
//! setup.

mod client;
mod error;
mod kv;

pub use client::{
    connect, default_servers, reconnect_delay, validate_bucket, validate_servers, Client, Config,
    ConnectionEvent, ConnectionState, CONNECT_TIMEOUT, DEFAULT_BUCKET, DEFAULT_SERVERS,
    MAX_BUCKET_LEN, MAX_RECONNECT_DELAY,
};
pub use error::{BoxError, RemoteError, Result};
pub use kv::{KvChange, KvWatch, Publisher, Viewer, OP_TIMEOUT, SNAPSHOT_TIMEOUT};
