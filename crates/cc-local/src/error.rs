//! The error type shared by the fallible operations in this crate.

use std::fmt;
use std::time::Duration;

/// Why a local source could not be read.
#[derive(Debug)]
pub enum LocalError {
    /// The `claude` executable was not found on `PATH` (nor in the usual
    /// install locations).
    ClaudeNotFound,
    /// The `claude` process did not finish within the allowed time and was killed.
    Timeout(Duration),
    /// The `claude` process exited unsuccessfully; carries its exit code if any.
    ExitStatus(Option<i32>),
    /// The output was not the JSON shape expected.
    Parse(String),
    /// A file or process I/O error.
    Io(std::io::Error),
}

impl fmt::Display for LocalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalError::ClaudeNotFound => write!(f, "the claude executable was not found on PATH"),
            LocalError::Timeout(d) => {
                write!(f, "claude did not answer within {} ms", d.as_millis())
            }
            LocalError::ExitStatus(Some(code)) => write!(f, "claude exited with code {code}"),
            LocalError::ExitStatus(None) => write!(f, "claude was terminated without an exit code"),
            LocalError::Parse(msg) => write!(f, "unexpected output: {msg}"),
            LocalError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for LocalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LocalError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LocalError {
    fn from(e: std::io::Error) -> Self {
        LocalError::Io(e)
    }
}
