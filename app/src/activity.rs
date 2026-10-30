//! The usage view's activity calendar: tokens per day for every machine,
//! kept in `activity.json` in the data folder.
//!
//! Claude Code keeps about 30 days of transcripts, so the days are saved as
//! they are seen and the calendar fills in over the months. This machine's
//! days come from its transcripts (the scan the project list already does),
//! other machines' from the activity value their publisher sends. A day's
//! count only grows: a count lower than the saved one means transcripts
//! were deleted, not that fewer tokens were used. A backup restore follows
//! the same rule ([`restore`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cc_core::projects::DayActivity;
use serde::{Deserialize, Serialize};

/// The file in the data folder.
pub const FILE: &str = "activity.json";

/// Days older than this are dropped: a little more than the calendar's six
/// months, so the first column is never cut.
pub const KEEP_DAYS: i64 = 200;

/// The key of this machine's days: its name can change, its days stay.
pub const LOCAL: &str = "";

/// One saved day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Day {
    /// Tokens that day.
    pub t: u64,
    /// The busiest hour (0-23, the machine's local time).
    pub h: u8,
}

/// The file format, `"v"` in the file.
pub const VERSION: u32 = 1;

/// The whole file: per machine ([`LOCAL`] or a publisher's alias), per day
/// (`YYYY-MM-DD`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    /// The format, [`VERSION`].
    v: u32,
    pub machines: BTreeMap<String, BTreeMap<String, Day>>,
}

impl Default for Store {
    /// An empty store.
    fn default() -> Self {
        Self {
            v: VERSION,
            machines: BTreeMap::new(),
        }
    }
}

/// One machine's days in a backup, for [`restore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// The key they go under here: [`LOCAL`] or an alias.
    pub machine: String,
    pub days: Vec<DayActivity>,
}

/// What the usage view draws: every machine's days, this machine under its
/// current name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub machines: Vec<MachineDays>,
}

/// One machine's days in the [`View`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MachineDays {
    pub host: String,
    pub remote: bool,
    pub days: Vec<DayActivity>,
}

/// The most tokens a day can hold: a trillion. Far above what one machine
/// can spend in a day, cache reads included, and far below `u64::MAX`, so
/// a publisher sending an absurd count cannot pin a day, which only grows,
/// at a value no real day will ever beat.
pub const MAX_DAY_TOKENS: u64 = 1_000_000_000_000;

/// What [`Store::open`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Opened {
    /// The saved store, or an empty one when there is no file yet. Safe to
    /// save over.
    Store(Store),
    /// The file is there but could not be read or parsed, and could not be
    /// moved aside either: the empty store stands in for this pass, and
    /// nothing must be saved over the file.
    ReadOnly(Store),
}

impl Store {
    /// The saved store, or an empty one if there is no readable file
    /// (tests).
    pub fn load(path: &Path) -> Self {
        match Self::open(path) {
            Opened::Store(s) | Opened::ReadOnly(s) => s,
        }
    }

    /// The saved store, without ever losing one. A file that exists but
    /// does not parse is moved aside ([`crate::paths::keep_corrupt`]) and
    /// an empty store starts in its place; if it cannot be moved, or cannot
    /// be read at all (locked, no permission), the result is
    /// [`Opened::ReadOnly`] and the caller must not save. Saving an empty
    /// store over it would throw away months of days the transcripts no
    /// longer have.
    pub fn open(path: &Path) -> Opened {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Opened::Store(Self::default())
            }
            Err(err) => {
                warn_once(&format!(
                    "cannot read {}: {err}; not saving over it",
                    path.display()
                ));
                return Opened::ReadOnly(Self::default());
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(store) => Opened::Store(store),
            Err(_) => match crate::paths::keep_corrupt(path) {
                Some(_) => Opened::Store(Self::default()),
                None => Opened::ReadOnly(Self::default()),
            },
        }
    }

    /// Writes the store, atomically: a crash halfway leaves the previous
    /// file whole, never a truncated one the next run could not parse.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string(self).map_err(std::io::Error::other)?;
        crate::paths::write_atomic(path, json.as_bytes())
    }

    /// Adds a machine's days up to `latest` (`YYYY-MM-DD`, tomorrow on the
    /// local clock: [`latest_day`]). A day's count only grows (see the
    /// module docs); the busiest hour follows the larger count. Days after
    /// `latest` and counts above [`MAX_DAY_TOKENS`] are ignored: another
    /// machine's values are not trusted, and either would otherwise stay in
    /// the file for good. Returns whether anything changed.
    pub fn merge<'a>(
        &mut self,
        machine: &str,
        days: impl IntoIterator<Item = &'a DayActivity>,
        latest: &str,
    ) -> bool {
        let saved = self.machines.entry(machine.to_string()).or_default();
        let mut changed = false;
        for d in days {
            let Some(next) = accepted(d, latest) else {
                continue;
            };
            match saved.get(&d.day) {
                Some(have) if have.t >= next.t => {}
                _ => {
                    saved.insert(d.day.clone(), next);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Drops days before `oldest` or after `latest` (`YYYY-MM-DD`, compared
    /// as text; a clock set back can leave days ahead of it), and machines
    /// left without days.
    pub fn prune(&mut self, oldest: &str, latest: &str) {
        for days in self.machines.values_mut() {
            days.retain(|day, _| day.as_str() >= oldest && day.as_str() <= latest);
        }
        self.machines.retain(|_, days| !days.is_empty());
    }

    /// The view, this machine's days under `local_name`.
    pub fn view(&self, local_name: &str) -> View {
        View {
            machines: self
                .machines
                .iter()
                .map(|(machine, days)| MachineDays {
                    host: if machine == LOCAL {
                        local_name.to_string()
                    } else {
                        machine.clone()
                    },
                    remote: machine != LOCAL,
                    days: days
                        .iter()
                        .map(|(day, d)| DayActivity {
                            day: day.clone(),
                            tokens: d.t,
                            peak_hour: d.h,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// The saved form of `d`, or `None` when [`Store::merge`] refuses it: no
/// tokens, an absurd count, a malformed day or one after `latest`.
fn accepted(d: &DayActivity, latest: &str) -> Option<Day> {
    if d.tokens == 0 || d.tokens > MAX_DAY_TOKENS || !is_day(&d.day) || d.day.as_str() > latest {
        return None;
    }
    Some(Day {
        t: d.tokens,
        h: d.peak_hour.min(23),
    })
}

/// `YYYY-MM-DD`, digits where digits go: anything else would sort and
/// compare wrongly.
fn is_day(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// The last day [`Store::merge`] accepts: tomorrow on the local clock. One
/// day of slack, since another machine's local day may already be the next
/// one here.
pub fn latest_day() -> String {
    (chrono::Local::now() + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

/// Logs `what` the first time only: [`update`] runs on every
/// usage view, and a file that cannot be read stays that way for a while.
fn warn_once(what: &str) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!("{what}");
    }
}

/// This machine's days from tokens per local day and hour (a transcript scan).
pub fn local_days(days: &BTreeMap<String, [u64; 24]>) -> Vec<DayActivity> {
    days.iter()
        .filter(|(_, hours)| hours.iter().any(|&t| t > 0))
        .map(|(day, hours)| DayActivity::from_hours(day, hours))
        .collect()
}

/// `activity.json` in the data folder.
pub fn path() -> PathBuf {
    cc_core::paths::data_dir().join(FILE)
}

/// Load, merge and save in one go, one caller at a time: two usage views
/// opened in a row must not lose each other's days.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// Merges `local` (this machine) and `remote` (alias, days) into the file
/// at `path`, keeps the days from `oldest` to `latest` (see
/// [`Store::merge`]), saves if anything changed and returns the view with
/// this machine named `local_name`. A file that cannot be read is never
/// saved over (see [`Store::open`]): the view then shows this pass only.
pub fn update(
    path: &Path,
    local: &[DayActivity],
    remote: &[(String, Vec<DayActivity>)],
    oldest: &str,
    latest: &str,
    local_name: &str,
) -> View {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut store, writable) = match Store::open(path) {
        Opened::Store(s) => (s, true),
        Opened::ReadOnly(s) => (s, false),
    };
    let before = store.clone();
    store.merge(LOCAL, local, latest);
    for (alias, days) in remote {
        if alias != LOCAL {
            store.merge(alias, days, latest);
        }
    }
    store.prune(oldest, latest);
    if writable && store != before {
        if let Err(err) = store.save(path) {
            tracing::warn!("cannot save {}: {err}", path.display());
        }
    }
    store.view(local_name)
}

/// What [`restore`] did to one machine's days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Restored {
    /// The key in the file: [`LOCAL`] or an alias.
    pub machine: String,
    /// Days the file did not have.
    pub added: usize,
    /// Days the backup counted higher.
    pub raised: usize,
}

/// Merges a backup's days into the file at `path` by the same rules as
/// [`update`]: a day only grows, days after `latest` and absurd counts are
/// refused, days before `oldest` dropped. Which key each machine goes under
/// is the caller's choice (see `backup.rs`). Returns what changed per
/// machine, machines without changes left out. Unlike [`update`], a file
/// that cannot be read is an error: a restore that saved nothing must not
/// look like one that found nothing new.
pub fn restore(
    path: &Path,
    incoming: &[Incoming],
    oldest: &str,
    latest: &str,
) -> std::io::Result<Vec<Restored>> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = match Store::open(path) {
        Opened::Store(s) => s,
        Opened::ReadOnly(_) => {
            return Err(std::io::Error::other(format!(
                "{} cannot be read; not restoring over it",
                path.display()
            )))
        }
    };
    let before = store.clone();
    for one in incoming {
        store.merge(&one.machine, &one.days, latest);
    }
    store.prune(oldest, latest);
    let mut report = Vec::new();
    for (machine, days) in &store.machines {
        let had = before.machines.get(machine);
        let mut added = 0;
        let mut raised = 0;
        for (day, d) in days {
            match had.and_then(|h| h.get(day)) {
                None => added += 1,
                Some(old) if old.t < d.t => raised += 1,
                Some(_) => {}
            }
        }
        if added + raised > 0 {
            report.push(Restored {
                machine: machine.clone(),
                added,
                raised,
            });
        }
    }
    if store != before {
        store.save(path)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Far enough ahead that none of the fixed days below is "the future".
    const LATEST: &str = "2099-12-31";

    fn incoming(machine: &str, days: Vec<DayActivity>) -> Incoming {
        Incoming {
            machine: machine.into(),
            days,
        }
    }

    fn day(d: &str, t: u64, h: u8) -> DayActivity {
        DayActivity {
            day: d.into(),
            tokens: t,
            peak_hour: h,
        }
    }

    #[test]
    fn a_day_only_grows_and_bad_days_are_ignored() {
        let mut s = Store::default();
        assert!(s.merge(LOCAL, &[day("2026-10-13", 100, 9)], LATEST));
        assert!(
            !s.merge(LOCAL, &[day("2026-10-13", 60, 22)], LATEST),
            "transcripts were deleted"
        );
        assert_eq!(s.machines[LOCAL]["2026-10-13"], Day { t: 100, h: 9 });
        assert!(s.merge(LOCAL, &[day("2026-10-13", 150, 22)], LATEST));
        assert_eq!(s.machines[LOCAL]["2026-10-13"], Day { t: 150, h: 22 });
        assert!(!s.merge(
            LOCAL,
            &[day("13/10/2026", 5, 1), day("2026-10-14", 0, 1)],
            LATEST
        ));
    }

    /// Another machine's values are not trusted: a day in the future or an
    /// absurd count would otherwise stay in the file for good.
    #[test]
    fn future_days_and_absurd_counts_are_refused() {
        let mut s = Store::default();
        let latest = "2026-10-15";
        assert!(!s.merge(
            "devbox",
            &[day("2026-10-16", 10, 1), day("2099-01-01", 10, 1)],
            latest
        ));
        assert!(!s.merge("devbox", &[day("2026-10-13", u64::MAX, 1)], latest));
        assert!(!s.merge(
            "devbox",
            &[day("2026-10-13", MAX_DAY_TOKENS + 1, 1)],
            latest
        ));
        assert!(
            s.merge("devbox", &[day("2026-10-15", MAX_DAY_TOKENS, 1)], latest),
            "tomorrow is fine"
        );
        assert_eq!(s.machines["devbox"].len(), 1);
        // Tomorrow on this clock, whatever the date.
        let tomorrow = latest_day();
        assert!(is_day(&tomorrow));
        assert!(tomorrow > chrono::Local::now().format("%Y-%m-%d").to_string());
    }

    #[test]
    fn old_and_future_days_and_empty_machines_go() {
        let mut s = Store::default();
        s.merge("devbox", &[day("2026-01-01", 5, 1)], LATEST);
        s.merge(
            LOCAL,
            &[
                day("2026-01-01", 5, 1),
                day("2026-10-01", 7, 2),
                day("2027-01-01", 7, 2),
            ],
            LATEST,
        );
        s.prune("2026-04-01", "2026-10-15");
        assert!(!s.machines.contains_key("devbox"));
        assert_eq!(s.machines[LOCAL].keys().collect::<Vec<_>>(), ["2026-10-01"]);
    }

    #[test]
    fn the_view_names_this_machine_and_marks_the_others_remote() {
        let mut s = Store::default();
        s.merge(LOCAL, &[day("2026-10-13", 10, 3)], LATEST);
        s.merge("devbox", &[day("2026-10-12", 20, 4)], LATEST);
        let v = s.view("office-pc-07");
        let hosts: Vec<(&str, bool)> = v
            .machines
            .iter()
            .map(|m| (m.host.as_str(), m.remote))
            .collect();
        assert_eq!(hosts, [("office-pc-07", false), ("devbox", true)]);
        assert_eq!(v.machines[1].days, [day("2026-10-12", 20, 4)]);
    }

    #[test]
    fn update_saves_what_it_merged_and_keeps_it_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        update(
            &path,
            &[day("2026-10-13", 10, 3)],
            &[("devbox".into(), vec![day("2026-10-12", 20, 4)])],
            "2026-01-01",
            LATEST,
            "pc",
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(r#"{"v":1,"#), "{text}");
        // A later run whose transcripts no longer reach the 13th still shows it.
        let v = update(
            &path,
            &[day("2026-10-14", 5, 1)],
            &[],
            "2026-01-01",
            LATEST,
            "pc",
        );
        let pc = v.machines.iter().find(|m| m.host == "pc").unwrap();
        assert_eq!(pc.days.len(), 2);
        assert!(v.machines.iter().any(|m| m.host == "devbox"));
    }

    /// A file that does not parse is kept aside, never saved over: it may
    /// hold months of days the transcripts no longer have.
    #[test]
    fn a_corrupt_file_is_kept_aside_and_a_new_one_starts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, r#"{"v":1,"machines":{"":{"2026-01-01":{"t":5,"#).unwrap();
        let v = update(
            &path,
            &[day("2026-10-13", 10, 3)],
            &[],
            "2026-01-01",
            LATEST,
            "pc",
        );
        assert_eq!(v.machines.len(), 1);
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("activity.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1, "{aside:?}");
        let kept = std::fs::read_to_string(dir.path().join(&aside[0])).unwrap();
        assert!(kept.contains("2026-01-01"), "the corrupt bytes are intact");
        assert_eq!(
            Store::load(&path).machines[LOCAL].len(),
            1,
            "the new file is valid"
        );
    }

    /// A restore counts what it added and raised, by the usual rules, and
    /// refuses a file it cannot read instead of pretending nothing was new.
    #[test]
    fn restore_reports_added_and_raised_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        update(
            &path,
            &[day("2026-10-13", 10, 3)],
            &[],
            "2026-01-01",
            LATEST,
            "pc",
        );
        let backup = vec![
            incoming(
                LOCAL,
                vec![day("2026-10-13", 30, 5), day("2026-10-12", 5, 1)],
            ),
            incoming(
                "devbox",
                vec![day("2026-10-11", 7, 2), day("2025-01-01", 7, 2)],
            ),
        ];
        let report = restore(&path, &backup, "2026-01-01", LATEST).unwrap();
        assert_eq!(
            report,
            [
                Restored {
                    machine: LOCAL.into(),
                    added: 1,
                    raised: 1
                },
                Restored {
                    machine: "devbox".into(),
                    added: 1,
                    raised: 0
                },
            ]
        );
        assert!(restore(&path, &backup, "2026-01-01", LATEST)
            .unwrap()
            .is_empty());

        // A lower count in a backup never lowers a day.
        let lower = vec![incoming(LOCAL, vec![day("2026-10-13", 1, 1)])];
        assert!(restore(&path, &lower, "2026-01-01", LATEST)
            .unwrap()
            .is_empty());
        assert_eq!(Store::load(&path).machines[LOCAL]["2026-10-13"].t, 30);

        let blocked = dir.path().join("blocked.json");
        std::fs::create_dir(&blocked).unwrap();
        assert!(restore(&blocked, &backup, "2026-01-01", LATEST).is_err());
    }

    /// A file that cannot even be read (here: a folder in its place) is
    /// left alone, and the view still shows this pass.
    #[test]
    fn an_unreadable_file_is_never_saved_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(Store::open(&path), Opened::ReadOnly(_)));
        let v = update(
            &path,
            &[day("2026-10-13", 10, 3)],
            &[],
            "2026-01-01",
            LATEST,
            "pc",
        );
        assert_eq!(v.machines.len(), 1);
        assert!(path.is_dir());
    }
}
