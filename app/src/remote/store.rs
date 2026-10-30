//! [`REMOTE_PROJECTS_FILE`]: other machines' projects kept on disk, and the
//! spaced writes that save them.
//!
//! [`REMOTE_PROJECTS_FILE`]: super::REMOTE_PROJECTS_FILE

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::hub::{lock, RemoteHub, SharedHub};
use super::map::StoredProject;
use super::PROJECTS_SAVE_INTERVAL;

/// Largest projects file loaded: 8 MB. A project takes a kilobyte or two
/// in the file, so that is thousands of projects, far more than the
/// machines a viewer follows have; a larger file is not parsed, so a file
/// grown or planted by something else cannot stall the start.
pub(super) const MAX_PROJECTS_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The projects saved in `path`: none when there is no file, or it cannot
/// be read or parsed (the values come back from the bucket). A file over
/// [`MAX_PROJECTS_FILE_BYTES`] is not parsed: it is kept aside
/// ([`crate::paths::keep_corrupt`], which says so in the log), and a new one
/// is written with the next change.
fn read_projects(path: &Path) -> Vec<StoredProject> {
    let mut bytes = Vec::new();
    {
        let Ok(file) = std::fs::File::open(path) else {
            return Vec::new();
        };
        // One byte past the limit tells a file over it, however large.
        if file
            .take(MAX_PROJECTS_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return Vec::new();
        }
    }
    if bytes.len() as u64 > MAX_PROJECTS_FILE_BYTES {
        tracing::warn!(
            "remote: {} is over {} MB, not loaded",
            path.display(),
            MAX_PROJECTS_FILE_BYTES / (1024 * 1024)
        );
        crate::paths::keep_corrupt(path);
        return Vec::new();
    }
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// [`REMOTE_PROJECTS_FILE`](super::REMOTE_PROJECTS_FILE) and the bookkeeping
/// of its writes.
#[derive(Debug)]
pub(super) struct ProjectsFile {
    path: PathBuf,
    /// When a write was last handed out.
    saved_at: Option<Instant>,
    /// Numbers the writes handed out, so that of two writes running at
    /// once the older one cannot finish last and win.
    generation: u64,
    /// The newest generation written, shared with the writes.
    written: Arc<Mutex<u64>>,
}

/// One write of the projects file: its content, taken while the hub was
/// locked, written by [`ProjectsWrite::run`] after the lock is let go.
#[derive(Debug)]
pub struct ProjectsWrite {
    path: PathBuf,
    json: String,
    generation: u64,
    written: Arc<Mutex<u64>>,
}

impl ProjectsWrite {
    /// Writes the file, unless a newer write got there first. Blocking. A
    /// failure costs only the copy on disk: the values in memory are right.
    pub fn run(self) {
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if *written >= self.generation {
            return;
        }
        // Atomic: a truncated file would load as no projects at all.
        match crate::paths::write_atomic(&self.path, self.json.as_bytes()) {
            Ok(()) => *written = self.generation,
            Err(err) => tracing::warn!("remote: cannot save {}: {err}", self.path.display()),
        }
    }
}

impl RemoteHub {
    /// Loads other machines' projects from `path` and saves them there
    /// whenever they change. Without it (tests) nothing touches the disk.
    /// What is loaded goes through [`RemoteMap::load_projects`], so it is
    /// cut like a value heard live; a file over 8 MB is not loaded at all
    /// but kept aside (see `read_projects`).
    ///
    /// [`RemoteMap::load_projects`]: super::RemoteMap::load_projects
    pub fn keep_projects_in(&mut self, path: PathBuf, now: i64) {
        let stored = read_projects(&path);
        self.map.load_projects(stored, now);
        self.map.take_projects_changed();
        self.projects_file = Some(ProjectsFile {
            path,
            saved_at: None,
            generation: 0,
            written: Arc::new(Mutex::new(0)),
        });
    }

    /// The write due after a change of the projects, or `None` when nothing
    /// changed or nothing is kept on disk. Unless `force`, also `None` while
    /// the last write handed out is less than [`PROJECTS_SAVE_INTERVAL`]
    /// old (at `now`): the change stays pending for a later call.
    pub fn projects_to_save(&mut self, now: Instant, force: bool) -> Option<ProjectsWrite> {
        if let Some(file) = &self.projects_file {
            let recent = file
                .saved_at
                .is_some_and(|at| now.saturating_duration_since(at) < PROJECTS_SAVE_INTERVAL);
            if recent && !force {
                return None;
            }
        }
        if !self.map.take_projects_changed() {
            return None;
        }
        let file = self.projects_file.as_mut()?;
        let json = serde_json::to_string_pretty(&self.map.projects()).ok()?;
        file.saved_at = Some(now);
        file.generation += 1;
        Some(ProjectsWrite {
            path: file.path.clone(),
            json,
            generation: file.generation,
            written: file.written.clone(),
        })
    }
}

/// Hands the projects file to tokio's blocking pool if the projects changed
/// and the last write is at least [`PROJECTS_SAVE_INTERVAL`] old; a change
/// held back is written by a later call (the ticker's, within
/// [`TICK_INTERVAL`]). Never waits for the disk. Needs a tokio runtime.
///
/// [`TICK_INTERVAL`]: super::TICK_INTERVAL
pub(super) fn save_projects_soon(hub: &SharedHub) {
    if let Some(write) = lock(hub).projects_to_save(Instant::now(), false) {
        tokio::task::spawn_blocking(move || write.run());
    }
}

/// A hub that holds only the projects of the file at `path`, loaded and
/// saved like the viewer's: what the Publisher role, which runs no viewer
/// hub, restores a backup's projects into, so they reach the file a later
/// switch to Viewer loads, merged with what it holds.
pub fn projects_file_hub(path: PathBuf, now: i64) -> SharedHub {
    let hub = RemoteHub::shared();
    lock(&hub).keep_projects_in(path, now);
    hub
}

/// Writes the projects file now if the projects changed, whatever the
/// interval, on the calling thread: for a restore, a change of this
/// machine's name, and the app's exit, so nothing pending is lost.
/// Blocking.
pub fn flush_projects_of(hub: &SharedHub) {
    let write = lock(hub).projects_to_save(Instant::now(), true);
    if let Some(write) = write {
        write.run();
    }
}
