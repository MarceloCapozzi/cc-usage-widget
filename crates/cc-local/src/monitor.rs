//! The local session monitor shared by the widget (`app/src/sessions.rs`) and
//! `cc-publish`: the glue between the readers of this crate and the
//! `cc-core` state machine.
//!
//! One pass is split in two, so the slow part never holds the state that the
//! widget's commands read:
//!
//! 1. [`Sources::gather`] does the I/O: the session registry (see
//!    [`RegistrySource`]: the registry files every pass, `claude agents
//!    --json` when due), draining the spool, and reading the status line
//!    files (one per session from `cc-hook statusline`).
//! 2. [`SessionMonitor::step`] feeds the result to the
//!    [`SessionTracker`], refreshes the transcript marks (incremental reads,
//!    usually a few bytes), applies the per-session context use and runs the
//!    time-based rules.
//!
//! The callers run a pass every [`POLL_INTERVAL`]. Every path, the CLI, the
//! clock and the process probe are injectable, so the whole pass runs in
//! tests against temporary folders. What differs between the widget and the
//! publisher is an option: the CLI [`Cadence`] (the publisher backs off on an
//! idle machine).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cc_core::events::SpoolEvent;
use cc_core::session::{RegistryEntry, RemotePayload};
use cc_core::state::{SessionTracker, SessionView, Transition};

use crate::live::{self, SessionLive};
use crate::registry::SessionFile;
use crate::transcript::TranscriptReader;

/// How often the local sources are read.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How often `claude agents --json` runs when nothing else asks for it.
pub const AGENTS_REFRESH: Duration = Duration::from_secs(30);

/// With [`Cadence::IDLE_BACKOFF`], how often it runs when neither the CLI nor
/// the registry knows of any session: an idle machine should not spend 0.6 s
/// of CPU every 30 s.
pub const AGENTS_REFRESH_IDLE: Duration = Duration::from_secs(120);

/// After this many consecutive failures of `claude agents --json`, the
/// registry files alone are taken as the list of live sessions. Before that,
/// the last confirmed list is kept.
pub const AGENTS_FAILURES_BEFORE_FALLBACK: u32 = 3;

/// A registry file that appeared after the CLI's list was taken is shown at
/// once (when its process is alive) and waits this long (seconds) for the CLI
/// to list it; the CLI runs again at that point, and a session it still does
/// not list is dropped.
pub const NEW_SESSION_CONFIRM_SECS: f64 = 10.0;

/// Day-old status line files are swept on the first pass and then every this
/// many passes (about an hour at [`POLL_INTERVAL`]). Files of sessions that
/// end are deleted on the pass that sees them end.
pub const LIVE_PRUNE_EVERY: u64 = 1800;

/// Tells whether a process runs: `Some(false)` only when it is known to have
/// exited. See [`crate::process::pid_alive`].
pub type PidProbe = Arc<dyn Fn(u32) -> Option<bool> + Send + Sync>;

/// The current time, Unix seconds.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Runs `claude agents --json`.
pub type AgentsCli = Box<dyn FnMut() -> Result<Vec<RegistryEntry>, String> + Send>;

/// How often `claude agents --json` runs when nothing else asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    /// While any session is known, or the CLI is failing.
    pub refresh: Duration,
    /// While neither the CLI nor the registry knows of any session.
    pub idle_refresh: Duration,
}

impl Cadence {
    /// Every [`AGENTS_REFRESH`], idle or not (the widget).
    pub const STEADY: Cadence = Cadence {
        refresh: AGENTS_REFRESH,
        idle_refresh: AGENTS_REFRESH,
    };

    /// Every [`AGENTS_REFRESH`], or [`AGENTS_REFRESH_IDLE`] on an idle
    /// machine (the publisher).
    pub const IDLE_BACKOFF: Cadence = Cadence {
        refresh: AGENTS_REFRESH,
        idle_refresh: AGENTS_REFRESH_IDLE,
    };
}

impl Default for Cadence {
    fn default() -> Self {
        Self::STEADY
    }
}

/// What [`RegistrySource::resolve`] decided for one pass.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Resolved {
    /// The registry snapshot for the tracker, `None` to skip the registry on
    /// this pass.
    pub registry: Option<Vec<RegistryEntry>>,
    /// Why the CLI failed, if it ran and did.
    pub error: Option<String>,
    /// Sessions known to be over: their registry file went away and their
    /// process has exited. They must leave the list at once.
    pub dead: Vec<String>,
}

/// The list of live sessions, from two sources of very different cost.
///
/// `claude agents --json` is authoritative (it knows which processes are
/// alive) but costs about 0.6 s of CPU per call, measured on the development
/// machine: every 2 s that would be a quarter of a core, all day. The
/// registry files in `~/.claude/sessions` cost almost nothing, but a file
/// left by a crashed process looks like a live session.
///
/// So the files are read every pass and the CLI only runs on the first pass,
/// when the set of files changed (a session started, ended or changed id),
/// when a new session waits for confirmation, and every [`Cadence`] period.
/// Between CLI runs, the list is the sessions the last CLI run confirmed,
/// each with the status from its file when it has one (fresher) or as the
/// CLI reported it otherwise. On top of that, using the process probe:
///
/// - A stale file never reaches the list: files present when the CLI first
///   answered and not listed by it are ignored.
/// - A new session shows up at once: a file that appears later, whose
///   process is alive, is listed from the file (status included) until the
///   CLI confirms it, or for [`NEW_SESSION_CONFIRM_SECS`] if the CLI lags.
/// - A dead session leaves at once: a session whose file went away and whose
///   process has exited is dropped and reported in [`Resolved::dead`], even
///   while the CLI fails or still lists it.
/// - After [`AGENTS_FAILURES_BEFORE_FALLBACK`] CLI failures in a row the
///   list is the files alone, except files whose process is known to have
///   exited.
#[derive(Clone)]
pub struct RegistrySource {
    cadence: Cadence,
    alive: PidProbe,
    /// The last CLI snapshot, enriched from the files.
    confirmed: Vec<RegistryEntry>,
    /// Session ids of the files seen at the last CLI run.
    last_file_ids: BTreeSet<String>,
    /// Unix seconds of the last CLI run, successful or not.
    last_cli_at: Option<f64>,
    /// Consecutive CLI failures.
    failures: u32,
    /// The CLI has answered once: files that appear after this are new.
    baseline: bool,
    /// Session ids of the files seen on the previous pass.
    prev_files: BTreeSet<String>,
    /// New files the CLI has not listed yet, with the time they appeared.
    pending: BTreeMap<String, f64>,
    /// Session ids that have had a registry file while being tracked here.
    had_file: BTreeSet<String>,
    /// Ids and pids of the last snapshot handed out.
    last_emitted: Vec<(String, Option<u32>)>,
}

impl std::fmt::Debug for RegistrySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrySource")
            .field("cadence", &self.cadence)
            .field("confirmed", &self.confirmed.len())
            .field("last_cli_at", &self.last_cli_at)
            .field("failures", &self.failures)
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl Default for RegistrySource {
    fn default() -> Self {
        Self::new(Cadence::default())
    }
}

impl RegistrySource {
    /// A source with this cadence that asks the OS whether processes run.
    pub fn new(cadence: Cadence) -> Self {
        Self {
            cadence,
            alive: Arc::new(crate::process::pid_alive),
            confirmed: Vec::new(),
            last_file_ids: BTreeSet::new(),
            last_cli_at: None,
            failures: 0,
            baseline: false,
            prev_files: BTreeSet::new(),
            pending: BTreeMap::new(),
            had_file: BTreeSet::new(),
            last_emitted: Vec::new(),
        }
    }

    /// Replaces the process probe (tests).
    pub fn with_probe(
        mut self,
        probe: impl Fn(u32) -> Option<bool> + Send + Sync + 'static,
    ) -> Self {
        self.alive = Arc::new(probe);
        self
    }

    /// The CLI cadence.
    pub fn cadence(&self) -> Cadence {
        self.cadence
    }

    /// Consecutive CLI failures so far.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    fn alive(&self, pid: Option<u32>) -> Option<bool> {
        pid.and_then(|p| (self.alive)(p))
    }

    /// Whether the CLI should run on this pass.
    pub fn cli_due(&self, files: &[SessionFile], now: f64) -> bool {
        let Some(last) = self.last_cli_at else {
            return true;
        };
        if file_ids(files) != self.last_file_ids {
            return true;
        }
        // A new session waiting for the CLI: ask again once it had its chance.
        let confirm_due = self.pending.values().any(|&first| {
            let at = first + NEW_SESSION_CONFIRM_SECS;
            last < at && now >= at
        });
        if confirm_due {
            return true;
        }
        let idle = files.is_empty() && self.confirmed.is_empty() && self.failures == 0;
        let every = if idle {
            self.cadence.idle_refresh
        } else {
            self.cadence.refresh
        };
        now - last >= every.as_secs_f64()
    }

    /// Combines this pass's files with the CLI result, when the CLI ran. See
    /// the type docs for the rules.
    pub fn resolve(
        &mut self,
        files: &[SessionFile],
        cli: Option<Result<Vec<RegistryEntry>, String>>,
        now: f64,
    ) -> Resolved {
        let ids_now = file_ids(files);

        // New files since the previous pass wait for the CLI to confirm them.
        self.pending.retain(|id, _| ids_now.contains(id));
        if self.baseline {
            for id in ids_now.difference(&self.prev_files) {
                if !self.confirmed.iter().any(|c| &c.session_id == id) {
                    self.pending.entry(id.clone()).or_insert(now);
                }
            }
        }
        self.prev_files.clone_from(&ids_now);
        self.had_file.extend(ids_now.iter().cloned());

        let mut error = None;
        let mut fresh = false;
        if let Some(result) = cli {
            self.last_cli_at = Some(now);
            self.last_file_ids.clone_from(&ids_now);
            match result {
                Ok(mut entries) => {
                    self.failures = 0;
                    crate::registry::apply(&mut entries, files);
                    let listed: HashSet<&str> =
                        entries.iter().map(|e| e.session_id.as_str()).collect();
                    // Listed: confirmed. Not listed: still provisional only
                    // while the CLI may be lagging behind the new file.
                    self.pending.retain(|id, first| {
                        !listed.contains(id.as_str()) && now - *first < NEW_SESSION_CONFIRM_SECS
                    });
                    self.confirmed = entries;
                    self.baseline = true;
                    fresh = true;
                }
                Err(e) => {
                    self.failures = self.failures.saturating_add(1);
                    error = Some(e);
                }
            }
        }

        // Dead: had a file, the file is gone, and the process has exited.
        let mut dead: Vec<String> = Vec::new();
        let candidates = self
            .confirmed
            .iter()
            .map(|c| (c.session_id.as_str(), c.pid))
            .chain(
                self.last_emitted
                    .iter()
                    .map(|(id, pid)| (id.as_str(), *pid)),
            );
        for (id, pid) in candidates {
            if self.had_file.contains(id)
                && !ids_now.contains(id)
                && !dead.iter().any(|d| d == id)
                && self.alive(pid) == Some(false)
            {
                dead.push(id.to_string());
            }
        }
        self.confirmed.retain(|c| !dead.contains(&c.session_id));

        let registry = if self.failures >= AGENTS_FAILURES_BEFORE_FALLBACK {
            // The CLI is missing or broken: the files are all there is, but
            // not those of a process known to have exited (a crash leaves
            // its file behind). A pid that cannot be probed stays listed.
            Some(
                files
                    .iter()
                    .filter(|f| self.alive(f.pid) != Some(false))
                    .map(SessionFile::to_entry)
                    .collect::<Vec<_>>(),
            )
        } else if self.last_cli_at.is_none() || (self.confirmed.is_empty() && error.is_some()) {
            // Nothing confirmed yet: better no registry than a guessed one.
            None
        } else {
            let mut entries: Vec<RegistryEntry> = if fresh {
                self.confirmed.clone()
            } else {
                self.confirmed
                    .iter()
                    .map(|c| merge_file(c, files))
                    .collect()
            };
            // New sessions the CLI has not listed yet, from their files.
            for file in files {
                if self.pending.contains_key(&file.session_id)
                    && !entries.iter().any(|e| e.session_id == file.session_id)
                    && self.alive(file.pid) == Some(true)
                {
                    entries.push(file.to_entry());
                }
            }
            Some(entries)
        };

        if let Some(entries) = &registry {
            self.last_emitted = entries
                .iter()
                .map(|e| (e.session_id.clone(), e.pid))
                .collect();
        }
        let keep: HashSet<&str> = self
            .confirmed
            .iter()
            .map(|c| c.session_id.as_str())
            .chain(self.last_emitted.iter().map(|(id, _)| id.as_str()))
            .chain(ids_now.iter().map(String::as_str))
            .collect();
        self.had_file.retain(|id| keep.contains(id.as_str()));

        Resolved {
            registry,
            error,
            dead,
        }
    }
}

/// A confirmed entry refreshed from its file, when it has one.
fn merge_file(confirmed: &RegistryEntry, files: &[SessionFile]) -> RegistryEntry {
    match files.iter().find(|f| f.session_id == confirmed.session_id) {
        Some(file) => {
            let mut e = file.to_entry();
            // A file without a status keeps the one the CLI reported.
            if file.status.is_none() {
                e.status.clone_from(&confirmed.status);
            }
            // Fields only the CLI reports survive from its snapshot.
            e.background_id.clone_from(&confirmed.background_id);
            if e.status == confirmed.status {
                e.waiting_for.clone_from(&confirmed.waiting_for);
            }
            fill_from(&mut e, confirmed);
            e
        }
        None => confirmed.clone(),
    }
}

fn file_ids(files: &[SessionFile]) -> BTreeSet<String> {
    files.iter().map(|f| f.session_id.clone()).collect()
}

/// Fills the empty fields of `entry` from `other`.
fn fill_from(entry: &mut RegistryEntry, other: &RegistryEntry) {
    let fill = |slot: &mut Option<String>, value: &Option<String>| {
        if slot.is_none() {
            slot.clone_from(value);
        }
    };
    fill(&mut entry.cwd, &other.cwd);
    fill(&mut entry.name, &other.name);
    fill(&mut entry.kind, &other.kind);
    fill(&mut entry.entrypoint, &other.entrypoint);
    if entry.pid.is_none() {
        entry.pid = other.pid;
    }
    if entry.started_at_ms.is_none() {
        entry.started_at_ms = other.started_at_ms;
    }
}

/// Where [`Sources`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The registry files, `~/.claude/sessions`.
    pub sessions_dir: PathBuf,
    /// The cc-hook spool, drained on every pass.
    pub spool_dir: PathBuf,
    /// The per-session status line files from `cc-hook statusline`.
    pub live_dir: PathBuf,
}

impl Paths {
    /// The real locations: [`crate::registry::sessions_dir`],
    /// [`cc_core::paths::spool_dir`] and [`cc_core::paths::live_dir`].
    pub fn real() -> Self {
        Self {
            sessions_dir: crate::registry::sessions_dir(),
            spool_dir: cc_core::paths::spool_dir(),
            live_dir: cc_core::paths::live_dir(),
        }
    }
}

/// What one pass read from disk and from the CLI.
#[derive(Debug, Default, Clone)]
pub struct Gathered {
    /// The registry snapshot, or `None` when it is not available this pass.
    pub registry: Option<Vec<RegistryEntry>>,
    /// Unix seconds when the registry snapshot was requested.
    pub registry_at: f64,
    /// Sessions whose file went away and whose process has exited.
    pub dead: Vec<String>,
    /// Spool events, oldest first.
    pub events: Vec<SpoolEvent>,
    /// Per-session status line files from `cc-hook statusline`, fresh ones only.
    pub live_sessions: Vec<SessionLive>,
    /// Why the CLI snapshot failed, if it did (for diagnostics only).
    pub agents_error: Option<String>,
}

/// The I/O half of a pass: the registry source, the CLI, the spool and the
/// status line files. Blocking (the CLI may take a few seconds), so the
/// widget runs it on a blocking thread, outside the [`SessionMonitor`] lock.
pub struct Sources {
    paths: Paths,
    registry: RegistrySource,
    agents: AgentsCli,
    clock: Clock,
    spool: crate::spool::Drainer,
}

impl std::fmt::Debug for Sources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sources")
            .field("paths", &self.paths)
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl Sources {
    /// Sources reading `paths`, running the real CLI with this cadence, on
    /// the system clock and the OS process probe.
    pub fn new(paths: Paths, cadence: Cadence) -> Self {
        Self {
            paths,
            registry: RegistrySource::new(cadence),
            agents: Box::new(|| crate::agents::snapshot().map_err(|e| e.to_string())),
            clock: Arc::new(crate::now_secs),
            spool: crate::spool::Drainer::new(),
        }
    }

    /// Replaces `claude agents --json` (tests).
    pub fn with_agents(
        mut self,
        cli: impl FnMut() -> Result<Vec<RegistryEntry>, String> + Send + 'static,
    ) -> Self {
        self.agents = Box::new(cli);
        self
    }

    /// Replaces the clock (tests).
    pub fn with_clock(mut self, clock: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Replaces the process probe (tests).
    pub fn with_probe(
        mut self,
        probe: impl Fn(u32) -> Option<bool> + Send + Sync + 'static,
    ) -> Self {
        self.registry.alive = Arc::new(probe);
        self
    }

    /// Where it reads.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The registry state carried between passes.
    pub fn registry(&self) -> &RegistrySource {
        &self.registry
    }

    /// The current time on this source's clock.
    pub fn now(&self) -> f64 {
        (self.clock)()
    }

    /// Reads every local source once.
    pub fn gather(&mut self) -> Gathered {
        let registry_at = self.now();
        let files = crate::registry::read_dir(&self.paths.sessions_dir);
        let cli = self
            .registry
            .cli_due(&files, registry_at)
            .then(|| (self.agents)());
        let resolved = self.registry.resolve(&files, cli, registry_at);
        let events = self.spool.drain(&self.paths.spool_dir);
        let live_sessions = live::read_sessions_from(&self.paths.live_dir, self.now());
        Gathered {
            registry: resolved.registry,
            registry_at,
            dead: resolved.dead,
            events,
            live_sessions,
            agents_error: resolved.error,
        }
    }
}

/// What a pass changed.
#[derive(Debug, Default)]
pub struct StepOutcome {
    /// The rows differ from the previous pass.
    pub changed: bool,
    /// State changes since the previous pass, oldest first.
    pub transitions: Vec<Transition>,
}

/// The state half of a pass: the tracker plus the caches around it.
pub struct SessionMonitor {
    tracker: SessionTracker,
    transcripts: TranscriptReader,
    /// `transcript_path` from hook events, per session.
    transcript_paths: HashMap<String, PathBuf>,
    last_views: Vec<SessionView>,
    passes: u64,
    /// Where the per-session status line files live, for pruning them. `None`
    /// (tests) prunes nothing.
    live_dir: Option<PathBuf>,
    /// Sessions taken off the list by [`SessionMonitor::clear`] that may
    /// still come back; once the tracker forgets one, its status line file
    /// is pruned like that of a session that ended.
    cleared: Vec<String>,
}

impl Default for SessionMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionMonitor {
    /// A monitor reading transcripts from the default projects folder and
    /// pruning the real status line folder.
    pub fn new() -> Self {
        Self::with_reader(TranscriptReader::new()).with_live_dir(cc_core::paths::live_dir())
    }

    /// A monitor with a custom transcript reader (tests). It prunes no status
    /// line files unless [`Self::with_live_dir`] says where.
    pub fn with_reader(transcripts: TranscriptReader) -> Self {
        Self {
            tracker: SessionTracker::new(),
            transcripts,
            transcript_paths: HashMap::new(),
            last_views: Vec::new(),
            passes: 0,
            live_dir: None,
            cleared: Vec::new(),
        }
    }

    /// Prunes the per-session status line files in `dir`.
    pub fn with_live_dir(mut self, dir: PathBuf) -> Self {
        self.live_dir = Some(dir);
        self
    }

    /// How many passes have run.
    pub fn passes(&self) -> u64 {
        self.passes
    }

    /// The tracker, read-only.
    pub fn tracker(&self) -> &SessionTracker {
        &self.tracker
    }

    /// The rows from the last pass, in panel order.
    pub fn views(&self) -> &[SessionView] {
        &self.last_views
    }

    /// One row by session id, from the last pass.
    pub fn view(&self, session_id: &str) -> Option<&SessionView> {
        self.last_views.iter().find(|v| v.id == session_id)
    }

    /// Takes every row off the list (the widget's "clear" button) and
    /// returns how many there were. See [`SessionTracker::clear`]: a session
    /// that is still alive comes back on the next pass, because the registry
    /// still lists it, with its state and hook flag; its current tool and
    /// subagents come back with its next hook event. A session that ended
    /// does not come back.
    pub fn clear(&mut self, now: f64) -> usize {
        let n = self.tracker.clear(now);
        self.cleared
            .extend(self.last_views.iter().map(|v| v.id.clone()));
        // The rows are gone now, not at the next pass: for what the widget
        // shows and for what the next pass compares with.
        self.last_views = self.tracker.sessions();
        n
    }

    /// The remote payload of every tracked session, in panel order (see
    /// [`SessionTracker::to_remote_payload`] for what it carries).
    pub fn payloads(&self, alias: &str, now: f64) -> Vec<RemotePayload> {
        self.tracker
            .sessions()
            .iter()
            .filter_map(|v| self.tracker.to_remote_payload(&v.id, alias, now))
            .collect()
    }

    /// Applies one pass. The registry goes first, with the time it was taken,
    /// then the sessions known dead, then the spool events, then the
    /// time-based rules at `now`, so the time the tracker sees only moves
    /// forward.
    pub fn step(&mut self, input: Gathered, now: f64) -> StepOutcome {
        self.passes += 1;

        if let Some(entries) = &input.registry {
            self.tracker
                .apply_registry(entries, input.registry_at.min(now));
        }
        for id in &input.dead {
            // As if its SessionEnd had fired now: removed at once, without the
            // registry's grace period, and stragglers from the spool cannot
            // bring it back.
            self.tracker
                .apply_event(&SpoolEvent::new("SessionEnd", id.as_str(), now), now);
        }
        for event in &input.events {
            if let Some(path) = event.transcript_path.as_deref().filter(|p| !p.is_empty()) {
                self.transcript_paths
                    .insert(event.session_id.clone(), PathBuf::from(path));
            }
            self.tracker.apply_event(event, now);
        }
        self.tracker.tick(now);

        self.refresh_marks();
        for live in &input.live_sessions {
            self.apply_session_live(live);
        }

        let views = self.tracker.sessions();
        let changed = views != self.last_views;
        let mut ended: Vec<String> = self
            .last_views
            .iter()
            .filter(|old| !views.iter().any(|v| v.id == old.id))
            .map(|old| old.id.clone())
            .collect();
        ended.extend(
            input
                .events
                .iter()
                .filter(|e| e.event == "SessionEnd")
                .map(|e| e.session_id.clone()),
        );
        ended.extend(input.dead.iter().cloned());
        // Cleared sessions that came back are done with; the ones the
        // tracker forgot are not coming back, so their files go.
        let cleared = std::mem::take(&mut self.cleared);
        let (waiting, forgotten): (Vec<String>, Vec<String>) = cleared
            .into_iter()
            .filter(|id| !views.iter().any(|v| &v.id == id))
            .partition(|id| self.tracker.is_cleared(id));
        self.cleared = waiting;
        ended.extend(forgotten);
        self.last_views = views;
        self.prune_live(&ended, now);
        StepOutcome {
            changed,
            transitions: self.tracker.take_transitions(),
        }
    }

    /// Title, custom title, colour and model from each session's transcript.
    /// Reads only what was appended since the last pass.
    fn refresh_marks(&mut self) {
        let sessions = self.tracker.sessions();
        let live: HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        self.transcript_paths
            .retain(|id, _| live.contains(id.as_str()));
        self.transcripts.retain(|id| live.contains(id));

        for s in &sessions {
            let marks = match (self.transcript_paths.get(&s.id), s.cwd.as_deref()) {
                (Some(path), cwd) => match self.transcripts.marks_at(&s.id, path) {
                    Ok(marks) => marks,
                    Err(_) => match cwd {
                        Some(cwd) => self.transcripts.marks(&s.id, cwd),
                        None => continue,
                    },
                },
                (None, Some(cwd)) => self.transcripts.marks(&s.id, cwd),
                (None, None) => continue,
            };
            self.tracker.apply_marks(&s.id, &marks);
        }
    }

    /// Context window use and model of one session, from its own status line
    /// file. Claude Code computed the percentage itself, so it is exact (to
    /// the integer the row shows). The model id here is the one in use now
    /// (it follows `/model`), so it replaces the one from `SessionStart`. A
    /// `null` percentage, early in a session, leaves the last one in place.
    fn apply_session_live(&mut self, live: &SessionLive) {
        if self.tracker.session(&live.session_id).is_none() {
            return;
        }
        self.tracker
            .set_context(&live.session_id, live.percent_u8(), live.model());
    }

    /// Deletes the status line files of sessions that ended on this pass,
    /// and, on the first pass and every [`LIVE_PRUNE_EVERY`], day-old ones.
    fn prune_live(&self, ended: &[String], now: f64) {
        let Some(dir) = &self.live_dir else {
            return;
        };
        if ended.is_empty() && self.passes % LIVE_PRUNE_EVERY != 1 {
            return;
        }
        live::prune(dir, ended, now);
    }
}

#[cfg(test)]
mod tests;
