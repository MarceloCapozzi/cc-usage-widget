//! The error type returned by every fallible call in this crate.

use std::path::PathBuf;
use std::time::Duration;

/// A boxed error from async-nats, kept as the source of [`RemoteError::Nats`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Everything that can go wrong talking to the bucket.
///
/// Messages never contain credentials: only file paths and NATS error text.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// The [`Config`](crate::Config) is unusable (no servers, bad URL, ...).
    #[error("invalid config: {0}")]
    Config(String),

    /// The `.creds` file could not be read or parsed.
    #[error("cannot load credentials file {}: {source}", path.display())]
    Credentials {
        /// Path of the `.creds` file.
        path: PathBuf,
        /// The underlying I/O or parse error (never the file contents).
        #[source]
        source: std::io::Error,
    },

    /// A NATS or JetStream operation failed. A permission denial on a
    /// JetStream request usually shows up here as a timeout, because the
    /// server drops the request instead of answering it.
    #[error("{op} failed: {source}")]
    Nats {
        /// The operation that failed, e.g. `"put"` or `"bind"`.
        op: &'static str,
        /// The async-nats error.
        #[source]
        source: BoxError,
    },

    /// An operation did not complete within this crate's own deadline.
    #[error("{op} timed out after {after:?}")]
    Timeout {
        /// The operation that timed out.
        op: &'static str,
        /// How long it was given.
        after: Duration,
    },
}

impl RemoteError {
    pub(crate) fn nats(op: &'static str, source: impl Into<BoxError>) -> Self {
        RemoteError::Nats {
            op,
            source: source.into(),
        }
    }
}

/// Result alias used throughout the crate.
pub type Result<T, E = RemoteError> = std::result::Result<T, E>;
