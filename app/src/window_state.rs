//! The widget's position and size, persisted, and the geometry rules around
//! them.
//!
//! Stored in `window.json` in the app config dir (outside OneDrive). On
//! restore the position is checked against the connected monitors: unplug the
//! screen the widget lived on and it comes back to the primary one instead of
//! sitting at invisible coordinates.
//!
//! Everything here is plain arithmetic on physical pixels, with no Tauri in
//! sight, so it is tested without a window; `window.rs` feeds it the monitors
//! and applies the answers.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = "window.json";

/// Gap from the work area's edges, in logical pixels, for the first launch
/// and for anything nudged back into view. It is added to the 8 px shadow
/// cushion the window already carries.
pub const MARGIN: f64 = 16.0;

/// Factory width in logical pixels: the 420 px card plus the 8 px shadow
/// cushion on each side. Matches `tauri.conf.json`; "reset" returns to it.
pub const BASE_WIDTH: f64 = 436.0;
/// Factory height before the content has been measured. Matches
/// `tauri.conf.json`.
pub const BASE_HEIGHT: f64 = 560.0;

/// Size limits in logical pixels, the same as `tauri.conf.json`. Below the
/// minimum the header and the footer no longer fit next to a row; above the
/// maximum the widget stops being a widget.
pub const MIN_WIDTH: f64 = 360.0;
pub const MIN_HEIGHT: f64 = 220.0;
pub const MAX_WIDTH: f64 = 900.0;
pub const MAX_HEIGHT: f64 = 1000.0;

/// How far above a monitor's top edge (in physical pixels) the window's top
/// may sit and still count as on that monitor: the header has to be
/// reachable, since it is what you drag the widget back with.
const GRAB_BAND: i32 = 40;

/// A window minimised by Windows reports this position; never worth saving.
const MINIMISED_COORD: i32 = -32000;

/// What is saved: outer position and size in physical pixels, plus whether
/// the height still follows the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// `true` until the user chooses a size: the height then follows the
    /// content. Resizing by hand turns it off for good; "reset size" turns
    /// it back on.
    pub auto_height: bool,
}

/// A rectangle in physical pixels: a monitor, its work area, or a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }
}

/// Would a window saved as `state` still be visible on one of `monitors`?
///
/// It has to overlap a monitor horizontally and have its top edge inside it
/// (or just above it, by [`GRAB_BAND`]), so a window nudged just past an edge
/// still counts while one on a monitor that no longer exists does not. An
/// empty list means the monitors could not be read: when in doubt, honour
/// what was saved.
pub fn is_visible(state: &WindowState, monitors: &[Rect]) -> bool {
    if monitors.is_empty() {
        return true;
    }
    monitors.iter().any(|m| {
        let inside_x = state.x + (state.width as i32) > m.x && state.x < m.right();
        let inside_y = state.y + GRAB_BAND > m.y && state.y < m.bottom();
        inside_x && inside_y
    })
}

/// Where the widget goes the first time: the top right of `work_area`, a
/// `margin` in from both edges. Never left of or above the work area, even
/// when the window is wider than it.
pub fn default_position(size: (u32, u32), work_area: Rect, margin: i32) -> (i32, i32) {
    let x = (work_area.right() - size.0 as i32 - margin).max(work_area.x);
    let y = work_area.y + margin;
    (x, y)
}

/// `pos` if a window of `size` fits there inside `work_area`, otherwise the
/// nearest spot that does.
///
/// The work area, not the monitor: they differ by the taskbar, and this
/// widget is always on top, so a window pushed down to the monitor's edge
/// would be drawn over the taskbar rather than tucked behind it.
///
/// Each edge in turn, and the `max` after the `min` on purpose: on a monitor
/// too small for the window at all, landing against the top left beats
/// landing past the bottom right.
pub fn nudged_into_view(
    pos: (i32, i32),
    size: (u32, u32),
    work_area: Rect,
    margin: i32,
) -> (i32, i32) {
    let right = work_area.right() - size.0 as i32 - margin;
    let bottom = work_area.bottom() - size.1 as i32 - margin;
    (
        pos.0.min(right).max(work_area.x + margin),
        pos.1.min(bottom).max(work_area.y + margin),
    )
}

/// A position Windows uses for minimised windows, or anything else that
/// cannot be a real place for the widget, is not saved.
pub fn worth_saving(state: &WindowState) -> bool {
    state.x > MINIMISED_COORD && state.y > MINIMISED_COORD && state.width > 0 && state.height > 0
}

/// Did a resize to `now` (logical pixels) come from somewhere other than the
/// content fit that asked for `expected`? A DPI change keeps the logical size,
/// so moving to a monitor with different scaling is not taken for a user's
/// resize; the tolerance absorbs rounding to whole physical pixels.
pub fn resized_by_hand(now: (f64, f64), expected: (f64, f64)) -> bool {
    const TOLERANCE: f64 = 2.5;
    (now.0 - expected.0).abs() > TOLERANCE || (now.1 - expected.1).abs() > TOLERANCE
}

/// Brings a saved physical size inside the window limits at `scale`.
/// Windows enforces the limits on resizes by hand, not on a size set
/// programmatically, and `window.json` can be hand-edited.
pub fn clamp_size(size: (u32, u32), scale: f64) -> (u32, u32) {
    let clamp = |v: u32, min: f64, max: f64| {
        (v as f64).clamp((min * scale).round(), (max * scale).round()) as u32
    };
    (
        clamp(size.0, MIN_WIDTH, MAX_WIDTH),
        clamp(size.1, MIN_HEIGHT, MAX_HEIGHT),
    )
}

/// Clamps a requested content height (logical pixels) to the window limits.
/// `None` for anything that is not a number.
pub fn clamp_height(height: f64) -> Option<f64> {
    height
        .is_finite()
        .then(|| height.clamp(MIN_HEIGHT, MAX_HEIGHT).round())
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The saved state, if there is a readable one.
pub fn read(dir: &Path) -> Option<WindowState> {
    let text = std::fs::read_to_string(path(dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Saves the state, atomically: it is written on every move and resize, and
/// a crash halfway must not leave a truncated file that loses the position.
pub fn write(dir: &Path, state: &WindowState) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    crate::paths::write_atomic(&path(dir), json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    /// A second monitor to the left, at a different height, as laptops on a
    /// dock tend to be.
    const LEFT: Rect = Rect {
        x: -2560,
        y: -300,
        width: 2560,
        height: 1440,
    };

    fn at(x: i32, y: i32) -> WindowState {
        WindowState {
            x,
            y,
            width: 436,
            height: 600,
            auto_height: false,
        }
    }

    #[test]
    fn a_window_on_a_connected_monitor_is_visible() {
        assert!(is_visible(&at(100, 100), &[PRIMARY]));
        assert!(is_visible(&at(-2000, -200), &[PRIMARY, LEFT]));
    }

    #[test]
    fn a_window_on_a_monitor_that_is_gone_is_not() {
        // It lived on the left monitor, which has been unplugged.
        assert!(!is_visible(&at(-2000, -200), &[PRIMARY]));
        // Far off to the right of everything.
        assert!(!is_visible(&at(5000, 100), &[PRIMARY, LEFT]));
        // Below the bottom of every monitor.
        assert!(!is_visible(&at(100, 1200), &[PRIMARY]));
    }

    #[test]
    fn a_window_just_past_an_edge_still_counts() {
        // Mostly off the right edge, but its left part is still on screen.
        assert!(is_visible(&at(1800, 100), &[PRIMARY]));
        // The top sits slightly above the monitor: the header is reachable.
        assert!(is_visible(&at(100, -30), &[PRIMARY]));
        // Too far above: only the bottom would show, and nothing to drag.
        assert!(!is_visible(&at(100, -100), &[PRIMARY]));
    }

    #[test]
    fn with_no_monitor_list_what_was_saved_is_honoured() {
        assert!(is_visible(&at(-9000, -9000), &[]));
    }

    #[test]
    fn the_first_launch_goes_to_the_top_right_of_the_work_area() {
        // A taskbar at the bottom: the work area is 48 px shorter.
        let work = Rect::new(0, 0, 1920, 1032);
        assert_eq!(
            default_position((436, 600), work, 16),
            (1920 - 436 - 16, 16)
        );
        // On a monitor that is not at the origin.
        let work = Rect::new(-2560, -300, 2560, 1392);
        assert_eq!(default_position((436, 600), work, 24), (-436 - 24, -276));
    }

    #[test]
    fn a_window_wider_than_the_work_area_starts_at_its_left_edge() {
        let work = Rect::new(0, 0, 400, 800);
        assert_eq!(default_position((436, 600), work, 16), (0, 16));
    }

    #[test]
    fn a_window_that_fits_is_not_moved() {
        let work = Rect::new(0, 0, 1920, 1032);
        assert_eq!(
            nudged_into_view((300, 200), (436, 600), work, 16),
            (300, 200)
        );
    }

    #[test]
    fn a_window_hanging_off_an_edge_is_pulled_back_by_the_overhang() {
        let work = Rect::new(0, 0, 1920, 1032);
        // Off the right edge and past the taskbar.
        assert_eq!(
            nudged_into_view((1700, 700), (436, 600), work, 16),
            (1920 - 436 - 16, 1032 - 600 - 16)
        );
        // Off the top left.
        assert_eq!(nudged_into_view((-50, -50), (436, 600), work, 16), (16, 16));
    }

    #[test]
    fn on_a_monitor_too_small_it_lands_top_left() {
        let work = Rect::new(100, 100, 300, 300);
        assert_eq!(
            nudged_into_view((500, 500), (436, 600), work, 10),
            (110, 110)
        );
    }

    #[test]
    fn nudging_works_on_a_monitor_left_of_the_primary() {
        let work = Rect::new(-2560, -300, 2560, 1392);
        assert_eq!(
            nudged_into_view((-300, 900), (436, 600), work, 16),
            (-436 - 16, -300 + 1392 - 600 - 16)
        );
    }

    #[test]
    fn a_minimised_position_is_not_saved() {
        assert!(!worth_saving(&at(-32000, -32000)));
        assert!(worth_saving(&at(-2000, -200)));
        assert!(!worth_saving(&WindowState {
            width: 0,
            ..at(0, 0)
        }));
    }

    #[test]
    fn a_dpi_change_is_not_a_resize_by_hand() {
        // Same logical size after rounding to physical pixels at 150%.
        assert!(!resized_by_hand((436.0, 600.67), (436.0, 600.0)));
        assert!(resized_by_hand((436.0, 700.0), (436.0, 600.0)));
        assert!(resized_by_hand((500.0, 600.0), (436.0, 600.0)));
    }

    #[test]
    fn a_saved_size_is_kept_inside_the_limits() {
        assert_eq!(clamp_size((520, 430), 1.0), (520, 430));
        assert_eq!(clamp_size((100, 100), 1.0), (360, 220));
        assert_eq!(clamp_size((3000, 3000), 1.0), (900, 1000));
        // At 150% the limits are in physical pixels too.
        assert_eq!(clamp_size((100, 3000), 1.5), (540, 1500));
    }

    #[test]
    fn content_heights_are_clamped_to_the_limits() {
        assert_eq!(clamp_height(100.0), Some(MIN_HEIGHT));
        assert_eq!(clamp_height(5000.0), Some(MAX_HEIGHT));
        assert_eq!(clamp_height(612.4), Some(612.0));
        assert_eq!(clamp_height(f64::NAN), None);
        assert_eq!(clamp_height(f64::INFINITY), None);
    }

    #[test]
    fn the_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), None);
        let state = WindowState {
            x: -1500,
            y: 40,
            width: 654,
            height: 900,
            auto_height: true,
        };
        write(dir.path(), &state).unwrap();
        assert_eq!(read(dir.path()), Some(state));

        std::fs::write(path(dir.path()), "{ not json").unwrap();
        assert_eq!(read(dir.path()), None);
    }
}
