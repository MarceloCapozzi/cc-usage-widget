//! Alerts when thresholds are crossed, only once per reset window.
//!
//! # The problem it solves
//!
//! The poll runs every few minutes. Without memory, a limit sitting at 85%
//! would fire an alert on *every* pass for hours. The window is identified by
//! its `resets_at`: while it does not change, each threshold alerts once; when
//! it changes, a new window starts and the alerts re-arm on their own.
//!
//! # `resets_at` jitters
//!
//! The API returns that timestamp with sub-second variation between
//! consecutive reads. Comparing for inequality yields "changed" on every poll
//! and re-arms the alerts all the time. That is why the window is identified by
//! its timestamp rounded to the minute, not by the exact value.

use crate::model::{Limit, Severity, UsageSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Window id of a limit that has no `resets_at`.
const NO_RESET_WINDOW: &str = "no-reset";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    pub warn_at: f64,
    pub critical_at: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            warn_at: 80.0,
            critical_at: 95.0,
        }
    }
}

impl Settings {
    /// The thresholds come from the user's config, which may have been edited
    /// by hand. They are bounded here and `warn <= critical` is enforced, or the
    /// critical alert would never fire.
    pub fn sanitized(&self) -> Self {
        let warn = self.warn_at.clamp(1.0, 100.0);
        let crit = self.critical_at.clamp(1.0, 100.0).max(warn);
        Self {
            enabled: self.enabled,
            warn_at: warn,
            critical_at: crit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    Warning,
    Critical,
}

/// A Windows toast. `title` and `body` are user-facing text.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub level: Level,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Flags {
    window: String,
    warned: bool,
    critical: bool,
}

/// What has already been alerted, per limit. It is persisted so restarting the
/// widget does not fire the same thing again.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    seen: HashMap<String, Flags>,
}

impl State {
    pub fn load() -> Self {
        Self::load_from(&default_path())
    }

    pub fn load_from(path: &PathBuf) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = self.save_to(&default_path());
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        let json = serde_json::to_string(self).unwrap_or_else(|_| "{}".into());
        // Atomic: a truncated file would load as "nothing alerted yet" and
        // repeat every alert of the current windows.
        crate::paths::write_atomic(path, json.as_bytes())
    }

    /// Decides what to alert on this pass and marks what was alerted.
    pub fn decide(&mut self, usage: &UsageSnapshot, settings: &Settings) -> Vec<Notification> {
        let settings = settings.sanitized();
        if !settings.enabled {
            return Vec::new();
        }

        let mut out = Vec::new();
        for limit in &usage.limits {
            let window = window_id(limit);
            let flags = self.seen.entry(limit.label.clone()).or_default();

            // New window: everything re-arms for this limit.
            if flags.window != window {
                *flags = Flags {
                    window,
                    ..Default::default()
                };
            }

            let critical = limit.percent >= settings.critical_at
                || matches!(limit.severity, Severity::Critical);
            let warning =
                limit.percent >= settings.warn_at || matches!(limit.severity, Severity::Warning);

            if critical && !flags.critical {
                flags.critical = true;
                // A critical alert also consumes the warning: otherwise, after
                // dropping and rising again, an "80%" would arrive after the "95%".
                flags.warned = true;
                out.push(build(limit, Level::Critical));
            } else if warning && !flags.warned {
                flags.warned = true;
                out.push(build(limit, Level::Warning));
            }
        }
        out
    }
}

/// Builds the toast text (user-facing).
fn build(limit: &Limit, level: Level) -> Notification {
    let title = format!("{} at {}%", limit.label, limit.percent.round() as i64);
    let body = match limit.resets_at {
        Some(t) => {
            let d = t - chrono::Utc::now();
            if d.num_seconds() <= 0 {
                "Resets shortly".to_string()
            } else if d.num_days() > 0 {
                format!("Resets in {}d {}h", d.num_days(), d.num_hours() % 24)
            } else if d.num_hours() > 0 {
                format!("Resets in {}h {}m", d.num_hours(), d.num_minutes() % 60)
            } else {
                format!("Resets in {}m", d.num_minutes())
            }
        }
        None => match level {
            Level::Critical => "Almost at the limit".into(),
            Level::Warning => "Getting close to the limit".into(),
        },
    };
    Notification { title, body, level }
}

/// Identity of the window, rounded to the minute to absorb the sub-second
/// jitter of `resets_at`.
fn window_id(limit: &Limit) -> String {
    match limit.resets_at {
        Some(t) => (t.timestamp() / 60).to_string(),
        None => NO_RESET_WINDOW.into(),
    }
}

fn default_path() -> PathBuf {
    crate::paths::file(crate::paths::NOTIFICATION_STATE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Limit, LimitKind, Severity, UsageSnapshot};
    use chrono::{Duration, TimeZone, Utc};

    fn snap(pct: f64, reset_offset_secs: i64) -> UsageSnapshot {
        UsageSnapshot {
            limits: vec![Limit {
                kind: LimitKind::Session,
                label: "5-hour session".into(),
                percent: pct,
                severity: Severity::Normal,
                resets_at: Some(Utc::now() + Duration::seconds(reset_offset_secs)),
                is_active: true,
            }],
            spend: None,
            fetched_at: Utc::now(),
        }
    }

    #[test]
    fn alerts_only_once_per_window() {
        let mut st = State::default();
        let s = Settings::default();
        assert_eq!(st.decide(&snap(85.0, 3600), &s).len(), 1);
        // Same threshold crossed, same window: silence.
        assert_eq!(st.decide(&snap(86.0, 3600), &s).len(), 0);
        assert_eq!(st.decide(&snap(90.0, 3600), &s).len(), 0);
    }

    #[test]
    fn critical_fires_after_warning() {
        let mut st = State::default();
        let s = Settings::default();
        assert_eq!(st.decide(&snap(85.0, 3600), &s)[0].level, Level::Warning);
        assert_eq!(st.decide(&snap(96.0, 3600), &s)[0].level, Level::Critical);
        assert_eq!(st.decide(&snap(99.0, 3600), &s).len(), 0);
    }

    /// Jumping straight to critical must not leave a late warning pending.
    #[test]
    fn critical_consumes_the_warning() {
        let mut st = State::default();
        let s = Settings::default();
        let n = st.decide(&snap(97.0, 3600), &s);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].level, Level::Critical);
        assert_eq!(st.decide(&snap(85.0, 3600), &s).len(), 0);
    }

    /// The case that led to rounding to the minute: `resets_at` jitters by less
    /// than a second between reads. Without rounding, every poll would be a
    /// "new window" and the alert would repeat indefinitely.
    #[test]
    fn sub_second_jitter_does_not_rearm_the_alerts() {
        // Anchor the reset in the middle of a minute: with `Utc::now()` the
        // test failed whenever it ran within a second of a minute boundary.
        let reset = Utc.timestamp_opt(1_789_980_030, 0).unwrap();
        let at = |offset: i64| {
            let mut u = snap(85.0, 0);
            u.limits[0].resets_at = Some(reset + Duration::seconds(offset));
            u
        };
        let mut st = State::default();
        let s = Settings::default();
        assert_eq!(st.decide(&at(0), &s).len(), 1);
        for jitter in [-1_i64, 1, 0] {
            assert_eq!(
                st.decide(&at(jitter), &s).len(),
                0,
                "a ±1 s shift is not a new window"
            );
        }
    }

    #[test]
    fn a_new_window_rearms() {
        let mut st = State::default();
        let s = Settings::default();
        assert_eq!(st.decide(&snap(85.0, 3600), &s).len(), 1);
        // Reset moved by several hours: it is another window.
        assert_eq!(st.decide(&snap(85.0, 3600 + 7200), &s).len(), 1);
    }

    #[test]
    fn can_be_turned_off() {
        let mut st = State::default();
        let s = Settings {
            enabled: false,
            ..Default::default()
        };
        assert_eq!(st.decide(&snap(99.0, 3600), &s).len(), 0);
    }

    #[test]
    fn inverted_thresholds_do_not_break() {
        let s = Settings {
            enabled: true,
            warn_at: 95.0,
            critical_at: 80.0,
        }
        .sanitized();
        // Critical can never end up below warning.
        assert!(s.critical_at >= s.warn_at);
        let out_of_range = Settings {
            enabled: true,
            warn_at: -20.0,
            critical_at: 500.0,
        }
        .sanitized();
        assert_eq!(out_of_range.warn_at, 1.0);
        assert_eq!(out_of_range.critical_at, 100.0);
    }

    #[test]
    fn server_severity_also_fires() {
        let mut st = State::default();
        let mut s = snap(5.0, 3600);
        s.limits[0].severity = Severity::Critical;
        // Even with a low percentage, if the server marks it critical we alert.
        assert_eq!(
            st.decide(&s, &Settings::default())[0].level,
            Level::Critical
        );
    }

    /// A saved `notification-state.json` keeps suppressing what was already
    /// alerted after a restart, including limits without a reset time.
    #[test]
    fn a_saved_state_file_keeps_suppressing_after_a_restart() {
        let reset = Utc.timestamp_opt(1_789_980_000, 0).unwrap();
        let window = (reset.timestamp() / 60).to_string();
        let saved = format!(
            r#"{{"seen":{{
                "5-hour session":{{"window":"{window}","warned":true,"critical":false}},
                "Fable · weekly":{{"window":"{NO_RESET_WINDOW}","warned":true,"critical":true}}
            }}}}"#
        );
        let mut p = std::env::temp_dir();
        p.push(format!("notif-saved-{}.json", std::process::id()));
        std::fs::write(&p, saved).unwrap();

        let mut st = State::load_from(&p);
        assert_eq!(st.seen.len(), 2);
        assert_eq!(st.seen["Fable · weekly"].window, NO_RESET_WINDOW);
        assert!(st.seen.contains_key("5-hour session"));

        let usage = UsageSnapshot {
            limits: vec![
                Limit {
                    kind: LimitKind::Session,
                    label: "5-hour session".into(),
                    percent: 85.0,
                    severity: Severity::Normal,
                    resets_at: Some(reset),
                    is_active: true,
                },
                Limit {
                    kind: LimitKind::WeeklyScoped,
                    label: "Fable · weekly".into(),
                    percent: 99.0,
                    severity: Severity::Normal,
                    resets_at: None,
                    is_active: false,
                },
            ],
            spend: None,
            fetched_at: Utc::now(),
        };
        assert!(
            st.decide(&usage, &Settings::default()).is_empty(),
            "alerts already sent must not fire again"
        );
        let _ = std::fs::remove_file(p);
    }
}
