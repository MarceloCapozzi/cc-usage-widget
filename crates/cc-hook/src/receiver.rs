//! The hook receiver: stdin payload in, one spool file out, total silence.
//!
//! Contract with Claude Code, enforced here:
//!
//! - nothing is ever written to stdout (Claude Code parses it as hook output)
//!   or stderr (Claude Code shows it to the user);
//! - the process always exits 0, whatever the input;
//! - no panics escape: the work runs under `catch_unwind` with a silent panic
//!   hook, and the code avoids panicking paths in the first place, because the
//!   release profile aborts on panic;
//! - no network and no heavy work, so the hook adds a few milliseconds per call.
//!
//! Diagnostics go to `<data_dir>/cc-hook.log` only when `CC_HOOK_DEBUG` is set.
//! With [`cc_core::paths::NO_SPOOL_ENV`] set, nothing is written at all.

use crate::{debuglog, payload, procs, spool};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// What one receiver call did, for diagnostics and tests.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The event was written to this spool file.
    Written(PathBuf),
    /// Nothing written: the input was empty, not JSON, or not a usable payload.
    Skipped(&'static str),
}

/// Runs the receiver against the real stdin and spool. Never panics, never
/// prints; any failure is logged (when enabled) and swallowed.
pub fn run() {
    let started = Instant::now();
    std::panic::set_hook(Box::new(|info| debuglog::log(&format!("panic: {info}"))));
    if std::env::var_os(cc_core::paths::NO_SPOOL_ENV).is_some() {
        // A Claude Code process the widget ran only to renew its token.
        let _ = read_stdin();
        debuglog::log("skipped: NO_SPOOL_ENV is set");
        return;
    }
    let result = std::panic::catch_unwind(|| {
        let input = read_stdin();
        process(&input, &cc_core::paths::spool_dir(), SystemTime::now())
    });
    match result {
        Ok(Ok(outcome)) => debuglog::log(&format!("{outcome:?} in {:?}", started.elapsed())),
        Ok(Err(e)) => debuglog::log(&format!("error: {e} after {:?}", started.elapsed())),
        Err(_) => debuglog::log("panic caught"),
    }
}

/// Most bytes the receiver reads from stdin. Most payloads are a few KiB,
/// but a `Write` carries the whole file and a `PostToolUse` the tool's
/// result, so the limit is generous; past it the input is cut, fails to
/// parse and is skipped, instead of growing cc-hook's memory without bound.
pub const MAX_STDIN_BYTES: u64 = 16 * 1024 * 1024;

/// stdin up to [`MAX_STDIN_BYTES`], or nothing when stdin is a terminal (a
/// person ran `cc-hook` by hand: waiting for input would just hang) or
/// unreadable.
fn read_stdin() -> Vec<u8> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = stdin.lock().take(MAX_STDIN_BYTES).read_to_end(&mut buf);
    buf
}

/// The receiver's work on an already read payload: parse, build the event,
/// resolve the pid, write it into `spool_dir`, and now and then prune.
pub fn process(input: &[u8], spool_dir: &Path, now: SystemTime) -> std::io::Result<Outcome> {
    let input = input.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(input);
    if input.iter().all(u8::is_ascii_whitespace) {
        return Ok(Outcome::Skipped("empty input"));
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(input) else {
        return Ok(Outcome::Skipped("input is not JSON"));
    };
    let ts = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let term = std::env::var("TERM_PROGRAM").ok();
    let Some(mut event) = payload::build_event(&value, ts, term) else {
        return Ok(Outcome::Skipped(
            "payload lacks hook_event_name or session_id",
        ));
    };
    let t = Instant::now();
    event.pid = procs::find_claude_pid();
    let pid_time = t.elapsed();

    let t = Instant::now();
    let random = spool::random_u64();
    let unique = format!("{:x}-{random:016x}", std::process::id());
    let path = spool::write_event(spool_dir, &event, &unique)?;
    debuglog::log(&format!(
        "pid {:?} found in {pid_time:?}, spool write {:?}",
        event.pid,
        t.elapsed()
    ));

    if random.is_multiple_of(spool::PRUNE_ONE_IN) {
        match spool::prune(spool_dir, now) {
            Ok(n) if n > 0 => debuglog::log(&format!("pruned {n} spool files")),
            Ok(_) => {}
            Err(e) => debuglog::log(&format!("prune failed: {e}")),
        }
    }
    Ok(Outcome::Written(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;

    /// A spool folder that does not exist yet, inside a fresh temp folder
    /// that goes away with the returned guard.
    fn temp_dir(tag: &str) -> (TestDir, PathBuf) {
        let root = TestDir::new(&format!("recv-{tag}"));
        let dir = root.join("spool");
        (root, dir)
    }

    #[test]
    fn writes_one_event_for_a_valid_payload() {
        let (_root, dir) = temp_dir("ok");
        let input =
            br#"{"hook_event_name":"Stop","session_id":"s-9","last_assistant_message":"done"}"#;
        let out = process(input, &dir, SystemTime::now()).unwrap();
        let Outcome::Written(path) = out else {
            panic!("not written: {out:?}")
        };
        let e: cc_core::events::SpoolEvent =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(e.event, "Stop");
        assert_eq!(e.session_id, "s-9");
        assert_eq!(e.last_message.as_deref(), Some("done"));
    }

    #[test]
    fn tolerates_a_utf8_bom() {
        let (_root, dir) = temp_dir("bom");
        let input = b"\xEF\xBB\xBF{\"hook_event_name\":\"Stop\",\"session_id\":\"s\"}";
        assert!(matches!(
            process(input, &dir, SystemTime::now()).unwrap(),
            Outcome::Written(_)
        ));
    }

    #[test]
    fn skips_garbage_without_touching_the_spool() {
        let (_root, dir) = temp_dir("bad");
        for input in [
            &b""[..],
            b"  \n",
            b"not json",
            b"{\"truncated\":",
            b"[1,2]",
            b"{\"session_id\":\"s\"}",
            b"\xff\xfe",
        ] {
            assert!(matches!(
                process(input, &dir, SystemTime::now()).unwrap(),
                Outcome::Skipped(_)
            ));
        }
        assert!(!dir.exists());
    }
}
