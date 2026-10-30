//! Opt-in diagnostics for the receiver.
//!
//! The receiver may not write to stdout or stderr, so when something needs
//! investigating, set `CC_HOOK_DEBUG=1` in the environment Claude Code runs
//! hooks with and read `<data_dir>/cc-hook.log`. Without the variable nothing
//! is written. Every failure in here is swallowed.

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Environment variable that turns diagnostics on (any non-empty value).
pub const DEBUG_ENV: &str = "CC_HOOK_DEBUG";

/// Whether diagnostics are enabled for this process.
pub fn enabled() -> bool {
    std::env::var_os(DEBUG_ENV).is_some_and(|v| !v.is_empty())
}

/// The log file: `<data_dir>/cc-hook.log`.
pub fn log_file() -> PathBuf {
    cc_core::paths::data_dir().join("cc-hook.log")
}

/// Appends one line to the log when diagnostics are enabled. Never fails.
pub fn log(line: &str) {
    if !enabled() {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let path = log_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{ts:.3} pid={} {line}", std::process::id());
    }
}
