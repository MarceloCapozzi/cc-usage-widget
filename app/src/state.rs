//! The state Tauri manages: [`AppState`] (the poller and the commands),
//! [`Widget`] (window and preferences) and [`TrayItems`] (the tray menu items
//! that change after it is built), and the small helpers every module uses.

use cc_core::state::SessionView;
use cc_usage::{
    api, history, notifications, projects, publisher, remote, sessions as local,
    settings::{self, Role, Settings},
};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tauri::{menu::CheckMenuItem, AppHandle, Manager};
use tokio::sync::Mutex;

/// State shared between the poller and the frontend commands.
pub(crate) struct AppState {
    pub(crate) client: api::Client,
    /// The profile rarely changes: it is fetched once per login and reused,
    /// so as not to spend a call per poll on data that is almost static.
    pub(crate) profile: Mutex<Option<cc_usage::CachedProfile>>,
    /// The usage view's last report: one scan at a time, and a report
    /// younger than [`projects::REPORT_REUSE`] is reused.
    pub(crate) usage_report: Mutex<projects::RecentReport>,
    pub(crate) notif_state: Mutex<notifications::State>,
    pub(crate) history: Mutex<history::History>,
    pub(crate) last: Mutex<Option<String>>,
    /// Local sessions. A std mutex: it is only locked on blocking threads and
    /// for short reads from commands, never across an `.await`.
    pub(crate) sessions: std::sync::Mutex<local::SessionMonitor>,
    /// The local rows of the last pass (`None` before the first one), copied
    /// out of the monitor so merging in remote rows never waits for a pass.
    pub(crate) local_rows: std::sync::Mutex<Option<Vec<SessionView>>>,
    /// Remote sessions from the KV bucket and the viewer connection state.
    pub(crate) remote: remote::SharedHub,
    /// The usage part of the tray tooltip, kept so the session poller can
    /// rebuild the tooltip without waiting for the next usage poll.
    pub(crate) usage_tooltip: std::sync::Mutex<String>,
    /// "Hide idle sessions after", from the settings.
    pub(crate) idle_filter: std::sync::Mutex<local::IdleFilter>,
    /// Which rows the flyout was last sent and which of them were hidden,
    /// so a row crossing the idle delay is sent without any other change.
    pub(crate) sent_rows: std::sync::Mutex<Option<Vec<(String, bool)>>>,
    /// "This machine's name" from the settings: the tag on local rows.
    pub(crate) machine_name: std::sync::Mutex<String>,
    /// The role this run was started in, read once at startup: switching
    /// it restarts the app.
    pub(crate) role: OnceLock<Role>,
    /// The Publisher role's state, set at startup when the role is
    /// Publisher.
    pub(crate) publisher: OnceLock<Arc<publisher::PublisherHub>>,
}

impl AppState {
    /// Whether this run publishes (Publisher role) instead of viewing.
    pub(crate) fn is_publisher(&self) -> bool {
        self.role.get() == Some(&Role::Publisher)
    }

    /// The footer's "Remote" line: the viewer connection's, or in the
    /// Publisher role the publishing one's.
    pub(crate) fn remote_status(&self) -> remote::RemoteStatus {
        match self.publisher.get() {
            Some(hub) => hub.status(),
            None => remote::status_of(&self.remote, remote::now_unix()),
        }
    }

    /// Every row the flyout shows, local and remote, in panel order and in
    /// the frontend's JSON shape, each marked `hidden` or not by the idle
    /// filter; `None` until the first local pass.
    pub(crate) fn merged_rows(&self) -> Option<Vec<serde_json::Value>> {
        let local = self.local_rows.lock().ok()?.clone()?;
        let now = remote::now_unix();
        let remote = remote::rows_of(&self.remote, now);
        let filter = *lock(&self.idle_filter);
        let mut rows = remote::merge_rows_with(&local, &remote, filter, local::now_secs());
        remote::name_local_rows(&mut rows, &lock(&self.machine_name));
        Some(rows)
    }

    /// Local and remote rows as plain views, for the tray tooltip: the ones
    /// the flyout shows, without those the idle filter hides.
    pub(crate) fn all_views(&self) -> Vec<SessionView> {
        let mut views = self
            .local_rows
            .lock()
            .ok()
            .and_then(|r| r.clone())
            .unwrap_or_default();
        views.extend(
            remote::rows_of(&self.remote, remote::now_unix())
                .into_iter()
                .map(|r| r.view),
        );
        let filter = *lock(&self.idle_filter);
        let now = local::now_secs();
        views.retain(|v| !filter.hides(v, now));
        views
    }

    /// Applies the settings that shape the rows: the idle filter, how long
    /// offline remote rows stay, and the local rows' machine name, which a
    /// viewer also ignores in the bucket (it may have published before).
    /// Blocking: it writes the projects file at once when a change of them
    /// is pending ([`remote::set_own_alias_of`]), so an async caller runs
    /// it on the blocking pool.
    pub(crate) fn apply_row_settings(&self, prefs: &Settings) {
        *lock(&self.idle_filter) = local::IdleFilter::after_minutes(prefs.hide_idle_after_mins);
        remote::set_drop_after_of(&self.remote, prefs.remove_offline_after_secs());
        let name = prefs.machine_name_or(&settings::default_machine_name());
        *lock(&self.machine_name) = name.clone();
        if !self.is_publisher() {
            remote::set_own_alias_of(&self.remote, Some(name));
        }
    }
}

/// Window and preference state. Std mutexes: locked briefly, never across an
/// `.await` and never while calling into the window (whose events can run
/// handlers that lock them too).
#[derive(Default)]
pub(crate) struct Widget {
    /// Current settings. The file on disk is the durable copy; this is the
    /// one consulted by the full-screen watcher every turn.
    pub(crate) settings: std::sync::Mutex<Settings>,
    /// Why the shortcut is not working, if it is not; shown in the ⚙ view.
    pub(crate) shortcut_error: std::sync::Mutex<Option<String>>,
    /// When it last moved or resized; `None` once saved.
    pub(crate) pending_move: std::sync::Mutex<Option<Instant>>,
    pub(crate) auto: std::sync::Mutex<AutoHeight>,
}

/// Whether the height follows the content, and the logical size the last
/// fit asked for: a resize to anything else was done by hand and ends it.
#[derive(Default, Clone, Copy)]
pub(crate) struct AutoHeight {
    pub(crate) on: bool,
    pub(crate) expected: Option<(f64, f64)>,
}

/// The tray menu items that change after the menu is built.
#[derive(Default)]
pub(crate) struct TrayItems {
    /// "Start with Windows", checked while the app starts with Windows.
    pub(crate) autostart: std::sync::Mutex<Option<CheckMenuItem<tauri::Wry>>>,
}

pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where `window.json` and `settings.json` live: the app config dir
/// (`%APPDATA%\ar.cc.usage`, outside OneDrive), or
/// `CC_USAGE_CONFIG_DIR` for a test instance that must not touch it.
pub(crate) fn config_dir(app: &AppHandle) -> PathBuf {
    cc_usage::paths::config_dir_override()
        .or_else(|| app.path().app_config_dir().ok())
        .unwrap_or_else(cc_usage::paths::data_dir)
}
