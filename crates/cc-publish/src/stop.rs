//! `cc-publish stop`: asks a running `cc-publish run` to stop cleanly.
//!
//! The request is a small file, `<data_dir>/cc-publish.stop`, naming the pid
//! of the run loop to stop. The run loop looks for it every
//! [`POLL_INTERVAL`] and, when the file names it, stops the way Ctrl-C does:
//! one last bounded flush, the connection closed, the state file marked
//! stopped. It then removes the file.
//!
//! Windows needs this: a publisher started by a scheduled task through a
//! headless console has no window to close and no SIGTERM, and ending the
//! task only kills the task's own process (the console host), not
//! cc-publish. On Linux systemd's SIGTERM keeps working as before; the file
//! is just another way in.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::state::PublishState;
use crate::util::now_secs;

/// File name inside the data directory.
pub const STOP_FILE: &str = "cc-publish.stop";

/// How often the run loop looks for the file.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long `cc-publish stop` waits by default.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(15);

/// The request file's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopRequest {
    /// The run loop to stop.
    pub pid: u32,
    /// Unix seconds of the request.
    pub requested_at: f64,
}

/// `<data_dir>/cc-publish.stop`.
pub fn default_path() -> PathBuf {
    cc_core::paths::data_dir().join(STOP_FILE)
}

/// Writes a request for `pid` (atomically).
pub fn request(path: &Path, pid: u32) -> std::io::Result<()> {
    let req = StopRequest {
        pid,
        requested_at: now_secs(),
    };
    let mut text = serde_json::to_vec(&req).map_err(std::io::Error::other)?;
    text.push(b'\n');
    crate::util::write_atomic(path, &text)
}

/// Whether the file asks `pid` to stop. A file that cannot be parsed asks
/// nobody.
pub fn requested_for(path: &Path, pid: u32) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<StopRequest>(&t).ok())
        .is_some_and(|r| r.pid == pid)
}

/// Removes the file; a missing file is fine.
pub fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// What [`stop`] found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// No live publisher (no state file, stopped, stale, or its process gone).
    NotRunning,
    /// The publisher with this pid stopped.
    Stopped(u32),
    /// The publisher with this pid did not stop within the wait.
    TimedOut(u32),
}

/// Asks the publisher recorded in `state_path` to stop and waits up to
/// `wait` for its state file to say it stopped (or for its process to go).
pub fn stop(state_path: &Path, stop_path: &Path, wait: Duration) -> std::io::Result<StopOutcome> {
    let Some(state) = PublishState::load(state_path).filter(|s| s.is_alive(now_secs())) else {
        clear(stop_path);
        return Ok(StopOutcome::NotRunning);
    };
    let pid = state.pid;
    request(stop_path, pid)?;
    let deadline = Instant::now() + wait;
    loop {
        let gone = match PublishState::load(state_path) {
            None => true,
            Some(s) => s.pid != pid || !s.running || crate::util::pid_alive(pid) == Some(false),
        };
        if gone {
            clear(stop_path);
            return Ok(StopOutcome::Stopped(pid));
        }
        if Instant::now() >= deadline {
            return Ok(StopOutcome::TimedOut(pid));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_names_one_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STOP_FILE);
        assert!(!requested_for(&path, 7));
        request(&path, 7).unwrap();
        assert!(requested_for(&path, 7));
        assert!(!requested_for(&path, 8));
        std::fs::write(&path, "garbage").unwrap();
        assert!(!requested_for(&path, 7));
        clear(&path);
        clear(&path);
        assert!(!path.exists());
    }

    #[test]
    fn stop_without_a_live_publisher_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(crate::state::STATE_FILE);
        let stop_file = dir.path().join(STOP_FILE);
        assert_eq!(
            stop(&state, &stop_file, Duration::ZERO).unwrap(),
            StopOutcome::NotRunning
        );
        // A stopped one, with a stale request lying around.
        PublishState {
            pid: std::process::id(),
            running: false,
            updated_at: now_secs(),
            ..Default::default()
        }
        .save(&state)
        .unwrap();
        request(&stop_file, 1234).unwrap();
        assert_eq!(
            stop(&state, &stop_file, Duration::ZERO).unwrap(),
            StopOutcome::NotRunning
        );
        assert!(!stop_file.exists(), "the stale request is removed");
    }

    #[test]
    fn stop_waits_for_the_state_file_to_say_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(crate::state::STATE_FILE);
        let stop_file = dir.path().join(STOP_FILE);
        let me = std::process::id();
        let live = PublishState {
            pid: me,
            running: true,
            updated_at: now_secs(),
            ..Default::default()
        };
        live.save(&state).unwrap();
        // Nobody acts on the request: times out and leaves it for the loop.
        assert_eq!(
            stop(&state, &stop_file, Duration::from_millis(300)).unwrap(),
            StopOutcome::TimedOut(me)
        );
        assert!(requested_for(&stop_file, me));
        // A fake run loop that honours it.
        let (s2, f2) = (state.clone(), stop_file.clone());
        let fake = std::thread::spawn(move || {
            while !requested_for(&f2, me) {
                std::thread::sleep(Duration::from_millis(20));
            }
            PublishState {
                running: false,
                ..live
            }
            .save(&s2)
            .unwrap();
        });
        assert_eq!(
            stop(&state, &stop_file, Duration::from_secs(10)).unwrap(),
            StopOutcome::Stopped(me)
        );
        fake.join().unwrap();
        assert!(!stop_file.exists());
    }
}
