//! [`RemoteHub`]: the remote map plus the connection state, shared between
//! the background tasks and the frontend commands, and the footer's
//! [`RemoteStatus`].

use std::sync::{Arc, Mutex, MutexGuard};

use cc_core::projects::ActivityPayload;
use cc_envelope::ViewerKeys;
use serde::Serialize;

use super::map::{RemoteMap, StoredProject};
use super::review::{review_text, PendingFile, PendingKey, MAX_PENDING_ALIASES};
use super::store::{flush_projects_of, ProjectsFile};
use super::view::RemoteRow;
use super::MAX_ALIASES;

/// The state of the viewer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnState {
    /// The viewer creds or the envelope key file are missing (or unusable).
    NotConfigured,
    /// First connection attempt in progress.
    Connecting,
    /// Connected and watching the bucket.
    Connected,
    /// Lost or failing; retrying with backoff.
    Disconnected,
}

/// The footer's "Remote" line, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    /// Connection state.
    pub state: ConnState,
    /// Machines with a fresh heartbeat.
    pub machines: usize,
    /// The line's text, e.g. `2 machines · connected`.
    pub text: String,
    /// More detail for the tooltip: why it is not configured, the last
    /// error, the server, values that could not be opened.
    pub detail: Option<String>,
    /// Values that failed to open since the app started.
    pub undecryptable: u64,
    /// Publisher keys waiting for the user's consent, for the flyout's
    /// notice; always empty in the Publisher role.
    pub pending: Vec<PendingKey>,
}

/// The footer text for a state. User-facing.
pub fn status_text(state: ConnState, machines: usize) -> String {
    match state {
        ConnState::NotConfigured => "not configured".into(),
        ConnState::Connecting => "connecting…".into(),
        ConnState::Connected => format!(
            "{machines} {} · connected",
            if machines == 1 { "machine" } else { "machines" }
        ),
        ConnState::Disconnected => "disconnected · retrying".into(),
    }
}

/// DEMO: the mockup's footer line, "3 machines · connected".
pub fn demo_status() -> RemoteStatus {
    RemoteStatus {
        state: ConnState::Connected,
        machines: 3,
        text: status_text(ConnState::Connected, 3),
        detail: Some("Demo data".into()),
        undecryptable: 0,
        pending: Vec::new(),
    }
}

/// The remote map plus the connection state, shared between the background
/// tasks and the frontend commands.
#[derive(Debug)]
pub struct RemoteHub {
    pub(super) map: RemoteMap,
    state: ConnState,
    /// The server in use while connected, or the reason for the state.
    detail: Option<String>,
    /// NATS settings that could not be used and were replaced by the
    /// defaults ([`crate::settings::Nats::problems`]).
    config_problems: Vec<String>,
    /// Where other machines' projects are kept, see [`RemoteHub::keep_projects_in`].
    pub(super) projects_file: Option<ProjectsFile>,
    /// Where the keys waiting for review are kept (the pin folder), see
    /// [`RemoteHub::keep_pending_in`].
    pub(super) pending_file: Option<PendingFile>,
    /// The keys the viewer task opens values with, so an answer about a
    /// waiting key counts at once ([`super::accept_key_of`]).
    pub(super) keys: Option<Arc<ViewerKeys>>,
}

impl Default for RemoteHub {
    fn default() -> Self {
        Self::new()
    }
}

/// A [`RemoteHub`] behind a mutex. Only ever locked briefly, never across
/// an `.await`.
pub type SharedHub = Arc<Mutex<RemoteHub>>;

/// Called whenever the rows or the status may have changed.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

impl RemoteHub {
    /// No rows, not configured until [`run`] says otherwise.
    ///
    /// [`run`]: super::run
    pub fn new() -> Self {
        Self {
            map: RemoteMap::new(),
            state: ConnState::NotConfigured,
            detail: None,
            config_problems: Vec::new(),
            projects_file: None,
            pending_file: None,
            keys: None,
        }
    }

    /// Hands the hub the keys the viewer task opens values with (or none).
    pub fn set_keys(&mut self, keys: Option<Arc<ViewerKeys>>) {
        self.keys = keys;
    }

    /// A new hub behind an `Arc<Mutex<_>>`.
    pub fn shared() -> SharedHub {
        Arc::new(Mutex::new(Self::new()))
    }

    /// The map, for applying changes.
    pub fn map_mut(&mut self) -> &mut RemoteMap {
        &mut self.map
    }

    /// The map.
    pub fn map(&self) -> &RemoteMap {
        &self.map
    }

    /// Sets the connection state and its detail. Returns true if it changed.
    pub fn set_state(&mut self, state: ConnState, detail: Option<String>) -> bool {
        let changed = self.state != state || self.detail != detail;
        self.state = state;
        self.detail = detail;
        changed
    }

    /// The connection state.
    pub fn state(&self) -> ConnState {
        self.state
    }

    /// Sets the NATS settings that were ignored, shown in the tooltip
    /// under the state's detail. Returns true if they changed.
    pub fn set_config_problems(&mut self, problems: Vec<String>) -> bool {
        let changed = self.config_problems != problems;
        self.config_problems = problems;
        changed
    }

    /// The remote rows at `now`.
    pub fn rows(&self, now: i64) -> Vec<RemoteRow> {
        self.map.rows(now)
    }

    /// See [`RemoteMap::tick`].
    pub fn tick(&mut self, now: i64) -> bool {
        self.map.tick(now)
    }

    /// See [`RemoteMap::dismiss_all`].
    pub fn dismiss_all(&mut self, now: i64) -> usize {
        self.map.dismiss_all(now)
    }

    /// See [`RemoteMap::set_drop_after`].
    pub fn set_drop_after(&mut self, secs: Option<i64>) {
        self.map.set_drop_after(secs);
    }

    /// See [`RemoteMap::set_own_alias`].
    pub fn set_own_alias(&mut self, alias: Option<String>) -> bool {
        self.map.set_own_alias(alias)
    }

    /// The footer line at `now`.
    pub fn status(&self, now: i64) -> RemoteStatus {
        let machines = self.map.machines(now);
        let undecryptable = self.map.undecryptable();
        let mut detail: Vec<String> = self.detail.iter().cloned().collect();
        detail.extend(self.config_problems.iter().cloned());
        let senders = self.map.unknown_senders();
        for (alias, _) in &senders {
            detail.push(format!("{alias}: unknown sender key — values rejected"));
        }
        let by_sender: u64 = senders.iter().map(|(_, n)| n).sum();
        let other = undecryptable.saturating_sub(by_sender);
        if other > 0 {
            detail.push(format!(
                "{other} value(s) could not be opened (wrong key, tampered, or an unsupported envelope version) and are not shown"
            ));
        }
        let pending = self.map.pending_keys();
        if let Some(text) = review_text(pending.len()) {
            detail.push(format!("{text} (values held back until accepted)"));
        }
        let unnoted = self.map.pending.over_cap();
        if unnoted > 0 {
            detail.push(format!(
                "more than {MAX_PENDING_ALIASES} machines wait for review: {unnoted} value(s) of the others ignored"
            ));
        }
        for (alias, n) in self.map.rejected_values() {
            detail.push(format!("{alias}: {n} value(s) from a rejected key dropped"));
        }
        let over = self.map.over_alias_cap();
        if over > 0 {
            detail.push(format!(
                "more than {MAX_ALIASES} machines: {over} value(s) of the others ignored"
            ));
        }
        RemoteStatus {
            state: self.state,
            machines,
            text: status_text(self.state, machines),
            detail: (!detail.is_empty()).then(|| detail.join("\n")),
            undecryptable,
            pending,
        }
    }
}

pub(super) fn lock(hub: &SharedHub) -> MutexGuard<'_, RemoteHub> {
    hub.lock().unwrap_or_else(|e| e.into_inner())
}

/// Other machines' activity held by `hub`, by alias.
pub fn activity_of(hub: &SharedHub) -> Vec<(String, ActivityPayload)> {
    lock(hub).map().activity()
}

/// Other machines' projects held by `hub`.
pub fn projects_of(hub: &SharedHub) -> Vec<StoredProject> {
    lock(hub).map().projects()
}

/// Adds projects from a restored backup to `hub` by the rules of
/// [`RemoteMap::load_projects`] and saves the projects file if they
/// changed. Returns how many were added and how many updated.
pub fn restore_projects_of(
    hub: &SharedHub,
    stored: Vec<StoredProject>,
    now: i64,
) -> (usize, usize) {
    let counts = lock(hub).map_mut().load_projects(stored, now);
    flush_projects_of(hub);
    counts
}

/// The remote rows of `hub` that started needing the user since the last
/// call, for a toast each (see [`RemoteMap::take_needs_you`]).
pub fn take_needs_you_of(hub: &SharedHub, now: i64) -> Vec<RemoteRow> {
    lock(hub).map_mut().take_needs_you(now)
}

/// The toast for a remote session that moved to *needs you*, as
/// `(title, body)`: the local one's, with the machine named. User-facing.
pub fn needs_you_toast(row: &RemoteRow) -> (String, String) {
    let (_, body) = crate::sessions::needs_you_toast(&row.view);
    (
        format!("{} on {} needs you", row.view.display_name, row.alias),
        body,
    )
}

/// The remote rows of `hub` at `now`, for callers that hold only the hub.
pub fn rows_of(hub: &SharedHub, now: i64) -> Vec<RemoteRow> {
    lock(hub).rows(now)
}

/// The footer line of `hub` at `now`.
pub fn status_of(hub: &SharedHub, now: i64) -> RemoteStatus {
    lock(hub).status(now)
}

/// Applies the time-based rules of `hub` now (see [`RemoteMap::tick`]).
pub fn tick_of(hub: &SharedHub, now: i64) -> bool {
    lock(hub).tick(now)
}

/// Dismisses every remote row of `hub` (the clear button).
pub fn dismiss_all_of(hub: &SharedHub, now: i64) -> usize {
    lock(hub).dismiss_all(now)
}

/// Sets the drop delay of `hub`'s offline rows, in seconds (`None`: keep them).
pub fn set_drop_after_of(hub: &SharedHub, secs: Option<i64>) {
    lock(hub).set_drop_after(secs);
}

/// Makes `hub` ignore this machine's own alias (see
/// [`RemoteMap::set_own_alias`]) and saves the projects file if that
/// dropped any. Returns true if anything was dropped.
pub fn set_own_alias_of(hub: &SharedHub, alias: Option<String>) -> bool {
    let dropped = lock(hub).set_own_alias(alias);
    flush_projects_of(hub);
    dropped
}
