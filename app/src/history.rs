//! Local usage history: one sample per successful refresh.
//!
//! It is what feeds the flyout's chart. It lives only on this machine, in
//! `%LOCALAPPDATA%\cc-usage\history.json`, and contains nothing but
//! percentages and timestamps.
//!
//! # Three rules that define the module
//!
//! **A failed refresh writes nothing.** A gap in the series means "could not
//! read", never "usage was zero". If a 0 were stored on every failure, the
//! chart would show drops that never happened and the user would draw false
//! conclusions about their consumption.
//!
//! **There is a minimum interval between samples.** With a 1-minute refresh and
//! 30 days of retention, 43,200 points would pile up to draw a 300 px line. The
//! 5-minute floor bounds that without losing shape.
//!
//! **Unless a window reset.** That point marks the edge of the cycle and has to
//! survive the floor, or the chart joins the end of one window with the start
//! of the next and draws a gradual drop that never happened.
//!
//! # Why JSON and not SQLite
//!
//! Measured over 20 days of real use: ~7,400 samples over 30 days, at 154 bytes
//! each, are ~1.1 MB, which filter in memory in microseconds. SQLite would add a
//! dependency that compiles C code for queries that today are a `filter`.
//! If per-model data with long retention is ever stored, this is worth
//! revisiting.

use crate::model::UsageSnapshot;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Minimum interval between consecutive samples.
pub const MIN_INTERVAL: Duration = Duration::minutes(5);
/// Default retention.
pub const RETENTION_DAYS: i64 = 30;
/// Hard cap on samples, in case the floor fails because the clock jumped.
pub const MAX_SAMPLES: usize = 12_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reading {
    /// The limit's label, as `model.rs` builds it ("5-hour session", "All
    /// models · weekly", "Fable · weekly"). The name is stored rather than an index: limits come
    /// and go depending on the plan, and an index would drift out of alignment.
    pub k: String,
    pub p: f64,
    /// `resets_at` as epoch ms. Needed to detect a window change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    /// Epoch in milliseconds.
    pub t: i64,
    pub v: Vec<Reading>,
}

impl Sample {
    pub fn from_snapshot(snapshot: &UsageSnapshot) -> Self {
        Self {
            t: Utc::now().timestamp_millis(),
            v: snapshot
                .limits
                .iter()
                .map(|l| Reading {
                    k: l.label.clone(),
                    p: l.percent,
                    r: l.resets_at.map(|t| t.timestamp_millis()),
                })
                .collect(),
        }
    }

    fn reset_of(&self, key: &str) -> Option<i64> {
        self.v.iter().find(|l| l.k == key)?.r
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub samples: Vec<Sample>,
}

pub fn default_path() -> PathBuf {
    crate::paths::file(crate::paths::HISTORY)
}

impl History {
    pub fn load() -> Self {
        Self::load_from(&default_path())
    }

    pub fn load_from(path: &PathBuf) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Adds the samples of `other` (a restored backup) this history lacks:
    /// a sample whose timestamp is already here is skipped, the rest are
    /// put in time order, and retention and the hard cap apply as usual.
    /// The minimum interval does not: both sides already obeyed it, and
    /// dropping a sample would only lose data. Returns how many were added
    /// and kept.
    pub fn merge(&mut self, other: History) -> usize {
        let have: std::collections::HashSet<i64> = self.samples.iter().map(|s| s.t).collect();
        let mut seen = have.clone();
        for s in other.samples {
            if seen.insert(s.t) {
                self.samples.push(s);
            }
        }
        self.samples.sort_by_key(|s| s.t);
        self.prune(RETENTION_DAYS);
        self.samples.iter().filter(|s| !have.contains(&s.t)).count()
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&default_path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        let json = serde_json::to_string(self).unwrap_or_else(|_| "{}".into());
        // Atomic write: the flyout may be reading right in the middle and
        // would find a truncated JSON.
        crate::paths::write_atomic(path, json.as_bytes())
    }

    /// Adds the sample if it qualifies. Returns `true` if it was stored.
    ///
    /// It is dropped when the minimum interval has not elapsed since the
    /// previous one, **unless** some window reset in between.
    pub fn add(&mut self, s: Sample) -> bool {
        if let Some(last) = self.samples.last() {
            let elapsed = Duration::milliseconds(s.t - last.t);
            if elapsed < MIN_INTERVAL && !window_reset_between(last, &s) {
                return false;
            }
        }
        self.samples.push(s);
        self.prune(RETENTION_DAYS);
        true
    }

    pub fn prune(&mut self, days: i64) {
        let cutoff = Utc::now().timestamp_millis() - days * 86_400_000;
        self.samples.retain(|s| s.t >= cutoff);
        if self.samples.len() > MAX_SAMPLES {
            let excess = self.samples.len() - MAX_SAMPLES;
            self.samples.drain(0..excess);
        }
    }

    /// Samples inside the requested window, oldest first.
    pub fn range(&self, since: DateTime<Utc>) -> Vec<&Sample> {
        let cutoff = since.timestamp_millis();
        self.samples.iter().filter(|s| s.t >= cutoff).collect()
    }
}

/// Did any window reset between the two samples?
///
/// A reset means the previous window **has ended**, not that the timestamp
/// changed. The API returns `resets_at` with sub-second variation between
/// consecutive reads, so comparing for inequality yields "changed" on every
/// refresh and defeats the floor entirely.
fn window_reset_between(previous: &Sample, current: &Sample) -> bool {
    current.v.iter().any(|l| {
        let (Some(after), Some(before)) = (l.r, previous.reset_of(&l.k)) else {
            return false;
        };
        after > before && before <= current.t
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t_ms: i64, pct: f64, reset_ms: Option<i64>) -> Sample {
        Sample {
            t: t_ms,
            v: vec![Reading {
                k: "5-hour session".into(),
                p: pct,
                r: reset_ms,
            }],
        }
    }

    fn now() -> i64 {
        Utc::now().timestamp_millis()
    }

    #[test]
    fn the_first_sample_always_goes_in() {
        let mut h = History::default();
        assert!(h.add(sample(now(), 10.0, None)));
        assert_eq!(h.samples.len(), 1);
    }

    #[test]
    fn the_floor_drops_samples_too_close_together() {
        let t = now();
        let mut h = History::default();
        assert!(h.add(sample(t, 10.0, None)));
        assert!(!h.add(sample(t + 60_000, 11.0, None)), "1 min < floor");
        assert!(h.add(sample(t + 6 * 60_000, 12.0, None)), "6 min > floor");
        assert_eq!(h.samples.len(), 2);
    }

    /// The reset point marks the edge of the cycle: it has to go in even if the
    /// floor has not elapsed, or the chart joins two different windows.
    #[test]
    fn a_reset_goes_through_the_floor() {
        let t = now();
        let mut h = History::default();
        h.add(sample(t, 96.0, Some(t + 1000)));
        // One minute later, the previous window has already expired and there
        // is a new one.
        let new = sample(t + 60_000, 2.0, Some(t + 5 * 3_600_000));
        assert!(h.add(new), "the reset must be stored anyway");
        assert_eq!(h.samples.len(), 2);
    }

    /// The bug that led to comparing by "the window ended" and not by "the
    /// value changed": `resets_at` jitters by less than a second on every read.
    /// Without this, every refresh would look like a new window and the floor
    /// would filter nothing.
    #[test]
    fn sub_second_jitter_does_not_count_as_a_reset() {
        let t = now();
        let future = t + 3 * 3_600_000;
        let mut h = History::default();
        h.add(sample(t, 50.0, Some(future)));
        for jitter in [1_i64, -270, 910, -1090] {
            let s = sample(t + 60_000, 51.0, Some(future + jitter));
            assert!(!h.add(s), "a shift of {jitter} ms is not a new window");
        }
        assert_eq!(h.samples.len(), 1);
    }

    /// A `resets_at` that moves into the future while the previous window has
    /// not expired yet is not a reset either.
    #[test]
    fn a_window_that_has_not_expired_is_not_a_reset() {
        let t = now();
        let mut h = History::default();
        h.add(sample(t, 50.0, Some(t + 3_600_000)));
        let s = sample(t + 60_000, 51.0, Some(t + 7_200_000));
        assert!(!h.add(s));
    }

    #[test]
    fn prunes_by_retention() {
        let t = now();
        let mut h = History {
            samples: vec![
                sample(t - 40 * 86_400_000, 1.0, None),
                sample(t - 10 * 86_400_000, 2.0, None),
                sample(t, 3.0, None),
            ],
        };
        h.prune(30);
        assert_eq!(h.samples.len(), 2, "the 40-day-old one is dropped");
        assert_eq!(h.samples[0].p_of("5-hour session"), Some(2.0));
    }

    #[test]
    fn the_range_filters_by_time() {
        let t = now();
        let h = History {
            samples: vec![
                sample(t - 10 * 3_600_000, 1.0, None),
                sample(t - 2 * 3_600_000, 2.0, None),
                sample(t, 3.0, None),
            ],
        };
        assert_eq!(h.range(Utc::now() - Duration::hours(6)).len(), 2);
        assert_eq!(h.range(Utc::now() - Duration::hours(24)).len(), 3);
    }

    #[test]
    fn the_hard_cap_trims_the_oldest() {
        let t = now();
        let mut h = History {
            samples: (0..(MAX_SAMPLES + 50))
                .map(|i| sample(t - (MAX_SAMPLES + 50 - i) as i64 * 1000, i as f64, None))
                .collect(),
        };
        h.prune(30);
        assert_eq!(h.samples.len(), MAX_SAMPLES);
        // The latest survive, not the first.
        assert_eq!(
            h.samples.last().unwrap().p_of("5-hour session"),
            Some((MAX_SAMPLES + 49) as f64)
        );
    }

    #[test]
    fn round_trips_through_disk() {
        let mut p = std::env::temp_dir();
        p.push(format!("hist-test-{}.json", std::process::id()));
        let mut h = History::default();
        h.add(sample(now(), 42.0, Some(now() + 1000)));
        h.save_to(&p).unwrap();
        let loaded = History::load_from(&p);
        assert_eq!(loaded.samples, h.samples);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_corrupt_file_breaks_nothing() {
        let mut p = std::env::temp_dir();
        p.push(format!("hist-bad-{}.json", std::process::id()));
        std::fs::write(&p, "{this is not json").unwrap();
        assert!(History::load_from(&p).samples.is_empty());
        let _ = std::fs::remove_file(p);
    }

    /// A restore adds what is missing, in time order, once per timestamp,
    /// and keeps retention.
    #[test]
    fn merge_is_a_union_by_timestamp() {
        let t = now();
        let mut h = History {
            samples: vec![sample(t - 20 * 60_000, 1.0, None), sample(t, 3.0, None)],
        };
        let other = History {
            samples: vec![
                sample(t - 40 * 86_400_000, 9.0, None),
                sample(t - 10 * 60_000, 2.0, None),
                sample(t, 99.0, None),
                sample(t - 10 * 60_000, 2.0, None),
            ],
        };
        assert_eq!(
            h.merge(other),
            1,
            "only the 10-minute-old sample is new and kept"
        );
        let got: Vec<f64> = h
            .samples
            .iter()
            .map(|s| s.p_of("5-hour session").unwrap())
            .collect();
        assert_eq!(got, [1.0, 2.0, 3.0], "sorted; the sample already here wins");
        assert_eq!(h.merge(History::default()), 0);
    }

    impl Sample {
        fn p_of(&self, key: &str) -> Option<f64> {
            self.v.iter().find(|l| l.k == key).map(|l| l.p)
        }
    }
}
