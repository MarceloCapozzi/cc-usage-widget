//! Local session sources for cc-usage, and the session monitor built on them.
//!
//! Everything this machine knows about its Claude Code sessions, read from
//! disk or from the `claude` CLI and turned into `cc-core` contract types:
//!
//! - [`agents`]: `claude agents --json`, the authoritative list of live
//!   sessions, as [`RegistryEntry`] values.
//! - [`registry`]: `~/.claude/sessions/*.json`, one file per live session, used
//!   to fill in what the CLI leaves out (mainly `entrypoint`). It can also act
//!   as a fast fallback list when the CLI is slow or missing.
//! - [`transcript`]: the title, custom title and color recorded in a session's
//!   `.jsonl` transcript, as [`TranscriptMarks`], read incrementally from the
//!   end of the file without looking at conversation content.
//! - [`spool`]: draining the directory where `cc-hook` drops one
//!   [`SpoolEvent`] per hook call.
//! - [`live`]: the status line files (`live/<session_id>.json` from
//!   `cc-hook statusline`).
//! - [`process`]: whether a pid still runs, and its start time.
//! - [`projects`]: token usage per project from the transcripts, for the
//!   widget's usage view and for `cc-publish`.
//! - [`monitor`]: one implementation of the whole pass, shared by the widget
//!   and `cc-publish`: [`monitor::Sources`] gathers, [`monitor::SessionMonitor`]
//!   feeds the `cc-core` state machine.
//!
//! Every parser is a pure function over text with its own tests; the I/O
//! around it is a thin wrapper. Each source also has an `#[ignore]` live test
//! that runs against the real machine (`cargo test -p cc-local -- --ignored`).
//!
//! [`RegistryEntry`]: cc_core::session::RegistryEntry
//! [`TranscriptMarks`]: cc_core::session::TranscriptMarks
//! [`SpoolEvent`]: cc_core::events::SpoolEvent

#![warn(missing_docs)]

pub mod agents;
mod error;
mod fields;
pub mod live;
pub mod monitor;
pub mod process;
pub mod projects;
pub mod registry;
pub mod spool;
pub mod transcript;

pub use error::LocalError;

use std::path::PathBuf;

/// The user's home directory, [`cc_core::paths::home_dir`]: `USERPROFILE`
/// on Windows, `HOME` elsewhere (each falling back to the other, absolute
/// paths only), then the folder the OS reports. Empty only when the OS
/// reports none either.
pub fn home_dir() -> PathBuf {
    cc_core::paths::home_dir().unwrap_or_default()
}

/// Claude Code's configuration directory, [`cc_core::paths::claude_config_dir`]:
/// `CLAUDE_CONFIG_DIR` when set to an absolute path (Claude Code honours it
/// too), otherwise `.claude` in [`home_dir`]. Empty only when there is no
/// home folder either.
pub fn claude_dir() -> PathBuf {
    cc_core::paths::claude_config_dir().unwrap_or_default()
}

/// Current time as Unix seconds with a fractional part.
pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
