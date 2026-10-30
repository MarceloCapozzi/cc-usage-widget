//! Detecting "a game or a presentation is running full screen in front of
//! us".
//!
//! The widget sits always on top, which is what you want nearly all the time
//! and exactly what you do not want during a presentation or a game. Windows
//! already tracks this to decide whether to show notifications, so we ask it
//! rather than guessing by comparing window sizes against the monitor, which
//! gets video players and borderless windows wrong.
//!
//! The levels are the calendar's ([`HideWhenFullscreen`]): Direct3D full
//! screen and presentation mode (a game, someone projecting) on one side;
//! on the other, `QUNS_BUSY`, which Windows reports for every other
//! full-screen application (a remote desktop client, a maximised borderless
//! window). Those are how someone works all day, so only "anything full
//! screen" hides for them.

use crate::settings::HideWhenFullscreen;

/// `QUERY_USER_NOTIFICATION_STATE` values (shellapi.h).
const QUNS_BUSY: i32 = 2;
const QUNS_RUNNING_D3D_FULL_SCREEN: i32 = 3;
const QUNS_PRESENTATION_MODE: i32 = 4;
#[cfg(test)]
const QUNS_ACCEPTS_NOTIFICATIONS: i32 = 5;

/// `true` when the widget should get out of the way, for the level asked.
///
/// Any error returns `false`: a widget that shows up once too often beats
/// one that vanishes for no reason and looks broken.
#[cfg(windows)]
pub fn active(level: HideWhenFullscreen) -> bool {
    if level == HideWhenFullscreen::Never {
        return false;
    }

    let mut state = 0i32;
    // SAFETY: a valid pointer to a stack variable, which is exactly what the
    // function expects; the value is only read back when it returned S_OK.
    let result = unsafe { windows_sys::Win32::UI::Shell::SHQueryUserNotificationState(&mut state) };
    if result != 0 {
        return false;
    }
    decide(state, level)
}

#[cfg(not(windows))]
pub fn active(_level: HideWhenFullscreen) -> bool {
    false
}

/// The rule itself, apart from the call that fetches the state, so it can be
/// tested: putting something full screen is not a thing a test can arrange.
fn decide(state: i32, level: HideWhenFullscreen) -> bool {
    if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
        return level != HideWhenFullscreen::Never;
    }
    state == QUNS_BUSY && level == HideWhenFullscreen::Always
}

/// What the watcher should do this turn, given whether something is in front
/// and whether the widget is hidden because of it. It remembers whose hiding
/// it was: a widget you hid yourself does not come back when the game ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Hide,
    Show,
    Nothing,
}

pub fn step(in_front: bool, we_hid_it: bool, visible: bool) -> Step {
    match (in_front, we_hid_it) {
        (true, false) if visible => Step::Hide,
        // Turning the setting off has to bring the widget back at once,
        // otherwise it appears not to work until whatever is in front closes.
        (false, true) => Step::Show,
        _ => Step::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use HideWhenFullscreen::{Always, GamesAndPresentations, Never};

    #[test]
    fn nothing_in_front_never_hides_it() {
        for level in [Never, GamesAndPresentations, Always] {
            assert!(!decide(QUNS_ACCEPTS_NOTIFICATIONS, level));
        }
    }

    #[test]
    fn a_game_or_a_projected_deck_hides_it_at_either_level() {
        for state in [QUNS_RUNNING_D3D_FULL_SCREEN, QUNS_PRESENTATION_MODE] {
            assert!(decide(state, GamesAndPresentations));
            assert!(decide(state, Always));
            assert!(!decide(state, Never));
        }
    }

    /// A full-screen remote desktop client reports `QUNS_BUSY`, the same as
    /// everything else that is merely full screen: only the top level hides.
    #[test]
    fn an_ordinary_full_screen_app_only_hides_it_at_the_top_level() {
        assert!(decide(QUNS_BUSY, Always));
        assert!(!decide(QUNS_BUSY, GamesAndPresentations));
        assert!(!decide(QUNS_BUSY, Never));
    }

    #[test]
    fn off_means_off() {
        assert!(!active(Never));
    }

    #[test]
    fn the_watcher_only_undoes_its_own_hiding() {
        assert_eq!(step(true, false, true), Step::Hide);
        // Already hidden by the user: nothing to hide, and nothing to restore.
        assert_eq!(step(true, false, false), Step::Nothing);
        assert_eq!(step(false, false, false), Step::Nothing);
        // Still in front, already hidden by us.
        assert_eq!(step(true, true, false), Step::Nothing);
        // The game ended, or the setting was switched off.
        assert_eq!(step(false, true, false), Step::Show);
    }
}
