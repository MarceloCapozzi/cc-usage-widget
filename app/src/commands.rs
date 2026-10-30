//! Commands exposed to the frontend (`invoke` in `ui/scripts/`), registered
//! in `main.rs`. Their names and argument names are the Rust ↔ JS contract.

use crate::devflags::{demo_sessions, dev_flags, DevFlags};
use crate::links::open_url;
use crate::poller::{clear_all, emit_sessions, poll, poll_wake, remote_changed, start_publisher};
use crate::state::{config_dir, lock, AppState, TrayItems, Widget};
use crate::window::{
    nudge_into_view, register_shortcut, reset_size, save, set_auto_height, set_logical_size, WINDOW,
};
use cc_usage::{
    autostart, backup, projects, publisher, remote,
    settings::{self, Role, Settings},
    window_state as geometry,
};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, LogicalSize, Manager, PhysicalSize};

#[tauri::command]
pub(crate) async fn refresh_now(
    app: AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), ()> {
    poll(&app, state.inner()).await;
    Ok(())
}

/// The last snapshot, already serialized: when the flyout opens, it paints
/// immediately with this instead of showing up empty while it waits for the
/// network.
#[tauri::command]
pub(crate) async fn last_snapshot(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Option<String>, ()> {
    Ok(state.last.lock().await.clone())
}

#[tauri::command]
pub(crate) fn open_usage() {
    open_url("https://claude.ai/settings/usage");
}

#[tauri::command]
pub(crate) fn close_flyout(app: AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW) {
        let _ = w.hide();
    }
}

/// The development flags the frontend needs (initial view, demo mode).
#[tauri::command]
pub(crate) fn ui_flags() -> DevFlags {
    dev_flags().clone()
}

/// Sizes the widget's height to its content: the frontend measures what it
/// would need and asks for it. Only while the height follows the content
/// (first launch, or after a reset); once someone resizes the widget by hand
/// their size is respected and this does nothing, and the session list
/// scrolls when it does not fit. The top edge stays put; a widget that would
/// grow past the bottom of the work area is pulled up.
#[tauri::command]
pub(crate) fn fit_height(app: AppHandle, height: f64) {
    let Some(w) = app.get_webview_window(WINDOW) else {
        return;
    };
    if !lock(&app.state::<Widget>().auto).on {
        return;
    }
    let Some(height) = geometry::clamp_height(height) else {
        return;
    };
    let scale = w.scale_factor().unwrap_or(1.0);
    let width = w
        .outer_size()
        .map(|s| (s.width as f64 / scale).round())
        .unwrap_or(geometry::BASE_WIDTH);
    let physical: PhysicalSize<u32> = LogicalSize::new(width, height).to_physical(scale);
    if w.outer_size().is_ok_and(|s| s == physical) {
        return;
    }
    set_logical_size(&w, width, height);
    nudge_into_view(&w);
}

/// Resizing from the grips along the panel's edges.
///
/// Windows only gives a 4 px native frame on an undecorated window, which is
/// an impossible target, so the panel's edges carry grips that call here. A
/// resize by hand also ends the height following the content.
#[tauri::command]
pub(crate) fn start_resize(app: AppHandle, direction: String) -> Result<(), String> {
    use tauri_runtime::ResizeDirection::*;

    let direction = match direction.as_str() {
        "n" => North,
        "s" => South,
        "e" => East,
        "w" => West,
        "ne" => NorthEast,
        "nw" => NorthWest,
        "se" => SouthEast,
        "sw" => SouthWest,
        other => return Err(format!("unknown direction: {other}")),
    };

    let window = app.get_webview_window(WINDOW).ok_or("window not found")?;
    set_auto_height(&window, false, None);
    // `start_resize_dragging` lives on `Window`, not on `WebviewWindow`, and
    // `Manager::get_window` sits behind the `unstable` feature. The stable
    // route is to go down through the webview to its hosting window.
    AsRef::<tauri::Webview<_>>::as_ref(&window)
        .window()
        .start_resize_dragging(direction)
        .map_err(|e| e.to_string())
}

/// The double click on a resize edge: size only, see [`reset_size`].
#[tauri::command]
pub(crate) fn reset_window(app: AppHandle) -> Result<(), String> {
    let window = app.get_webview_window(WINDOW).ok_or("window not found")?;
    reset_size(&window);
    Ok(())
}

/// What the ⚙ view shows: the settings, the default shortcut for the
/// placeholder, and why the shortcut is not working, if it is not.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsView {
    shortcut: String,
    hide_on_fullscreen: settings::HideWhenFullscreen,
    hide_idle_after_mins: u32,
    remove_offline_after_mins: u32,
    refresh_mins: u32,
    theme: settings::Theme,
    /// Whether the app starts with Windows now, read from the system (the
    /// tray menu can change it too); not part of `settings.json`.
    start_with_windows: bool,
    /// The saved machine name; `None` follows the computer name.
    machine_name: Option<String>,
    /// What the local rows show: the saved name or the default.
    effective_machine_name: String,
    /// The computer name by the alias rules, for "Reset to default".
    default_machine_name: String,
    default_shortcut: &'static str,
    shortcut_error: Option<String>,
    /// The saved role (the one in force, unless a switch is restarting).
    role: Role,
    /// In the Publisher role: the alias it publishes as (this machine's
    /// name) and whether its creds and both keys are there.
    publisher: Option<publisher::Readiness>,
    /// The alias the running publisher uses, once it runs.
    publishing_as: Option<String>,
    /// The machine name changed since the Publisher role started: the new
    /// alias applies after a restart.
    restart_needed: bool,
    /// The "Remote connection" group: the servers and bucket saved, in
    /// force and by default, which variable overrides each, and what could
    /// not be used.
    nats: settings::NatsForm,
    /// The Publisher role runs with other servers or another bucket than
    /// the settings name now: they apply after a restart.
    nats_restart_needed: bool,
    /// Folder overrides set to a relative path and ignored, one line each
    /// ([`cc_usage::paths::ignored_overrides`]).
    path_problems: Vec<String>,
    /// The data folder (`%LOCALAPPDATA%\cc-usage`, or `CC_USAGE_DATA_DIR`),
    /// for texts that name a file in it.
    data_dir: String,
    /// The diagnostics log, `cc-usage.log` in the data folder.
    log_path: String,
}

fn settings_view(app: &AppHandle) -> SettingsView {
    let widget = app.state::<Widget>();
    let prefs = lock(&widget.settings).clone();
    let shortcut_error = lock(&widget.shortcut_error).clone();
    let default_machine_name = settings::default_machine_name();
    let effective = prefs.machine_name_or(&default_machine_name);
    let state = app.state::<Arc<AppState>>();
    let hub = state.publisher.get();
    let publisher = (prefs.role == Role::Publisher)
        .then(|| publisher::readiness(&publisher::Setup::from_data_dir(&effective)));
    let nats = settings::nats_form(&config_dir(app));
    let nats_restart_needed = hub.is_some_and(|h| {
        let used = &h.setup().nats;
        used.servers != nats.effective_servers || used.bucket != nats.effective_bucket
    });
    SettingsView {
        role: prefs.role,
        publisher,
        publishing_as: hub.and_then(|h| h.running()).map(|p| p.alias().to_string()),
        restart_needed: hub.is_some_and(|h| h.setup().alias != effective),
        nats,
        nats_restart_needed,
        path_problems: cc_usage::paths::ignored_overrides().to_vec(),
        effective_machine_name: prefs.machine_name_or(&default_machine_name),
        machine_name: prefs.machine_name.clone(),
        default_machine_name,
        shortcut: prefs.shortcut,
        hide_on_fullscreen: prefs.hide_on_fullscreen,
        hide_idle_after_mins: prefs.hide_idle_after_mins,
        remove_offline_after_mins: prefs.remove_offline_after_mins,
        refresh_mins: prefs.refresh_mins,
        theme: prefs.theme,
        start_with_windows: autostart::checked(&autostart::system()),
        default_shortcut: settings::DEFAULT_SHORTCUT,
        shortcut_error,
        data_dir: cc_usage::paths::data_dir().to_string_lossy().into_owned(),
        log_path: cc_usage::paths::file(cc_usage::paths::LOG)
            .to_string_lossy()
            .into_owned(),
    }
}

#[tauri::command]
pub(crate) fn get_settings(app: AppHandle) -> SettingsView {
    settings_view(&app)
}

/// Saves the settings and applies them.
///
/// A new shortcut that will not register is not saved: the previous one is
/// registered again and stays in force, and the error goes back to the field
/// it was typed into. Async, off the main thread, like the calendar's: the
/// hotkey manager hops onto the main thread itself.
///
/// `nats`, sent by the "Remote connection" group only, also saves the
/// `"nats"` object of `settings.json`: checked first with cc-remote's
/// rules ([`settings::check_nats`]), so a value the connection would refuse
/// saves nothing and its reason comes back. A setting an environment
/// variable overrides keeps what the file has. The viewer reads the new
/// values at once ([`remote::reload_nats`]); a running Publisher on its
/// next start (`natsRestartNeeded`).
#[tauri::command]
pub(crate) async fn set_settings(
    app: AppHandle,
    settings: Settings,
    nats: Option<settings::NatsInput>,
) -> Result<SettingsView, String> {
    let nats_change = match &nats {
        Some(input) => {
            let (keep_servers, keep_bucket) = settings::nats_overridden();
            Some(settings::check_nats(input, keep_servers, keep_bucket)?)
        }
        None => None,
    };
    // The machine name is checked as typed, before `sanitize` would quietly
    // reshape it: a name that breaks the alias rules is not saved, and the
    // reason goes back under its field. The default itself is stored as
    // "none", so the tag keeps following the computer name.
    let mut settings = settings;
    settings.machine_name = match settings.machine_name.as_deref() {
        Some(typed) => settings::check_machine_name(typed)?,
        None => None,
    }
    .filter(|name| *name != settings::default_machine_name());
    let previous = lock(&app.state::<Widget>().settings).clone();
    // The role changes only through `set_role`, which restarts the app.
    settings.role = previous.role;
    let next = settings.sanitize();

    if next.shortcut != previous.shortcut {
        if let Err(problem) = register_shortcut(&app, &next) {
            let restored = register_shortcut(&app, &previous).err();
            *lock(&app.state::<Widget>().shortcut_error) = restored;
            return Err(problem);
        }
        *lock(&app.state::<Widget>().shortcut_error) = None;
    }

    match &nats_change {
        Some(change) => settings::write_with_nats(&config_dir(&app), &next, change)?,
        None => settings::write(&config_dir(&app), &next)?,
    }
    let state = app.state::<Arc<AppState>>().inner().clone();
    if nats_change.is_some() && !state.is_publisher() {
        remote::reload_nats();
    }
    // Off the executor: a new machine name can write the projects file.
    let (applying, prefs) = (state.clone(), next.clone());
    let _ = tauri::async_runtime::spawn_blocking(move || applying.apply_row_settings(&prefs)).await;
    let refresh_changed = next.refresh_mins != previous.refresh_mins;
    *lock(&app.state::<Widget>().settings) = next;
    if refresh_changed {
        poll_wake().notify_one();
    }
    // A new idle delay can hide or show rows, a new offline delay drop them.
    if !dev_flags().demo {
        remote::tick_of(&state.remote, remote::now_unix());
        emit_sessions(&app, &state);
    }
    Ok(settings_view(&app))
}

/// The ⚙ view's "Start with Windows". The same switch as the tray menu's
/// item, whose check follows; the state really in force comes back in the
/// view, and a failure as the error.
#[tauri::command]
pub(crate) async fn set_start_with_windows(
    app: AppHandle,
    enabled: bool,
) -> Result<SettingsView, String> {
    let launcher = autostart::system();
    if autostart::checked(&launcher) != enabled {
        let result = autostart::toggle(&launcher);
        if let Some(item) = lock(&app.state::<TrayItems>().autostart).as_ref() {
            let _ = item.set_checked(result.checked);
        }
        if let Some(error) = result.error {
            return Err(error);
        }
    }
    Ok(settings_view(&app))
}

/// The header's clear button: see [`clear_all`].
#[tauri::command]
pub(crate) async fn clear_sessions(
    app: AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || clear_all(&app, &st))
        .await
        .map_err(|e| e.to_string())
}

/// While the ⚙ view's shortcut field is capturing, the global shortcut is
/// let go: otherwise pressing the current combination there would hide the
/// widget instead of being captured. Resuming registers the saved one again.
#[tauri::command]
pub(crate) async fn pause_shortcut(app: AppHandle, paused: bool) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    if paused {
        return app
            .global_shortcut()
            .unregister_all()
            .map_err(|e| e.to_string());
    }
    let prefs = lock(&app.state::<Widget>().settings).clone();
    let problem = register_shortcut(&app, &prefs).err();
    *lock(&app.state::<Widget>().shortcut_error) = problem.clone();
    problem.map_or(Ok(()), Err)
}

/// The current session rows, local and remote, in panel order; `null` until
/// the first local pass finished, so the flyout keeps saying it is looking
/// instead of "none".
///
/// It reads the copy of the last pass, never the monitor, so a pass scanning
/// transcripts cannot stall it; async all the same, off the main thread.
#[tauri::command]
pub(crate) async fn sessions(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Option<serde_json::Value>, String> {
    if dev_flags().demo {
        let mut rows = demo_sessions().to_vec();
        remote::name_local_rows(&mut rows, &lock(&state.machine_name));
        return Ok(Some(serde_json::Value::Array(rows)));
    }
    Ok(state.merged_rows().map(serde_json::Value::Array))
}

/// The footer's "Remote" line: not configured, connecting, connected with
/// the machine count, or retrying. Demo mode answers with the mockup's line.
#[tauri::command]
pub(crate) fn remote_status(state: tauri::State<'_, Arc<AppState>>) -> remote::RemoteStatus {
    if dev_flags().demo {
        return remote::demo_status();
    }
    state.remote_status()
}

/// The publisher keys waiting for the user's consent (the flyout's notice;
/// `RemoteStatus.pending` carries the same list). Empty in demo mode and in
/// the Publisher role.
#[tauri::command]
pub(crate) fn remote_pending_keys(
    state: tauri::State<'_, Arc<AppState>>,
) -> Vec<remote::PendingKey> {
    if dev_flags().demo || state.is_publisher() {
        return Vec::new();
    }
    remote::pending_keys_of(&state.remote)
}

/// The notice's Accept and Reject: writes the answer about the waiting key
/// `key` (its public key line) of `alias`, which must be on the list, then
/// resends the rows and the Remote line (the notice follows that event).
/// Only ever run from the click.
async fn decide_publisher_key(
    app: AppHandle,
    alias: String,
    key: String,
    accept: bool,
) -> Result<(), String> {
    if dev_flags().demo {
        return Err("demo mode".into());
    }
    let state = app.state::<Arc<AppState>>().inner().clone();
    if state.is_publisher() {
        return Err("Only the viewer accepts publisher keys".into());
    }
    let deciding = state.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if accept {
            remote::accept_key_of(&deciding.remote, &alias, &key)
        } else {
            remote::reject_key_of(&deciding.remote, &alias, &key)
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    remote_changed(&app, &state);
    Ok(())
}

/// The notice's Accept: pins the waiting key; the machine's values open
/// from its next write.
#[tauri::command]
pub(crate) async fn accept_publisher_key(
    app: AppHandle,
    alias: String,
    key: String,
) -> Result<(), String> {
    decide_publisher_key(app, alias, key, true).await
}

/// The notice's Reject: the key's values are dropped from now on.
#[tauri::command]
pub(crate) async fn reject_publisher_key(
    app: AppHandle,
    alias: String,
    key: String,
) -> Result<(), String> {
    decide_publisher_key(app, alias, key, false).await
}

/// What focusing a local session needs, from its last known row.
struct FocusTarget {
    pid: u32,
    entrypoint: Option<String>,
    cwd: Option<String>,
}

impl FocusTarget {
    fn find(state: &AppState, session_id: &str) -> Result<Self, String> {
        // Demo rows carry made-up pids: never probe or focus real windows.
        if dev_flags().demo {
            return Err("demo session".to_string());
        }
        let monitor = lock(&state.sessions);
        let view = monitor
            .view(session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        Ok(Self {
            pid: view
                .pid
                .ok_or_else(|| "the session has no known pid".to_string())?,
            entrypoint: view.entrypoint.clone(),
            cwd: view.cwd.clone(),
        })
    }

    fn request(&self) -> cc_focus::FocusRequest<'_> {
        cc_focus::FocusRequest {
            pid: self.pid,
            entrypoint: self.entrypoint.as_deref(),
            cwd: self.cwd.as_deref(),
        }
    }
}

/// Brings the window that owns a local session to the front and returns how
/// it went. It runs from the row's click: Windows only lets the foreground
/// process hand the focus over, and right after a click that is us.
///
/// The widget stays where it is: it is always on top, so the session's window
/// comes up behind it, and that is fine.
#[tauri::command]
pub(crate) async fn focus_session(
    session_id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<cc_focus::FocusReport, String> {
    let target = FocusTarget::find(state.inner(), &session_id)?;
    tauri::async_runtime::spawn_blocking(move || cc_focus::focus_with(&target.request()))
        .await
        .map_err(|e| e.to_string())
}

/// Where a click on the row would land, without changing the focus. The row
/// tooltip uses it to say whether focus will be precise.
#[tauri::command]
pub(crate) async fn focus_preview(
    session_id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<cc_focus::FocusReport, String> {
    let target = FocusTarget::find(state.inner(), &session_id)?;
    tauri::async_runtime::spawn_blocking(move || cc_focus::preview(&target.request()))
        .await
        .map_err(|e| e.to_string())
}

/// History series for the requested range, ready to draw.
///
/// It returns one point per sample with its timestamp; the frontend neither
/// interpolates nor fills gaps, because a gap is information.
#[tauri::command]
pub(crate) async fn history(
    range: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let hours = match range.as_str() {
        "1h" => 1,
        "6h" => 6,
        "1d" => 24,
        "7d" => 24 * 7,
        "30d" => 24 * 30,
        _ => 6,
    };
    let since = chrono::Utc::now() - chrono::Duration::hours(hours);
    let h = state.history.lock().await;
    serde_json::to_string(&h.range(since)).map_err(|e| e.to_string())
}

/// The widest window [`usage_by_project`] scans, days.
const MAX_USAGE_DAYS: i64 = 365;

/// The window [`usage_by_project`] scans for the `days` it was asked:
/// within 1 to [`MAX_USAGE_DAYS`], the widest when absent.
fn usage_days(days: Option<i64>) -> i64 {
    days.unwrap_or(MAX_USAGE_DAYS).clamp(1, MAX_USAGE_DAYS)
}

/// The usage view's projects: this machine's, from its transcripts, tagged
/// with its name, and the other machines' their publishers sent, tagged
/// with their alias; all priced with one table. Scanning can take a while
/// over hundreds of MB, so it runs on a blocking thread to avoid stalling
/// the async runtime.
///
/// `days` is the window, kept within 1 to [`MAX_USAGE_DAYS`]; absent, it
/// is the widest one. The frontend asks for 30, but the command is the
/// boundary: a stray value must not turn into a scan of every transcript
/// ever kept or a window that ends before it starts.
///
/// One scan runs at a time: a request made while one runs waits for it,
/// and a report younger than [`projects::REPORT_REUSE`] made for the same
/// window and machine name is answered again without scanning. The report
/// carries `pricing_path`, where this machine's `pricing.json` goes.
#[tauri::command]
pub(crate) async fn usage_by_project(
    days: Option<i64>,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let days = usage_days(days);
    let host = lock(&state.machine_name).clone();
    let mut recent = state.usage_report.lock().await;
    let key = format!("{days}|{host}");
    if let Some(json) = recent.get(&key, std::time::Instant::now()) {
        return Ok(json);
    }
    let json = scan_usage(&state, days, host).await?;
    recent.put(key, json.clone(), std::time::Instant::now());
    Ok(json)
}

/// The scan behind [`usage_by_project`], on a blocking thread.
async fn scan_usage(state: &AppState, days: i64, host: String) -> Result<String, String> {
    let days = Some(days);
    // A Publisher shows this machine only: no other machine's projects, and
    // of the calendar's saved days only its own.
    let publishing = state.is_publisher();
    let (remote, remote_activity) = if publishing {
        (Vec::new(), Vec::new())
    } else {
        (
            remote::projects_of(&state.remote),
            remote::activity_of(&state.remote),
        )
    };
    tauri::async_runtime::spawn_blocking(move || {
        let pricing = projects::load_pricing();
        let (mut all, days_scanned) = projects::local_scan(&projects::projects_dir(), days);
        for p in &mut all {
            p.host = Some(host.clone());
        }
        all.extend(
            remote
                .iter()
                .map(|r| projects::remote_usage(&r.alias, &r.payload)),
        );
        let report = projects::report_from(all, &pricing);
        // The activity calendar: this pass's days and the other machines'
        // added to activity.json, which keeps what the transcripts forget.
        let oldest = (chrono::Local::now() - chrono::Duration::days(cc_usage::activity::KEEP_DAYS))
            .format("%Y-%m-%d")
            .to_string();
        let remote_days: Vec<(String, Vec<cc_core::projects::DayActivity>)> = remote_activity
            .into_iter()
            .map(|(alias, p)| (alias, p.days))
            .collect();
        let mut activity = cc_usage::activity::update(
            &cc_usage::activity::path(),
            &cc_usage::activity::local_days(&days_scanned),
            &remote_days,
            &oldest,
            &cc_usage::activity::latest_day(),
            &host,
        );
        // Days saved under this machine's own name (its activity value from
        // when it was a Publisher) are this machine's local ones already.
        activity
            .machines
            .retain(|m| !m.remote || (!publishing && m.host != host));
        let mut json = serde_json::to_value(&report).map_err(|e| e.to_string())?;
        json["activity"] = serde_json::to_value(&activity).map_err(|e| e.to_string())?;
        // For the missing-price notice: the data folder can be moved.
        json["pricing_path"] = cc_usage::paths::file(cc_usage::paths::PRICING)
            .to_string_lossy()
            .into_owned()
            .into();
        serde_json::to_string(&json).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ── Settings → Data: backup and restore ─────────────────────────────────────

/// The backup `inspect_backup` last opened in this run: the only file
/// `restore_backup` accepts, so the webview cannot name an arbitrary one.
#[derive(Default)]
pub(crate) struct Inspected(std::sync::Mutex<Option<PathBuf>>);

/// A backup written by `backup_data`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackupSaved {
    path: String,
    /// Data files left out because they could not be read.
    skipped: Vec<String>,
}

/// The native file dialog, over the widget (which is always on top, so a
/// dialog without it as parent could open behind it), in Documents.
fn backup_dialog(
    app: &AppHandle,
    title: &str,
) -> tauri_plugin_dialog::FileDialogBuilder<tauri::Wry> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app
        .dialog()
        .file()
        .set_title(title)
        .add_filter("cc-usage backup", &[backup::EXTENSION]);
    if let Ok(docs) = app.path().document_dir() {
        dialog = dialog.set_directory(docs);
    }
    if let Some(w) = app.get_webview_window(WINDOW) {
        dialog = dialog.set_parent(&w);
    }
    dialog
}

/// "Back up…": asks where, then writes the data files (and, with
/// `include_secrets`, the keys and credentials sealed with `password`) to
/// one `.ccbak` file. `None` when the dialog was cancelled.
#[tauri::command]
pub(crate) async fn backup_data(
    app: AppHandle,
    include_secrets: bool,
    password: Option<String>,
) -> Result<Option<BackupSaved>, String> {
    let password = zeroize::Zeroizing::new(password.unwrap_or_default());
    if include_secrets {
        backup::check_password(&password)?;
    }
    let machine = lock(&app.state::<Arc<AppState>>().machine_name).clone();
    let name = backup::default_file_name(&machine, chrono::Local::now());
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<BackupSaved>, String> {
        let Some(picked) = backup_dialog(&app, "Back up cc-usage data")
            .set_file_name(name)
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let mut path = picked.into_path().map_err(|e| e.to_string())?;
        if path.extension().and_then(|e| e.to_str()) != Some(backup::EXTENSION) {
            let mut named = path.into_os_string();
            named.push(format!(".{}", backup::EXTENSION));
            path = PathBuf::from(named);
        }
        let secrets = include_secrets.then(|| (password.as_str(), backup::DEFAULT_KDF));
        let created = backup::create(
            &cc_usage::paths::data_dir(),
            &machine,
            secrets,
            chrono::Utc::now(),
        )?;
        backup::write(&path, &created.backup)?;
        Ok(Some(BackupSaved {
            path: path.to_string_lossy().into_owned(),
            skipped: created.skipped,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// "Restore…", first step: asks for a backup and says what it holds,
/// changing nothing. `None` when the dialog was cancelled.
#[tauri::command]
pub(crate) async fn inspect_backup(app: AppHandle) -> Result<Option<backup::Summary>, String> {
    let machine = lock(&app.state::<Arc<AppState>>().machine_name).clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<backup::Summary>, String> {
        let Some(picked) = backup_dialog(&app, "Restore cc-usage data").blocking_pick_file() else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(|e| e.to_string())?;
        *lock(&app.state::<Inspected>().0) = None;
        let found = backup::read(&path)?;
        let contents = backup::contents(&found)?;
        let summary = backup::summary(&found, &contents, &path, &machine);
        *lock(&app.state::<Inspected>().0) = Some(path);
        Ok(Some(summary))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// "Restore": merges the backup `inspect_backup` opened into this
/// machine's data (see `backup.rs`) and reports what changed. With a
/// password, the keys and credentials too; a wrong one changes nothing.
#[tauri::command]
pub(crate) async fn restore_backup(
    app: AppHandle,
    path: String,
    password: Option<String>,
) -> Result<backup::Report, String> {
    let password = password.map(zeroize::Zeroizing::new);
    let path = PathBuf::from(path);
    if lock(&app.state::<Inspected>().0).as_ref() != Some(&path) {
        return Err("choose the backup again with Restore…".into());
    }
    let state = app.state::<Arc<AppState>>().inner().clone();
    let machine = lock(&state.machine_name).clone();
    let report = tauri::async_runtime::spawn_blocking(move || -> Result<backup::Report, String> {
        let found = backup::read(&path)?;
        let prepared = backup::prepare(&found, password.as_deref().map(String::as_str))?;
        let data_dir = cc_usage::paths::data_dir();
        let target = backup::Target::new(&data_dir, &machine);
        // A Publisher runs no viewer hub: the projects go to the file a
        // later switch to Viewer loads.
        let hub = if state.is_publisher() {
            remote::projects_file_hub(data_dir.join(remote::REMOTE_PROJECTS_FILE), target.now)
        } else {
            state.remote.clone()
        };
        let mut history = state.history.blocking_lock();
        backup::restore(&prepared, &mut history, &hub, &target).map_err(String::from)
    })
    .await
    .map_err(|e| e.to_string())??;
    *lock(&app.state::<Inspected>().0) = None;
    Ok(report)
}

// ── Settings → Role ─────────────────────────────────────────────────────────

/// "Role": saves the new role and restarts the app, which starts in it
/// (there is no live switch between viewing and publishing). The flyout
/// asks first; nothing happens when the role is already in force.
#[tauri::command]
pub(crate) async fn set_role(app: AppHandle, role: Role) -> Result<(), String> {
    let mut next = lock(&app.state::<Widget>().settings).clone();
    if next.role == role {
        return Ok(());
    }
    next.role = role;
    settings::write(&config_dir(&app), &next)?;
    *lock(&app.state::<Widget>().settings) = next;
    tracing::info!("role set to {role:?}; restarting");
    restart(&app)
}

/// "Restart cc-usage", for a new machine name in the Publisher role.
#[tauri::command]
pub(crate) async fn restart_app(app: AppHandle) -> Result<(), String> {
    restart(&app)
}

/// Saves the window and starts the app again. The data folder is let go
/// first, so the new process is not taken for a second copy and ended.
fn restart(app: &AppHandle) -> ! {
    if let Some(w) = app.get_webview_window(WINDOW) {
        save(&w);
    }
    cc_usage::instance::release();
    app.restart()
}

/// What a Publisher setup action did: the settings view again and a line
/// to show under the buttons.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublisherAction {
    view: SettingsView,
    note: String,
    /// The file was installed but the running publisher could not use it
    /// (it keeps what it had): the note shows as an error.
    failed: bool,
    /// "Generate key": the public key line, for the Copy button.
    public_key: Option<String>,
}

/// The Publisher files of this machine's current name, or why they cannot
/// be changed now: only in the Publisher role (the viewer's own
/// `viewer.pub` must never be replaced by another machine's).
fn publisher_setup(app: &AppHandle) -> Result<publisher::Setup, String> {
    let prefs = lock(&app.state::<Widget>().settings).clone();
    if prefs.role != Role::Publisher {
        return Err("Switch the role to Publisher first".into());
    }
    Ok(publisher::Setup::from_data_dir(
        &prefs.machine_name_or(&settings::default_machine_name()),
    ))
}

/// After a setup action: a running publisher picks up the files now and
/// the note says what it applied; otherwise try to start publishing (it
/// may be ready now). Answers with the fresh view.
fn publisher_done(
    app: &AppHandle,
    mut note: String,
    public_key: Option<String>,
) -> PublisherAction {
    let applied = app
        .state::<Arc<AppState>>()
        .publisher
        .get()
        .and_then(|hub| hub.apply_files());
    let mut failed = false;
    if let Some((line, not_applied)) = applied {
        note = format!("{note} {line}");
        failed = not_applied;
    }
    start_publisher(app);
    PublisherAction {
        view: settings_view(app),
        note,
        failed,
        public_key,
    }
}

/// "Generate key": creates `keys\publisher.key` if it is missing and
/// returns its public key line, which the viewer's user is asked to accept.
/// Without `confirm` an existing key is kept; "Replace key…" passes the
/// alias typed back as `confirm`, and only a match replaces it (see
/// [`publisher::replace_key`]).
#[tauri::command]
pub(crate) async fn publisher_generate_key(
    app: AppHandle,
    confirm: Option<String>,
) -> Result<PublisherAction, String> {
    let setup = publisher_setup(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let had_key = setup.publisher_key.exists();
        let generated = match confirm.as_deref() {
            Some(typed) => publisher::replace_key(&setup, typed, remote::now_unix())?,
            None => publisher::generate_key(&setup, remote::now_unix())?,
        };
        let note = match (generated.created, had_key) {
            (true, false) => "Created this machine's publisher key. The viewer asks to accept it when its first value arrives.",
            (true, true) => "Replaced this machine's publisher key. Until the new key is accepted on the viewer, its values are held back there.",
            (false, _) => "This machine already has a publisher key; it was kept.",
        };
        Ok(publisher_done(&app, note.to_string(), Some(generated.public_key)))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A native open dialog over the widget, for one kind of file.
fn import_dialog(
    app: &AppHandle,
    title: &str,
    filter: &str,
    extension: &str,
) -> tauri_plugin_dialog::FileDialogBuilder<tauri::Wry> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app
        .dialog()
        .file()
        .set_title(title)
        .add_filter(filter, &[extension]);
    if let Ok(docs) = app.path().document_dir() {
        dialog = dialog.set_directory(docs);
    }
    if let Some(w) = app.get_webview_window(WINDOW) {
        dialog = dialog.set_parent(&w);
    }
    dialog
}

/// "Import viewer key…": asks for the viewer's `viewer.pub` and installs it
/// as `keys\viewer.pub`. Only the file the dialog returned is read. `None`
/// when the dialog was cancelled.
#[tauri::command]
pub(crate) async fn publisher_import_viewer_key(
    app: AppHandle,
) -> Result<Option<PublisherAction>, String> {
    let setup = publisher_setup(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let Some(picked) = import_dialog(
            &app,
            "Import the viewer's public key",
            "Viewer public key",
            "pub",
        )
        .blocking_pick_file() else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(|e| e.to_string())?;
        let imported = publisher::import_viewer_key(&setup, &path)?;
        let note = if imported.unchanged {
            format!("The viewer key {} was already installed.", imported.kid)
        } else {
            format!("Installed the viewer key {}.", imported.kid)
        };
        Ok(Some(publisher_done(&app, note, None)))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// "Import creds…": asks for this machine's `cc-sessions.pub-<alias>.creds`
/// and copies it, privately, to the `nats` folder. Only the file the dialog
/// returned is read. `None` when the dialog was cancelled.
#[tauri::command]
pub(crate) async fn publisher_import_creds(
    app: AppHandle,
) -> Result<Option<PublisherAction>, String> {
    let setup = publisher_setup(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let title = format!("Import the credentials of {}", setup.user());
        let Some(picked) =
            import_dialog(&app, &title, "NATS credentials", "creds").blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(|e| e.to_string())?;
        let written = publisher::import_creds(&setup, &path)?;
        let note = format!(
            "Installed the credentials of {} as {}.",
            setup.user(),
            written.display()
        );
        Ok(Some(publisher_done(&app, note, None)))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usage_window_stays_within_a_year() {
        assert_eq!(usage_days(Some(30)), 30);
        assert_eq!(usage_days(Some(400)), MAX_USAGE_DAYS);
        assert_eq!(usage_days(Some(0)), 1);
        assert_eq!(usage_days(Some(-5)), 1);
        assert_eq!(usage_days(None), MAX_USAGE_DAYS);
    }
}
