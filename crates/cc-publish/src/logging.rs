//! Logging: timestamped lines on stderr (the systemd journal captures them),
//! optionally also appended to `<data_dir>/cc-publish.log`, which is rotated
//! to `cc-publish.log.1` when it reaches [`MAX_LOG_BYTES`].
//!
//! The level comes from `CC_PUBLISH_LOG` (`error`, `warn`, `info`, `debug`,
//! `trace`; default `info`). async-nats is held to `warn` unless the level is
//! `debug` or finer. Nothing in this crate logs payloads, credentials or keys.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The log file is rotated once it reaches this size.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// File name of the log inside the data directory.
pub const LOG_FILE: &str = "cc-publish.log";

/// `<data_dir>/cc-publish.log`.
pub fn default_log_path() -> PathBuf {
    cc_core::paths::data_dir().join(LOG_FILE)
}

/// A log file that rotates itself at [`MAX_LOG_BYTES`].
#[derive(Debug)]
pub struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    len: u64,
    max: u64,
}

impl RotatingFile {
    /// Opens (appends to) `path`, rotating at `max` bytes.
    pub fn open(path: &Path, max: u64) -> io::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let len = file.metadata()?.len();
        Ok(RotatingFile {
            path: path.to_path_buf(),
            file: Some(file),
            len,
            max,
        })
    }

    /// The rotated file's path, `<path>.1`.
    pub fn rotated_path(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".1");
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        std::fs::rename(&self.path, self.rotated_path())?;
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.len = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let full = self.len > 0 && self.len + buf.len() as u64 > self.max;
        if full && self.rotate().is_err() && self.file.is_none() {
            // Rotation failed (e.g. the old file is locked): keep logging to
            // the current file rather than losing lines.
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file closed"))?;
        let n = file.write(buf)?;
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

/// Writes each formatted line to stderr and, when configured, to the file.
#[derive(Clone)]
struct Tee {
    file: Option<Arc<Mutex<RotatingFile>>>,
}

/// One line's writer: buffers the line, emits it whole on drop.
struct TeeWriter {
    buf: Vec<u8>,
    file: Option<Arc<Mutex<RotatingFile>>>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for TeeWriter {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let _ = io::stderr().write_all(&self.buf);
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = f.write_all(&self.buf);
            }
        }
    }
}

impl<'a> MakeWriter<'a> for Tee {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter {
            buf: Vec::with_capacity(256),
            file: self.file.clone(),
        }
    }
}

fn level_from_env() -> LevelFilter {
    match std::env::var("CC_PUBLISH_LOG")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "error" => LevelFilter::ERROR,
        "warn" | "warning" => LevelFilter::WARN,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        "off" => LevelFilter::OFF,
        _ => LevelFilter::INFO,
    }
}

/// Installs the global logger. `file` adds the rotating log file; if it
/// cannot be opened, logging goes to stderr only and a warning says why.
/// Calling it twice is harmless (the second call does nothing).
pub fn init(file: Option<&Path>) {
    let mut open_error = None;
    let file = file.and_then(|p| match RotatingFile::open(p, MAX_LOG_BYTES) {
        Ok(f) => Some(Arc::new(Mutex::new(f))),
        Err(err) => {
            open_error = Some(format!("cannot open log file {}: {err}", p.display()));
            None
        }
    });
    let level = level_from_env();
    let nats_level = if level >= LevelFilter::DEBUG {
        level
    } else {
        level.min(LevelFilter::WARN)
    };
    let filter = Targets::new()
        .with_default(level)
        .with_target("async_nats", nats_level);
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(Tee { file })
        .with_ansi(false)
        .with_target(false);
    let _ = tracing_subscriber::registry()
        .with(layer)
        .with(filter)
        .try_init();
    if let Some(msg) = open_error {
        tracing::warn!("{msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_file_rotates_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let mut f = RotatingFile::open(&path, 100).unwrap();
        let line = [b'x'; 39];
        for _ in 0..2 {
            f.write_all(&line).unwrap();
            f.write_all(b"\n").unwrap();
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 80);
        f.write_all(&line).unwrap();
        f.write_all(b"\n").unwrap();
        let rotated = f.rotated_path();
        assert_eq!(std::fs::metadata(&rotated).unwrap().len(), 80);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 40);

        // Reopening appends and keeps counting.
        drop(f);
        let mut f = RotatingFile::open(&path, 100).unwrap();
        f.write_all(&[b'y'; 70]).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 70, "rotated again");
        assert_eq!(std::fs::metadata(&rotated).unwrap().len(), 40);
    }
}
