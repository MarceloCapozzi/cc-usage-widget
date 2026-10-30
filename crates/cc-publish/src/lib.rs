//! `cc-publish`: publishes the Claude Code sessions of a machine without the
//! cc-usage widget, encrypted, to the `cc-sessions` NATS KV bucket on Synadia
//! Cloud, where the widget on another machine shows them as remote rows.
//!
//! Every 2 s it gathers local sessions the way the widget does (`claude
//! agents --json`, `~/.claude/sessions`, the cc-hook spool, transcripts and
//! the per-session status line files) into a [`cc_core::state::SessionTracker`].
//! A session whose state, tool, model or context changed is sealed with
//! cc-envelope and put at `<alias>.s.<sid>` at once; every session is re-put
//! every 60 s as a heartbeat, its key is deleted when it ends, and
//! `<alias>.host` is put every 60 s. Each project's token usage over the
//! last 30 days goes to `<alias>.p.<pid>` (scanned every 5 minutes, re-put
//! every 2), and the machine's tokens per day and busiest hour, for the
//! activity calendar, to `<alias>.activity` with them. One NATS connection
//! (`tls://` or `wss://` only), user `cc-sessions.pub-<alias>`, which may
//! only write under `<alias>.>`.
//!
//! The servers and the bucket come from `publish.json` (`servers`,
//! `bucket`), Synadia Cloud and `cc-sessions` when it names none
//! ([`cc_remote::DEFAULT_SERVERS`], [`cc_remote::DEFAULT_BUCKET`]), so
//! another NATS deployment needs no rebuild. The bucket's limits are tied
//! to constants here (the envelope size, [`planner::HEARTBEAT_SECS`],
//! [`projects::REPUBLISH_EVERY`]); `tests/project_file.rs` checks
//! `infra/nats/cc-sessions.example.yaml` against them.
//!
//! Modules:
//!
//! - [`config`] reads `publish.json`.
//! - The local sources are read by [`cc_local::monitor`], the same code the
//!   widget runs (with the CLI backing off on an idle machine).
//! - [`planner`] decides what to put, heartbeat and delete on each pass.
//! - [`projects`] scans the transcripts and decides the project puts.
//! - [`outbox`] hands decisions to the network (newest wins), seals and sends.
//! - [`pipeline`] ties them together: the gathering pass, the network loop
//!   with its reconnects, backoff and resync, and the state they share. The
//!   cc-usage app runs the same pipeline when its role is Publisher, fed by
//!   its own session monitor.
//! - [`run`] is the long-running loop; [`check`] the end-to-end validation.
//! - [`instance`] keeps it to one publisher per data directory (a lock
//!   file the app's Publisher role takes too).
//! - [`stop`] lets `cc-publish stop` end a run loop cleanly.
//! - [`sealing`] loads the sealer from this machine's key
//!   pair and the viewer's public key; without both there is none.
//! - [`reload`] lets a running publisher pick up replaced key and creds
//!   files without a restart, keeping the last good keys when they break.
//! - [`keygen`], [`state`], [`logging`], [`cli`] and [`util`] do the rest.

#![deny(missing_docs)]

pub mod check;
pub mod cli;
pub mod config;
pub mod instance;
pub mod keygen;
pub mod logging;
pub mod outbox;
pub mod pipeline;
pub mod planner;
pub mod projects;
pub mod reload;
pub mod run;
pub mod sealing;
pub mod state;
pub mod stop;
pub mod util;
