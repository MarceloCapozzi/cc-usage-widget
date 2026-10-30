//! The projects half of the gathering thread: this machine's token usage
//! per project, published so the widget's usage view can list it under this
//! machine's alias.
//!
//! [`SCAN_EVERY`] after the last scan ended the transcripts are scanned
//! again ([`cc_local::projects::scan_all`], the last [`PROJECT_WINDOW_DAYS`]
//! days) on a thread of its own, one scan at a time: reading a month of
//! transcripts can take seconds, and [`ProjectsFeed::step`] runs inside the
//! session pass, so it never waits for it. The scan hands its result back
//! through a channel and the first step after it finishes applies it: each
//! project becomes one [`ProjectPayload`] under `<alias>.p.<pid>`. Every
//! [`REPUBLISH_EVERY`], shorter than the bucket's 3-minute TTL, all of
//! them are put again (the last scan's, while a new one runs) so they stay
//! in the bucket for a widget that connects later; a project that left the
//! window is deleted. What travels is the folder name, session count,
//! tokens per model and last activity: no path, no content, no cost (the
//! widget prices the tokens itself).
//!
//! The same scan gives the machine's activity: tokens per local day and the
//! busiest hour of each, for the last [`ACTIVITY_DAYS`] days with any, put
//! under `<alias>.activity` with the projects.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cc_core::projects::{
    ActivityPayload, DayActivity, ProjectPayload, ACTIVITY_DAYS, PROJECT_WINDOW_DAYS,
};
use cc_envelope::{activity_key, pid_for, project_key};
use cc_local::projects::Scan;

/// How often the transcripts are scanned again.
pub const SCAN_EVERY: Duration = Duration::from_secs(5 * 60);

/// How often every project is put again: under the bucket's 3-minute TTL,
/// so the values never expire while the publisher runs.
pub const REPUBLISH_EVERY: Duration = Duration::from_secs(2 * 60);

/// Where the feed's decisions go: the [`crate::outbox::Outbox`], or a fake.
pub trait ProjectSink {
    /// Puts one project's usage under `key`.
    fn put_project(&mut self, key: &str, payload: &ProjectPayload);
    /// Deletes a project that left the window.
    fn delete_project(&mut self, key: &str);
    /// Puts the machine's activity under `key`.
    fn put_activity(&mut self, key: &str, payload: &ActivityPayload);
}

/// What one [`ProjectsFeed::step`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FeedStep {
    /// Whether a transcript scan was started on this step.
    pub started: bool,
    /// Whether a finished scan's result was applied on this step.
    pub scanned: bool,
    /// Projects put.
    pub puts: usize,
    /// Projects deleted.
    pub deletes: usize,
}

/// What reads the transcripts under a folder; [`scan_window`] in a real run.
type Scanner = fn(&Path) -> Scan;

/// The real scan: every transcript of the last [`PROJECT_WINDOW_DAYS`]
/// days (the activity keeps at most [`ACTIVITY_DAYS`] of them).
fn scan_window(dir: &Path) -> Scan {
    cc_local::projects::scan_all(dir, Some(i64::from(PROJECT_WINDOW_DAYS)))
}

/// See the module docs.
#[derive(Debug)]
pub struct ProjectsFeed {
    alias: String,
    dir: PathBuf,
    scanner: Scanner,
    /// When the last scan ended (was applied or died), or started while it
    /// runs: the next one is due [`SCAN_EVERY`] later, so a slow scan never
    /// turns into back-to-back scans.
    last_scan: Option<Instant>,
    last_push: Option<Instant>,
    /// The result of the scan running now, when one runs.
    scanning: Option<mpsc::Receiver<Scan>>,
    /// The projects of the last scan, by KV key.
    current: BTreeMap<String, ProjectPayload>,
    /// The activity of the last scan; `None` with no tokens in the window.
    activity: Option<ActivityPayload>,
}

impl ProjectsFeed {
    /// A feed for `alias` over the transcripts under `dir`
    /// ([`cc_local::projects::projects_dir`] in a real run).
    pub fn new(alias: &str, dir: PathBuf) -> Self {
        Self::with_scanner(alias, dir, scan_window)
    }

    fn with_scanner(alias: &str, dir: PathBuf, scanner: Scanner) -> Self {
        Self {
            alias: alias.to_string(),
            dir,
            scanner,
            last_scan: None,
            last_push: None,
            scanning: None,
            current: BTreeMap::new(),
            activity: None,
        }
    }

    /// Projects known from the last scan.
    pub fn len(&self) -> usize {
        self.current.len()
    }

    /// True before the first scan finished, or when nothing was active in
    /// the window.
    pub fn is_empty(&self) -> bool {
        self.current.is_empty()
    }

    /// Whether a scan is running now.
    pub fn is_scanning(&self) -> bool {
        self.scanning.is_some()
    }

    /// One turn of the gathering loop; never waits for a scan. Applies the
    /// result of a scan that finished since the last step, starts one when
    /// none runs and [`SCAN_EVERY`] passed since the last one ended (or none
    /// ever ran), and puts every project after a scan was applied, when
    /// [`REPUBLISH_EVERY`] passed, or on `resync` (a reconnect).
    pub fn step(
        &mut self,
        now: Instant,
        now_unix: i64,
        resync: bool,
        sink: &mut impl ProjectSink,
    ) -> FeedStep {
        let mut step = FeedStep::default();
        // Before starting one: a scan is never applied on the step it started.
        if let Some(rx) = &self.scanning {
            match rx.try_recv() {
                Ok(scan) => {
                    self.scanning = None;
                    self.last_scan = Some(now);
                    step.scanned = true;
                    let (next, activity) = self.payloads(scan, now_unix);
                    for key in self.current.keys().filter(|k| !next.contains_key(*k)) {
                        sink.delete_project(key);
                        step.deletes += 1;
                    }
                    self.current = next;
                    self.activity = activity;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scanning = None;
                    self.last_scan = Some(now);
                    tracing::warn!(
                        "the transcript scan ended without a result; trying again in {} s",
                        SCAN_EVERY.as_secs()
                    );
                }
            }
        }
        let due = |last: Option<Instant>, every: Duration| {
            last.is_none_or(|t| now.duration_since(t) >= every)
        };
        if self.scanning.is_none() && due(self.last_scan, SCAN_EVERY) {
            self.last_scan = Some(now);
            match self.start_scan() {
                Ok(rx) => {
                    self.scanning = Some(rx);
                    step.started = true;
                }
                Err(err) => tracing::warn!(
                    "cannot start the transcript scan: {err}; trying again in {} s",
                    SCAN_EVERY.as_secs()
                ),
            }
        }
        if step.scanned || resync || due(self.last_push, REPUBLISH_EVERY) {
            self.last_push = Some(now);
            for (key, payload) in &self.current {
                sink.put_project(key, payload);
                step.puts += 1;
            }
            if let Some(activity) = &self.activity {
                sink.put_activity(&activity_key(&self.alias), activity);
                step.puts += 1;
            }
        }
        step
    }

    /// Runs the scanner on a thread of its own. If the feed is dropped
    /// first, the thread finishes its scan and the result goes nowhere.
    fn start_scan(&self) -> std::io::Result<mpsc::Receiver<Scan>> {
        let (tx, rx) = mpsc::sync_channel(1);
        let (dir, scanner) = (self.dir.clone(), self.scanner);
        std::thread::Builder::new()
            .name("projects-scan".into())
            .spawn(move || {
                let _ = tx.send(scanner(&dir));
            })?;
        Ok(rx)
    }

    /// The payloads of `scan`, stamped `now_unix`.
    fn payloads(
        &self,
        scan: Scan,
        now_unix: i64,
    ) -> (BTreeMap<String, ProjectPayload>, Option<ActivityPayload>) {
        let projects = scan
            .projects
            .into_iter()
            .map(|p| {
                let key = project_key(&self.alias, &pid_for(&p.key));
                let payload = ProjectPayload::new(
                    &self.alias,
                    &p.name,
                    p.sessions,
                    &p.by_model,
                    p.last_seen,
                    now_unix,
                );
                (key, payload)
            })
            .collect();
        let mut days: Vec<DayActivity> = scan
            .days
            .iter()
            .filter(|(_, hours)| hours.iter().any(|&t| t > 0))
            .map(|(day, hours)| DayActivity::from_hours(day, hours))
            .collect();
        if days.len() > ACTIVITY_DAYS {
            days.drain(..days.len() - ACTIVITY_DAYS);
        }
        let activity = (!days.is_empty()).then(|| ActivityPayload {
            machine: self.alias.clone(),
            days,
            hb: now_unix,
        });
        (projects, activity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, Mutex};

    #[derive(Default)]
    struct Recorder {
        puts: Vec<(String, ProjectPayload)>,
        deletes: Vec<String>,
        activity: Vec<(String, ActivityPayload)>,
    }

    impl ProjectSink for Recorder {
        fn put_project(&mut self, key: &str, payload: &ProjectPayload) {
            self.puts.push((key.to_string(), payload.clone()));
        }
        fn delete_project(&mut self, key: &str) {
            self.deletes.push(key.to_string());
        }
        fn put_activity(&mut self, key: &str, payload: &ActivityPayload) {
            self.activity.push((key.to_string(), payload.clone()));
        }
    }

    fn transcript(dir: &std::path::Path, folder: &str, cwd: &str, input: u64) {
        let d = dir.join(folder);
        std::fs::create_dir_all(&d).unwrap();
        let line = format!(
            r#"{{"timestamp":"2099-01-01T00:00:00Z","cwd":"{cwd}","message":{{"model":"claude-opus-5-5","usage":{{"input_tokens":{input},"output_tokens":1}}}}}}"#
        );
        std::fs::write(d.join("s1.jsonl"), line).unwrap();
    }

    /// Steps at `now` until the running scan is applied (5 s at most).
    fn settle(
        feed: &mut ProjectsFeed,
        now: Instant,
        now_unix: i64,
        sink: &mut Recorder,
    ) -> FeedStep {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let step = feed.step(now, now_unix, false, sink);
            if step.scanned {
                return step;
            }
            assert!(Instant::now() < deadline, "the scan never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn scans_puts_republishes_and_deletes_what_left() {
        let dir = tempfile::tempdir().unwrap();
        transcript(dir.path(), "c--p-app", "/home/k/app", 100);
        transcript(dir.path(), "c--p-web", "/home/k/web", 10);
        let mut feed = ProjectsFeed::new("devbox", dir.path().to_path_buf());
        let mut sink = Recorder::default();
        let t0 = Instant::now();

        // The first step starts the scan; nothing is known yet.
        let step = feed.step(t0, 1000, false, &mut sink);
        assert_eq!(
            step,
            FeedStep {
                started: true,
                ..Default::default()
            }
        );
        assert!(feed.is_scanning() && feed.is_empty());
        let step = settle(&mut feed, t0, 1000, &mut sink);
        assert_eq!(
            step,
            FeedStep {
                scanned: true,
                puts: 3,
                ..Default::default()
            },
            "two projects and the activity"
        );
        assert!(!feed.is_scanning());
        let (akey, activity) = &sink.activity[0];
        assert_eq!(akey, "devbox.activity");
        assert_eq!(activity.days.len(), 1);
        assert_eq!(activity.days[0].tokens, 100 + 1 + 10 + 1);
        let (key, app) = &sink.puts[0];
        assert!(
            key.starts_with("devbox.p.") && cc_envelope::KvKey::parse(key).is_some(),
            "{key}"
        );
        assert!(!key.contains("app"), "the name never travels in clear");
        let names: Vec<&str> = sink.puts.iter().map(|(_, p)| p.name.as_str()).collect();
        assert!(
            names.contains(&"app") && names.contains(&"web"),
            "{names:?}"
        );
        assert_eq!(app.machine, "devbox");
        assert_eq!(app.sessions, 1);

        // Nothing is due a minute later; a reconnect puts everything again.
        let step = feed.step(t0 + Duration::from_secs(60), 1060, false, &mut sink);
        assert_eq!(step, FeedStep::default());
        let step = feed.step(t0 + Duration::from_secs(61), 1061, true, &mut sink);
        assert_eq!(step.puts, 3);
        // Before the TTL runs out, without a scan.
        let step = feed.step(t0 + Duration::from_secs(61 + 120), 1181, false, &mut sink);
        assert_eq!(
            step,
            FeedStep {
                puts: 3,
                ..Default::default()
            }
        );

        // A project that left the window is deleted once the next scan ends.
        std::fs::remove_dir_all(dir.path().join("c--p-web")).unwrap();
        let later = t0 + SCAN_EVERY + Duration::from_secs(61);
        assert!(feed.step(later, 1361, false, &mut sink).started);
        assert_eq!(feed.len(), 2, "kept until the scan ends");
        let step = settle(&mut feed, later, 1361, &mut sink);
        assert_eq!(
            step,
            FeedStep {
                scanned: true,
                puts: 2,
                deletes: 1,
                ..Default::default()
            }
        );
        assert_eq!(feed.len(), 1);
    }

    /// Holds [`gated_scan`] until opened.
    static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

    fn gated_scan(dir: &Path) -> Scan {
        let (open, cv) = &GATE;
        let _open = cv.wait_while(open.lock().unwrap(), |open| !*open).unwrap();
        scan_window(dir)
    }

    #[test]
    fn a_slow_scan_never_holds_up_a_step() {
        let dir = tempfile::tempdir().unwrap();
        transcript(dir.path(), "c--p-app", "/home/k/app", 100);
        let mut feed = ProjectsFeed::with_scanner("devbox", dir.path().to_path_buf(), gated_scan);
        let mut sink = Recorder::default();
        let t0 = Instant::now();

        assert!(feed.step(t0, 1000, false, &mut sink).started);
        // While it runs every step returns at once, and none starts a
        // second scan, however late.
        let late = t0 + SCAN_EVERY * 3;
        for (i, at) in [t0 + Duration::from_secs(2), late].into_iter().enumerate() {
            let begun = Instant::now();
            let step = feed.step(at, 1002 + i as i64, true, &mut sink);
            assert!(
                begun.elapsed() < Duration::from_secs(1),
                "the step waited for the scan"
            );
            assert_eq!(
                step,
                FeedStep::default(),
                "nothing known yet, nothing started"
            );
            assert!(feed.is_scanning());
        }

        {
            let (open, cv) = &GATE;
            *open.lock().unwrap() = true;
            cv.notify_all();
        }
        let step = settle(&mut feed, late, 1900, &mut sink);
        assert_eq!(
            step,
            FeedStep {
                scanned: true,
                puts: 2,
                ..Default::default()
            }
        );
        assert_eq!(sink.puts[0].1.hb, 1900, "stamped when applied");
        assert!(!feed.is_scanning());
        // The next one starts SCAN_EVERY after this one ended, not at once.
        assert!(
            !feed
                .step(late + Duration::from_secs(1), 1901, false, &mut sink)
                .started
        );
        assert!(feed.step(late + SCAN_EVERY, 2200, false, &mut sink).started);
    }

    fn failing_scan(_: &Path) -> Scan {
        panic!("injected scan failure");
    }

    #[test]
    fn a_scan_that_dies_is_retried_on_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let mut feed = ProjectsFeed::with_scanner("devbox", dir.path().to_path_buf(), failing_scan);
        let mut sink = Recorder::default();
        let t0 = Instant::now();
        assert!(feed.step(t0, 1000, false, &mut sink).started);
        let deadline = Instant::now() + Duration::from_secs(5);
        while feed.is_scanning() {
            assert!(Instant::now() < deadline, "the dead scan was never noticed");
            std::thread::sleep(Duration::from_millis(5));
            assert!(!feed.step(t0, 1000, false, &mut sink).scanned);
        }
        assert!(
            !feed
                .step(t0 + Duration::from_secs(1), 1001, false, &mut sink)
                .started,
            "not before SCAN_EVERY"
        );
        assert!(feed.step(t0 + SCAN_EVERY, 1300, false, &mut sink).started);
    }
}
