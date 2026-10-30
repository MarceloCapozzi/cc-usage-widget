//! `<data_dir>/publish-state.json`: what the run loop is doing, for
//! `cc-publish status` and `cc-publish clear`.
//!
//! Written atomically every few seconds while `run` is going, and once more
//! with `running: false` when it stops. It holds counters, times, the
//! connection state, the session keys currently published (hashed ids, not
//! secret), and the key ids and NATS user the run loop uses ([`InUse`]).
//! Never payloads, key material or credentials.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name inside the data directory.
pub const STATE_FILE: &str = "publish-state.json";

/// How often the run loop rewrites the file (seconds).
pub const WRITE_EVERY_SECS: f64 = 10.0;

/// A state file older than this (seconds) with `running: true` means the
/// publisher died without cleaning up.
pub const STALE_AFTER_SECS: f64 = 30.0;

/// `status` reports the publisher healthy (exit 0) only when its last
/// successful write is at most this old (seconds). Same as the viewer's
/// offline threshold: with the 60 s heartbeat a working publisher always
/// writes well within it.
pub const HEALTHY_WRITE_SECS: f64 = 150.0;

/// The state file's content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PublishState {
    /// Publisher version.
    pub version: String,
    /// Process id of the run loop.
    pub pid: u32,
    /// Start time of that process ([`cc_local::process::start_time`]), so a
    /// pid reused by another process is not taken for the publisher; `None`
    /// when the OS cannot tell.
    pub pid_started: Option<u64>,
    /// Machine alias.
    pub alias: String,
    /// Unix seconds the run loop started.
    pub started_at: f64,
    /// Unix seconds of this write.
    pub updated_at: f64,
    /// False once the run loop has stopped cleanly.
    pub running: bool,
    /// Whether the NATS connection is up and the bucket bound.
    pub connected: bool,
    /// The server URL whose transport is in use.
    pub server: Option<String>,
    /// Sessions tracked on the last gathering pass.
    pub sessions: usize,
    /// Session keys currently published.
    pub keys: Vec<String>,
    /// Gathering passes so far.
    pub passes: u64,
    /// Session puts acknowledged.
    pub puts: u64,
    /// Deletes acknowledged.
    pub deletes: u64,
    /// Host puts acknowledged.
    pub host_puts: u64,
    /// Project and activity puts acknowledged.
    pub project_puts: u64,
    /// Projects found by the last transcript scan.
    pub projects: usize,
    /// Failed flushes (each retried later).
    pub failures: u64,
    /// Successful (re)connects.
    pub connects: u64,
    /// Unix seconds of the last acknowledged write.
    pub last_publish_at: Option<f64>,
    /// The last network error (NATS error text only).
    pub last_error: Option<String>,
    /// Unix seconds of the last network error.
    pub last_error_at: Option<f64>,
    /// Why `claude agents --json` failed on the last run, if it did.
    pub agents_error: Option<String>,
    /// Operations waiting to be sent.
    pub pending: usize,
    /// Why the run loop ended on its own (a panic in the gathering thread),
    /// when it did. Only the exiting process writes it.
    pub exit_reason: Option<String>,
    /// What the run loop seals and connects with right now, which can
    /// differ from what the files on disk hold until it picks them up (see
    /// [`crate::reload`]). `None` until the run loop has loaded its keys.
    pub in_use: Option<InUse>,
}

/// The keys and credentials a run loop actually uses: key ids and a user
/// name, never key material.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InUse {
    /// Key id of the publisher key every value is sealed from.
    pub publisher_kid: String,
    /// Key id of the viewer key every value is sealed to.
    pub viewer_kid: String,
    /// The NATS user of the credentials of the current connection (read
    /// from the creds file when it was opened); `None` before the first
    /// connect, or when the file does not name one.
    pub creds_user: Option<String>,
    /// Why the key files on disk are not in use, when they changed and do
    /// not load: the run loop keeps sealing with the keys above.
    pub key_error: Option<String>,
    /// Why the creds file on disk is not in use, when it changed and is not
    /// this publisher's, cannot be used (expired, no seed) or did not
    /// connect on its trial: the connection keeps its credentials.
    pub creds_error: Option<String>,
}

impl InUse {
    /// The same line as [`cc_envelope::Sealer::describe`] for these keys,
    /// e.g. `publisher key p…, sealed to viewer key v…`.
    pub fn describe(&self) -> String {
        format!(
            "publisher key {}, sealed to viewer key {}",
            self.publisher_kid, self.viewer_kid
        )
    }
}

impl PublishState {
    /// `<data_dir>/publish-state.json`.
    pub fn default_path() -> PathBuf {
        cc_core::paths::data_dir().join(STATE_FILE)
    }

    /// Reads a state file; `None` when it is missing or unreadable.
    pub fn load(path: &Path) -> Option<PublishState> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Writes the file atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut text = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        text.push(b'\n');
        crate::util::write_atomic(path, &text)
    }

    /// True when a run loop is alive: `running` and written recently.
    pub fn is_live(&self, now: f64) -> bool {
        self.running && now - self.updated_at <= STALE_AFTER_SECS
    }

    /// Whether the run loop that wrote this file is still running.
    ///
    /// The OS decides when it can: the process has exited, or its pid now
    /// belongs to another process (a different start time), means not
    /// alive, however fresh the file; the same pid with the same start time
    /// means alive, however stale (a hung publisher still holds the spool).
    /// Only when the OS cannot tell (pid 0, no start time recorded, an OS
    /// other than Windows and Linux) does the file's freshness
    /// decide, as [`is_live`](Self::is_live). A file marked stopped is never
    /// alive.
    pub fn is_alive(&self, now: f64) -> bool {
        if !self.running {
            return false;
        }
        match cc_local::process::same_process(self.pid, self.pid_started) {
            Some(same) => same,
            None => self.is_live(now) && crate::util::pid_alive(self.pid) != Some(false),
        }
    }

    /// Whether this file stops the run loop `me` from starting: another
    /// process that [`is_alive`](Self::is_alive).
    pub fn blocks_run(&self, me: u32, now: f64) -> bool {
        self.pid != me && self.is_alive(now)
    }

    /// The pid to name when another process holds the publisher lock
    /// ([`crate::instance`]): the one this file says runs, when it is not
    /// `me`. Only for the message: the lock decides.
    pub fn holder(&self, me: u32) -> Option<u32> {
        (self.running && self.pid != me).then_some(self.pid)
    }

    /// The session keys this file says were published under `alias`; a file
    /// written for another alias gives none (those keys cannot be written by
    /// this publisher's user anyway).
    pub fn session_keys_for(&self, alias: &str) -> Vec<String> {
        if self.alias != alias {
            return Vec::new();
        }
        let prefix = format!("{alias}.s.");
        self.keys
            .iter()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect()
    }

    /// Why the publisher is not healthy at `now`, or `None` when it is:
    /// alive and with a successful write within [`HEALTHY_WRITE_SECS`].
    pub fn unhealthy_reason(&self, now: f64) -> Option<String> {
        if !self.is_alive(now) {
            return Some("the publisher is not running".into());
        }
        match self.last_publish_at {
            None => Some("no successful write yet".into()),
            Some(t) if now - t > HEALTHY_WRITE_SECS => {
                Some(format!("no successful write for {:.0} s", now - t))
            }
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        assert!(PublishState::load(&path).is_none());
        let s = PublishState {
            version: "0.1.0".into(),
            pid: 42,
            alias: "lab-01".into(),
            started_at: 100.0,
            updated_at: 110.0,
            running: true,
            connected: true,
            server: Some("wss://connect.ngs.global".into()),
            sessions: 2,
            keys: vec!["lab-01.s.3f9a1c2e".into()],
            puts: 7,
            last_publish_at: Some(109.0),
            ..Default::default()
        };
        s.save(&path).unwrap();
        assert_eq!(PublishState::load(&path).unwrap(), s);

        std::fs::write(&path, "{ not json").unwrap();
        assert!(PublishState::load(&path).is_none());
    }

    #[test]
    fn liveness_needs_running_and_a_recent_write() {
        let s = PublishState {
            running: true,
            updated_at: 100.0,
            ..Default::default()
        };
        assert!(s.is_live(120.0));
        assert!(!s.is_live(100.0 + STALE_AFTER_SECS + 1.0), "crashed");
        // Health: alive and a write within HEALTHY_WRITE_SECS.
        let mut h = s.clone();
        h.running = true;
        h.updated_at = 1000.0;
        h.last_publish_at = Some(990.0);
        assert_eq!(h.unhealthy_reason(1005.0), None);
        h.last_publish_at = Some(1005.0 - HEALTHY_WRITE_SECS - 1.0);
        assert!(h
            .unhealthy_reason(1005.0)
            .unwrap()
            .contains("no successful write for"));
        h.last_publish_at = None;
        assert_eq!(
            h.unhealthy_reason(1005.0).as_deref(),
            Some("no successful write yet")
        );
        h.last_publish_at = Some(1000.0);
        assert!(h
            .unhealthy_reason(1000.0 + STALE_AFTER_SECS + 1.0)
            .unwrap()
            .contains("not running"));
        let stopped = PublishState {
            running: false,
            ..s
        };
        assert!(!stopped.is_live(101.0));
    }

    /// Whether the OS can answer here (Windows and Linux).
    const OS_KNOWS: bool = cfg!(any(windows, target_os = "linux"));

    /// A child that has exited; its handle stays open while it is held, so
    /// Windows cannot reuse the pid meanwhile.
    fn exited() -> std::process::Child {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd.exe" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit 0"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        child.wait().unwrap();
        child
    }

    #[test]
    #[allow(clippy::zombie_processes)] // `exited` already waited for it
    fn a_fresh_state_of_an_exited_process_is_not_alive() {
        let mut s = PublishState {
            running: true,
            updated_at: 100.0,
            pid: std::process::id(),
            ..Default::default()
        };
        assert!(s.is_alive(110.0), "this process");
        let gone = exited();
        s.pid = gone.id();
        assert!(s.is_live(110.0));
        // Windows and Linux ask the OS; elsewhere only the file counts.
        assert_eq!(s.is_alive(110.0), !OS_KNOWS);
        assert!(s.unhealthy_reason(110.0).is_some());
    }

    #[test]
    #[allow(clippy::zombie_processes)] // `exited` already waited for it
    fn a_new_run_is_blocked_only_by_a_publisher_that_still_runs() {
        let me = std::process::id();
        let started = cc_local::process::start_time(me);
        let other = |pid: u32, pid_started: Option<u64>, updated_at: f64| PublishState {
            running: true,
            pid,
            pid_started,
            updated_at,
            ..Default::default()
        };
        // A killed publisher's fresh file does not block the next run: its
        // exited pid decides at once.
        let gone = exited();
        let killed = other(gone.id(), started, 100.0);
        assert_eq!(killed.blocks_run(1, 101.0), !OS_KNOWS);

        // This test process plays the running publisher (as seen by run loop 1).
        let alive = other(me, started, 100.0);
        assert!(alive.blocks_run(1, 101.0));
        assert!(!alive.blocks_run(me, 101.0), "its own file never blocks");
        if OS_KNOWS {
            // Same pid and start time: still that publisher, even when it
            // has not written for a while (hung).
            assert!(alive.blocks_run(1, 100.0 + STALE_AFTER_SECS + 60.0));
            // The pid was reused by another process: not the publisher, even
            // with a fresh file.
            let reused = other(me, started.map(|t| t + 1), 100.0);
            assert!(!reused.blocks_run(1, 101.0));
        }
        // No start time recorded (the OS could not tell): the pid and
        // the file's freshness decide.
        let no_start = other(me, None, 100.0);
        assert!(no_start.blocks_run(1, 101.0));
        assert!(!no_start.blocks_run(1, 100.0 + STALE_AFTER_SECS + 1.0));
        // Stopped cleanly: never blocks.
        let stopped = PublishState {
            running: false,
            ..alive
        };
        assert!(!stopped.blocks_run(1, 101.0));
    }

    #[test]
    fn the_session_keys_of_a_previous_run_are_kept_per_alias() {
        let s = PublishState {
            alias: "lab-01".into(),
            keys: vec![
                "lab-01.s.aaaa".into(),
                "lab-01.s.bbbb".into(),
                "lab-01.host".into(),
                "lab-02.s.cccc".into(),
            ],
            ..Default::default()
        };
        assert_eq!(
            s.session_keys_for("lab-01"),
            ["lab-01.s.aaaa", "lab-01.s.bbbb"]
        );
        assert!(
            s.session_keys_for("lab-02").is_empty(),
            "written for another alias"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        s.save(&path).unwrap();
        assert_eq!(PublishState::load(&path).unwrap().keys, s.keys, "persisted");
    }
}
