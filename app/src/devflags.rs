//! Development flags (`--demo`, `--view`, `--reference`, ...), **debug
//! builds only**, and what they switch on.

use crate::window::{nudge_into_view, set_auto_height, WINDOW};
use cc_usage::{notifications, sessions as local};
use std::sync::OnceLock;
use tauri::{AppHandle, LogicalSize, Manager};

/// The mockup page shown by `--reference`, in logical pixels.
const REFERENCE_SIZE: (f64, f64) = (436.0, 720.0);

/// Development flags, read once from the command line, **in debug builds
/// only** (the ones the screenshots are taken with). A release build
/// ignores them all: an installed app started with `--reference <page>`
/// would load any local page in a window that can call the backend, and
/// `--click` would let a shortcut click its buttons. Unknown arguments are
/// ignored in both, so scripts passing these to a release build still start
/// it, as the real app.
///
/// - `--demo`: the session list and the usage footer are the fixtures in
///   `cc_usage::demo` (the mockup's data); the real session sources are not
///   read.
/// - `--demo-empty`: the same, with an empty session list.
/// - `--view usage`: opens the flyout on the
///   usage view (account, history, per-project report); `--view settings`
///   on the settings view.
/// - `--backdrop <colour>`: paints the transparent cushion around the panel
///   (`white`, `#f4f5f9`), so a capture shows the shadow as on a page; a
///   window capture renders transparency as black.
/// - `--reference <file.html>`: loads that page in the flyout window instead
///   of the app, at the maximum height, so the design reference
///   (`docs/reference/flyout-mockup.html`) renders in the same WebView2 and
///   can be captured the same way as the app.
/// - `--click <element id or CSS selector>`: clicks that element of the
///   flyout once it exists (up to 15 s), e.g. `--click btn-hide` to check
///   that the header's hide button hides the widget, or `--click
///   button.host-filter` for the usage view's machine filter, where
///   synthetic input is not available.
///
/// `--low-thresholds` (see [`notification_settings`]) follows the same rule.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub(crate) struct DevFlags {
    pub(crate) demo: bool,
    pub(crate) demo_empty: bool,
    view: Option<String>,
    backdrop: Option<String>,
    click: Option<String>,
    #[serde(skip)]
    pub(crate) reference: Option<String>,
}

pub(crate) fn dev_flags() -> &'static DevFlags {
    static FLAGS: OnceLock<DevFlags> = OnceLock::new();
    FLAGS.get_or_init(|| {
        if !cfg!(debug_assertions) {
            return DevFlags::default();
        }
        let args: Vec<String> = std::env::args().collect();
        let value_of = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let demo_empty = args.iter().any(|a| a == "--demo-empty");
        DevFlags {
            demo: demo_empty || args.iter().any(|a| a == "--demo"),
            demo_empty,
            view: value_of("--view"),
            // Only a colour name or a hex colour reaches the page's style.
            backdrop: value_of("--backdrop").filter(|c| {
                !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '#')
            }),
            click: value_of("--click"),
            reference: value_of("--reference"),
        }
    })
}

/// DEMO: the fixture session list, already serialized for the frontend
/// (it carries the remote rows' `origin`), built once so its clocks keep
/// ticking.
pub(crate) fn demo_sessions() -> &'static [serde_json::Value] {
    static LIST: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    LIST.get_or_init(|| {
        if dev_flags().demo_empty {
            Vec::new()
        } else {
            cc_usage::demo::sessions_json(local::now_secs())
        }
    })
}

/// DEV (`--reference`): shows a local HTML page in the widget window, at the
/// mockup's height, instead of the app's UI. Nothing it does to the window
/// is saved.
pub(crate) fn show_reference(app: &AppHandle, page: &str) {
    let Some(w) = app.get_webview_window(WINDOW) else {
        return;
    };
    let Ok(path) = std::fs::canonicalize(page) else {
        tracing::warn!("--reference: {page} not found");
        return;
    };
    // canonicalize gives a verbatim path (\\?\C:\...), which file URLs cannot carry.
    let path = path
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    let Ok(url) = tauri::Url::from_file_path(&path) else {
        return;
    };
    set_auto_height(&w, false, None);
    let _ = w.set_size(LogicalSize::new(REFERENCE_SIZE.0, REFERENCE_SIZE.1));
    nudge_into_view(&w);
    let _ = w.navigate(url);
    let _ = w.show();
}

/// Alert thresholds. `--low-thresholds` lowers them to 1/2% so a real toast can
/// be seen without waiting to consume 80% of the quota: without it, the
/// usage alerts can only be verified with unit tests. Debug builds only, like
/// the other development flags ([`DevFlags`]).
pub(crate) fn notification_settings() -> notifications::Settings {
    if cfg!(debug_assertions) && std::env::args().any(|a| a == "--low-thresholds") {
        notifications::Settings {
            enabled: true,
            warn_at: 1.0,
            critical_at: 2.0,
        }
    } else {
        notifications::Settings::default()
    }
}
