//! Logic shared by the cc-usage widget, the `cc-hook` hook binary and the
//! `cc-publish` publisher. No UI and no network I/O, so everything here is
//! covered by plain `cargo test`.
//!
//! - [`events`]: the spool event cc-hook writes (local schema, never leaves the machine).
//! - [`session`]: session vocabulary, registry entry, transcript marks and the remote payload.
//! - [`state`]: [`state::SessionTracker`], the state machine that turns spool
//!   events and registry snapshots into the rows the widget shows.
//! - [`models`]: context window sizes per model, for the context meter when
//!   no status line reports it.
//! - [`paths`]: data locations on Windows and Linux, and the home and
//!   Claude Code folders.
//! - [`fsutil`]: atomic whole-file writes (temporary file, sync, rename)
//!   with a rename that rides out a file held open for a moment on Windows.
//! - [`projects`]: token counters per project and the project payload a
//!   publisher sends.
//! - [`process_tree`]: containing a child process and everything it
//!   starts, so a timeout kills all of it (a job object on Windows, a
//!   process group on Unix).

pub mod events;
pub mod fsutil;
pub mod models;
pub mod paths;
pub mod process_tree;
pub mod projects;
pub mod session;
pub mod state;

/// The NATS project this app belongs to in the shared `tooling` account.
pub const PROJECT: &str = "cc-sessions";

/// The KV bucket holding encrypted session state. Same as the project name.
pub const BUCKET: &str = PROJECT;

/// NATS user name for a role in this project, e.g. `cc-sessions.viewer`.
pub fn nats_user(role: &str) -> String {
    format!("{PROJECT}.{role}")
}

/// Inbox prefix a client must connect with; its permissions allow nothing else.
pub fn inbox_prefix(user: &str) -> String {
    format!("_INBOX.{user}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_tooling_account_conventions() {
        assert_eq!(nats_user("viewer"), "cc-sessions.viewer");
        assert_eq!(nats_user("pub-lab-01"), "cc-sessions.pub-lab-01");
        assert_eq!(
            inbox_prefix(&nats_user("viewer")),
            "_INBOX.cc-sessions.viewer"
        );
        assert_eq!(BUCKET, "cc-sessions");
    }
}
