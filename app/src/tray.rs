//! The tray icon: its menu, its image (the app icon, with a red badge while
//! a session needs you) and its tooltip.

use crate::links::open_url;
use crate::poller::{clear_all, poll};
use crate::state::{lock, AppState, TrayItems};
use crate::window::{quit_app, reset, toggle_widget, WINDOW};
use cc_usage::{autostart, remote, sessions as local, Snapshot};
use std::sync::Arc;
use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

const TRAY_ID: &str = "main";

/// Tray tooltip before the first usage refresh.
pub(crate) const TOOLTIP_LOADING: &str = "cc-usage — loading…";

/// Builds the tray icon and its menu. Menu labels are user-facing; the ids
/// are internal.
pub(crate) fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    // The version, greyed out, as the menu's first line.
    let version = format!("cc-usage {}", app.package_info().version);
    let about = MenuItem::with_id(app, "about", version, false, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Show/Hide", true, None::<&str>)?;
    let reset_item =
        MenuItem::with_id(app, "reset", "Reset position and size", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Refresh now", true, None::<&str>)?;
    let clear = MenuItem::with_id(app, "clear", "Clear sessions", true, None::<&str>)?;
    let web = MenuItem::with_id(
        app,
        "web",
        "Open claude.ai/settings/usage",
        true,
        None::<&str>,
    )?;
    // Checked from the real state (a registry read), kept so a click can set
    // it to what is really in force afterwards.
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        autostart::MENU_LABEL,
        true,
        autostart::checked(&autostart::system()),
        None::<&str>,
    )?;
    *lock(&app.state::<TrayItems>().autostart) = Some(autostart.clone());
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &about,
            &PredefinedMenuItem::separator(app)?,
            &toggle,
            &reset_item,
            &refresh,
            &clear,
            &PredefinedMenuItem::separator(app)?,
            &web,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID);
    if let Some(icon) = grid_image(false) {
        builder = builder.icon(icon);
    }
    builder
        .tooltip(TOOLTIP_LOADING)
        .menu(&menu)
        // Without this, a left click opens the menu and the event we use to
        // show the widget never arrives.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => toggle_widget(app),
            "reset" => {
                if let Some(w) = app.get_webview_window(WINDOW) {
                    reset(&w);
                }
            }
            "refresh" => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move {
                    let st = h.state::<Arc<AppState>>().inner().clone();
                    poll(&h, &st).await;
                });
            }
            "clear" => {
                // Off the main thread: it waits for a pass in progress.
                let h = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let st = h.state::<Arc<AppState>>().inner().clone();
                    clear_all(&h, &st);
                });
            }
            "web" => open_url("https://claude.ai/settings/usage"),
            "autostart" => toggle_autostart(app),
            "quit" => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_widget(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// The tray image: the app icon (the activity grid), with a red badge while
/// some session needs you. Two PNGs from tools/make-icon.ps1, embedded.
fn grid_image(needs_you: bool) -> Option<Image<'static>> {
    const PLAIN: &[u8] = include_bytes!("../icons/tray-grid.png");
    const ALERT: &[u8] = include_bytes!("../icons/tray-grid-alert.png");
    let bytes = if needs_you { ALERT } else { PLAIN };
    Image::from_bytes(bytes).map(|i| i.to_owned()).ok()
}

/// Last badge drawn (0 none, 1 plain, 2 alert), so the two-second session
/// pass only touches the tray when the badge changes.
static GRID_SHOWN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Shows or clears the "needs you" badge.
fn refresh_grid(app: &AppHandle, state: &AppState, force: bool) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let needs_you = local::needs_you_count(&state.all_views()) > 0;
    let want = if needs_you { 2 } else { 1 };
    if force || GRID_SHOWN.swap(want, std::sync::atomic::Ordering::Relaxed) != want {
        GRID_SHOWN.store(want, std::sync::atomic::Ordering::Relaxed);
        if let Some(icon) = grid_image(needs_you) {
            let _ = tray.set_icon(Some(icon));
        }
    }
}

/// The tray after a usage poll: the badge and the tooltip's usage part.
pub(crate) fn update_tray(app: &AppHandle, snap: &Snapshot) {
    let state = app.state::<Arc<AppState>>();
    refresh_grid(app, &state, true);
    if let Ok(mut t) = state.usage_tooltip.lock() {
        *t = tooltip(snap);
    }
    refresh_tooltip(app, &state);
}

/// Sets the tray tooltip: the session summary, then the last usage text. The
/// session line goes first because Windows cuts tooltips at 127 characters.
pub(crate) fn refresh_tooltip(app: &AppHandle, state: &AppState) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let usage = state
        .usage_tooltip
        .lock()
        .map(|t| t.clone())
        .unwrap_or_default();
    let mut line = local::tooltip_line(&state.all_views());
    // Publisher keys waiting for the user's consent, on the first line too.
    if let Some(review) = remote::review_text(remote::pending_count_of(&state.remote)) {
        line = Some(match line {
            Some(sessions) => format!("{sessions} · {review}"),
            None => review,
        });
    }
    // The Publisher role says so, on the first line: Windows cuts the rest.
    if let Some(hub) = state.publisher.get() {
        let role = format!("publishing as {}", hub.setup().alias);
        line = Some(match line {
            Some(sessions) => format!("{sessions} · {role}"),
            None => format!("Publishing as {}", hub.setup().alias),
        });
    }
    let text = match line {
        Some(line) => format!("{line}\n{usage}"),
        None => usage,
    };
    let _ = tray.set_tooltip(Some(text));
    refresh_grid(app, state, false);
}

/// Tray tooltip text (user-facing).
fn tooltip(snap: &Snapshot) -> String {
    let Some(u) = &snap.usage else {
        return snap
            .usage_error
            .clone()
            .unwrap_or_else(|| "cc-usage — no data".into());
    };
    let mut lines: Vec<String> = u
        .limits
        .iter()
        .map(|l| format!("{}: {}%", l.label, l.percent.round() as i64))
        .collect();
    if snap.is_stale() {
        lines.push("(stale data)".into());
    }
    lines.join("\n")
}

/// The "Start with Windows" click: flips it, then sets the check to the
/// state really in force (the menu has already flipped its own check).
fn toggle_autostart(app: &AppHandle) {
    let result = autostart::toggle(&autostart::system());
    if let Some(error) = &result.error {
        tracing::warn!("{error}");
    }
    if let Some(item) = lock(&app.state::<TrayItems>().autostart).as_ref() {
        let _ = item.set_checked(result.checked);
    }
}
