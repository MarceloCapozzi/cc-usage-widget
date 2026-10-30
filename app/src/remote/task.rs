//! The background tasks: [`run`] connects as the viewer and watches the
//! bucket, [`run_ticker`] applies the time-based rules.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use cc_envelope::{KvKey, ViewerKeys};
use cc_remote::{ConnectionEvent, ConnectionState, KvChange, Viewer};
use futures_util::{FutureExt, StreamExt};

use super::hub::{lock, ConnState, Notify, SharedHub};
use super::review::save_pending_soon;
use super::store::save_projects_soon;
use super::{
    nats_reload, now_unix, viewer_user, KEY_RECHECK, MIN_RETRY_DELAY, SETUP_RECHECK, TICK_INTERVAL,
};
use crate::settings::{self, Nats};

/// Where the viewer finds its creds and keys, and where it reads which
/// servers and bucket to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// `<data_dir>/nats/cc-sessions.viewer.creds`.
    pub creds_path: PathBuf,
    /// `<data_dir>/keys/viewer.key`: the viewer's private keys,
    /// created on first start.
    pub viewer_key_path: PathBuf,
    /// `<data_dir>/keys/viewer.pub`: the matching public keys, kept in step
    /// with `viewer.key`, for the publisher bundles.
    pub viewer_pub_path: PathBuf,
    /// `<data_dir>/keys/publishers`: each publisher's pinned public keys.
    pub publishers_dir: PathBuf,
    /// The folder of `settings.json`, whose `"nats"` object (and the
    /// `CC_USAGE_NATS_*` variables) name the servers and the bucket, read
    /// again before every connection and every [`KEY_RECHECK`]
    /// ([`settings::nats`]). `None`: cc-remote's defaults, nothing read.
    pub settings_dir: Option<PathBuf>,
}

impl Default for Setup {
    fn default() -> Self {
        Self::from_data_dir()
    }
}

impl Setup {
    /// The standard locations under [`cc_core::paths::data_dir`], and
    /// `settings.json` in [`settings::default_dir`].
    pub fn from_data_dir() -> Self {
        Self {
            creds_path: cc_core::paths::nats_dir().join(format!("{}.creds", viewer_user())),
            viewer_key_path: cc_core::paths::viewer_key_file(),
            viewer_pub_path: cc_core::paths::viewer_pub_file(),
            publishers_dir: cc_core::paths::publishers_dir(),
            settings_dir: Some(settings::default_dir()),
        }
    }

    /// The servers and bucket to use now. Reads `settings.json`; call it
    /// off the executor.
    pub fn nats(&self) -> Nats {
        match &self.settings_dir {
            Some(dir) => settings::nats(dir),
            None => Nats::default(),
        }
    }

    /// True when the creds and the viewer key exist. Says nothing about
    /// their contents.
    pub fn is_configured(&self) -> bool {
        self.creds_path.is_file() && self.viewer_key_path.is_file()
    }

    /// Makes sure the viewer's key pair exists (this machine is the viewer):
    /// creates `viewer.key` and `viewer.pub` when missing, rewrites
    /// `viewer.pub` when it no longer matches, and logs the public key line
    /// once when it creates them.
    pub fn ensure_viewer_keys(&self) -> Result<cc_envelope::SecretKeys, String> {
        let kid = cc_envelope::new_kid("v", now_unix()).map_err(|e| e.to_string())?;
        let state =
            cc_envelope::ensure_viewer_keys(&self.viewer_key_path, &self.viewer_pub_path, &kid)
                .map_err(|e| format!("Unusable viewer key file: {e}"))?;
        if state.created {
            tracing::info!(
                "remote: created the viewer key pair {}; public key {} (publishers seal to {})",
                self.viewer_key_path.display(),
                state.keys.current().public_key().to_line(),
                self.viewer_pub_path.display()
            );
        } else if state.wrote_public {
            tracing::info!(
                "remote: rewrote {} to match {}",
                self.viewer_pub_path.display(),
                self.viewer_key_path.display()
            );
        }
        Ok(state.keys)
    }

    /// The keys to open values with, or why the viewer is not configured
    /// (user-facing, paths only, never file contents): the viewer key pair
    /// (created when missing) and the pinned publisher keys.
    pub(super) fn check(&self) -> Result<ViewerKeys, String> {
        if !self.creds_path.is_file() {
            return Err(format!("Missing {}", self.creds_path.display()));
        }
        let secrets = self.ensure_viewer_keys()?;
        Ok(ViewerKeys::new(secrets, Some(self.publishers_dir.clone())))
    }

    /// What, when changed, makes the watch reload the keys.
    fn key_stamp(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.viewer_key_path)
            .and_then(|m| m.modified())
            .ok()
    }
}

/// True when `now` names other servers or another bucket than `in_use`.
pub(super) fn nats_moved(in_use: &Nats, now: &Nats) -> bool {
    in_use.servers != now.servers || in_use.bucket != now.bucket
}

pub(super) fn retry_delay(failures: usize) -> Duration {
    cc_remote::reconnect_delay(failures.saturating_add(1)).max(MIN_RETRY_DELAY)
}

/// Waits `delay`, or less when Settings saved the connection settings
/// ([`super::reload_nats`]).
async fn wait_or_reload(delay: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(delay) => {}
        _ = nats_reload().notified() => {}
    }
}

/// The viewer task: waits for the creds and the key, connects, watches the
/// bucket into `hub` and reconnects with backoff, forever. Holds at most one
/// connection, and closes it before retrying. `notify` runs after every
/// change of rows or status. Key and pin files are read on tokio's blocking
/// pool, never on the executor. Connection settings saved in Settings cut
/// a wait short and are compared at once while watching.
pub async fn run(hub: SharedHub, setup: Setup, notify: Notify) {
    // This machine is the viewer: its key pair exists from the first start,
    // creds or not, so a publisher bundle can be built at any time.
    let first = setup.clone();
    match tokio::task::spawn_blocking(move || first.ensure_viewer_keys()).await {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => tracing::warn!("remote: {err}"),
        Err(err) => tracing::warn!("remote: {err}"),
    }
    let mut failures: usize = 0;
    loop {
        let checking = setup.clone();
        let (checked, nats) =
            tokio::task::spawn_blocking(move || (checking.check(), checking.nats()))
                .await
                .unwrap_or_else(|e| (Err(e.to_string()), Nats::default()));
        note_nats_problems(&hub, &nats, &notify);
        let keys = match checked {
            Ok(keys) => keys,
            Err(reason) => {
                if lock(&hub).set_state(ConnState::NotConfigured, Some(reason)) {
                    notify();
                }
                wait_or_reload(SETUP_RECHECK).await;
                continue;
            }
        };
        // The first attempt says "connecting"; retries keep "retrying".
        if failures == 0 && lock(&hub).set_state(ConnState::Connecting, None) {
            notify();
        }
        let (was_connected, error) = connect_and_watch(&hub, &setup, &nats, keys, &notify).await;
        if was_connected {
            failures = 0;
        }
        failures += 1;
        tracing::warn!("remote: {error}");
        if lock(&hub).set_state(ConnState::Disconnected, Some(error)) {
            notify();
        }
        wait_or_reload(retry_delay(failures)).await;
    }
}

/// Hands `nats`'s problems to the hub (the footer's tooltip) and logs them
/// when they changed.
fn note_nats_problems(hub: &SharedHub, nats: &Nats, notify: &Notify) {
    if lock(hub).set_config_problems(nats.problems.clone()) {
        for problem in &nats.problems {
            tracing::warn!("remote: {problem}");
        }
        notify();
    }
}

/// One connection's life, to `nats`'s servers and bucket. Returns whether
/// it got as far as watching, and why it ended.
async fn connect_and_watch(
    hub: &SharedHub,
    setup: &Setup,
    nats: &Nats,
    keys: ViewerKeys,
    notify: &Notify,
) -> (bool, String) {
    let config = cc_remote::Config {
        servers: nats.servers.clone(),
        bucket: nats.bucket.clone(),
        creds_path: setup.creds_path.clone(),
        user: viewer_user(),
        name: "cc-usage-viewer".into(),
    };
    let client = match cc_remote::connect(config).await {
        Ok(client) => client,
        Err(err) => return (false, format!("connect: {err}")),
    };
    let keys = Arc::new(keys);
    // Answers about waiting keys (the notice) refresh these at once.
    lock(hub).set_keys(Some(keys.clone()));
    let result = watch(hub, setup, nats, &client, keys, notify).await;
    // Free the account's connection slot before any retry.
    client.close().await;
    result
}

/// The aliases of the puts in `batch` whose pin files `keys` has not read
/// yet and whose values `hub` would let in: what to read before the batch
/// is applied, so opening it under the hub's lock reads and writes no
/// file.
fn pins_to_read(hub: &SharedHub, keys: &ViewerKeys, batch: &[(KvChange, bool)]) -> Vec<String> {
    let hub = lock(hub);
    let aliases: Vec<String> = batch
        .iter()
        .filter_map(|(change, _)| match change {
            KvChange::Put { key, .. } => KvKey::parse(key).map(|k| k.alias().to_string()),
            KvChange::Delete { .. } => None,
        })
        .filter(|alias| hub.map().admits(alias))
        .collect();
    drop(hub);
    keys.uncached_pins(aliases.iter().map(String::as_str))
}

/// One key check of a live watch, blocking: the stamp to remember and the
/// keys to swap in, if any. While the viewer key file's stamp is still
/// `stamp_before`, only `held`'s pin files are reread. A new stamp loads the
/// keys afresh; when that fails (the creds gone for a moment, a key file
/// caught mid-write by a restore) `held` stays in use with its pins reread
/// and `stamp_before` is kept, so the next check tries again instead of
/// waiting for another change of the file.
fn recheck_keys(
    setup: &Setup,
    held: &ViewerKeys,
    aliases: &[String],
    stamp_before: Option<SystemTime>,
) -> (Option<SystemTime>, Option<ViewerKeys>) {
    let stamp = setup.key_stamp();
    if stamp == stamp_before {
        held.reload_pins(aliases);
        return (stamp, None);
    }
    match setup.check() {
        Ok(fresh) => {
            fresh.load_pins(aliases);
            (stamp, Some(fresh))
        }
        Err(err) => {
            tracing::warn!("remote: keys not reloaded: {err}");
            held.reload_pins(aliases);
            (stamp_before, None)
        }
    }
}

async fn watch(
    hub: &SharedHub,
    setup: &Setup,
    nats: &Nats,
    client: &cc_remote::Client,
    mut keys: Arc<ViewerKeys>,
    notify: &Notify,
) -> (bool, String) {
    let viewer = match Viewer::bind(client, &nats.bucket).await {
        Ok(v) => v,
        Err(err) => return (false, format!("bind {}: {err}", nats.bucket)),
    };
    let mut watch = match viewer.watch_all().await {
        Ok(w) => w,
        Err(err) => return (false, format!("watch: {err}")),
    };
    let mut events = client.events();
    let connected_detail = format!("Watching {} over {}", nats.bucket, client.server());
    if lock(hub).set_state(ConnState::Connected, Some(connected_detail.clone())) {
        notify();
    }

    let mut key_stamp = setup.key_stamp();
    let mut key_check = tokio::time::interval(KEY_RECHECK);
    key_check.tick().await; // the first tick is immediate
    loop {
        let live = watch.is_current();
        tokio::select! {
            change = watch.next() => {
                let Some(change) = change else {
                    let why = watch.last_error().unwrap_or("watch ended").to_string();
                    return (true, format!("watch: {why}"));
                };
                let mut batch = vec![(change, live)];
                // Apply whatever else is already waiting (the initial
                // snapshot arrives in a burst) before notifying once.
                loop {
                    let live = watch.is_current();
                    match watch.next().now_or_never() {
                        Some(Some(change)) => batch.push((change, live)),
                        _ => break,
                    }
                }
                // The pin files of machines not seen yet are read first,
                // off the executor and with the hub let go.
                let missing = pins_to_read(hub, &keys, &batch);
                if !missing.is_empty() {
                    let reading = keys.clone();
                    let _ = tokio::task::spawn_blocking(move || reading.load_pins(&missing)).await;
                }
                let now = now_unix();
                let mut changed = false;
                {
                    let mut hub = lock(hub);
                    for (change, live) in &batch {
                        changed |= hub.map_mut().apply(change, &*keys, now, *live);
                    }
                }
                save_projects_soon(hub);
                save_pending_soon(hub);
                if changed {
                    notify();
                }
            }
            event = events.recv() => {
                let update = match event {
                    Ok(ConnectionEvent::Connected) => {
                        Some((ConnState::Connected, Some(connected_detail.clone())))
                    }
                    Ok(ConnectionEvent::Disconnected) => Some((
                        ConnState::Disconnected,
                        Some("Connection lost; reconnecting".to_string()),
                    )),
                    Ok(ConnectionEvent::Closed) => return (true, "connection closed".into()),
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return (true, "connection events ended".into())
                    }
                };
                if let Some((state, detail)) = update {
                    if lock(hub).set_state(state, detail) {
                        notify();
                    }
                }
            }
            _ = nats_reload().notified() => {
                // Settings saved the connection settings: compared now,
                // not at the next key check.
                let checking = setup.clone();
                if let Ok(now_nats) = tokio::task::spawn_blocking(move || checking.nats()).await {
                    note_nats_problems(hub, &now_nats, notify);
                    if nats_moved(nats, &now_nats) {
                        return (true, "NATS settings changed; reconnecting".into());
                    }
                }
            }
            _ = key_check.tick() => {
                // A rotated or restored viewer key file is picked up
                // without a restart; values sealed with a new key arrive
                // again with the next heartbeat. Pin files are read again
                // too, so a line added by hand (a publisher's rotated key)
                // counts within this interval. Only the machines held keep
                // a cached pin file. All of it on the blocking pool, the
                // files read with no lock held and swapped in.
                let aliases = lock(hub).map().aliases();
                let (checking, held, stamp_before) = (setup.clone(), keys.clone(), key_stamp);
                let reloaded = tokio::task::spawn_blocking(move || {
                    (recheck_keys(&checking, &held, &aliases, stamp_before), checking.nats())
                })
                .await;
                if let Ok(((stamp, fresh), now_nats)) = reloaded {
                    key_stamp = stamp;
                    if let Some(fresh) = fresh {
                        keys = Arc::new(fresh);
                        lock(hub).set_keys(Some(keys.clone()));
                    }
                    // Other servers or another bucket in settings.json:
                    // this connection ends and the next one uses them.
                    note_nats_problems(hub, &now_nats, notify);
                    if nats_moved(nats, &now_nats) {
                        return (true, "NATS settings changed; reconnecting".into());
                    }
                }
                // Events can be missed (lagged channel); the client knows.
                let state = match client.state() {
                    ConnectionState::Connected => ConnState::Connected,
                    _ => ConnState::Disconnected,
                };
                let detail = match state {
                    ConnState::Connected => Some(connected_detail.clone()),
                    _ => Some("Connection lost; reconnecting".to_string()),
                };
                if lock(hub).set_state(state, detail) {
                    notify();
                }
            }
        }
    }
}

/// Re-evaluates the offline and drop rules every [`TICK_INTERVAL`], whether
/// or not the connection is up, and notifies when the rows changed. Also
/// writes a change of the projects held back by [`PROJECTS_SAVE_INTERVAL`],
/// and the keys waiting for review when a write of them is due.
///
/// [`PROJECTS_SAVE_INTERVAL`]: super::PROJECTS_SAVE_INTERVAL
pub async fn run_ticker(hub: SharedHub, notify: Notify) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    loop {
        interval.tick().await;
        let changed = lock(&hub).tick(now_unix());
        save_projects_soon(&hub);
        save_pending_soon(&hub);
        if changed {
            notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_key_reload_is_tried_again_on_the_next_check() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        let setup = Setup {
            creds_path: dir.path().join("nats").join("cc-sessions.viewer.creds"),
            viewer_key_path: keys.join("viewer.key"),
            viewer_pub_path: keys.join("viewer.pub"),
            publishers_dir: keys.join("publishers"),
            settings_dir: None,
        };
        let held = ViewerKeys::new(
            setup.ensure_viewer_keys().unwrap(),
            Some(setup.publishers_dir.clone()),
        );
        let stamp = setup.key_stamp();
        assert!(stamp.is_some());

        // The key file changed since the watch started (`None` stands for
        // the earlier stamp), but the creds are missing for the moment: the
        // reload fails and the earlier stamp is kept, check after check.
        for _ in 0..2 {
            let (kept, fresh) = recheck_keys(&setup, &held, &[], None);
            assert_eq!(kept, None);
            assert!(fresh.is_none());
        }

        // Once the creds are back, the next check loads the keys.
        std::fs::create_dir_all(setup.creds_path.parent().unwrap()).unwrap();
        std::fs::write(&setup.creds_path, "creds").unwrap();
        let (now, fresh) = recheck_keys(&setup, &held, &[], None);
        assert_eq!(now, stamp);
        assert!(fresh.is_some());

        // And with the stamp taken, nothing is reloaded again.
        let (same, fresh) = recheck_keys(&setup, &held, &[], now);
        assert_eq!(same, stamp);
        assert!(fresh.is_none());
    }

    /// A save in Settings ends a wait at once, also one that starts after
    /// the save; the wake is used up by it.
    #[test]
    fn a_saved_connection_setting_cuts_the_wait_short() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            super::super::reload_nats();
            let woke = tokio::time::timeout(
                Duration::from_secs(5),
                wait_or_reload(Duration::from_secs(600)),
            )
            .await;
            assert!(woke.is_ok(), "the wait should end at once");
            let slept = tokio::time::timeout(
                Duration::from_millis(50),
                wait_or_reload(Duration::from_secs(600)),
            )
            .await;
            assert!(slept.is_err(), "no wake left over");
        });
    }
}
