//! The Publisher role: this machine's sessions, host record, projects and
//! activity, published to the `cc-sessions` bucket for the viewer on
//! another PC, as `cc-publish run` does on a machine without the app.
//!
//! A machine with the app must never run `cc-publish` as well (both would
//! drain the cc-hook spool), so when it has to publish, the app does it
//! itself, with cc-publish's own pipeline ([`cc_publish::pipeline`]):
//!
//! - the gathering side is fed from the app's own session pass (the
//!   monitor that already runs every 2 s, see `poller.rs`): its payloads go
//!   through the [`Gatherer`] (planner, 60 s heartbeat, deletes, host
//!   record, projects and activity) into the outbox, [`Publishing::pass`];
//! - the network side is [`cc_publish::pipeline::serve`] on a thread of its
//!   own with a current-thread runtime: one NATS connection, as
//!   `cc-sessions.pub-<alias>`, over TLS-only servers (WebSocket first),
//!   with cc-publish's reconnects, backoff and resync. Servers and bucket
//!   are the viewer's: cc-remote's defaults, or the `"nats"` object of
//!   `settings.json` and the `CC_USAGE_NATS_*` variables
//!   ([`settings::nats`]), read when the role starts; a value that cannot
//!   be used is replaced by the default and named in the footer's tooltip.
//!
//! The alias is this machine's name (Settings, or the computer name). The
//! app seals every value from `keys\publisher.key` to
//! `keys\viewer.pub`: without both files it does not publish. The state goes to
//! `publish-state.json`, the file `cc-publish` keeps, and while publishing
//! the app holds `cc-publish.lock` next to it ([`cc_publish::instance`]),
//! so a `cc-publish run` started next to the app refuses to start, the app
//! refuses to publish next to a live `cc-publish`, and the session keys of
//! a previous run are taken over (those of sessions that ended meanwhile
//! are deleted).
//!
//! Settings → Role: Publisher shows whether the machine is ready
//! ([`readiness`]) and gets it there: [`generate_key`] (and, with the alias
//! typed back, [`replace_key`]), [`import_viewer_key`] and [`import_creds`]. Nothing here knows about Tauri; `commands.rs` opens
//! the dialogs and passes only the paths they return.
//!
//! A running publisher never needs a restart for new files: the network
//! side looks at the key and creds files every 30 s
//! ([`cc_publish::reload`]), and after an import Settings makes it look at
//! once ([`PublisherHub::apply_files`]) and says what it applied. Files that
//! do not load are reported and the keys in use stay.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use cc_core::session::RemotePayload;
use cc_envelope::{PublicKeys, SecretKeys};
use cc_publish::instance::InstanceLock;
use cc_publish::pipeline::{Gatherer, Link, Observer, Pass};
use cc_publish::reload::{Change, KeyFiles, Keys, Reload};
use cc_publish::state::{InUse, PublishState, STATE_FILE};
use serde::Serialize;

use crate::remote::{ConnState, RemoteStatus};
use crate::settings::{self, Nats};

/// A credentials or key file larger than this is not one.
const MAX_IMPORT_BYTES: u64 = 64 * 1024;

/// The NATS user of the publisher `alias`, `cc-sessions.pub-<alias>`.
pub fn publisher_user(alias: &str) -> String {
    cc_publish::config::publisher_user(alias)
}

/// Where the Publisher role finds its files, for one alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// This machine's alias: the first token of every key it writes.
    pub alias: String,
    /// `<data_dir>/nats/cc-sessions.pub-<alias>.creds`.
    pub creds: PathBuf,
    /// `<data_dir>/keys/publisher.key`: this machine's private keys.
    pub publisher_key: PathBuf,
    /// `<data_dir>/keys/viewer.pub`: the viewer's public keys.
    pub viewer_pub: PathBuf,
    /// `<data_dir>/publish-state.json`, shared with `cc-publish`.
    pub state_file: PathBuf,
    /// The servers, the bucket, and what in the settings could not be
    /// used: [`settings::nats`], read when the setup is made (the role
    /// starts once per run, so a change applies on the next start).
    pub nats: Nats,
}

impl Setup {
    /// The files of `alias` under `data_dir`, cc-remote's default servers
    /// and bucket.
    pub fn in_dir(data_dir: &Path, alias: &str) -> Self {
        let keys = data_dir.join("keys");
        Self {
            alias: alias.to_string(),
            creds: data_dir
                .join("nats")
                .join(format!("{}.creds", publisher_user(alias))),
            publisher_key: keys.join("publisher.key"),
            viewer_pub: keys.join("viewer.pub"),
            state_file: data_dir.join(STATE_FILE),
            nats: Nats::default(),
        }
    }

    /// The files of `alias` in the data folder, and the servers and bucket
    /// of `settings.json` in [`settings::default_dir`] (or the
    /// `CC_USAGE_NATS_*` variables). Reads that file.
    pub fn from_data_dir(alias: &str) -> Self {
        Self {
            nats: settings::nats(&settings::default_dir()),
            ..Self::in_dir(&cc_core::paths::data_dir(), alias)
        }
    }

    /// The NATS user, `cc-sessions.pub-<alias>`.
    pub fn user(&self) -> String {
        publisher_user(&self.alias)
    }

    /// The key and creds files, as the pipeline watches them.
    pub fn key_files(&self) -> KeyFiles {
        KeyFiles {
            publisher_key: self.publisher_key.clone(),
            viewer_pub: self.viewer_pub.clone(),
            creds: self.creds.clone(),
            user: self.user(),
        }
    }

    /// The connection config: this alias's user and creds, the servers and
    /// bucket of [`Setup::nats`].
    pub fn remote(&self) -> cc_remote::Config {
        cc_remote::Config {
            servers: self.nats.servers.clone(),
            bucket: self.nats.bucket.clone(),
            creds_path: self.creds.clone(),
            user: self.user(),
            name: format!("cc-usage-publisher-{}", self.alias),
        }
    }
}

/// One line of the readiness list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// ✓ or ✗.
    pub ok: bool,
    /// The file it is about.
    pub path: String,
    /// What is wrong, or what was found. User-facing; never file contents
    /// beyond a NATS user name or a public key.
    pub detail: String,
}

/// What Settings → Role: Publisher shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Readiness {
    /// The alias the machine publishes as.
    pub alias: String,
    /// Its NATS user.
    pub user: String,
    /// The creds file, present and for this user.
    pub creds: Check,
    /// `keys\publisher.key`, present and loading.
    pub publisher_key: Check,
    /// `keys\viewer.pub`, present and loading.
    pub viewer_key: Check,
    /// The public key line of `publisher.key`, the one the viewer's user
    /// accepts, when there is one.
    pub public_key: Option<String>,
    /// All three checks pass.
    pub ready: bool,
}

impl Readiness {
    /// Why it is not ready, one reason per failed check; `None` when ready.
    pub fn reason(&self) -> Option<String> {
        let failed: Vec<&str> = [&self.creds, &self.publisher_key, &self.viewer_key]
            .into_iter()
            .filter(|c| !c.ok)
            .map(|c| c.detail.as_str())
            .collect();
        (!failed.is_empty()).then(|| failed.join("\n"))
    }
}

/// Checks the three files of `setup`. Reads the creds' user name and the
/// keys' public halves, nothing else.
pub fn readiness(setup: &Setup) -> Readiness {
    let user = setup.user();
    let shown = |p: &Path| p.display().to_string();

    let creds = if !setup.creds.is_file() {
        Check {
            ok: false,
            path: shown(&setup.creds),
            detail: format!("No credentials for {user} yet (Import creds…)"),
        }
    } else {
        match cc_publish::check::creds_user_name(&setup.creds) {
            Some(name) if name == user => Check {
                ok: true,
                path: shown(&setup.creds),
                detail: format!("Credentials for {name}"),
            },
            Some(name) => Check {
                ok: false,
                path: shown(&setup.creds),
                detail: format!("The credentials are for {name}, not {user}"),
            },
            None => Check {
                ok: false,
                path: shown(&setup.creds),
                detail: "Not a NATS credentials file".into(),
            },
        }
    };

    let mut public_key = None;
    let publisher_key = if !setup.publisher_key.is_file() {
        Check {
            ok: false,
            path: shown(&setup.publisher_key),
            detail: "No publisher key yet (Generate key)".into(),
        }
    } else {
        match SecretKeys::load(&setup.publisher_key) {
            Ok(keys) => {
                let line = keys.current().public_key().to_line();
                public_key = Some(line);
                Check {
                    ok: true,
                    path: shown(&setup.publisher_key),
                    detail: format!("Publisher key {}", keys.current().kid()),
                }
            }
            Err(err) => Check {
                ok: false,
                path: shown(&setup.publisher_key),
                detail: format!("Unusable publisher key: {err}"),
            },
        }
    };

    let viewer_key = if !setup.viewer_pub.is_file() {
        Check {
            ok: false,
            path: shown(&setup.viewer_pub),
            detail: "No viewer public key yet (Import viewer key…)".into(),
        }
    } else {
        match PublicKeys::load(&setup.viewer_pub) {
            Ok(keys) => Check {
                ok: true,
                path: shown(&setup.viewer_pub),
                detail: format!("Viewer key {}", keys.current().kid()),
            },
            Err(err) => Check {
                ok: false,
                path: shown(&setup.viewer_pub),
                detail: format!("Unusable viewer public key: {err}"),
            },
        }
    };

    let ready = creds.ok && publisher_key.ok && viewer_key.ok;
    Readiness {
        alias: setup.alias.clone(),
        user,
        creds,
        publisher_key,
        viewer_key,
        public_key,
        ready,
    }
}

/// What "Generate key" or "Replace key…" did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedKey {
    /// The public key line, `<kid>:<base64>`, which the viewer's user is
    /// asked to accept.
    pub public_key: String,
    /// False when the key was already there and was kept.
    pub created: bool,
}

/// Creates `keys\publisher.key` when it is missing, with a fresh key id
/// made at `now` (Unix seconds). An existing file is kept: the key is this
/// machine's identity, and the viewer holds back the values of a new one
/// until its user accepts it. [`replace_key`] replaces it on purpose.
pub fn generate_key(setup: &Setup, now: i64) -> Result<GeneratedKey, String> {
    keygen(setup, now, cc_publish::keygen::IfExists::Keep)
}

/// "Replace key…": replaces `keys\publisher.key` with a single fresh key
/// made at `now`, once this machine's alias was typed back as `confirm`
/// (surrounding spaces ignored, as `cc-publish keygen` does). Anything else
/// changes nothing; the error says why. Without a key file it simply makes
/// the first key.
pub fn replace_key(setup: &Setup, confirm: &str, now: i64) -> Result<GeneratedKey, String> {
    use cc_publish::keygen::{check_confirmation, IfExists};
    if !setup.publisher_key.exists() {
        return generate_key(setup, now);
    }
    check_confirmation(&setup.alias, Some(confirm), || None).map_err(|e| e.to_string())?;
    keygen(setup, now, IfExists::Replace)
}

fn keygen(
    setup: &Setup,
    now: i64,
    mode: cc_publish::keygen::IfExists,
) -> Result<GeneratedKey, String> {
    let kid = cc_envelope::new_kid("p", now).map_err(|e| e.to_string())?;
    let generated = cc_publish::keygen::publisher_keygen(&setup.publisher_key, &kid, mode)
        .map_err(|e| format!("Cannot create the publisher key: {e}"))?;
    Ok(GeneratedKey {
        public_key: generated.public.to_line(),
        created: generated.changed,
    })
}

/// What "Import viewer key…" did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedKey {
    /// The newest key id of the imported file.
    pub kid: String,
    /// True when `viewer.pub` already held exactly these keys.
    pub unchanged: bool,
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Reads a small file to import, refusing anything that is not a plain
/// file or is larger than any key or credential.
fn read_small(path: &Path) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a plain file", file_name(path)));
    }
    if meta.len() > MAX_IMPORT_BYTES {
        return Err(format!(
            "{} is too large to be a key or credentials file",
            file_name(path)
        ));
    }
    std::fs::read(path)
        .map(zeroize::Zeroizing::new)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))
}

/// Imports the viewer's public key file `src` (a `viewer.pub` from the
/// viewer machine, or from a publisher bundle) as `keys\viewer.pub`,
/// replacing the one there, atomically. Refuses a file that is not named
/// `*.pub`, does not parse as public key lines, or holds this machine's own
/// publisher key (the wrong file picked).
pub fn import_viewer_key(setup: &Setup, src: &Path) -> Result<ImportedKey, String> {
    if !has_extension(src, "pub") {
        return Err(format!(
            "{} is not a public key file: choose the viewer's viewer.pub",
            file_name(src)
        ));
    }
    let bytes = read_small(src)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("{} is not a viewer public key file", file_name(src)))?;
    let keys = PublicKeys::parse(text)
        .map_err(|e| format!("{} is not a viewer public key file: {e}", file_name(src)))?;
    if let Ok(own) = SecretKeys::load(&setup.publisher_key) {
        let own = own.public_keys();
        if keys.keys().iter().any(|k| own.find(k.bytes()).is_some()) {
            return Err(format!(
                "{} holds this machine's own publisher key, not the viewer's",
                file_name(src)
            ));
        }
    }
    let unchanged = PublicKeys::load(&setup.viewer_pub).ok().as_ref() == Some(&keys);
    if !unchanged {
        keys.save(&setup.viewer_pub)
            .map_err(|e| format!("Cannot write {}: {e}", setup.viewer_pub.display()))?;
    }
    Ok(ImportedKey {
        kid: keys.current().kid().to_string(),
        unchanged,
    })
}

/// Imports the NATS credentials file `src` as
/// `nats\cc-sessions.pub-<alias>.creds`, written privately and atomically.
/// Refuses a file that is not named `*.creds`, is not NATS credentials, or
/// is for another user than this machine's publisher; the error names the
/// user it found, never the secrets. Returns where it was written.
pub fn import_creds(setup: &Setup, src: &Path) -> Result<PathBuf, String> {
    if !has_extension(src, "creds") {
        return Err(format!("{} is not a .creds file", file_name(src)));
    }
    let bytes = read_small(src)?;
    let user = setup.user();
    let looks_like_creds = std::str::from_utf8(&bytes)
        .is_ok_and(|t| t.contains("BEGIN NATS USER JWT") && t.contains("NKEY SEED"));
    let name = cc_publish::check::creds_user_name(src).filter(|_| looks_like_creds);
    match name {
        None => return Err(format!("{} is not a NATS credentials file", file_name(src))),
        Some(name) if name != user => {
            return Err(format!(
                "{} is for the user {name}; this machine publishes as {user} (its name is {})",
                file_name(src),
                setup.alias
            ))
        }
        Some(_) => {}
    }
    crate::backup::write_private(&setup.creds, &bytes)
        .map_err(|e| format!("Cannot write {}: {e}", setup.creds.display()))?;
    Ok(setup.creds.clone())
}

/// Everything [`launch`] needs, checked: what [`prepare`] found.
pub struct Prepared {
    setup: Setup,
    keys: Keys,
    /// Session keys the previous run on this machine published.
    inherited: Vec<String>,
    /// The publisher lock, for the life of the [`Publishing`].
    instance: InstanceLock,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("alias", &self.setup.alias)
            .field("envelope", &self.keys.sealer().describe())
            .field("inherited", &self.inherited.len())
            .finish()
    }
}

/// Checks that `setup` can publish now, for the process `me` at `now`
/// (Unix seconds): the three files ready, the two key files loading as an
/// sealer, the publisher lock free
/// ([`cc_publish::instance`]: it is taken here and held until the process
/// ends) and no live `cc-publish run` in the state file. The reason, when
/// it cannot, is user-facing.
pub fn prepare(setup: &Setup, me: u32, now: f64) -> Result<Prepared, String> {
    let ready = readiness(setup);
    if let Some(reason) = ready.reason() {
        return Err(reason);
    }
    let keys = Keys::load(setup.key_files()).map_err(|e| e.to_string())?;
    // Taken before the state file is read; held by the Publishing.
    let instance = cc_publish::run::acquire_instance(&setup.state_file, me).map_err(|err| match err {
        cc_publish::run::RunError::Locked { pid, .. } => format!(
            "Another publisher{} is already publishing from this machine (cc-publish, or a second cc-usage); stop it (cc-publish stop) and remove its scheduled task",
            pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
        ),
        err => format!("Cannot start publishing: {err}"),
    })?;
    let prev = PublishState::load(&setup.state_file);
    if let Some(prev) = &prev {
        if prev.blocks_run(me, now) {
            return Err(format!(
                "cc-publish (pid {}) is already publishing from this machine; stop it (cc-publish stop) and remove its scheduled task",
                prev.pid
            ));
        }
    }
    let inherited = prev
        .as_ref()
        .map(|p| p.session_keys_for(&setup.alias))
        .unwrap_or_default();
    Ok(Prepared {
        setup: setup.clone(),
        keys,
        inherited,
        instance,
    })
}

/// A running publisher: the gathering side the session pass calls, and
/// the link and keys its network thread shares.
pub struct Publishing {
    alias: String,
    link: Arc<Link>,
    gatherer: Mutex<Gatherer>,
    /// The keys the network thread seals with; `None` offline.
    keys: Option<Arc<Keys>>,
    /// The publisher lock [`prepare`] took; `None` offline.
    _instance: Option<InstanceLock>,
    /// The servers and bucket it publishes to, for the footer.
    nats: Nats,
}

impl std::fmt::Debug for Publishing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Publishing")
            .field("alias", &self.alias)
            .finish_non_exhaustive()
    }
}

impl Publishing {
    /// The gathering side alone, over `link`, for `alias` with the
    /// transcripts under `projects_dir`: what [`launch`] builds, without the
    /// network (tests drain the outbox themselves).
    pub fn offline(
        alias: &str,
        link: Arc<Link>,
        projects_dir: PathBuf,
        inherited: Vec<String>,
    ) -> Self {
        Self {
            alias: alias.to_string(),
            gatherer: Mutex::new(Gatherer::new(alias, projects_dir, inherited)),
            link,
            keys: None,
            _instance: None,
            nats: Nats::default(),
        }
    }

    /// Looks at the key and creds files now, as the network thread does
    /// every 30 s, and wakes it: after an import, so the change applies at
    /// once (new creds: it reconnects). `None` offline.
    pub fn reload(&self) -> Option<Reload> {
        let keys = self.keys.as_ref()?;
        Some(cc_publish::pipeline::reload(keys, &self.link, true))
    }

    /// The alias it publishes as.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// The link shared with the network thread.
    pub fn link(&self) -> &Arc<Link> {
        &self.link
    }

    /// One pass of the app's session monitor: `payloads` are
    /// `SessionMonitor::payloads` for this alias at `now`, `has_registry`
    /// whether that pass had a registry snapshot, `passes` the monitor's
    /// pass count.
    pub fn pass(
        &self,
        payloads: &[RemotePayload],
        has_registry: bool,
        passes: u64,
        now: f64,
    ) -> Pass {
        let pass = self
            .gatherer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .step(payloads, has_registry, now, &self.link);
        self.link.state().passes = passes;
        pass
    }

    /// The footer line now.
    pub fn status(&self) -> RemoteStatus {
        status_from(&self.alias, &self.nats, &self.link.snapshot())
    }
}

/// Starts publishing what `prepared` checked: the network side on a
/// thread of its own (a current-thread runtime, one connection, until the
/// process ends, picking up key and creds changes on its own), `observer`
/// called whenever its state changes. The returned [`Publishing`] is fed by
/// the session pass.
pub fn launch(prepared: Prepared, observer: Observer) -> std::io::Result<Arc<Publishing>> {
    let Prepared {
        setup,
        keys,
        inherited,
        instance,
    } = prepared;
    let keys = Arc::new(keys);
    let link = Link::new(
        cc_publish::run::initial_state(&setup.alias, &inherited),
        Some(setup.state_file.clone()),
        Some(observer),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    {
        let link = Arc::clone(&link);
        let keys = Arc::clone(&keys);
        let remote = setup.remote();
        let alias = setup.alias.clone();
        std::thread::Builder::new()
            .name("publish".into())
            .spawn(move || {
                runtime.block_on(cc_publish::pipeline::serve(
                    &remote,
                    &keys,
                    &alias,
                    &link,
                    std::future::pending::<()>(),
                ));
            })?;
    }
    cc_publish::pipeline::record_keys(&keys, &link);
    link.write_state();
    tracing::info!(
        "publisher: publishing as {} ({}) to {} over {}, {}",
        setup.alias,
        setup.user(),
        setup.nats.bucket,
        setup.nats.servers.join(", "),
        keys.sealer().describe()
    );
    for problem in &setup.nats.problems {
        tracing::warn!("publisher: {problem}");
    }
    let mut publishing = Publishing::offline(
        &setup.alias,
        link,
        cc_local::projects::projects_dir(),
        inherited,
    );
    publishing.keys = Some(keys);
    publishing._instance = Some(instance);
    publishing.nats = setup.nats;
    Ok(Arc::new(publishing))
}

/// The line Settings adds to an import's note when the publisher was
/// running: what the running publisher now uses (new creds: that they are
/// being tried, since they replace the connection only once one made with
/// them is up; how that went shows in the footer), and whether it could
/// (`true` when the files on disk were not applied). `None` when nothing
/// changed for it. User-facing; key ids and user names only.
pub fn applied_note(reload: &Reload, in_use: &InUse) -> Option<(String, bool)> {
    let mut lines = Vec::new();
    let mut failed = false;
    match &reload.keys {
        Change::Applied => lines.push(format!(
            "Applied: now sealing with publisher key {} to viewer key {}.",
            in_use.publisher_kid, in_use.viewer_kid
        )),
        Change::Rejected(why) => {
            failed = true;
            lines.push(format!(
                "Not applied ({why}); still sealing to viewer key {}.",
                in_use.viewer_kid
            ));
        }
        Change::Unchanged => {}
    }
    match &reload.creds {
        Change::Applied => lines.push(
            "Accepted: trying the new credentials; they replace the connection once it is up."
                .into(),
        ),
        Change::Rejected(why) => {
            failed = true;
            lines.push(format!(
                "Not applied ({why}); the connection keeps its credentials."
            ));
        }
        Change::Unchanged => {}
    }
    (!lines.is_empty()).then(|| (lines.join(" "), failed))
}

/// The footer line for `alias` publishing to `nats` from the pipeline's
/// state, with the NATS settings that were ignored. User-facing.
pub fn status_from(alias: &str, nats: &Nats, state: &PublishState) -> RemoteStatus {
    let (conn, mut detail) = if state.connected {
        let mut detail = format!(
            "Publishing {} as {} over {}",
            nats.bucket,
            publisher_user(alias),
            state.server.as_deref().unwrap_or("?")
        );
        detail.push_str(&format!(
            "\n{} {}, {} {}",
            state.sessions,
            if state.sessions == 1 {
                "session"
            } else {
                "sessions"
            },
            state.projects,
            if state.projects == 1 {
                "project"
            } else {
                "projects"
            }
        ));
        (ConnState::Connected, Some(detail))
    } else if state.connects == 0 && state.last_error.is_none() {
        (ConnState::Connecting, None)
    } else {
        (
            ConnState::Disconnected,
            Some(
                state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "Connection lost; reconnecting".into()),
            ),
        )
    };
    // Changed files it could not use: it goes on with the ones in use.
    if let Some(in_use) = &state.in_use {
        let kept = [
            in_use
                .key_error
                .as_ref()
                .map(|e| format!("Key files not applied: {e}")),
            in_use
                .creds_error
                .as_ref()
                .map(|e| format!("Credentials not applied: {e}")),
        ];
        for line in kept.into_iter().flatten() {
            detail = Some(match detail {
                Some(d) => format!("{d}\n{line}"),
                None => line,
            });
        }
    }
    detail = with_lines(detail, &nats.problems);
    RemoteStatus {
        state: conn,
        machines: 0,
        text: publisher_text(alias, conn),
        detail,
        undecryptable: 0,
        pending: Vec::new(),
    }
}

/// `detail` with `lines` added below it, one per line.
fn with_lines(detail: Option<String>, lines: &[String]) -> Option<String> {
    let all: Vec<String> = detail.into_iter().chain(lines.iter().cloned()).collect();
    (!all.is_empty()).then(|| all.join("\n"))
}

/// The footer text of the Publisher role. User-facing; the flyout splits it
/// at the last ` · ` like the viewer's.
pub fn publisher_text(alias: &str, state: ConnState) -> String {
    let what = match state {
        ConnState::NotConfigured => "not configured",
        ConnState::Connecting => "connecting…",
        ConnState::Connected => "connected",
        ConnState::Disconnected => "disconnected · retrying",
    };
    format!("publishing as {alias} · {what}")
}

/// The Publisher role of this run: started once it is ready, then running
/// until the process ends. Shared between the session pass, the commands
/// and the start loop in `main.rs`.
pub struct PublisherHub {
    setup: Setup,
    running: OnceLock<Arc<Publishing>>,
    /// Why it is not running yet.
    reason: Mutex<Option<String>>,
    /// One start attempt at a time.
    starting: Mutex<()>,
    /// The footer line last handed out by [`PublisherHub::status_change`].
    sent: Mutex<Option<RemoteStatus>>,
}

impl std::fmt::Debug for PublisherHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublisherHub")
            .field("alias", &self.setup.alias)
            .field("running", &self.running.get().is_some())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl PublisherHub {
    /// Not running yet, for `setup`.
    pub fn new(setup: Setup) -> Self {
        Self {
            setup,
            running: OnceLock::new(),
            reason: Mutex::new(None),
            starting: Mutex::new(()),
            sent: Mutex::new(None),
        }
    }

    /// The files it uses.
    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    /// The running publisher, once started.
    pub fn running(&self) -> Option<&Arc<Publishing>> {
        self.running.get()
    }

    /// Starts publishing if it is not running yet and the machine is ready
    /// (see [`prepare`]); `observer` builds the state observer of a start.
    /// Returns whether it runs now.
    pub fn try_start(&self, observer: impl FnOnce() -> Observer) -> bool {
        let _one = lock(&self.starting);
        if self.running.get().is_some() {
            return true;
        }
        let started =
            prepare(&self.setup, std::process::id(), cc_local::now_secs()).and_then(|prepared| {
                launch(prepared, observer()).map_err(|e| format!("Cannot start publishing: {e}"))
            });
        match started {
            Ok(publishing) => {
                let _ = self.running.set(publishing);
                *lock(&self.reason) = None;
                true
            }
            Err(reason) => {
                let previous = lock(&self.reason).replace(reason.clone());
                if previous.as_deref() != Some(reason.as_str()) {
                    tracing::warn!("publisher: not publishing: {}", reason.replace('\n', "; "));
                }
                false
            }
        }
    }

    /// After a setup action: when publishing, the running publisher picks
    /// up the files now ([`Publishing::reload`]); the line for Settings
    /// and whether it is a failure ([`applied_note`]). `None` when not
    /// running or nothing changed for it.
    pub fn apply_files(&self) -> Option<(String, bool)> {
        let publishing = self.running.get()?;
        let reload = publishing.reload()?;
        applied_note(&reload, &publishing.link.snapshot().in_use?)
    }

    /// The footer line now: the running publisher's, or "not configured"
    /// with the reason in the tooltip.
    pub fn status(&self) -> RemoteStatus {
        match self.running.get() {
            Some(p) => p.status(),
            None => RemoteStatus {
                state: ConnState::NotConfigured,
                machines: 0,
                text: publisher_text(&self.setup.alias, ConnState::NotConfigured),
                detail: with_lines(
                    lock(&self.reason)
                        .clone()
                        .or_else(|| readiness(&self.setup).reason()),
                    &self.setup.nats.problems,
                ),
                undecryptable: 0,
                pending: Vec::new(),
            },
        }
    }

    /// The footer line when it differs from the one this last returned:
    /// the state observer and the session pass call it often, the flyout
    /// needs only the changes.
    pub fn status_change(&self) -> Option<RemoteStatus> {
        let now = self.status();
        let mut sent = lock(&self.sent);
        (sent.as_ref() != Some(&now)).then(|| {
            *sent = Some(now.clone());
            now
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use cc_core::events::SpoolEvent;
    use cc_core::session::{RegistryEntry, RegistryStatus};
    use cc_local::monitor::{Gathered, SessionMonitor};
    use cc_local::transcript::TranscriptReader;

    /// Not real credentials: a JWT with only a `name` claim and a fake seed.
    fn fake_creds(name: &str) -> String {
        let b64 = |v: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        let jwt = format!(
            "{}.{}.c2ln",
            b64(serde_json::json!({"typ":"JWT","alg":"ed25519-nkey"})),
            b64(serde_json::json!({"name": name, "sub": "UABC"}))
        );
        format!(
            "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
             -----BEGIN USER NKEY SEED-----\nSUAFAKEFAKEFAKE\n------END USER NKEY SEED------\n"
        )
    }

    fn ready_setup(dir: &Path) -> Setup {
        let setup = Setup::in_dir(dir, "devbox");
        std::fs::create_dir_all(setup.creds.parent().unwrap()).unwrap();
        std::fs::write(&setup.creds, fake_creds("cc-sessions.pub-devbox")).unwrap();
        generate_key(&setup, 1_800_000_000).unwrap();
        SecretKeys::generate("k1")
            .unwrap()
            .public_keys()
            .save(&setup.viewer_pub)
            .unwrap();
        setup
    }

    #[test]
    fn the_setup_names_the_alias_files_and_the_viewer_servers() {
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::in_dir(dir.path(), "devbox");
        assert_eq!(
            s.creds,
            dir.path().join("nats").join("cc-sessions.pub-devbox.creds")
        );
        assert_eq!(
            s.publisher_key,
            dir.path().join("keys").join("publisher.key")
        );
        assert_eq!(s.viewer_pub, dir.path().join("keys").join("viewer.pub"));
        assert_eq!(s.state_file, dir.path().join("publish-state.json"));
        let r = s.remote();
        assert_eq!(r.user, "cc-sessions.pub-devbox");
        assert_eq!(r.servers[0], "wss://connect.ngs.global", "WebSocket first");
        assert!(r
            .servers
            .iter()
            .all(|u| u.starts_with("wss://") || u.starts_with("tls://")));
        assert_eq!(r.bucket, "cc-sessions");

        // The viewer's settings name the servers and the bucket.
        let s = Setup {
            nats: crate::settings::resolve_nats(
                Some(r#"{"nats":{"servers":["tls://nats.example:4222"],"bucket":"team"}}"#),
                None,
                None,
            ),
            ..s
        };
        let r = s.remote();
        assert_eq!(
            (r.servers, r.bucket),
            (vec!["tls://nats.example:4222".to_string()], "team".into())
        );
    }

    #[test]
    fn readiness_checks_the_three_files() {
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::in_dir(dir.path(), "devbox");
        let r = readiness(&s);
        assert!(!r.ready && !r.creds.ok && !r.publisher_key.ok && !r.viewer_key.ok);
        assert_eq!(r.public_key, None);
        assert_eq!(r.creds.path, s.creds.display().to_string());
        assert_eq!(r.reason().unwrap().lines().count(), 3);

        std::fs::create_dir_all(s.creds.parent().unwrap()).unwrap();
        std::fs::write(&s.creds, fake_creds("cc-sessions.pub-other")).unwrap();
        let r = readiness(&s);
        assert!(!r.creds.ok);
        assert!(
            r.creds.detail.contains("cc-sessions.pub-other"),
            "{}",
            r.creds.detail
        );
        assert!(!r.creds.detail.contains("SUAFAKE"), "never the seed");

        let s = ready_setup(dir.path());
        let r = readiness(&s);
        assert!(r.ready, "{r:?}");
        assert_eq!(r.reason(), None);
        let line = r.public_key.unwrap();
        assert!(line.starts_with('p') && line.contains(':'), "{line}");
        cc_envelope::PublicKey::parse_line(&line).unwrap();

        std::fs::write(&s.viewer_pub, "garbage\n").unwrap();
        let r = readiness(&s);
        assert!(!r.ready && !r.viewer_key.ok);
        assert!(!r.viewer_key.detail.contains("garbage"));
    }

    #[test]
    fn generate_key_creates_once_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::in_dir(dir.path(), "devbox");
        let first = generate_key(&s, 1_800_000_000).unwrap();
        assert!(first.created);
        let before = std::fs::read(&s.publisher_key).unwrap();
        let again = generate_key(&s, 1_800_000_100).unwrap();
        assert!(!again.created);
        assert_eq!(again.public_key, first.public_key);
        assert_eq!(std::fs::read(&s.publisher_key).unwrap(), before);

        std::fs::write(&s.publisher_key, "broken\n").unwrap();
        assert!(
            generate_key(&s, 1_800_000_200).is_err(),
            "kept, not replaced"
        );
        assert_eq!(
            std::fs::read_to_string(&s.publisher_key).unwrap(),
            "broken\n"
        );
    }

    #[test]
    fn replace_key_needs_the_alias_typed_back() {
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::in_dir(dir.path(), "devbox");
        // No key yet: the first one needs no confirmation.
        let first = replace_key(&s, "", 1_800_000_000).unwrap();
        assert!(first.created);
        let before = std::fs::read(&s.publisher_key).unwrap();
        for wrong in ["", "Devbox", "lab"] {
            let err = replace_key(&s, wrong, 1_800_000_100).unwrap_err();
            assert!(err.contains("nothing changed"), "{err}");
            assert_eq!(
                std::fs::read(&s.publisher_key).unwrap(),
                before,
                "{wrong:?}"
            );
        }
        let replaced = replace_key(&s, " devbox ", 1_800_000_200).unwrap();
        assert!(replaced.created);
        assert_ne!(replaced.public_key, first.public_key);
        let keys = SecretKeys::load(&s.publisher_key).unwrap();
        assert_eq!(keys.keys().len(), 1, "a single fresh key");
        assert_eq!(keys.current().public_key().to_line(), replaced.public_key);
    }

    #[test]
    fn a_viewer_key_import_is_validated_and_written_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::in_dir(dir.path(), "devbox");
        let src = dir.path().join("from-viewer");
        std::fs::create_dir_all(&src).unwrap();

        let viewer = SecretKeys::generate("v20261014-aaaa")
            .unwrap()
            .public_keys();
        let good = src.join("viewer.pub");
        viewer.save(&good).unwrap();

        // Malformed contents and the wrong kind of file are refused.
        let bad = src.join("bad.pub");
        std::fs::write(&bad, "not a key\n").unwrap();
        let err = import_viewer_key(&s, &bad).unwrap_err();
        assert!(err.contains("not a viewer public key file"), "{err}");
        assert!(!err.contains("not a key"), "contents are not echoed: {err}");
        let named = src.join("viewer.key");
        viewer.save(&named).unwrap();
        assert!(import_viewer_key(&s, &named)
            .unwrap_err()
            .contains("not a public key file"));
        let big = src.join("big.pub");
        std::fs::write(&big, vec![b'#'; MAX_IMPORT_BYTES as usize + 1]).unwrap();
        assert!(import_viewer_key(&s, &big)
            .unwrap_err()
            .contains("too large"));
        assert!(
            !s.viewer_pub.exists(),
            "nothing written by a refused import"
        );

        let done = import_viewer_key(&s, &good).unwrap();
        assert_eq!(
            (done.kid.as_str(), done.unchanged),
            ("v20261014-aaaa", false)
        );
        assert_eq!(PublicKeys::load(&s.viewer_pub).unwrap(), viewer);
        assert!(import_viewer_key(&s, &good).unwrap().unchanged);
        let leftovers: Vec<_> = std::fs::read_dir(s.viewer_pub.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(leftovers, ["viewer.pub"], "no temporary file left");

        // This machine's own publisher key is the wrong file.
        let own = generate_key(&s, 1_800_000_000).unwrap();
        let mine = src.join("mine.pub");
        std::fs::write(&mine, format!("{}\n", own.public_key)).unwrap();
        assert!(import_viewer_key(&s, &mine)
            .unwrap_err()
            .contains("own publisher key"));
        assert_eq!(PublicKeys::load(&s.viewer_pub).unwrap(), viewer, "kept");
    }

    #[test]
    fn a_creds_import_must_be_for_this_publisher() {
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::in_dir(dir.path(), "devbox");
        let src = dir.path().join("bundle");
        std::fs::create_dir_all(&src).unwrap();

        let wrong = src.join("cc-sessions.pub-lab-01.creds");
        std::fs::write(&wrong, fake_creds("cc-sessions.pub-lab-01")).unwrap();
        let err = import_creds(&s, &wrong).unwrap_err();
        assert!(
            err.contains("cc-sessions.pub-lab-01") && err.contains("cc-sessions.pub-devbox"),
            "{err}"
        );
        assert!(!err.contains("SUAFAKE"), "never the seed: {err}");

        let viewer = src.join("cc-sessions.viewer.creds");
        std::fs::write(&viewer, fake_creds("cc-sessions.viewer")).unwrap();
        assert!(import_creds(&s, &viewer).is_err());

        let junk = src.join("junk.creds");
        std::fs::write(&junk, "hello").unwrap();
        assert!(import_creds(&s, &junk)
            .unwrap_err()
            .contains("not a NATS credentials file"));
        let txt = src.join("cc-sessions.pub-devbox.txt");
        std::fs::write(&txt, fake_creds("cc-sessions.pub-devbox")).unwrap();
        assert!(import_creds(&s, &txt)
            .unwrap_err()
            .contains("not a .creds file"));
        assert!(!s.creds.exists(), "nothing written by a refused import");

        let good = src.join("cc-sessions.pub-devbox.creds");
        std::fs::write(&good, fake_creds("cc-sessions.pub-devbox")).unwrap();
        assert_eq!(import_creds(&s, &good).unwrap(), s.creds);
        assert_eq!(
            std::fs::read(&s.creds).unwrap(),
            std::fs::read(&good).unwrap()
        );
        assert!(readiness(&s).creds.ok);
    }

    #[test]
    fn the_publisher_refuses_to_start_without_both_v2_keys() {
        let dir = tempfile::tempdir().unwrap();
        let s = ready_setup(dir.path());
        assert!(prepare(&s, std::process::id(), 1.0).is_ok());

        std::fs::remove_file(&s.viewer_pub).unwrap();
        let err = prepare(&s, std::process::id(), 1.0).unwrap_err();
        assert!(err.contains("viewer public key"), "{err}");

        SecretKeys::generate("k1")
            .unwrap()
            .public_keys()
            .save(&s.viewer_pub)
            .unwrap();
        std::fs::remove_file(&s.publisher_key).unwrap();
        let err = prepare(&s, std::process::id(), 1.0).unwrap_err();
        assert!(err.contains("publisher key"), "{err}");

        let hub = PublisherHub::new(s.clone());
        assert!(!hub.try_start(|| unreachable!("never launched")));
        let status = hub.status();
        assert_eq!(status.state, ConnState::NotConfigured);
        assert_eq!(status.text, "publishing as devbox · not configured");
        assert!(status.detail.unwrap().contains("publisher key"));
    }

    #[test]
    fn a_live_cc_publish_on_this_machine_blocks_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let s = ready_setup(dir.path());
        let now = cc_local::now_secs();
        // Pid 0: the OS cannot tell, so a fresh file counts as alive.
        let other = PublishState {
            pid: 0,
            alias: "devbox".into(),
            running: true,
            updated_at: now,
            keys: vec!["devbox.s.0000aaaa".into()],
            ..Default::default()
        };
        other.save(&s.state_file).unwrap();
        let err = prepare(&s, std::process::id(), now).unwrap_err();
        assert!(
            err.contains("cc-publish (pid 0) is already publishing"),
            "{err}"
        );

        // Stopped: its keys are taken over.
        PublishState {
            running: false,
            ..other
        }
        .save(&s.state_file)
        .unwrap();
        let prepared = prepare(&s, std::process::id(), now).unwrap();
        assert_eq!(prepared.inherited, ["devbox.s.0000aaaa"]);
        assert!(prepared
            .keys
            .sealer()
            .describe()
            .starts_with("publisher key "));
    }

    #[test]
    fn the_publisher_lock_is_held_from_prepare_on() {
        let dir = tempfile::tempdir().unwrap();
        let s = ready_setup(dir.path());
        let me = std::process::id();
        let prepared = prepare(&s, me, 1.0).unwrap();
        // A second starter inside the window, before any state is written.
        let err = prepare(&s, me, 1.0).unwrap_err();
        assert!(
            err.starts_with("Another publisher is already publishing"),
            "{err}"
        );

        // A cc-publish run holds it: its state file names it.
        drop(prepared);
        let held =
            InstanceLock::acquire(&cc_publish::instance::lock_path_for(&s.state_file)).unwrap();
        PublishState {
            pid: 4242,
            running: true,
            ..Default::default()
        }
        .save(&s.state_file)
        .unwrap();
        let err = prepare(&s, me, 1.0).unwrap_err();
        assert!(
            err.starts_with("Another publisher (pid 4242) is already publishing"),
            "{err}"
        );
        let hub = PublisherHub::new(s.clone());
        assert!(!hub.try_start(|| unreachable!("never launched")));
        assert!(hub.status().detail.unwrap().contains("pid 4242"));

        drop(held);
        PublishState {
            running: false,
            ..Default::default()
        }
        .save(&s.state_file)
        .unwrap();
        assert!(prepare(&s, me, 1.0).is_ok(), "free again");
    }

    fn reg(id: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: id.into(),
            pid: Some(4242),
            cwd: Some("C:/work/demo-repo".into()),
            name: Some("demo-repo-1".into()),
            kind: Some("interactive".into()),
            status: RegistryStatus::Busy,
            waiting_for: None,
            entrypoint: Some("cli".into()),
            started_at_ms: None,
            background_id: None,
        }
    }

    /// The planner is fed by the app's own monitor: one pass of it, no
    /// second monitor, and its payloads become the session puts.
    #[test]
    fn the_app_monitor_feeds_the_planner() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor =
            SessionMonitor::with_reader(TranscriptReader::with_projects_dir(dir.path()));
        let link = Link::new(PublishState::default(), None, None);
        let publishing = Publishing::offline(
            "devbox",
            Arc::clone(&link),
            dir.path().to_path_buf(),
            Vec::new(),
        );
        let t0 = 1_800_000_000.0;
        let mut start = SpoolEvent::new("UserPromptSubmit", "s1", t0 - 1.0);
        start.cwd = Some("C:/work/demo-repo".into());
        monitor.step(
            Gathered {
                registry: Some(vec![reg("s1")]),
                registry_at: t0,
                events: vec![start],
                ..Default::default()
            },
            t0,
        );
        let payloads = monitor.payloads(publishing.alias(), t0);
        assert_eq!(payloads.len(), 1);
        let pass = publishing.pass(&payloads, true, monitor.passes(), t0);
        assert_eq!((pass.plan.changed, pass.plan.host), (1, true));
        let keys: Vec<String> = link.outbox.take().into_iter().map(|(k, _)| k).collect();
        let session = cc_envelope::session_key("devbox", &cc_envelope::sid_for("s1"));
        assert!(keys.contains(&session), "{keys:?}");
        assert!(keys.contains(&"devbox.host".to_string()), "{keys:?}");
        assert_eq!(link.state().sessions, 1);
        assert_eq!(link.state().passes, 1);

        // The session ends: the next pass deletes its key.
        monitor.step(
            Gathered {
                registry: Some(Vec::new()),
                registry_at: t0 + 2.0,
                events: vec![SpoolEvent::new("SessionEnd", "s1", t0 + 1.5)],
                ..Default::default()
            },
            t0 + 2.0,
        );
        let pass = publishing.pass(
            &monitor.payloads("devbox", t0 + 2.0),
            true,
            monitor.passes(),
            t0 + 2.0,
        );
        assert_eq!(pass.plan.deletes, 1);
        assert_eq!(link.outbox.pending_deletes(), [session]);
    }

    #[test]
    fn the_footer_follows_the_connection() {
        let mut s = PublishState::default();
        let st = status_from("devbox", &Nats::default(), &s);
        assert_eq!(
            (st.state, st.text.as_str()),
            (ConnState::Connecting, "publishing as devbox · connecting…")
        );
        s.last_error = Some("connect: timed out".into());
        let st = status_from("devbox", &Nats::default(), &s);
        assert_eq!(st.state, ConnState::Disconnected);
        assert_eq!(st.text, "publishing as devbox · disconnected · retrying");
        assert_eq!(st.detail.as_deref(), Some("connect: timed out"));
        s.connected = true;
        s.connects = 1;
        s.server = Some("wss://connect.ngs.global".into());
        s.sessions = 2;
        let st = status_from("devbox", &Nats::default(), &s);
        assert_eq!(
            (st.state, st.text.as_str()),
            (ConnState::Connected, "publishing as devbox · connected")
        );
        assert!(st.detail.unwrap().contains("2 sessions"));
        assert_eq!(st.machines, 0);

        // Another bucket, and a setting that was ignored: both in the tooltip.
        let nats = crate::settings::resolve_nats(
            Some(r#"{"nats":{"bucket":"team","servers":["nats://plain:4222"]}}"#),
            None,
            None,
        );
        let detail = status_from("devbox", &nats, &s).detail.unwrap();
        assert!(
            detail.starts_with("Publishing team as cc-sessions.pub-devbox"),
            "{detail}"
        );
        assert!(detail.ends_with("using the default servers"), "{detail}");
    }

    /// A hub publishing for `setup` without the network: the gathering side
    /// and the keys, as [`launch`] builds them.
    fn publishing_hub(setup: &Setup) -> PublisherHub {
        let prepared = prepare(setup, std::process::id(), 1.0).unwrap();
        let link = Link::new(
            PublishState::default(),
            Some(setup.state_file.clone()),
            None,
        );
        let keys = Arc::new(prepared.keys);
        cc_publish::pipeline::record_keys(&keys, &link);
        let projects = setup.state_file.parent().unwrap().join("projects");
        let mut publishing = Publishing::offline(&setup.alias, link, projects, Vec::new());
        publishing.keys = Some(keys);
        let hub = PublisherHub::new(setup.clone());
        let _ = hub.running.set(Arc::new(publishing));
        hub
    }

    #[test]
    fn an_import_while_publishing_applies_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let s = ready_setup(dir.path());
        let hub = publishing_hub(&s);
        assert!(hub.try_start(|| unreachable!("already running")));
        assert_eq!(hub.apply_files(), None, "nothing changed");

        // A rotated viewer key, imported in Settings: in use right away.
        let src = dir.path().join("viewer-rotated.pub");
        SecretKeys::generate("v20261014-dddd")
            .unwrap()
            .public_keys()
            .save(&src)
            .unwrap();
        import_viewer_key(&s, &src).unwrap();
        let (note, failed) = hub.apply_files().unwrap();
        assert!(!failed);
        assert!(
            note.starts_with("Applied:") && note.contains("viewer key v20261014-dddd"),
            "{note}"
        );
        let link = Arc::clone(hub.running().unwrap().link());
        assert!(link.take_resync(), "every value is sealed with it soon");
        let saved = PublishState::load(&s.state_file).unwrap().in_use.unwrap();
        assert_eq!(
            saved.viewer_kid, "v20261014-dddd",
            "recorded in the state file"
        );

        // New creds: a new connection.
        let creds = dir.path().join("cc-sessions.pub-devbox.creds");
        std::fs::write(&creds, fake_creds("cc-sessions.pub-devbox") + "\n").unwrap();
        import_creds(&s, &creds).unwrap();
        let (note, failed) = hub.apply_files().unwrap();
        assert_eq!(
            (note.as_str(), failed),
            (
                "Accepted: trying the new credentials; they replace the connection once it is up.",
                false
            )
        );

        // A key file broken by hand: reported, the keys in use stay.
        std::fs::write(&s.viewer_pub, "garbage\n").unwrap();
        let (note, failed) = hub.apply_files().unwrap();
        assert!(
            failed && note.starts_with("Not applied") && note.contains("v20261014-dddd"),
            "{note}"
        );
        let st = status_from("devbox", &Nats::default(), &link.snapshot());
        assert!(st.detail.unwrap().contains("Key files not applied"));
    }

    #[test]
    fn the_applied_note_names_key_ids_only() {
        let in_use = InUse {
            publisher_kid: "p1".into(),
            viewer_kid: "k2".into(),
            ..Default::default()
        };
        let reload = |keys, creds| Reload { keys, creds };
        assert_eq!(
            applied_note(&reload(Change::Unchanged, Change::Unchanged), &in_use),
            None
        );
        let (note, failed) =
            applied_note(&reload(Change::Applied, Change::Applied), &in_use).unwrap();
        assert!(!failed);
        assert_eq!(
            note,
            "Applied: now sealing with publisher key p1 to viewer key k2. Accepted: trying the new credentials; they replace the connection once it is up."
        );
        let (note, failed) = applied_note(
            &reload(
                Change::Unchanged,
                Change::Rejected("u.creds is for x".into()),
            ),
            &in_use,
        )
        .unwrap();
        assert!(
            failed && note.contains("u.creds is for x") && note.contains("keeps its credentials"),
            "{note}"
        );
    }

    #[test]
    fn the_status_is_handed_out_once_per_change() {
        let dir = tempfile::tempdir().unwrap();
        let hub = PublisherHub::new(Setup::in_dir(dir.path(), "devbox"));
        assert!(hub.status_change().is_some());
        assert!(hub.status_change().is_none());
        assert!(!hub.try_start(|| unreachable!()));
        // The reason went from the readiness list to the failed start's,
        // which says the same: no change for the flyout.
        let _ = hub.status_change();
        assert!(hub.status_change().is_none());
    }
}
