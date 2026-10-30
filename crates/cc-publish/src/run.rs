//! `cc-publish run`: the long-running publisher.
//!
//! Two halves that never wait for each other, both from [`crate::pipeline`]
//! (the cc-usage app runs the same pipeline when its role is Publisher):
//!
//! - **The gathering thread** reads the local sources every
//!   [`POLL_INTERVAL`] through [`Sources`] (the code the widget runs, with
//!   the CLI backing off on an idle machine), feeds the [`SessionMonitor`],
//!   and lets the [`Gatherer`] record its decisions in the link's outbox.
//! - **The network task** (one tokio current-thread runtime, one NATS
//!   connection) is [`serve`]: connect, bind, flush, retry with backoff,
//!   resync after a reconnect. It picks up replaced key files and a
//!   replaced creds file within [`crate::reload::RECHECK_EVERY`], without
//!   a restart, and records what it uses in the state file.
//!
//! A run loop holds the publisher lock ([`crate::instance`]) from before it
//! reads the state file until it stops, so a second `run`, or the cc-usage
//! app as Publisher, does not start next to it. `--force` skips only the
//! state-file check, never the lock.
//!
//! A panic in the gathering thread is fatal: the process logs why and exits
//! with [`EXIT_GATHER_PANIC`] (or aborts, in a release build), so systemd or
//! the scheduled task starts a fresh one instead of leaving a process that is
//! connected but publishes nothing.
//!
//! The state file keeps the session keys published so far. A new run takes
//! them over: once its first registry snapshot is in, it deletes the keys of
//! sessions that ended while no publisher was running (see
//! [`crate::planner::Planner::adopt`]).
//!
//! On Ctrl-C, SIGTERM, `cc-publish stop` (see [`crate::stop`]) or, on
//! Windows, console close, logoff or shutdown, the network task makes one
//! last bounded flush and closes the connection. Session keys are left in
//! place: the bucket's TTL and the viewer's offline logic take care of them.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use cc_local::monitor::{Cadence, Paths, SessionMonitor, Sources, POLL_INTERVAL};

use crate::config::Config;
use crate::instance::{lock_path_for, InstanceLock, LockError};
use crate::pipeline::{serve, Gatherer, Link};
use crate::reload::{KeyFiles, Keys};
use crate::state::PublishState;
use crate::util::now_secs;

pub use crate::pipeline::{backoff, MAX_BACKOFF, REBUILD_AFTER_SECS};

/// Exit code when the gathering thread panicked (`EX_SOFTWARE`).
pub const EXIT_GATHER_PANIC: i32 = 70;

/// Test-only fault injection: `CC_PUBLISH_FAULT=gather-panic` makes the
/// gathering thread panic on its first pass, so the integration tests can
/// check that a panic there ends the process.
pub const FAULT_ENV: &str = "CC_PUBLISH_FAULT";

/// Options of `cc-publish run`.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Start even if the state file says another run loop is alive (never
    /// while another publisher holds the lock).
    pub force: bool,
    /// Stop on its own after this long (tests and smoke checks).
    pub duration: Option<Duration>,
}

/// Why `run` could not start. Once running, nothing is fatal.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// No envelope keys could be loaded.
    #[error("{0}")]
    Key(#[from] crate::sealing::SealingError),
    /// The credentials file is missing.
    #[error("credentials file {} not found (provision the publisher user and copy its .creds file there)", .0.display())]
    NoCreds(std::path::PathBuf),
    /// The state file says another run loop is alive.
    #[error("another cc-publish run (pid {0}) is active; stop it first, or pass --force")]
    AlreadyRunning(u32),
    /// Another publisher holds the lock file ([`crate::instance`]): the pid
    /// the state file names, when it names one.
    #[error(
        "another publisher{} is active on this machine (cc-publish run, or the cc-usage app as Publisher; it holds {}); stop it first",
        .pid.map(|p| format!(" (pid {p})")).unwrap_or_default(),
        .path.display()
    )]
    Locked {
        /// The lock file.
        path: std::path::PathBuf,
        /// The pid the state file names.
        pid: Option<u32>,
    },
    /// The lock file could not be opened or locked.
    #[error("{0}")]
    Lock(crate::instance::LockError),
    /// The runtime or the gathering thread could not be started.
    #[error("cannot start: {0}")]
    Start(#[from] std::io::Error),
}

/// The state a new run loop starts from: this process, `alias`, and the
/// session keys `inherited` from the previous run (kept until the first
/// pass takes them over, so a crash before it does not forget them).
pub fn initial_state(alias: &str, inherited: &[String]) -> PublishState {
    let me = std::process::id();
    let started_at = now_secs();
    PublishState {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: me,
        pid_started: cc_local::process::start_time(me),
        alias: alias.to_string(),
        started_at,
        updated_at: started_at,
        running: true,
        keys: inherited.to_vec(),
        ..Default::default()
    }
}

/// The key and creds files of `config`, as the pipeline watches them.
pub fn key_files(config: &Config) -> KeyFiles {
    KeyFiles {
        publisher_key: config.publisher_key.clone(),
        viewer_pub: config.viewer_pub.clone(),
        creds: config.creds.clone(),
        user: config.user(),
    }
}

/// Runs until Ctrl-C, SIGTERM or `opts.duration`.
pub fn run(config: Config, opts: RunOptions) -> Result<(), RunError> {
    let keys = Keys::load(key_files(&config))?;
    if !config.creds.is_file() {
        return Err(RunError::NoCreds(config.creds.clone()));
    }
    let state_path = PublishState::default_path();
    let me = std::process::id();
    // Held until this function returns (the OS drops it if the process
    // dies): the state file is read and taken over under it.
    let _instance = acquire_instance(&state_path, me)?;
    let prev = PublishState::load(&state_path);
    if let Some(prev) = &prev {
        // Whether that process still runs (pid and start time), not how old
        // its file is: a killed publisher must not hold up its replacement.
        if prev.blocks_run(me, now_secs()) && !opts.force {
            return Err(RunError::AlreadyRunning(prev.pid));
        }
    }
    let inherited = prev
        .as_ref()
        .map(|p| p.session_keys_for(&config.alias))
        .unwrap_or_default();

    // A request left for an earlier run loop must not stop this one; a
    // request for this pid is impossible before this point.
    crate::stop::clear(&crate::stop::default_path());
    log_panics();

    let link = Link::new(
        initial_state(&config.alias, &inherited),
        Some(state_path.clone()),
        None,
    );
    link.write_state();
    tracing::info!(
        alias = %config.alias,
        user = %config.user(),
        envelope = %keys.sealer().describe(),
        "cc-publish {} starting",
        env!("CARGO_PKG_VERSION")
    );
    tracing::info!(
        "sealing with {} to the viewer key in {}; changes to them and to {} are picked up within {} s",
        config.publisher_key.display(),
        config.viewer_pub.display(),
        config.creds.display(),
        crate::reload::RECHECK_EVERY.as_secs()
    );

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let gatherer = {
        let link = std::sync::Arc::clone(&link);
        let alias = config.alias.clone();
        std::thread::Builder::new()
            .name("gather".into())
            .spawn(move || {
                supervise(
                    || gather_loop(&alias, &link, &stop_rx, inherited),
                    |why| gather_panicked(&link, &why),
                )
            })?
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let deadline = opts.duration.map(|d| tokio::time::Instant::now() + d);
    runtime.block_on(serve(
        &config.remote(),
        &keys,
        &config.alias,
        &link,
        shutdown_signal(deadline),
    ));
    drop(runtime);

    drop(stop_tx);
    let _ = gatherer.join();
    {
        let mut s = link.state();
        s.running = false;
        s.connected = false;
    }
    link.write_state();
    tracing::info!("cc-publish stopped");
    Ok(())
}

/// Takes the publisher lock next to `state_path` for the process `me`
/// ([`crate::instance`]); when another publisher holds it, the error names
/// the pid its state file records. `--force` never skips this: the lock
/// is held only by a process that is still running.
pub fn acquire_instance(state_path: &std::path::Path, me: u32) -> Result<InstanceLock, RunError> {
    match InstanceLock::acquire(&lock_path_for(state_path)) {
        Ok(lock) => Ok(lock),
        Err(LockError::Held(path)) => Err(RunError::Locked {
            path,
            pid: PublishState::load(state_path).and_then(|s| s.holder(me)),
        }),
        Err(err) => Err(RunError::Lock(err)),
    }
}

/// Logs every panic through tracing (so it reaches the log file, not only a
/// stderr nobody reads under a scheduled task), then runs the default hook.
/// In a release build a panic aborts right after this.
fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default(info);
    }));
}

/// The text of a panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Runs `body`; if it panics, calls `on_panic` with the panic's message.
pub fn supervise(body: impl FnOnce(), on_panic: impl FnOnce(String)) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        on_panic(panic_message(payload.as_ref()));
    }
}

/// The gathering thread died: nothing would be published any more while the
/// connection stays up. Record why, mark the state file stopped (the next
/// run starts at once) and exit non-zero, so the service manager restarts
/// the publisher.
fn gather_panicked(link: &Link, why: &str) -> ! {
    tracing::error!(
        "the gathering thread panicked ({why}); exiting with code {EXIT_GATHER_PANIC} so the service manager restarts cc-publish"
    );
    {
        let mut s = link.state();
        s.running = false;
        s.connected = false;
        s.exit_reason = Some(format!("the gathering thread panicked: {why}"));
    }
    link.write_state();
    std::process::exit(EXIT_GATHER_PANIC);
}

/// The gathering thread. Stops when `stop` is dropped. `inherited` are the
/// session keys the previous run published.
fn gather_loop(alias: &str, link: &Link, stop: &mpsc::Receiver<()>, inherited: Vec<String>) {
    let fault = std::env::var(FAULT_ENV).is_ok_and(|v| v == "gather-panic");
    let mut sources = Sources::new(Paths::real(), Cadence::IDLE_BACKOFF);
    let mut monitor = SessionMonitor::new();
    let mut gatherer = Gatherer::new(alias, cc_local::projects::projects_dir(), inherited);
    let mut last_agents_error: Option<String> = None;
    loop {
        if fault {
            panic!("injected fault ({FAULT_ENV}=gather-panic)");
        }
        let started = Instant::now();
        let input = sources.gather();
        if input.agents_error != last_agents_error {
            match &input.agents_error {
                Some(err) => tracing::warn!(
                    "claude agents --json failed ({} in a row): {err}",
                    sources.registry().failures()
                ),
                None if last_agents_error.is_some() => {
                    tracing::info!("claude agents --json works again")
                }
                None => {}
            }
            last_agents_error.clone_from(&input.agents_error);
        }
        let events = input.events.len();
        let has_registry = input.registry.is_some();
        let now = sources.now();
        monitor.step(input, now);
        let payloads = monitor.payloads(alias, now);
        let pass = gatherer.step(&payloads, has_registry, now, link);
        if pass.feed.scanned {
            tracing::debug!(
                projects = gatherer.projects(),
                deleted = pass.feed.deletes,
                "projects scanned"
            );
        }
        if pass.adopted > 0 {
            tracing::info!(
                "took over {} session key(s) from the previous run, {} no longer live",
                pass.adopted,
                pass.plan.deletes
            );
        }
        if pass.plan.changed > 0 || pass.plan.deletes > 0 {
            tracing::debug!(
                sessions = payloads.len(),
                changed = pass.plan.changed,
                deleted = pass.plan.deletes,
                spool_events = events,
                "sessions changed"
            );
        }
        {
            let mut s = link.state();
            s.passes = monitor.passes();
            s.agents_error.clone_from(&last_agents_error);
        }
        let wait = POLL_INTERVAL.saturating_sub(started.elapsed());
        match stop.recv_timeout(wait) {
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            _ => break,
        }
    }
}

/// Resolves on Ctrl-C, SIGTERM (Unix), console close, logoff or shutdown
/// (Windows), a `cc-publish stop` request for this process, or the deadline.
async fn shutdown_signal(deadline: Option<tokio::time::Instant>) {
    let timer = async {
        match deadline {
            Some(d) => {
                tokio::time::sleep_until(d).await;
                tracing::info!("run duration reached, stopping");
            }
            None => std::future::pending().await,
        }
    };
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("interrupted, stopping");
        } else {
            std::future::pending::<()>().await;
        }
    };
    let requested = async {
        let path = crate::stop::default_path();
        let me = std::process::id();
        loop {
            tokio::time::sleep(crate::stop::POLL_INTERVAL).await;
            if crate::stop::requested_for(&path, me) {
                tracing::info!("stop requested, stopping");
                break;
            }
        }
    };
    #[cfg(unix)]
    let term = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
                tracing::info!("SIGTERM, stopping");
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(windows)]
    let term = async {
        use tokio::signal::windows::{ctrl_close, ctrl_logoff, ctrl_shutdown};
        // A handler that cannot be installed just never fires.
        let (mut close, mut logoff, mut shutdown) =
            (ctrl_close().ok(), ctrl_logoff().ok(), ctrl_shutdown().ok());
        tokio::select! {
            Some(_) = async { close.as_mut()?.recv().await } => tracing::info!("console closed, stopping"),
            Some(_) = async { logoff.as_mut()?.recv().await } => tracing::info!("logging off, stopping"),
            Some(_) = async { shutdown.as_mut()?.recv().await } => tracing::info!("shutting down, stopping"),
            else => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(any(unix, windows)))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = timer => {}
        _ = ctrl_c => {}
        _ = requested => {}
        _ = term => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_is_caught_with_its_message() {
        let mut why = None;
        supervise(|| panic!("boom {}", 42), |w| why = Some(w));
        assert_eq!(why.as_deref(), Some("boom 42"));
        let mut why = None;
        supervise(|| std::panic::panic_any("static"), |w| why = Some(w));
        assert_eq!(why.as_deref(), Some("static"));
        let mut called = false;
        supervise(|| {}, |_| called = true);
        assert!(!called, "no panic, no call");
    }

    #[test]
    fn a_new_run_starts_running_with_the_inherited_keys() {
        let s = initial_state("lab-01", &["lab-01.s.0000aaaa".to_string()]);
        assert!(s.running && !s.connected);
        assert_eq!(s.pid, std::process::id());
        assert_eq!(s.alias, "lab-01");
        assert_eq!(s.keys, ["lab-01.s.0000aaaa"]);
        assert_eq!(s.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_held_lock_refuses_and_names_the_recorded_pid() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join(crate::state::STATE_FILE);
        let me = std::process::id();
        let held = acquire_instance(&state_path, me).unwrap();

        // No state file: no pid to name.
        let err = acquire_instance(&state_path, me).unwrap_err();
        assert!(matches!(err, RunError::Locked { pid: None, .. }), "{err:?}");
        let text = err.to_string();
        assert!(
            text.contains("another publisher is active") && text.contains("cc-publish.lock"),
            "{text}"
        );
        assert!(
            !text.contains("--force"),
            "--force does not pass the lock: {text}"
        );

        // The holder's state file names it, even a stale one.
        PublishState {
            pid: 4242,
            running: true,
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        let err = acquire_instance(&state_path, me).unwrap_err();
        assert!(
            err.to_string()
                .contains("another publisher (pid 4242) is active"),
            "{err}"
        );
        PublishState {
            pid: 4242,
            running: false,
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        assert!(matches!(
            acquire_instance(&state_path, me),
            Err(RunError::Locked { pid: None, .. })
        ));

        drop(held);
        assert!(
            acquire_instance(&state_path, me).is_ok(),
            "free once the holder is gone"
        );
    }
}
