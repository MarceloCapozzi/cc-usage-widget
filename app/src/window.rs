//! The floating widget: geometry, persistence, shortcut.
//!
//! The geometry rules are in the library (`window_state`, `fullscreen`);
//! this module applies them to the window: restoring and saving its place,
//! the factory reset, the height that follows the content until a resize by
//! hand, hiding while something runs full screen, and the show/hide
//! shortcut.

use crate::devflags::dev_flags;
use crate::state::{config_dir, lock, AutoHeight, Widget};
use cc_usage::{
    fullscreen,
    settings::{self, Settings},
    window_state::{self as geometry, Rect, WindowState},
};
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, WebviewWindow,
};

/// The widget window's label (`tauri.conf.json`).
pub(crate) const WINDOW: &str = "flyout";

/// The widget was shown by the tray or the shortcut: the frontend paints the
/// last snapshot at once (listened to in `ui/scripts/main.js`).
const EVENT_REFRESH_REQUESTED: &str = "refresh-requested";

/// Asks the frontend to measure its content again and send `fit_height`:
/// after a reset, the height goes back to following the content.
const EVENT_FIT_REQUEST: &str = "fit-request";

/// How much stillness to wait for before persisting the position.
///
/// Windows fires `Moved` on every pixel of a drag. Saving every few hundred
/// milliseconds *during* the movement sounds reasonable but is wrong: once
/// you let go no further event arrives, so the last save is of some midway
/// point and the widget reopens halfway there. Hence waiting for it to settle.
const STILLNESS_BEFORE_SAVE: Duration = Duration::from_millis(500);
/// How often to check whether it has settled.
const STILLNESS_POLL: Duration = Duration::from_millis(250);
/// How often the full-screen watcher asks Windows, when it is switched on.
const FULLSCREEN_POLL: Duration = Duration::from_secs(2);

fn rect_of(m: &tauri::Monitor) -> Rect {
    Rect::new(
        m.position().x,
        m.position().y,
        m.size().width,
        m.size().height,
    )
}

fn work_area_of(m: &tauri::Monitor) -> Rect {
    let area = m.work_area();
    Rect::new(
        area.position.x,
        area.position.y,
        area.size.width,
        area.size.height,
    )
}

fn margin_px(scale: f64) -> i32 {
    (geometry::MARGIN * scale).round() as i32
}

pub(crate) fn set_auto_height(w: &WebviewWindow, on: bool, expected: Option<(f64, f64)>) {
    *lock(&w.state::<Widget>().auto) = AutoHeight {
        on,
        expected: if on { expected } else { None },
    };
}

/// Sets a logical size the auto-height check should not take for a resize
/// by hand. The expectation is recorded before the call, because the
/// `Resized` it raises may be handled before `set_size` returns.
pub(crate) fn set_logical_size(w: &WebviewWindow, width: f64, height: f64) {
    {
        let widget = w.state::<Widget>();
        let mut auto = lock(&widget.auto);
        if auto.on {
            auto.expected = Some((width, height));
        }
    }
    let _ = w.set_size(LogicalSize::new(width, height));
}

/// Restores size and position from `window.json`, then shows the window.
pub(crate) fn restore(w: &WebviewWindow) {
    let saved = geometry::read(&config_dir(w.app_handle()));
    let monitors: Vec<Rect> = w
        .available_monitors()
        .map(|ms| ms.iter().map(rect_of).collect())
        .unwrap_or_default();

    match saved {
        Some(state) if geometry::is_visible(&state, &monitors) => {
            // Position first, size second, not the other way round. If the
            // widget lived on a monitor with different scaling, moving it
            // there raises a DPI change, and Windows rescales the window to
            // keep its *logical* size, overwriting the physical size just
            // set. Putting it on the right monitor first means the saved size
            // is the one that matches that DPI.
            let _ = w.set_position(PhysicalPosition::new(state.x, state.y));
            let scale = w.scale_factor().unwrap_or(1.0);
            let (width, height) = geometry::clamp_size((state.width, state.height), scale);
            let logical = (width as f64 / scale, height as f64 / scale);
            set_auto_height(w, state.auto_height, Some(logical));
            let _ = w.set_size(PhysicalSize::new(width, height));
        }
        // No file, an unreadable one, or a position on a monitor that is no
        // longer there: the first-launch default on the primary monitor.
        _ => place_default(w),
    }

    let _ = w.show();
}

/// The first-launch placement: factory size, top right of the primary
/// monitor's work area, height following the content again.
fn place_default(w: &WebviewWindow) {
    set_auto_height(w, true, None);
    let monitor = w
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| w.current_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        set_logical_size(w, geometry::BASE_WIDTH, geometry::BASE_HEIGHT);
        return;
    };
    let work = work_area_of(&monitor);
    let scale = monitor.scale_factor();
    let margin = margin_px(scale);

    // Onto the primary monitor first, so the logical size below is applied at
    // that monitor's DPI; then the corner, which depends on the width.
    let _ = w.set_position(PhysicalPosition::new(work.x + margin, work.y + margin));
    set_logical_size(w, geometry::BASE_WIDTH, geometry::BASE_HEIGHT);
    let size = (
        (geometry::BASE_WIDTH * scale).round() as u32,
        (geometry::BASE_HEIGHT * scale).round() as u32,
    );
    let (x, y) = geometry::default_position(size, work, margin);
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

/// Back to the factory size and position: the tray's "Reset position and
/// size", and the way back for a widget stranded somewhere unreachable.
pub(crate) fn reset(w: &WebviewWindow) {
    place_default(w);
    let _ = w.show();
    let _ = w.emit(EVENT_FIT_REQUEST, ());
    save(w);
}

/// Back to the factory size, **where the widget already is**: the double
/// click on a resize edge. A hand on the edge is asking about size; being
/// moved back to the corner as well would be an answer nobody asked for.
pub(crate) fn reset_size(w: &WebviewWindow) {
    if w.outer_position().is_err() {
        return reset(w);
    }
    set_auto_height(w, true, None);
    set_logical_size(w, geometry::BASE_WIDTH, geometry::BASE_HEIGHT);
    // Windows keeps the top left corner. Growing against a screen edge is
    // the one case where holding it is worse than shifting: the widget is
    // then pulled back by exactly as much as it overhangs.
    nudge_into_view(w);
    let _ = w.emit(EVENT_FIT_REQUEST, ());
    save(w);
}

/// Pulls the window back inside its monitor's work area if it overhangs.
pub(crate) fn nudge_into_view(w: &WebviewWindow) {
    let (Ok(pos), Ok(Some(monitor))) = (w.outer_position(), w.current_monitor()) else {
        return;
    };
    let scale = monitor.scale_factor();
    // `outer_size` can lag right after `set_size`; the logical size we asked
    // for is the truth, so it is used when known. Copied out first: the
    // lock must not be held while the window is asked for its size.
    let expected = lock(&w.state::<Widget>().auto).expected;
    let size = match expected {
        Some((lw, lh)) => ((lw * scale).round() as u32, (lh * scale).round() as u32),
        None => match w.outer_size() {
            Ok(s) => (s.width, s.height),
            Err(_) => return,
        },
    };
    let work = work_area_of(&monitor);
    let (x, y) = geometry::nudged_into_view((pos.x, pos.y), size, work, margin_px(scale));
    if (x, y) != (pos.x, pos.y) {
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
}

/// A `Resized` event: if the height follows the content and this is not the
/// size the last fit asked for, someone resized it by hand, and from now on
/// that size is respected.
pub(crate) fn note_resize(w: &WebviewWindow, size: PhysicalSize<u32>) {
    if size.width == 0 || size.height == 0 {
        return;
    }
    let scale = w.scale_factor().unwrap_or(1.0);
    let now = (size.width as f64 / scale, size.height as f64 / scale);
    let widget = w.state::<Widget>();
    let mut auto = lock(&widget.auto);
    if let (true, Some(expected)) = (auto.on, auto.expected) {
        if geometry::resized_by_hand(now, expected) {
            *auto = AutoHeight::default();
        }
    }
}

pub(crate) fn mark_moved(app: &AppHandle) {
    *lock(&app.state::<Widget>().pending_move) = Some(Instant::now());
}

/// Writes the current position and size to `window.json`.
pub(crate) fn save(w: &WebviewWindow) {
    // `--reference` resizes the window to the mockup's height; that is not a
    // preference.
    if dev_flags().reference.is_some() {
        return;
    }
    let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) else {
        return;
    };
    let state = WindowState {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
        auto_height: lock(&w.state::<Widget>().auto).on,
    };
    if !geometry::worth_saving(&state) {
        return;
    }
    if let Err(e) = geometry::write(&config_dir(w.app_handle()), &state) {
        tracing::warn!("could not save the window position: {e}");
    }
}

/// Persists position and size once the widget has settled.
pub(crate) fn watch_movement(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(STILLNESS_POLL);
        let settled = {
            let widget = app.state::<Widget>();
            let mut pending = lock(&widget.pending_move);
            match *pending {
                Some(since) if since.elapsed() >= STILLNESS_BEFORE_SAVE => {
                    *pending = None;
                    true
                }
                _ => false,
            }
        };
        if settled {
            if let Some(w) = app.get_webview_window(WINDOW) {
                save(&w);
            }
        }
    });
}

/// Hides the widget while a game or a presentation runs full screen, when
/// that setting is on. It remembers whether the hiding was its own doing:
/// a widget you hid yourself does not come back when the game ends.
pub(crate) fn watch_fullscreen(app: AppHandle) {
    std::thread::spawn(move || {
        let mut we_hid_it = false;
        loop {
            std::thread::sleep(FULLSCREEN_POLL);
            let Some(w) = app.get_webview_window(WINDOW) else {
                continue;
            };
            let level = lock(&app.state::<Widget>().settings).hide_on_fullscreen;
            let visible = w.is_visible().unwrap_or(false);
            match fullscreen::step(fullscreen::active(level), we_hid_it, visible) {
                fullscreen::Step::Hide => {
                    let _ = w.hide();
                    we_hid_it = true;
                }
                fullscreen::Step::Show => {
                    let _ = w.show();
                    we_hid_it = false;
                }
                fullscreen::Step::Nothing => {}
            }
        }
    });
}

/// (Re)registers the global show/hide shortcut.
///
/// A shortcut can fail because another application already took it. That is
/// no reason to bring anything down: the error is kept for the ⚙ view and the
/// widget keeps working from the tray.
pub(crate) fn register_shortcut(app: &AppHandle, prefs: &Settings) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let manager = app.global_shortcut();
    let _ = manager.unregister_all();

    let typed = prefs.shortcut.trim().to_string();
    if typed.is_empty() {
        return Ok(());
    }

    manager
        .on_shortcut(prefs.accelerator().as_str(), |app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                toggle_widget(app);
            }
        })
        .map_err(|e| {
            tracing::warn!("could not register the shortcut \"{typed}\": {e}");
            settings::shortcut_problem(&typed, &e.to_string())
        })
}

/// Shows the widget and brings it to the front, or hides it if it is showing.
pub(crate) fn toggle_widget(app: &AppHandle) {
    let Some(w) = app.get_webview_window(WINDOW) else {
        return;
    };
    if w.is_visible().unwrap_or(false) {
        save(&w);
        let _ = w.hide();
        return;
    }
    show_widget(app);
}

/// Shows the widget and brings it to the front, whether or not it was
/// showing: the shortcut, and a second copy of the app that was started
/// (see `cc_usage::instance`).
pub(crate) fn show_widget(app: &AppHandle) {
    let Some(w) = app.get_webview_window(WINDOW) else {
        return;
    };
    let _ = w.show();
    let _ = w.set_focus();
    let _ = app.emit(EVENT_REFRESH_REQUESTED, ());
}

/// Saves the window and ends the process (the tray's Quit).
pub(crate) fn quit_app(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW) {
        save(&w);
    }
    app.exit(0);
}
