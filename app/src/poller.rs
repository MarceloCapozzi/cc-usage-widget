//! The loops that feed the flyout and the tray: the usage poll (with its
//! alerts and history samples), the local session pass, the Publisher
//! role's start, and sending the session rows to the frontend.

use crate::devflags::{dev_flags, notification_settings};
use crate::state::{lock, AppState};
use crate::tray::{refresh_tooltip, update_tray};
use cc_core::state::SessionView;
use cc_usage::{history, refresh_cached, remote, sessions as local, CachedProfile, Snapshot};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

/// Wakes the usage poll early: a new "Refresh usage every" applies at once
/// instead of after the interval in force runs out.
static POLL_WAKE: OnceLock<tokio::sync::Notify> = OnceLock::new();

pub(crate) fn poll_wake() -> &'static tokio::sync::Notify {
    POLL_WAKE.get_or_init(tokio::sync::Notify::new)
}

/// Events emitted to the frontend (listened to in `ui/scripts/`).
const EVENT_USAGE: &str = "usage";
/// The session rows changed; the payload is the full list of rows, local and
/// remote: `SessionView`s plus `origin` (see `remote::merge_rows`).
const EVENT_SESSIONS: &str = "sessions";
/// The footer's "Remote" line changed; the payload is a `remote::RemoteStatus`.
const EVENT_REMOTE_STATUS: &str = "remote-status";

/// Set to any value to log the first session pass (`cc-usage.log`).
const DEBUG_ENV: &str = "CC_USAGE_DEBUG";

/// One usage refresh. Returns whether the token was expired, so the loop
/// can watch for Claude Code's renewal instead of waiting a full interval.
pub(crate) async fn poll(app: &AppHandle, state: &Arc<AppState>) -> bool {
    let cache = state.profile.lock().await.clone();
    let mut snap = refresh_cached(&state.client, cache.as_ref()).await;
    // A relative CC_USAGE_DATA_DIR or CLAUDE_CONFIG_DIR, ignored: named in
    // the footer's tooltips (and logged once, at the first poll).
    snap.path_problems = cc_usage::paths::ignored_overrides().to_vec();
    {
        let mut note = RENEW_NOTE.lock().unwrap_or_else(|e| e.into_inner());
        if snap.token_expired {
            snap.renew_note = note.clone();
        } else {
            // Renewed: the next expiry starts with a fresh request.
            *note = None;
            *LAST_NUDGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
    // DEMO: the footer shows the mockup's usage, so a screenshot never
    // depends on the network or on the account.
    if dev_flags().demo {
        snap.usage = Some(cc_usage::demo::usage(chrono::Utc::now()));
        snap.usage_error = None;
        snap.token_expired = false;
    }

    // The profile is cached only once it brought an email, so as not to pin
    // an empty one that would prevent retrying on the next poll, and with
    // the login it belongs to: another account's login fetches it again.
    if let (Some(email), Some(account)) = (&snap.email, snap.account) {
        let fresh = CachedProfile {
            account,
            email: email.clone(),
        };
        if cache.as_ref() != Some(&fresh) {
            *state.profile.lock().await = Some(fresh);
        }
    }

    update_tray(app, &snap);
    notify(app, state, &snap).await;
    // History is only recorded for refreshes that brought data: a failure
    // leaves a gap, which means "could not read" and not "usage was zero".
    if let Some(usage) = &snap.usage {
        record_history(state, usage).await;
    }

    if let Ok(json) = serde_json::to_string(&snap) {
        *state.last.lock().await = Some(json.clone());
        let _ = app.emit(EVENT_USAGE, json);
    }
    snap.token_expired
}

/// While the token is expired, how often the credentials file is read again.
/// Only the local file: the API is not called until the token is renewed.
const RENEWAL_CHECK: std::time::Duration = std::time::Duration::from_secs(20);

/// While the token is expired, how often Claude Code is asked to renew it
/// (`claude -p` with no prompt, see [`ask_claude_to_renew`]).
const RENEWAL_NUDGE_EVERY: std::time::Duration = std::time::Duration::from_secs(300);

/// How the last request to renew went, for the footer (Snapshot::renew_note).
static RENEW_NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// When Claude Code was last asked to renew, for [`RENEWAL_NUDGE_EVERY`].
static LAST_NUDGE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Whether a nudge is due at `now` after one at `last`; records `now` when it is.
fn nudge_due(last: &mut Option<std::time::Instant>, now: std::time::Instant) -> bool {
    if last.is_some_and(|t| now.saturating_duration_since(t) < RENEWAL_NUDGE_EVERY) {
        return false;
    }
    *last = Some(now);
    true
}

/// Asks Claude Code to renew its expired token by running `claude -p` with
/// no prompt and an empty stdin (cc-local's `ask_to_renew`), at most every
/// [`RENEWAL_NUDGE_EVERY`]. The widget never renews the token itself: the
/// file is Claude Code's, and a renewal can rotate the refresh token it
/// holds. Off the async executor; failures are only logged.
async fn ask_claude_to_renew() {
    let due = nudge_due(
        &mut LAST_NUDGE.lock().unwrap_or_else(|e| e.into_inner()),
        std::time::Instant::now(),
    );
    if !due || dev_flags().demo {
        return;
    }
    let result = tauri::async_runtime::spawn_blocking(cc_local::agents::ask_to_renew).await;
    let at = chrono::Local::now().format("%H:%M");
    let note = match result {
        Ok(Ok(())) => format!("Asked Claude Code to renew it at {at}."),
        Ok(Err(cc_local::LocalError::ClaudeNotFound)) => {
            format!("Could not ask Claude Code to renew it at {at}: claude was not found.")
        }
        Ok(Err(e)) => format!("Asking Claude Code to renew it failed at {at}: {e}."),
        Err(e) => format!("Asking Claude Code to renew it failed at {at}: {e}."),
    };
    tracing::info!("{note}");
    *RENEW_NOTE.lock().unwrap_or_else(|e| e.into_inner()) = Some(note);
}

/// Waits up to `max` for Claude Code to renew an expired token, reading
/// `.credentials.json` every [`RENEWAL_CHECK`]; returns as soon as the token
/// in it is valid again, so the usage comes back within seconds. Claude Code
/// renews on its next request; meanwhile it is asked to, with `claude -p`
/// and no prompt ([`ask_claude_to_renew`]).
pub(crate) async fn wait_for_renewal(max: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + max;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return;
        }
        ask_claude_to_renew().await;
        tokio::time::sleep(left.min(RENEWAL_CHECK)).await;
        let renewed = tauri::async_runtime::spawn_blocking(|| {
            cc_usage::credentials::load().is_ok_and(|c| !c.is_expired())
        })
        .await
        .unwrap_or(false);
        if renewed {
            return;
        }
    }
}

/// What one session pass hands back to the async side.
struct SessionPass {
    sources: local::Sources,
    first_pass: bool,
    changed: bool,
    views: Vec<SessionView>,
    toasts: Vec<(String, String)>,
    agents_error: Option<String>,
}

/// One pass over the local session sources. Takes and returns the sources,
/// whose registry state is carried from one pass to the next.
pub(crate) async fn poll_sessions(
    app: &AppHandle,
    state: &Arc<AppState>,
    sources: local::Sources,
) -> local::Sources {
    let st = state.clone();
    // The Publisher role, once running, is fed from this same pass: one
    // monitor, so the cc-hook spool is drained once.
    let publishing = state.publisher.get().and_then(|hub| hub.running().cloned());
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut sources = sources;
        // The slow part (the CLI, when due) runs before the monitor is locked.
        let gathered = sources.gather();
        let agents_error = gathered.agents_error.clone();
        let has_registry = gathered.registry.is_some();
        let now = sources.now();
        let mut monitor = st.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let first_pass = monitor.passes() == 0;
        let outcome = monitor.step(gathered, now);
        let payloads = publishing
            .as_ref()
            .map(|p| (monitor.payloads(p.alias(), now), monitor.passes()));
        let views = monitor.views().to_vec();
        // Published while the monitor is still locked, so a clear cannot
        // land between this pass and its copy and be undone by it.
        if outcome.changed || first_pass {
            if let Ok(mut rows) = st.local_rows.lock() {
                *rows = Some(views.clone());
            }
        }
        let toasts = local::toast_worthy(&outcome.transitions, first_pass)
            .into_iter()
            .filter_map(|t| views.iter().find(|v| v.id == t.session_id))
            .filter(|v| v.state == cc_core::session::SessionState::NeedsYou)
            .map(local::needs_you_toast)
            .collect();
        // The planner and the transcript scan run with the monitor let go.
        drop(monitor);
        if let (Some(p), Some((payloads, passes))) = (&publishing, payloads) {
            p.pass(&payloads, has_registry, passes, now);
        }
        SessionPass {
            sources,
            first_pass,
            changed: outcome.changed,
            views,
            toasts,
            agents_error,
        }
    })
    .await;

    let Ok(pass) = result else {
        // A panicked pass loses its sources and their registry state; start over.
        return local::sources();
    };

    if pass.first_pass && std::env::var_os(DEBUG_ENV).is_some() {
        tracing::info!("first session pass: {} session(s)", pass.views.len());
        for v in &pass.views {
            tracing::info!(
                "  {:?} {} (pid {:?}, entrypoint {:?}, hooks {})",
                v.state,
                v.display_name,
                v.pid,
                v.entrypoint,
                v.has_hooks
            );
        }
        if let Some(e) = &pass.agents_error {
            tracing::warn!("  claude agents --json failed: {e}");
        }
    }

    for (title, body) in &pass.toasts {
        let _ = app.notification().builder().title(title).body(body).show();
    }
    if pass.changed || pass.first_pass {
        emit_sessions(app, state);
    } else {
        // Nothing changed, but time alone can move a row past the idle delay.
        emit_if_hidden_changed(app, state);
    }
    publisher_changed(app);
    pass.sources
}

/// The Publisher role's footer line, sent when it changed.
fn publisher_changed(app: &AppHandle) {
    let state = app.state::<Arc<AppState>>();
    if let Some(status) = state.publisher.get().and_then(|hub| hub.status_change()) {
        let _ = app.emit(EVENT_REMOTE_STATUS, status);
    }
}

/// Starts the Publisher role if it is not running and the machine is
/// ready; whether it runs now. Blocking: it reads the key and creds files.
pub(crate) fn start_publisher(app: &AppHandle) -> bool {
    let state = app.state::<Arc<AppState>>().inner().clone();
    let Some(hub) = state.publisher.get() else {
        return false;
    };
    let handle = app.clone();
    let started = hub.try_start(move || Box::new(move || publisher_changed(&handle)));
    publisher_changed(app);
    refresh_tooltip(app, &state);
    started
}

/// Each row's id and whether it is hidden: what decides whether the flyout
/// needs the list again when only the clock moved.
fn row_marks(rows: &[serde_json::Value]) -> Vec<(String, bool)> {
    rows.iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap_or_default().to_string(),
                r["hidden"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

/// Sends the full row list, local and remote, to the flyout and refreshes
/// the tray tooltip. Nothing is sent before the first local pass, so the
/// flyout keeps saying it is looking instead of showing a partial list.
pub(crate) fn emit_sessions(app: &AppHandle, state: &AppState) {
    if let Some(rows) = state.merged_rows() {
        *lock(&state.sent_rows) = Some(row_marks(&rows));
        let _ = app.emit(EVENT_SESSIONS, rows);
    }
    refresh_tooltip(app, state);
}

/// Resends the rows when the idle filter now hides a different set than
/// the flyout was last sent.
fn emit_if_hidden_changed(app: &AppHandle, state: &AppState) {
    let Some(rows) = state.merged_rows() else {
        return;
    };
    let marks = row_marks(&rows);
    if lock(&state.sent_rows).as_ref() != Some(&marks) {
        emit_sessions(app, state);
    }
}

/// The remote side changed (rows, offline states, connection): toast the
/// remote sessions that started needing the user, then resend the rows and
/// the footer's "Remote" line.
pub(crate) fn remote_changed(app: &AppHandle, state: &AppState) {
    for row in remote::take_needs_you_of(&state.remote, remote::now_unix()) {
        let (title, body) = remote::needs_you_toast(&row);
        let _ = app.notification().builder().title(title).body(body).show();
    }
    emit_sessions(app, state);
    let _ = app.emit(EVENT_REMOTE_STATUS, state.remote_status());
}

/// Orders the writes of copies taken under a lock and saved after it is let
/// go: each copy gets a number while the lock is still held, and of two
/// writes running at once the older one cannot finish last and win.
struct SaveOrder {
    /// The last number handed out.
    issued: AtomicU64,
    /// The newest number written; held across a write.
    written: std::sync::Mutex<u64>,
}

impl SaveOrder {
    const fn new() -> Self {
        Self {
            issued: AtomicU64::new(0),
            written: std::sync::Mutex::new(0),
        }
    }

    /// The next number. Taken while the state it numbers is still locked,
    /// so the numbers follow the order of the changes.
    fn ticket(&self) -> u64 {
        self.issued.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Runs `write` (which says whether it saved) unless a newer copy was
    /// written already; whether it ran. A failed write does not count as
    /// written, so an older copy may still try. Blocking.
    fn write(&self, ticket: u64, write: impl FnOnce() -> bool) -> bool {
        let mut written = lock(&self.written);
        if *written >= ticket {
            return false;
        }
        if write() {
            *written = ticket;
        }
        true
    }
}

/// The saves of `notification-state.json`.
static NOTIFICATION_SAVES: SaveOrder = SaveOrder::new();

async fn notify(app: &AppHandle, state: &Arc<AppState>, snap: &Snapshot) {
    let Some(usage) = &snap.usage else { return };
    let (alerts, snapshot, ticket) = {
        let mut st = state.notif_state.lock().await;
        let alerts = st.decide(usage, &notification_settings());
        if alerts.is_empty() {
            return;
        }
        (alerts, st.clone(), NOTIFICATION_SAVES.ticket())
    };
    // Saved from a copy, with the lock let go and off the executor. Polls
    // run at once (the loop, the tray, the refresh command), so the copy
    // is numbered: one older than what is on disk already is not written.
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let path = cc_usage::paths::file(cc_usage::paths::NOTIFICATION_STATE);
        NOTIFICATION_SAVES.write(ticket, || match snapshot.save_to(&path) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("cannot save {}: {err}", path.display());
                false
            }
        });
    })
    .await;

    for a in alerts {
        let _ = app
            .notification()
            .builder()
            .title(&a.title)
            .body(&a.body)
            .show();
    }
}

/// Adds a sample to the history. `History` itself decides whether it goes in:
/// there is a minimum interval between samples that only yields to a window
/// reset.
async fn record_history(state: &Arc<AppState>, usage: &cc_usage::model::UsageSnapshot) {
    let mut snapshot = {
        let mut h = state.history.lock().await;
        if !h.add(history::Sample::from_snapshot(usage)) {
            return;
        }
        h.clone()
    };
    // Written from a copy, with the lock let go and off the executor. A write
    // failure cannot bring the refresh down: the data on screen is already
    // correct, only that point of the chart is lost.
    for _ in 0..3 {
        let written = (snapshot.samples.len(), snapshot.samples.last().cloned());
        let _ = tauri::async_runtime::spawn_blocking(move || snapshot.save()).await;
        // A restore that merged and saved meanwhile may have been written
        // over by this older copy: write the current one again.
        let h = state.history.lock().await;
        if (h.samples.len(), h.samples.last().cloned()) == written {
            return;
        }
        snapshot = h.clone();
    }
}

/// The clear button and the tray's "Clear sessions": every row goes.
/// Local sessions leave the tracker, and the live ones come back on the next
/// pass (about 2 s) because Claude Code still lists them, with their state
/// and hook flag; their current tool and subagents come back with their next
/// hook event. Remote rows are dismissed until their key gets a newer write,
/// so a machine still publishing shows its sessions again with the next
/// heartbeat (60 s at most), and a dead session stays gone.
pub(crate) fn clear_all(app: &AppHandle, state: &AppState) {
    // Demo rows are a fixture, not sessions.
    if dev_flags().demo {
        return;
    }
    {
        let mut monitor = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        monitor.clear(local::now_secs());
        if let Ok(mut rows) = state.local_rows.lock() {
            // Before the first pass there is nothing to clear, and `None`
            // keeps the flyout saying it is looking.
            if rows.is_some() {
                *rows = Some(monitor.views().to_vec());
            }
        }
    }
    remote::dismiss_all_of(&state.remote, remote::now_unix());
    emit_sessions(app, state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_is_asked_to_renew_at_most_every_five_minutes() {
        let t0 = std::time::Instant::now();
        let mut last = None;
        assert!(nudge_due(&mut last, t0), "the first time at once");
        assert!(!nudge_due(
            &mut last,
            t0 + std::time::Duration::from_secs(20)
        ));
        assert!(!nudge_due(
            &mut last,
            t0 + RENEWAL_NUDGE_EVERY - std::time::Duration::from_secs(1)
        ));
        assert!(nudge_due(&mut last, t0 + RENEWAL_NUDGE_EVERY));
        assert_eq!(last, Some(t0 + RENEWAL_NUDGE_EVERY));
    }

    #[test]
    fn an_older_copy_finishing_last_is_not_written() {
        let order = SaveOrder::new();
        let older = order.ticket();
        let newer = order.ticket();
        assert!(newer > older);
        let file = std::sync::Mutex::new(None);
        let disk = &file;
        let save = |copy: u64| {
            move || {
                *lock(disk) = Some(copy);
                true
            }
        };
        // The newer copy's write gets there first.
        assert!(order.write(newer, save(newer)));
        assert!(!order.write(older, save(older)));
        assert_eq!(*lock(&file), Some(newer));
    }

    #[test]
    fn a_failed_write_lets_an_older_copy_through() {
        let order = SaveOrder::new();
        let older = order.ticket();
        let newer = order.ticket();
        assert!(order.write(newer, || false));
        let mut ran = false;
        assert!(order.write(older, || {
            ran = true;
            true
        }));
        assert!(ran);
        // A copy no newer than the one written stays out.
        assert!(!order.write(older, || true));
    }

    #[test]
    fn writes_racing_on_threads_leave_the_newest_copy() {
        let order = Arc::new(SaveOrder::new());
        let disk = Arc::new(std::sync::Mutex::new(0u64));
        let tickets: Vec<u64> = (0..16).map(|_| order.ticket()).collect();
        // Started newest first, so older copies tend to finish later.
        let handles: Vec<_> = tickets
            .iter()
            .rev()
            .map(|&ticket| {
                let (order, disk) = (order.clone(), disk.clone());
                std::thread::spawn(move || {
                    order.write(ticket, || {
                        *lock(&disk) = ticket;
                        true
                    })
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(*lock(&disk), *tickets.last().unwrap());
    }
}
