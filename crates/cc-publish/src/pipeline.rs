//! The publishing pipeline, shared by `cc-publish run` and by the cc-usage
//! app when its role is Publisher.
//!
//! Three pieces, which never wait for each other:
//!
//! - **[`Link`]** is what both halves share: the [`Outbox`] (newest per key
//!   wins, so it is bounded and never blocks), the [`PublishState`] the
//!   status commands and the app's footer read, and the resync flag the
//!   network sets after a reconnect. It writes the state file, when it has
//!   one, and calls its observer, when it has one, after every change worth
//!   reporting.
//! - **[`Gatherer`]** is the gathering side of one pass: given the current
//!   session payloads (from a [`cc_local::monitor::SessionMonitor`], which
//!   the caller runs: `cc-publish` on its own thread, the app in its own
//!   session pass, so the cc-hook spool is drained once), it lets the
//!   [`Planner`] record the session and host decisions and the
//!   [`ProjectsFeed`] the project and activity ones in the outbox. The
//!   transcript scan behind the latter runs on a thread of its own, so a
//!   pass never waits for it.
//! - **[`serve`]** is the network side (one NATS connection): it connects,
//!   binds the bucket and [`flush`]es the outbox whenever something is
//!   pending. Connect and bind failures are retried with capped backoff;
//!   after the first connect async-nats reconnects on its own. If writes
//!   keep failing for [`REBUILD_AFTER_SECS`] the connection is closed and
//!   built again, which also retries the other transport. Network errors are
//!   logged and retried, never fatal. When `shutdown` resolves it makes one
//!   last bounded flush and closes the connection. It seals with the
//!   [`Keys`] it is given and [`reload`]s them every
//!   [`RECHECK_EVERY`], or at once when
//!   someone else did: new keys are used from the next flush (with a
//!   resync, so every value is sealed with them soon). New creds are tried
//!   on a second connection, make before break: it replaces the one in use
//!   only once it is up with the bucket bound; until then, and when it
//!   fails (retried after [`crate::reload::trial_delay`]), the connection
//!   in use goes on with the credentials it holds, and a rebuild after
//!   failing writes becomes that trial instead of reading the untried file.
//!   Only when the account has no free connection for a second one
//!   ([`is_connection_limit`], which proves the new creds authenticated)
//!   does it close the current connection first.
//!
//! Nothing here reads a config file or a signal: the caller picks the
//! alias, the servers, the key and creds files and when to stop.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use cc_core::session::RemotePayload;
use cc_remote::{Client, ConnectionEvent, Publisher};
use tokio::sync::broadcast::error::RecvError;

use crate::outbox::{flush, Outbox};
use crate::planner::{Plan, Planner};
use crate::projects::{FeedStep, ProjectsFeed};
use crate::reload::{Change, CredsTrial, Keys, Reload, TrialStep, RECHECK_EVERY};
use crate::state::{PublishState, WRITE_EVERY_SECS};
use crate::util::now_secs;

/// Writes failing continuously for this long (seconds) rebuild the connection.
pub const REBUILD_AFTER_SECS: f64 = 180.0;

/// Upper bound for the delay between connect attempts and between retries.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How long the final flush on shutdown may take.
const FINAL_FLUSH_TIMEOUT: Duration = Duration::from_secs(3);

/// While failing, an error is logged at most this often (seconds).
const ERROR_LOG_EVERY_SECS: f64 = 60.0;

/// A summary line is logged this often (seconds).
const SUMMARY_EVERY_SECS: f64 = 15.0 * 60.0;

/// Called after the link's state changed or was written.
pub type Observer = Box<dyn Fn() + Send + Sync>;

/// What the gathering side and the network side share. See the module docs.
pub struct Link {
    /// Pending operations.
    pub outbox: Outbox,
    state: Mutex<PublishState>,
    /// Set by the network side after a reconnect: re-put everything.
    resync: AtomicBool,
    /// Where [`Link::write_state`] saves the state; `None` keeps it in memory.
    state_path: Option<PathBuf>,
    observer: Option<Observer>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("pending", &self.outbox.len())
            .field("state_path", &self.state_path)
            .finish_non_exhaustive()
    }
}

impl Link {
    /// A link starting from `state`, saved to `state_path` (when given) by
    /// [`Link::write_state`], with `observer` (when given) called after each
    /// write.
    pub fn new(
        state: PublishState,
        state_path: Option<PathBuf>,
        observer: Option<Observer>,
    ) -> Arc<Self> {
        Arc::new(Self {
            outbox: Outbox::new(),
            state: Mutex::new(state),
            resync: AtomicBool::new(false),
            state_path,
            observer,
        })
    }

    /// The state, locked. Held briefly, never across an `.await`.
    pub fn state(&self) -> MutexGuard<'_, PublishState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A copy of the state, with the pending count filled in.
    pub fn snapshot(&self) -> PublishState {
        let mut s = self.state().clone();
        s.pending = self.outbox.len();
        s
    }

    /// Stamps the state, saves it (when the link has a file) and tells the
    /// observer.
    pub fn write_state(&self) {
        let snapshot = {
            let mut s = self.state();
            s.updated_at = now_secs();
            s.pending = self.outbox.len();
            s.clone()
        };
        if let Some(path) = &self.state_path {
            if let Err(err) = snapshot.save(path) {
                tracing::debug!("cannot write {}: {err}", path.display());
            }
        }
        if let Some(observer) = &self.observer {
            observer();
        }
    }

    /// Asks the next gathering pass to put everything again.
    pub fn request_resync(&self) {
        self.resync.store(true, Ordering::SeqCst);
    }

    /// Whether a resync was asked for since the last call (and resets it).
    pub fn take_resync(&self) -> bool {
        self.resync.swap(false, Ordering::SeqCst)
    }
}

/// What one [`Gatherer::step`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// The session and host decisions.
    pub plan: Plan,
    /// The project and activity decisions.
    pub feed: FeedStep,
    /// Keys of the previous run taken over on this pass.
    pub adopted: usize,
    /// Whether this pass followed a reconnect.
    pub resync: bool,
}

/// The gathering side of the pipeline. See the module docs.
#[derive(Debug)]
pub struct Gatherer {
    alias: String,
    planner: Planner,
    projects: ProjectsFeed,
    /// Session keys the previous run published, until the first registry
    /// snapshot lets the planner take them over.
    inherited: Vec<String>,
}

impl Gatherer {
    /// A gatherer for `alias` over the transcripts under `projects_dir`
    /// ([`cc_local::projects::projects_dir`] in a real run). `inherited`
    /// are the session keys the previous run published (see
    /// [`Planner::adopt`]).
    pub fn new(alias: &str, projects_dir: PathBuf, inherited: Vec<String>) -> Self {
        Self {
            alias: alias.to_string(),
            planner: Planner::new(alias),
            projects: ProjectsFeed::new(alias, projects_dir),
            inherited,
        }
    }

    /// The machine alias.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// One pass over `payloads` (as produced by
    /// [`cc_local::monitor::SessionMonitor::payloads`] for this alias) at
    /// Unix time `now`. `has_registry` says whether the monitor got a
    /// registry snapshot on this pass: with the first one it knows which
    /// sessions run, so the previous run's keys of any other session can go.
    /// Records the decisions in `link`'s outbox and the counters in its
    /// state.
    pub fn step(
        &mut self,
        payloads: &[RemotePayload],
        has_registry: bool,
        now: f64,
        link: &Link,
    ) -> Pass {
        let resync = link.take_resync();
        if resync {
            self.planner.force_resync();
        }
        let adopted = if has_registry && !self.inherited.is_empty() {
            let n = self.inherited.len();
            self.planner.adopt(std::mem::take(&mut self.inherited));
            n
        } else {
            0
        };
        let plan = self.planner.plan(payloads, now, &mut &link.outbox);
        let feed = self.projects.step(
            Instant::now(),
            now.floor() as i64,
            resync,
            &mut &link.outbox,
        );
        {
            let mut s = link.state();
            s.sessions = payloads.len();
            s.projects = self.projects.len();
            s.keys = published_keys(&self.planner, &link.outbox, &self.inherited);
        }
        Pass {
            plan,
            feed,
            adopted,
            resync,
        }
    }

    /// Projects known from the last transcript scan.
    pub fn projects(&self) -> usize {
        self.projects.len()
    }
}

/// The keys to persist as published: what the planner believes is in the
/// bucket, deletes not sent yet, and keys of the previous run not taken over
/// yet. Sorted, without duplicates.
pub fn published_keys(planner: &Planner, outbox: &Outbox, inherited: &[String]) -> Vec<String> {
    let mut keys = planner.keys();
    keys.extend(outbox.pending_deletes());
    keys.extend(inherited.iter().cloned());
    keys.sort();
    keys.dedup();
    keys
}

/// Records what `keys` has in use in `link`'s state.
pub fn record_keys(keys: &Keys, link: &Link) {
    link.state().in_use = Some(keys.in_use());
}

/// Looks at the key and creds files of `keys` again ([`Keys::recheck`],
/// `force` loading the key files even when they look untouched), logs what
/// changed, records it in `link`'s state and writes it. New keys ask for a
/// resync, so every value is sealed with them soon; new creds that pass
/// the checks wait for the trial connection [`serve`] opens next. Safe to
/// call from any thread.
pub fn reload(keys: &Keys, link: &Link, force: bool) -> Reload {
    let reload = keys.recheck(force);
    match &reload.keys {
        Change::Applied => {
            tracing::info!(
                "key files changed; now sealing {}",
                keys.sealer().describe()
            );
            link.request_resync();
        }
        Change::Rejected(why) => {
            tracing::warn!("key files changed but cannot be used, keeping the keys in use: {why}")
        }
        Change::Unchanged => {}
    }
    match &reload.creds {
        Change::Applied => tracing::info!(
            "the credentials file changed; trying it on a new connection (the current one stays until that is up)"
        ),
        Change::Rejected(why) => {
            tracing::warn!("the credentials file changed but cannot be used, keeping the connection: {why}")
        }
        Change::Unchanged => {}
    }
    if reload.changed() {
        record_keys(keys, link);
        link.write_state();
    }
    reload
}

/// Delay before retry number `attempt` (1-based): at least 1 s, doubling,
/// capped at [`MAX_BACKOFF`], with cc-remote's jitter.
pub fn backoff(attempt: u32) -> Duration {
    cc_remote::reconnect_delay(attempt as usize + 1)
        .max(Duration::from_secs(1))
        .min(MAX_BACKOFF)
}

async fn connect_and_bind(remote: &cc_remote::Config) -> Result<(Client, Publisher), String> {
    let client = cc_remote::connect(remote.clone())
        .await
        .map_err(|e| e.to_string())?;
    match Publisher::bind(&client, &remote.bucket).await {
        Ok(publisher) => Ok((client, publisher)),
        Err(err) => {
            client.close().await;
            Err(err.to_string())
        }
    }
}

/// A connection being opened next to the one in use (a creds trial). It
/// runs on a task of its own, so the handshake goes on while the loop
/// flushes, and is aborted when dropped (on shutdown or a rebuild).
struct Opening(tokio::task::JoinHandle<Result<(Client, Publisher), String>>);

impl Opening {
    fn start(remote: &cc_remote::Config) -> Self {
        let remote = remote.clone();
        Opening(tokio::spawn(async move { connect_and_bind(&remote).await }))
    }
}

impl Future for Opening {
    type Output = Result<(Client, Publisher), String>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(|joined| {
            joined.unwrap_or_else(|err| Err(format!("the connect task failed: {err}")))
        })
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What the future in `slot` resolves to; never resolves while it is empty.
async fn next_of<F: Future + Unpin>(slot: &mut Option<F>) -> F::Output {
    match slot {
        Some(fut) => fut.await,
        None => std::future::pending().await,
    }
}

/// Whether a connect error is the account's connection limit. The server
/// checks it only after the credentials authenticated, so creds refused
/// with it are good: there is just no room for a second connection.
pub fn is_connection_limit(err: &str) -> bool {
    err.to_ascii_lowercase()
        .contains("maximum account active connections")
}

/// Runs `fut` to completion, calling `tick` every `every` while it waits
/// (first after `every`).
pub(crate) async fn ticking<F: Future>(
    fut: F,
    every: Duration,
    mut tick: impl FnMut(),
) -> F::Output {
    tokio::pin!(fut);
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            out = &mut fut => return out,
            _ = interval.tick() => tick(),
        }
    }
}

enum Leave {
    Shutdown,
    Rebuild,
}

/// What [`serve`] does when a trial connection with new creds resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialOutcome {
    /// It is up with the bucket bound: it replaces the connection in use.
    Swap,
    /// The account has no room for a second connection, but the creds
    /// authenticated: close the connection in use, then connect again.
    SwitchClosingFirst,
    /// It failed: the connection in use stays, the creds are tried later.
    Keep,
}

/// The [`TrialOutcome`] of a trial connection that resolved with `error`
/// (`None` when it is up).
pub fn trial_outcome(error: Option<&str>) -> TrialOutcome {
    match error {
        None => TrialOutcome::Swap,
        Some(err) if is_connection_limit(err) => TrialOutcome::SwitchClosingFirst,
        Some(_) => TrialOutcome::Keep,
    }
}

/// Failure bookkeeping for the current connection.
#[derive(Default)]
struct Failing {
    since: Option<f64>,
    count: u32,
    logged_at: Option<f64>,
}

/// The network side: connects as `remote` says, binds the bucket and sends
/// what `link`'s outbox holds, sealed by `keys` for `alias`, until
/// `shutdown` resolves. `remote.creds_path` should be the creds file of
/// `keys`. See the module docs.
pub async fn serve(
    remote: &cc_remote::Config,
    keys: &Keys,
    alias: &str,
    link: &Link,
    shutdown: impl Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    let mut attempt: u32 = 0;
    let mut last_state_write = 0.0;
    let mut last_summary = (now_secs(), 0u64);
    record_keys(keys, link);

    // A connect attempt can take 24 s (two transports, 12 s each) and the
    // backoff after it 30 s: keep the state file fresh meanwhile, or it goes
    // stale (STALE_AFTER_SECS) and `status`, `clear` and a second `run` take
    // this live publisher for a dead one.
    let state_every = Duration::from_secs_f64(WRITE_EVERY_SECS);
    let keep_fresh = || link.write_state();

    'outer: loop {
        attempt += 1;
        // The connect reads the creds file: what it reads is what is in use.
        // With no connection to keep, it is the trial of a new file too.
        reload(keys, link, false);
        keys.connecting();
        record_keys(keys, link);
        let connected = tokio::select! {
            r = ticking(connect_and_bind(remote), state_every, keep_fresh) => r,
            _ = &mut shutdown => break 'outer,
        };
        let (mut client, mut publisher) = match connected {
            Ok(pair) => pair,
            Err(err) => {
                let delay = backoff(attempt);
                // Long outages: the first attempts, then every tenth.
                if attempt <= 5 || attempt.is_multiple_of(10) {
                    tracing::warn!(
                        "connect failed (attempt {attempt}), retrying in {delay:?}: {err}"
                    );
                }
                {
                    let mut s = link.state();
                    s.connected = false;
                    s.last_error = Some(err);
                    s.last_error_at = Some(now_secs());
                }
                link.write_state();
                tokio::select! {
                    _ = ticking(tokio::time::sleep(delay), state_every, keep_fresh) => continue 'outer,
                    // New creds or keys (an import in the app): try them now.
                    _ = keys.changed() => continue 'outer,
                    _ = &mut shutdown => break 'outer,
                }
            }
        };
        attempt = 0;
        let reconnect = {
            let mut s = link.state();
            s.connected = true;
            s.server = Some(client.server().to_string());
            s.connects += 1;
            s.connects > 1
        };
        if reconnect {
            // The bucket may have expired our keys while we were away.
            link.request_resync();
        }
        tracing::info!(server = %client.server(), "connected, bucket bound");
        link.write_state();

        let mut events = client.events();
        let mut failing = Failing::default();
        let mut next_try = Instant::now();
        let mut next_recheck = Instant::now() + RECHECK_EVERY;
        let mut disconnected = false;
        // A trial connection with new creds, and which file it is for.
        let mut opening: Option<Opening> = None;
        let mut trying: Option<CredsTrial> = None;
        let leave = loop {
            tokio::select! {
                _ = &mut shutdown => break Leave::Shutdown,
                _ = link.outbox.notified(), if failing.since.is_none() => {}
                _ = keys.changed() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                opened = next_of(&mut opening) => {
                    opening = None;
                    let Some(trial) = trying.take() else { continue };
                    let error = opened.as_ref().err().map(String::as_str);
                    match (trial_outcome(error), opened) {
                        (_, Ok((fresh, bound))) => {
                            keys.trial_succeeded(&trial);
                            let old = std::mem::replace(&mut client, fresh);
                            publisher = bound;
                            events = client.events();
                            if disconnected {
                                // The bucket may have expired our keys meanwhile.
                                link.request_resync();
                            }
                            disconnected = false;
                            failing = Failing::default();
                            next_try = Instant::now();
                            {
                                let mut s = link.state();
                                s.connected = true;
                                s.server = Some(client.server().to_string());
                                s.connects += 1;
                            }
                            tracing::info!(
                                server = %client.server(),
                                "connected with the new credentials as {}, bucket bound; closing the previous connection",
                                trial.user
                            );
                            old.close().await;
                            record_keys(keys, link);
                            link.write_state();
                        }
                        (TrialOutcome::SwitchClosingFirst, Err(err)) => {
                            tracing::warn!(
                                "the new credentials authenticated but the account has no free connection for a \
                                 second one; closing the current connection to switch: {err}"
                            );
                            break Leave::Rebuild;
                        }
                        (_, Err(err)) => {
                            match keys.trial_failed(&trial, &err, Instant::now()) {
                                Some(delay) => tracing::warn!(
                                    "the new credentials did not connect, keeping the current connection; \
                                     trying them again in {delay:?}: {err}"
                                ),
                                None => tracing::debug!("credentials replaced since did not connect: {err}"),
                            }
                            record_keys(keys, link);
                            link.write_state();
                        }
                    }
                }
                ev = events.recv() => match ev {
                    Ok(ConnectionEvent::Connected) => {
                        if disconnected {
                            tracing::info!("reconnected");
                            link.request_resync();
                            next_try = Instant::now();
                        }
                        disconnected = false;
                        link.state().connected = true;
                    }
                    Ok(ConnectionEvent::Disconnected) => {
                        tracing::warn!("connection lost; async-nats is reconnecting");
                        disconnected = true;
                        link.state().connected = false;
                    }
                    Ok(ConnectionEvent::Closed) | Err(RecvError::Closed) => break Leave::Rebuild,
                    Ok(ev @ ConnectionEvent::ServerError(_)) if ev.is_permission_violation() => {
                        tracing::error!("permission denied by the server: {ev:?}");
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                },
            }

            if Instant::now() >= next_recheck {
                reload(keys, link, false);
                next_recheck = Instant::now() + RECHECK_EVERY;
            }
            // One trial at a time, when due: the connection in use stays
            // until the new one is up.
            if opening.is_none() {
                match keys.begin_trial(Instant::now()) {
                    TrialStep::Wait => {}
                    TrialStep::Rejected(why) => {
                        tracing::warn!("the new credentials file cannot be used any more, keeping the connection: {why}");
                        record_keys(keys, link);
                        link.write_state();
                    }
                    TrialStep::Open(trial) => {
                        tracing::info!(
                            "opening a second connection with the new credentials; the current one stays until it is up"
                        );
                        opening = Some(Opening::start(remote));
                        trying = Some(trial);
                    }
                }
            }

            let now = now_secs();
            if !link.outbox.is_empty() && Instant::now() >= next_try {
                let sealer = keys.sealer();
                let report = flush(&link.outbox, &publisher, &sealer, alias, now_secs).await;
                {
                    let mut s = link.state();
                    s.puts += report.puts;
                    s.deletes += report.deletes;
                    s.host_puts += report.host_puts;
                    s.project_puts += report.project_puts;
                    if report.sent() > 0 {
                        s.last_publish_at = Some(now_secs());
                    }
                    if let Some(err) = &report.error {
                        s.failures += 1;
                        s.last_error = Some(err.clone());
                        s.last_error_at = Some(now);
                    }
                }
                match report.error {
                    Some(err) => {
                        failing.count += 1;
                        let since = *failing.since.get_or_insert(now);
                        let delay = backoff(failing.count);
                        next_try = Instant::now() + delay;
                        if failing
                            .logged_at
                            .is_none_or(|t| now - t >= ERROR_LOG_EVERY_SECS)
                        {
                            tracing::warn!(
                                pending = link.outbox.len(),
                                "publish failed ({} in a row), retrying in {delay:?}: {err}",
                                failing.count
                            );
                            failing.logged_at = Some(now);
                        }
                        if now - since >= REBUILD_AFTER_SECS {
                            // A rebuild reads the creds file; while a new one
                            // waits for its trial, that trial stands in for
                            // the rebuild and the connection in use stays.
                            if opening.is_some() || keys.trial_now(Instant::now()) {
                                tracing::warn!(
                                    "publishing has failed for {:.0} s; trying the new credentials on a new \
                                     connection instead of rebuilding with them",
                                    now - since
                                );
                                failing.since = Some(now);
                            } else if keys.disk_creds_refused() {
                                // A rebuild would connect with the refused
                                // file; async-nats keeps reconnecting with
                                // the credentials it holds.
                                tracing::warn!(
                                    "publishing has failed for {:.0} s; not rebuilding the connection, because the \
                                     credentials file on disk is not usable",
                                    now - since
                                );
                                failing.since = Some(now);
                            } else {
                                tracing::warn!(
                                    "publishing has failed for {:.0} s, rebuilding the connection",
                                    now - since
                                );
                                break Leave::Rebuild;
                            }
                        }
                    }
                    None => {
                        if failing.since.is_some() {
                            tracing::info!(
                                "publishing again after {} failed attempts",
                                failing.count
                            );
                        }
                        failing = Failing::default();
                        tracing::debug!(
                            puts = report.puts,
                            deletes = report.deletes,
                            host = report.host_puts,
                            projects = report.project_puts,
                            "flushed"
                        );
                    }
                }
            }

            if now - last_state_write >= WRITE_EVERY_SECS {
                link.write_state();
                last_state_write = now;
            }
            if now - last_summary.0 >= SUMMARY_EVERY_SECS {
                let (sessions, total) = {
                    let s = link.state();
                    (
                        s.sessions,
                        s.puts + s.deletes + s.host_puts + s.project_puts,
                    )
                };
                tracing::info!(
                    sessions,
                    "{} writes in the last {:.0} min",
                    total - last_summary.1,
                    (now - last_summary.0) / 60.0
                );
                last_summary = (now, total);
            }
        };

        link.state().connected = false;
        match leave {
            Leave::Shutdown => {
                if !link.outbox.is_empty() && !disconnected {
                    let sealer = keys.sealer();
                    let final_flush = flush(&link.outbox, &publisher, &sealer, alias, now_secs);
                    match tokio::time::timeout(FINAL_FLUSH_TIMEOUT, final_flush).await {
                        Ok(report) => {
                            let mut s = link.state();
                            s.puts += report.puts;
                            s.deletes += report.deletes;
                            s.host_puts += report.host_puts;
                            s.project_puts += report.project_puts;
                        }
                        Err(_) => tracing::debug!("final flush timed out"),
                    }
                }
                client.close().await;
                break 'outer;
            }
            Leave::Rebuild => {
                client.close().await;
                link.write_state();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::session::SessionState;

    fn payload(session: &str, state: SessionState) -> RemotePayload {
        RemotePayload {
            machine: "lab-01".into(),
            sid: cc_envelope::sid_for(session),
            alias: None,
            title: None,
            dir: None,
            text: None,
            agents: None,
            ags: None,
            state,
            since: 1,
            tool: None,
            model: None,
            ctx: None,
            hb: 1,
        }
    }

    #[test]
    fn the_persisted_keys_cover_pending_deletes_and_keys_not_taken_over() {
        let outbox = Outbox::new();
        let mut planner = Planner::new("lab-01");
        planner.adopt(["lab-01.s.old".to_string()]);
        planner.plan(&[payload("a", SessionState::Idle)], 1.0, &mut &outbox);
        let a = cc_envelope::session_key("lab-01", &cc_envelope::sid_for("a"));
        let keys = published_keys(&planner, &outbox, &["lab-01.s.later".to_string()]);
        let mut want = vec![a, "lab-01.s.later".to_string(), "lab-01.s.old".to_string()];
        want.sort();
        assert_eq!(keys, want, "the delete of .old is not sent yet");
        outbox.take();
        assert!(!published_keys(&planner, &outbox, &[]).contains(&"lab-01.s.old".to_string()));
    }

    #[test]
    fn a_gathering_pass_fills_the_outbox_and_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let link = Link::new(PublishState::default(), None, None);
        let gone = cc_envelope::session_key("lab-01", &cc_envelope::sid_for("gone"));
        let mut g = Gatherer::new("lab-01", dir.path().to_path_buf(), vec![gone.clone()]);

        // No registry yet: the previous run's keys are kept, not deleted.
        let pass = g.step(&[payload("a", SessionState::Working)], false, 10.0, &link);
        assert_eq!(
            (pass.plan.changed, pass.plan.deletes, pass.adopted),
            (1, 0, 0)
        );
        assert!(
            pass.plan.host && pass.feed.started,
            "the scan runs on its own thread"
        );
        assert_eq!(link.state().sessions, 1);
        assert!(
            link.state().keys.contains(&gone),
            "inherited until taken over"
        );
        let ops = link.outbox.take();
        assert_eq!(ops.len(), 2, "session and host: {ops:?}");

        // The first registry snapshot takes them over: the ended one goes.
        let pass = g.step(&[payload("a", SessionState::Working)], true, 12.0, &link);
        assert_eq!((pass.adopted, pass.plan.deletes), (1, 1));
        assert_eq!(link.outbox.pending_deletes(), std::slice::from_ref(&gone));
        assert!(
            link.state().keys.contains(&gone),
            "until the delete is sent"
        );
        link.outbox.take();

        // A reconnect makes everything due again.
        link.request_resync();
        let pass = g.step(&[payload("a", SessionState::Working)], true, 14.0, &link);
        assert!(pass.resync && pass.plan.heartbeats == 1 && pass.plan.host);
        assert!(!link.take_resync(), "consumed by the pass");
    }

    #[test]
    fn writing_the_state_saves_it_and_tells_the_observer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("publish-state.json");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let link = Link::new(
            PublishState {
                alias: "lab-01".into(),
                ..Default::default()
            },
            Some(path.clone()),
            Some(Box::new(move || {
                seen.fetch_add(1, Ordering::SeqCst);
            })),
        );
        link.outbox.push("lab-01.host", crate::outbox::Op::Host);
        link.write_state();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let saved = PublishState::load(&path).unwrap();
        assert_eq!((saved.alias.as_str(), saved.pending), ("lab-01", 1));
        assert_eq!(link.snapshot().pending, 1);

        // Without a file or an observer, writing only stamps the state.
        let link = Link::new(PublishState::default(), None, None);
        link.write_state();
        assert!(link.state().updated_at > 0.0);
    }

    #[test]
    fn a_reload_records_what_is_in_use_and_resyncs_new_keys() {
        use cc_envelope::SecretKeys;
        let dir = tempfile::tempdir().unwrap();
        let files = crate::reload::KeyFiles {
            publisher_key: dir.path().join("publisher.key"),
            viewer_pub: dir.path().join("viewer.pub"),
            creds: dir.path().join("u.creds"),
            user: "cc-sessions.pub-lab-01".into(),
        };
        SecretKeys::generate("p1")
            .unwrap()
            .save_new(&files.publisher_key)
            .unwrap();
        SecretKeys::generate("k1")
            .unwrap()
            .public_keys()
            .save(&files.viewer_pub)
            .unwrap();
        let keys = Keys::load(files.clone()).unwrap();
        let path = dir.path().join("publish-state.json");
        let link = Link::new(PublishState::default(), Some(path.clone()), None);
        record_keys(&keys, &link);
        assert_eq!(link.state().in_use.as_ref().unwrap().viewer_kid, "k1");

        assert!(!reload(&keys, &link, true).changed());
        assert!(!link.take_resync(), "the same keys need no resync");

        SecretKeys::generate("v20261014-cccc")
            .unwrap()
            .public_keys()
            .save(&files.viewer_pub)
            .unwrap();
        assert_eq!(reload(&keys, &link, false).keys, Change::Applied);
        assert!(
            link.take_resync(),
            "everything is sealed with the new key soon"
        );
        let saved = PublishState::load(&path).unwrap().in_use.unwrap();
        assert_eq!(saved.viewer_kid, "v20261014-cccc", "written at once");
        assert_eq!(saved.describe(), keys.sealer().describe());

        std::fs::write(&files.viewer_pub, "broken\n").unwrap();
        assert!(matches!(
            reload(&keys, &link, false).keys,
            Change::Rejected(_)
        ));
        assert!(!link.take_resync());
        let saved = PublishState::load(&path).unwrap().in_use.unwrap();
        assert_eq!(saved.viewer_kid, "v20261014-cccc", "the last good key");
        assert!(saved.key_error.unwrap().contains("viewer.pub"));
    }

    #[test]
    fn only_a_trial_that_is_up_replaces_the_connection() {
        assert_eq!(trial_outcome(None), TrialOutcome::Swap);
        for err in [
            "connect failed: authorization violation",
            "connect failed: IO error: user authentication expired",
            "connect failed: IO error: authorization violation - user revoked",
            "bind timed out after 10s",
            "connect failed: IO error: maximum connections exceeded",
        ] {
            assert_eq!(trial_outcome(Some(err)), TrialOutcome::Keep, "{err}");
        }
        // Checked by the server after authenticating: the creds are good.
        let full = "connect failed: IO error: Maximum Account Active Connections Exceeded";
        assert!(is_connection_limit(full));
        assert_eq!(trial_outcome(Some(full)), TrialOutcome::SwitchClosingFirst);
    }

    #[test]
    fn an_empty_opening_slot_never_resolves() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut empty: Option<Pin<Box<dyn Future<Output = u8>>>> = None;
            let waited = tokio::time::timeout(Duration::from_millis(20), next_of(&mut empty)).await;
            assert!(waited.is_err());
            let mut slot: Option<Pin<Box<dyn Future<Output = u8>>>> = Some(Box::pin(async { 7 }));
            assert_eq!(next_of(&mut slot).await, 7);
        });
    }

    #[test]
    fn a_trial_that_cannot_load_its_creds_fails_without_the_network() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let remote = cc_remote::Config {
            servers: vec!["tls://127.0.0.1:1".into()],
            bucket: cc_remote::DEFAULT_BUCKET.into(),
            creds_path: dir.path().join("missing.creds"),
            user: "cc-sessions.pub-lab-01".into(),
            name: "test".into(),
        };
        let opened = rt.block_on(async {
            let mut slot = Some(Opening::start(&remote));
            tokio::time::timeout(Duration::from_secs(5), next_of(&mut slot)).await
        });
        match opened.expect("resolves at once") {
            Ok(_) => panic!("connected without creds"),
            Err(err) => assert!(err.contains("missing.creds"), "{err}"),
        }
    }

    /// The trial a network side would run against these keys, with the
    /// connection replaced by `connect`'s result (no NATS involved): what
    /// [`serve`] records for it.
    fn run_trial(keys: &Keys, link: &Link, connect: Result<(), &str>) -> Option<TrialOutcome> {
        let TrialStep::Open(trial) = keys.begin_trial(Instant::now()) else {
            return None;
        };
        let outcome = trial_outcome(connect.err());
        match (outcome, connect) {
            (_, Ok(())) => keys.trial_succeeded(&trial),
            (TrialOutcome::SwitchClosingFirst, Err(_)) => {}
            (_, Err(err)) => {
                keys.trial_failed(&trial, err, Instant::now());
            }
        }
        record_keys(keys, link);
        Some(outcome)
    }

    #[test]
    fn a_revoked_creds_file_is_recorded_and_the_connection_kept() {
        let dir = tempfile::tempdir().unwrap();
        let files = test_files(dir.path());
        let keys = Keys::load(files.clone()).unwrap();
        let link = Link::new(PublishState::default(), None, None);
        keys.connecting();
        std::fs::write(&files.creds, creds_text(&files.user) + "\n").unwrap();
        assert_eq!(reload(&keys, &link, true).creds, Change::Applied);
        assert_eq!(
            link.state().in_use.as_ref().unwrap().creds_error,
            None,
            "not tried yet"
        );

        let revoked = "connect failed: authorization violation";
        assert_eq!(
            run_trial(&keys, &link, Err(revoked)),
            Some(TrialOutcome::Keep)
        );
        let in_use = link.state().in_use.clone().unwrap();
        assert!(in_use.creds_error.unwrap().ends_with(revoked));
        assert_eq!(
            in_use.creds_user.as_deref(),
            Some(files.user.as_str()),
            "the one in use"
        );
        assert_eq!(run_trial(&keys, &link, Ok(())), None, "backing off");

        // Fixed by hand: tried at once, and in use once it is up.
        std::fs::write(&files.creds, creds_text(&files.user) + "\n\n").unwrap();
        assert_eq!(reload(&keys, &link, false).creds, Change::Applied);
        assert_eq!(run_trial(&keys, &link, Ok(())), Some(TrialOutcome::Swap));
        assert_eq!(link.state().in_use.as_ref().unwrap().creds_error, None);
        assert_eq!(run_trial(&keys, &link, Ok(())), None, "nothing left to try");
    }

    fn test_files(dir: &std::path::Path) -> crate::reload::KeyFiles {
        use cc_envelope::SecretKeys;
        let files = crate::reload::KeyFiles {
            publisher_key: dir.join("publisher.key"),
            viewer_pub: dir.join("viewer.pub"),
            creds: dir.join("u.creds"),
            user: "cc-sessions.pub-lab-01".into(),
        };
        SecretKeys::generate("p1")
            .unwrap()
            .save_new(&files.publisher_key)
            .unwrap();
        SecretKeys::generate("k1")
            .unwrap()
            .public_keys()
            .save(&files.viewer_pub)
            .unwrap();
        std::fs::write(&files.creds, creds_text(&files.user)).unwrap();
        files
    }

    /// Not real credentials: a JWT with only a `name` claim and a fake seed.
    fn creds_text(name: &str) -> String {
        use base64::Engine as _;
        let b64 = |v: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        format!(
            "-----BEGIN NATS USER JWT-----\n{}.{}.c2ln\n------END NATS USER JWT------\n\n\
             -----BEGIN USER NKEY SEED-----\nSUAFAKEFAKEFAKE\n------END USER NKEY SEED------\n",
            b64(serde_json::json!({"typ":"JWT","alg":"ed25519-nkey"})),
            b64(serde_json::json!({"name": name}))
        )
    }

    #[test]
    fn backoff_is_at_least_a_second_and_capped() {
        assert!(backoff(1) >= Duration::from_secs(1));
        assert!(backoff(1) <= Duration::from_millis(1200));
        for n in 1..100 {
            assert!(backoff(n) <= MAX_BACKOFF);
            assert!(backoff(n) >= Duration::from_secs(1));
        }
        assert!(backoff(30) >= Duration::from_secs(25));
    }

    #[test]
    fn a_failed_connect_and_its_backoff_can_outlast_the_stale_threshold() {
        // Why the connect and the backoff run under `ticking`: without it the
        // state file is only written between attempts.
        let connect = cc_remote::CONNECT_TIMEOUT.as_secs_f64() * 2.0 + 4.0;
        let gap = connect + MAX_BACKOFF.as_secs_f64();
        assert!(gap > crate::state::STALE_AFTER_SECS, "{gap}");
        const { assert!(WRITE_EVERY_SECS < crate::state::STALE_AFTER_SECS) };
    }

    #[test]
    fn ticking_calls_back_while_the_future_waits_and_returns_its_output() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut ticks = 0;
        let out = rt.block_on(ticking(
            async {
                tokio::time::sleep(Duration::from_millis(260)).await;
                7
            },
            Duration::from_millis(50),
            || ticks += 1,
        ));
        assert_eq!(out, 7);
        assert!((3..=6).contains(&ticks), "{ticks} ticks");

        let mut ticks = 0;
        let out = rt.block_on(ticking(async { 1 }, Duration::from_millis(50), || {
            ticks += 1
        }));
        assert_eq!((out, ticks), (1, 0), "a ready future never ticks");
    }
}
