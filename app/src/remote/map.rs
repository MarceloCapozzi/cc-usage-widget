//! [`RemoteMap`]: what the bucket says, opened, and the rules that keep a
//! publisher's values in bounds.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use cc_core::projects::{ActivityPayload, ProjectPayload};
use cc_core::session::{RemotePayload, SessionState};
use cc_envelope::{EnvelopeError, HostPayload, KvKey, OpenKeys, Opened};
use cc_remote::KvChange;
use serde::Serialize;

use super::review::{PendingKey, PendingKeys};
use super::view::{remote_order, remote_view, RemoteRow};
use super::{
    DISMISS_RETENTION_SECS, MAX_ALIASES, MAX_CLOCK_SKEW_SECS, MAX_LOGGED_KEYS,
    MAX_PROJECTS_PER_ALIAS, MAX_SESSIONS_PER_ALIAS, OFFLINE_AFTER_SECS, PROJECT_KEEP_SECS,
};

/// What a value of the bucket was, once looked at.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SessionEntry {
    /// Machine alias, from the KV key (which the envelope authenticates).
    pub(super) alias: String,
    /// The session's hashed id, from the KV key: what the row goes by,
    /// whatever the payload's `sid` says.
    pub(super) sid: String,
    pub(super) payload: RemotePayload,
    /// Unix seconds this viewer last heard of the session (see the module docs).
    pub(super) heard: i64,
    /// KV revision of the write the payload came from.
    revision: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct HostEntry {
    pub(super) payload: HostPayload,
    heard: i64,
}

/// One machine's project, as last heard of. Also the shape of an entry of
/// [`REMOTE_PROJECTS_FILE`](super::REMOTE_PROJECTS_FILE).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct StoredProject {
    /// The KV key, `<alias>.p.<pid>`.
    pub key: String,
    /// Machine alias, from the KV key (which the envelope authenticates).
    pub alias: String,
    /// What the publisher sent.
    pub payload: ProjectPayload,
    /// Unix seconds this viewer last heard of it.
    pub heard: i64,
}

impl StoredProject {
    fn expired(&self, now: i64) -> bool {
        now.saturating_sub(self.payload.last.unwrap_or(self.heard)) > PROJECT_KEEP_SECS
    }
}

/// What the bucket says, opened. Keys are the KV keys (`<alias>.s.<sid>`)
/// for sessions and the alias for hosts.
#[derive(Debug, Default)]
pub struct RemoteMap {
    sessions: BTreeMap<String, SessionEntry>,
    pub(super) hosts: BTreeMap<String, HostEntry>,
    /// Session keys dismissed by [`RemoteMap::dismiss_all`]: the KV revision
    /// they held then, when, and whether the session needed the user then.
    /// Writes up to that revision are ignored.
    pub(super) dismissed: BTreeMap<String, (u64, i64, bool)>,
    /// Session keys whose live write moved them into *needs you*, for a
    /// toast ([`RemoteMap::take_needs_you`]).
    needs_you: BTreeSet<String>,
    /// Seconds after the last heartbeat a row (or a host) is dropped;
    /// `None` (the default) keeps it until it is cleared.
    drop_after: Option<i64>,
    /// Values that failed to open, in total.
    undecryptable: u64,
    /// Values rejected because their sender key is not the one pinned for
    /// the alias (pins in memory) or the pin file is unusable, by alias.
    /// Cleared when the alias sends one that opens.
    unknown_senders: BTreeMap<String, u64>,
    /// Sender keys not pinned for their alias, waiting for the user's
    /// consent (see [`super::review`]).
    pub(super) pending: PendingKeys,
    /// Values dropped because their sender key was rejected, by alias.
    rejected_values: BTreeMap<String, u64>,
    /// Keys already logged as unreadable, so each is logged once; at most
    /// [`MAX_LOGGED_KEYS`], then cleared.
    pub(super) logged: HashSet<String>,
    /// The machines held, at most [`MAX_ALIASES`]: added when a value of a
    /// new one is let in, rebuilt from what is held by [`RemoteMap::prune`].
    aliases: BTreeSet<String>,
    /// Values ignored because they came from a machine past [`MAX_ALIASES`].
    over_alias_cap: u64,
    /// What [`RemoteMap::tick`] last saw, to report only real changes.
    last_shape: Option<Shape>,
    /// Other machines' projects, by KV key.
    projects: BTreeMap<String, StoredProject>,
    /// Set when `projects` changed since [`RemoteMap::take_projects_changed`].
    projects_changed: bool,
    /// Each machine's last activity value, by alias.
    activity: BTreeMap<String, ActivityPayload>,
    /// This machine's own name: values under it are ignored (see
    /// [`RemoteMap::set_own_alias`]).
    own_alias: Option<String>,
}

/// What the flyout shows that time alone can change.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shape {
    rows: Vec<(String, SessionState)>,
    machines: usize,
}

impl RemoteMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets how many seconds after its last heartbeat an offline row is
    /// dropped, or `None` to keep it. Never less than
    /// [`OFFLINE_AFTER_SECS`], so a row always shows offline before it goes.
    pub fn set_drop_after(&mut self, secs: Option<i64>) {
        self.drop_after = secs.map(|s| s.max(OFFLINE_AFTER_SECS));
    }

    /// The drop delay in force, seconds; `None` keeps offline rows.
    pub fn drop_after(&self) -> Option<i64> {
        self.drop_after
    }

    /// Ignores every value under `alias`, this machine's own name: a viewer
    /// that published before (Publisher role, or `cc-publish`) may still
    /// find its own sessions, host record, projects and activity in the
    /// bucket, and they are already shown as local. What is held under it
    /// goes at once, projects included (saved without them). Returns true if
    /// anything was dropped.
    pub fn set_own_alias(&mut self, alias: Option<String>) -> bool {
        self.own_alias = alias;
        let Some(own) = self.own_alias.clone() else {
            return false;
        };
        let before =
            self.sessions.len() + self.hosts.len() + self.projects.len() + self.activity.len();
        self.sessions.retain(|_, e| e.alias != own);
        self.hosts.remove(&own);
        let projects = self.projects.len();
        self.projects.retain(|_, p| p.alias != own);
        if self.projects.len() != projects {
            self.projects_changed = true;
        }
        self.activity.remove(&own);
        self.unknown_senders.remove(&own);
        self.pending.drop_alias(&own);
        self.rejected_values.remove(&own);
        self.rebuild_aliases();
        before != self.sessions.len() + self.hosts.len() + self.projects.len() + self.activity.len()
    }

    fn is_own(&self, alias: &str) -> bool {
        self.own_alias.as_deref() == Some(alias)
    }

    /// Whether a value of `alias` would be let in now: not this machine's
    /// own, and a machine already held or room for one more (see
    /// [`MAX_ALIASES`]). Lets the watch read ahead only the pin files of
    /// machines that count.
    pub fn admits(&self, alias: &str) -> bool {
        !self.is_own(alias) && (self.aliases.contains(alias) || self.aliases.len() < MAX_ALIASES)
    }

    /// The machines held, by alias.
    pub fn aliases(&self) -> Vec<String> {
        self.aliases.iter().cloned().collect()
    }

    /// Values ignored because their machine was past [`MAX_ALIASES`].
    pub fn over_alias_cap(&self) -> u64 {
        self.over_alias_cap
    }

    /// Lets `alias` in when it is held or there is room (and holds it from
    /// now on); otherwise counts the value as ignored, and says so in the
    /// log the first time.
    fn admit(&mut self, alias: &str) -> bool {
        if self.aliases.contains(alias) {
            return true;
        }
        if self.aliases.len() < MAX_ALIASES {
            self.aliases.insert(alias.to_string());
            return true;
        }
        self.over_alias_cap = self.over_alias_cap.saturating_add(1);
        if self.over_alias_cap == 1 {
            tracing::warn!(
                "remote: more than {MAX_ALIASES} machines in the bucket; values of further machines are ignored"
            );
        }
        false
    }

    /// Makes [`Self::aliases`] the machines something is held for, which
    /// frees the place of a machine whose last row, host record, project,
    /// activity, waiting key or rejected-value count went.
    fn rebuild_aliases(&mut self) {
        let mut held: BTreeSet<&str> = BTreeSet::new();
        held.extend(self.sessions.values().map(|e| e.alias.as_str()));
        held.extend(self.hosts.keys().map(String::as_str));
        held.extend(self.projects.values().map(|p| p.alias.as_str()));
        held.extend(self.activity.keys().map(String::as_str));
        held.extend(self.unknown_senders.keys().map(String::as_str));
        held.extend(self.pending.aliases().map(String::as_str));
        held.extend(self.rejected_values.keys().map(String::as_str));
        let held: BTreeSet<String> = held.into_iter().map(str::to_string).collect();
        self.aliases = held;
    }

    /// Hides every row until its key receives a write newer than the one
    /// shown now (the widget's clear button). Returns how many rows there
    /// were. A session that is still publishing comes back with its next
    /// heartbeat; one that stopped does not, not even when a reconnect
    /// replays its last value from the bucket.
    pub fn dismiss_all(&mut self, now: i64) -> usize {
        let n = self.sessions.len();
        for (key, e) in std::mem::take(&mut self.sessions) {
            let needed_you = e.payload.state == SessionState::NeedsYou;
            self.dismissed.insert(key, (e.revision, now, needed_you));
        }
        self.needs_you.clear();
        n
    }

    /// Applies one change from the watch at local time `now`. `live` is
    /// false for the entries of the initial snapshot. Returns true when what
    /// the flyout shows may have changed.
    pub fn apply<K: OpenKeys>(
        &mut self,
        change: &KvChange,
        keys: &K,
        now: i64,
        live: bool,
    ) -> bool {
        match change {
            KvChange::Put {
                key,
                bytes,
                revision,
            } => self.put(key, bytes, *revision, keys, now, live),
            KvChange::Delete { key } => self.delete(key),
        }
    }

    fn put(
        &mut self,
        key: &str,
        bytes: &[u8],
        revision: u64,
        keys: &impl OpenKeys,
        now: i64,
        live: bool,
    ) -> bool {
        let Some(parsed) = KvKey::parse(key) else {
            self.log_once(key, "not a cc-sessions key, ignored");
            return false;
        };
        if self.is_own(parsed.alias()) {
            // This machine's own values: shown as local already.
            return false;
        }
        if !self.admit(parsed.alias()) {
            // One machine too many: not even opened, so no pin is read or
            // written for it either.
            return false;
        }
        let heard = |hb: i64| if live { now } else { hb.min(now) };
        match parsed {
            KvKey::Session { alias, sid } => {
                // The state the user last saw this session in: its row, or
                // the row the clear button took away.
                let mut before = self.sessions.get(key).map(|e| e.payload.state);
                if let Some(&(dismissed_at, _, needed_you)) = self.dismissed.get(key) {
                    if revision <= dismissed_at {
                        // The value that was dismissed, replayed.
                        return false;
                    }
                    self.dismissed.remove(key);
                    before = before.or(needed_you.then_some(SessionState::NeedsYou));
                }
                let opened = keys.open_any(bytes, key);
                let opened = self.opened(&alias, opened);
                let changed = self.put_session(key, (alias, sid), opened, revision, now, heard);
                self.note_needs_you(key, before, live);
                changed
            }
            KvKey::Project { alias, .. } => {
                let opened = keys.open_any::<ProjectPayload>(bytes, key);
                match self.opened(&alias, opened) {
                    Ok(payload) if payload.machine != alias => self.reject_machine(key),
                    Ok(mut payload) => {
                        let held = self.projects.get(key).map(|p| p.payload.hb);
                        if is_replay(payload.hb, held) {
                            self.log_once(key, "older than the value held, ignored");
                            return false;
                        }
                        if held.is_none()
                            && count_prefixed(&self.projects, &format!("{alias}.p."))
                                >= MAX_PROJECTS_PER_ALIAS
                        {
                            self.log_once(key, "too many projects for one machine, ignored");
                            return false;
                        }
                        bound_project(&mut payload, now);
                        let entry = StoredProject {
                            key: key.to_string(),
                            alias,
                            heard: heard(payload.hb),
                            payload,
                        };
                        // A re-put that only moves `hb` is not worth a write.
                        let same = |a: &StoredProject, b: &StoredProject| {
                            a.alias == b.alias
                                && ProjectPayload {
                                    hb: 0,
                                    ..a.payload.clone()
                                } == ProjectPayload {
                                    hb: 0,
                                    ..b.payload.clone()
                                }
                        };
                        let previous = self.projects.insert(key.to_string(), entry.clone());
                        if !previous.is_some_and(|p| same(&p, &entry)) {
                            self.projects_changed = true;
                        }
                    }
                    Err(err) => return self.reject(key, &err, now),
                }
                // The session list is not affected.
                false
            }
            KvKey::Activity { alias } => {
                let opened = keys.open_any::<ActivityPayload>(bytes, key);
                match self.opened(&alias, opened) {
                    Ok(payload) if payload.machine != alias => self.reject_machine(key),
                    Ok(mut payload) => {
                        let held = self.activity.get(&alias).map(|a| a.hb);
                        if is_replay(payload.hb, held) {
                            self.log_once(key, "older than the value held, ignored");
                        } else {
                            payload.hb = cap_hb(payload.hb, now);
                            self.activity.insert(alias, payload);
                        }
                    }
                    Err(err) => return self.reject(key, &err, now),
                }
                false
            }
            KvKey::Host { alias } => {
                let opened = keys.open_any::<HostPayload>(bytes, key);
                match self.opened(&alias, opened) {
                    Ok(payload) if payload.machine != alias => {
                        self.reject_machine(key);
                        self.hosts.remove(&alias).is_some()
                    }
                    Ok(mut payload) => {
                        if is_replay(payload.hb, self.hosts.get(&alias).map(|h| h.payload.hb)) {
                            self.log_once(key, "older than the value held, ignored");
                            return false;
                        }
                        payload.hb = cap_hb(payload.hb, now);
                        let entry = HostEntry {
                            heard: heard(payload.hb),
                            payload,
                        };
                        self.hosts.insert(alias, entry.clone()) != Some(entry)
                    }
                    Err(err) => {
                        let notice = self.reject(key, &err, now);
                        self.hosts.remove(&alias).is_some() || notice
                    }
                }
            }
        }
    }

    /// Books a session value of `key`, whose alias and sid are `ids`.
    fn put_session(
        &mut self,
        key: &str,
        ids: (String, String),
        opened: Result<RemotePayload, cc_envelope::EnvelopeError>,
        revision: u64,
        now: i64,
        heard: impl Fn(i64) -> i64,
    ) -> bool {
        let (alias, sid) = ids;
        match opened {
            Ok(payload) if payload.machine != alias => {
                self.reject_machine(key);
                // Like a value that does not open: the row it replaced goes.
                self.sessions.remove(key).is_some()
            }
            Ok(mut payload) => {
                let held = self.sessions.get(key).map(|e| e.payload.hb);
                if is_replay(payload.hb, held) {
                    self.log_once(key, "older than the value held, ignored");
                    return false;
                }
                if held.is_none()
                    && count_prefixed(&self.sessions, &format!("{alias}.s."))
                        >= MAX_SESSIONS_PER_ALIAS
                {
                    self.log_once(key, "too many sessions for one machine, ignored");
                    return false;
                }
                payload.hb = cap_hb(payload.hb, now);
                let entry = SessionEntry {
                    alias,
                    sid,
                    heard: heard(payload.hb),
                    payload,
                    revision,
                };
                // A heartbeat that changes nothing but the revision is not a
                // change the flyout can see.
                let shown = |e: &SessionEntry| (e.alias.clone(), e.payload.clone(), e.heard);
                let previous = self.sessions.insert(key.to_string(), entry.clone());
                previous.as_ref().map(shown) != Some(shown(&entry))
            }
            Err(err) => {
                let notice = self.reject(key, &err, now);
                // The bucket no longer holds the value that row came from.
                self.sessions.remove(key).is_some() || notice
            }
        }
    }

    /// After a write of `key`: notes it for a toast when the write was
    /// heard live and moved the session into *needs you* from `before` (a
    /// write that was not taken leaves the state as it was). Values of the
    /// initial snapshot never toast: the sessions already waiting when the
    /// viewer connects (or reconnects) would all fire at once. Nor does a
    /// session that needed the user before a clear and still does when it
    /// comes back. A session that no longer needs the user loses a toast
    /// not taken yet.
    fn note_needs_you(&mut self, key: &str, before: Option<SessionState>, live: bool) {
        let needs_you = self
            .sessions
            .get(key)
            .is_some_and(|e| e.payload.state == SessionState::NeedsYou);
        if !needs_you {
            self.needs_you.remove(key);
        } else if live && before != Some(SessionState::NeedsYou) {
            self.needs_you.insert(key.to_string());
        }
    }

    /// The rows that moved into *needs you* on a live write since the last
    /// call and still need the user at `now` (not offline, not cleared),
    /// for a toast each.
    pub fn take_needs_you(&mut self, now: i64) -> Vec<RemoteRow> {
        let keys = std::mem::take(&mut self.needs_you);
        if keys.is_empty() {
            return Vec::new();
        }
        self.sessions
            .iter()
            .filter(|(key, e)| {
                keys.contains(*key) && effective_state(e, now) == SessionState::NeedsYou
            })
            .map(|(key, e)| RemoteRow {
                alias: e.alias.clone(),
                view: remote_view(key, e, now),
            })
            .collect()
    }

    fn delete(&mut self, key: &str) -> bool {
        match KvKey::parse(key) {
            Some(KvKey::Session { .. }) => {
                self.dismissed.remove(key);
                self.needs_you.remove(key);
                self.sessions.remove(key).is_some()
            }
            Some(KvKey::Host { alias }) => self.hosts.remove(&alias).is_some(),
            // Kept: see the module docs.
            Some(KvKey::Project { .. } | KvKey::Activity { .. }) => false,
            None => false,
        }
    }

    /// Books a value that opened (or not) for `alias`: a sender key pinned
    /// on first use (pins in memory only, tests), and a rejected sender
    /// that is accepted again. Returns the payload.
    fn opened<T>(
        &mut self,
        alias: &str,
        result: Result<Opened<T>, EnvelopeError>,
    ) -> Result<T, EnvelopeError> {
        let opened = result?;
        if let Some(pin) = &opened.pinned {
            tracing::info!(
                "remote: pinned the key of {alias} on first use (in memory): {}",
                pin.to_line()
            );
        }
        self.unknown_senders.remove(alias);
        Ok(opened.value)
    }

    /// Books a value that did not open, at `now`. A key not pinned for its
    /// alias is noted for review, not counted as unreadable; a rejected
    /// one is counted apart. Returns true when the keys waiting for review
    /// changed (the notice must be drawn again).
    fn reject(&mut self, key: &str, err: &EnvelopeError, now: i64) -> bool {
        match err {
            EnvelopeError::NewSender {
                alias,
                key: sender,
                known,
            } => {
                if self.pending.note(alias, sender, *known, now) {
                    return true;
                }
                if !self.pending.aliases().any(|a| a == alias) {
                    self.log_once(
                        key,
                        "a new sender key, not noted: too many machines wait for review",
                    );
                }
                return false;
            }
            EnvelopeError::RejectedSender { alias } => {
                let n = self.rejected_values.entry(alias.clone()).or_insert(0);
                *n = n.saturating_add(1);
                self.log_once(key, "sealed by a rejected sender key, dropped");
                return false;
            }
            // A pin file that holds no usable key (a revoked machine)
            // rejects the alias the same way as a key that is not pinned.
            EnvelopeError::UnknownSender { alias } | EnvelopeError::Pins { alias, .. } => {
                let n = self.unknown_senders.entry(alias.clone()).or_insert(0);
                *n = n.saturating_add(1);
            }
            _ => {}
        }
        self.undecryptable = self.undecryptable.saturating_add(1);
        // The error names the failure (unknown key id, authentication), never
        // the value or the key.
        self.log_once(key, &format!("cannot open value: {err}"));
        false
    }

    /// The user answered about the waiting key `line` of `alias`: it leaves
    /// the list; when it was accepted the alias's other waiting keys become
    /// new keys of a known machine, and its warnings go.
    pub(super) fn resolve_pending(&mut self, alias: &str, line: &str, accepted: bool) {
        self.pending.remove(alias, line);
        if accepted {
            self.pending.mark_known(alias);
            self.unknown_senders.remove(alias);
        }
    }

    /// Adds keys read from the `.pending` files: their machines count as
    /// held, up to [`MAX_ALIASES`]; this machine's own name is skipped.
    pub(super) fn load_pending(&mut self, entries: Vec<PendingKey>) {
        let entries: Vec<PendingKey> = entries
            .into_iter()
            .filter(|e| !self.is_own(&e.alias))
            .collect();
        self.pending.load(entries);
        let aliases: Vec<String> = self.pending.aliases().cloned().collect();
        for alias in aliases {
            if self.admits(&alias) {
                self.aliases.insert(alias);
            } else {
                self.pending.drop_alias(&alias);
            }
        }
    }

    /// The keys waiting for review.
    pub fn pending_keys(&self) -> Vec<PendingKey> {
        self.pending.list()
    }

    /// Values dropped because their sender key was rejected, by alias.
    pub fn rejected_values(&self) -> Vec<(String, u64)> {
        self.rejected_values
            .iter()
            .map(|(a, n)| (a.clone(), *n))
            .collect()
    }

    /// Rejects a value that opened but names another machine than the alias
    /// of its key: counted like a value that does not open. The envelope
    /// binds the value to the key, so only a broken publisher writes one.
    fn reject_machine(&mut self, key: &str) {
        self.undecryptable = self.undecryptable.saturating_add(1);
        self.log_once(key, "the value names another machine, rejected");
    }

    pub(super) fn log_once(&mut self, key: &str, what: &str) {
        if self.logged.contains(key) {
            return;
        }
        if self.logged.len() >= MAX_LOGGED_KEYS {
            self.logged.clear();
        }
        self.logged.insert(key.to_string());
        tracing::warn!("remote key {key}: {what}");
    }

    /// Drops rows and hosts last heard of longer ago than the drop delay
    /// (none unless [`Self::set_drop_after`] set one), and
    /// forgets dismissals older than [`DISMISS_RETENTION_SECS`]. A machine
    /// left with nothing held frees its place among the [`MAX_ALIASES`].
    /// Returns true if a row or a host was dropped.
    pub fn prune(&mut self, now: i64) -> bool {
        let before = self.sessions.len() + self.hosts.len();
        if let Some(drop_after) = self.drop_after {
            self.sessions
                .retain(|_, e| now.saturating_sub(e.heard) <= drop_after);
            self.hosts.retain(|_, h| now - h.heard <= drop_after);
        }
        self.dismissed
            .retain(|_, (_, at, _)| now - *at <= DISMISS_RETENTION_SECS);
        let projects = self.projects.len();
        self.projects.retain(|_, p| !p.expired(now));
        if self.projects.len() != projects {
            self.projects_changed = true;
        }
        self.rebuild_aliases();
        before != self.sessions.len() + self.hosts.len()
    }

    /// Other machines' activity, by alias.
    pub fn activity(&self) -> Vec<(String, ActivityPayload)> {
        self.activity
            .iter()
            .map(|(a, p)| (a.clone(), p.clone()))
            .collect()
    }

    /// Other machines' projects, by key.
    pub fn projects(&self) -> Vec<StoredProject> {
        self.projects.values().cloned().collect()
    }

    /// Adds projects saved by an earlier run (a newer value already heard
    /// wins), dropping expired ones. The file (or a restored backup) is
    /// checked like the bucket: the key, its alias and the payload's machine
    /// must agree, every project is cut like a value heard live
    /// (`bound_project`: the name, the models, timestamps cut to
    /// [`MAX_CLOCK_SKEW_SECS`] ahead), at most [`MAX_PROJECTS_PER_ALIAS`]
    /// per machine and [`MAX_ALIASES`] machines (one past that is skipped
    /// without counting). Returns how many were added and how many replaced
    /// an older value, and marks the projects changed when either is not
    /// zero.
    pub fn load_projects(&mut self, stored: Vec<StoredProject>, now: i64) -> (usize, usize) {
        let (mut added, mut updated) = (0, 0);
        for mut p in stored {
            let valid = KvKey::parse(&p.key)
                .is_some_and(|k| matches!(&k, KvKey::Project { alias, .. } if *alias == p.alias))
                && p.payload.machine == p.alias;
            if !valid || self.is_own(&p.alias) {
                continue;
            }
            bound_project(&mut p.payload, now);
            p.heard = p.heard.min(now);
            if p.expired(now) {
                continue;
            }
            match self.projects.get(&p.key) {
                Some(have) if have.heard >= p.heard => {}
                Some(_) => {
                    self.projects.insert(p.key.clone(), p);
                    updated += 1;
                }
                None => {
                    if !self.admits(&p.alias) {
                        continue;
                    }
                    if count_prefixed(&self.projects, &format!("{}.p.", p.alias))
                        < MAX_PROJECTS_PER_ALIAS
                    {
                        self.aliases.insert(p.alias.clone());
                        self.projects.insert(p.key.clone(), p);
                        added += 1;
                    }
                }
            }
        }
        if added + updated > 0 {
            self.projects_changed = true;
        }
        (added, updated)
    }

    /// Whether the projects changed since the last call (and resets it).
    pub fn take_projects_changed(&mut self) -> bool {
        std::mem::take(&mut self.projects_changed)
    }

    /// Prunes and reports whether what the flyout shows changed since the
    /// previous tick (a row went offline or was dropped, the machine count
    /// moved).
    pub fn tick(&mut self, now: i64) -> bool {
        let pruned = self.prune(now);
        let shape = Shape {
            rows: self
                .sessions
                .iter()
                .map(|(key, e)| (key.clone(), effective_state(e, now)))
                .collect(),
            machines: self.machines(now),
        };
        let changed = self.last_shape.as_ref() != Some(&shape);
        self.last_shape = Some(shape);
        pruned || changed
    }

    /// The remote rows at `now`, sorted by state rank, then the most recent
    /// state change first, then key.
    pub fn rows(&self, now: i64) -> Vec<RemoteRow> {
        let mut rows: Vec<RemoteRow> = self
            .sessions
            .iter()
            .map(|(key, e)| RemoteRow {
                alias: e.alias.clone(),
                view: remote_view(key, e, now),
            })
            .collect();
        rows.sort_by(|a, b| remote_order(&a.view, &b.view));
        rows
    }

    /// Distinct machines with a fresh host record or a fresh session.
    pub fn machines(&self, now: i64) -> usize {
        let fresh = |heard: i64| now.saturating_sub(heard) <= OFFLINE_AFTER_SECS;
        let mut aliases: BTreeSet<&str> = BTreeSet::new();
        for (alias, h) in &self.hosts {
            if fresh(h.heard) {
                aliases.insert(alias);
            }
        }
        for e in self.sessions.values() {
            if fresh(e.heard) {
                aliases.insert(&e.alias);
            }
        }
        aliases.len()
    }

    /// How many values failed to open since the app started.
    pub fn undecryptable(&self) -> u64 {
        self.undecryptable
    }

    /// Aliases whose values are being rejected for an unknown sender key,
    /// with how many were rejected.
    pub fn unknown_senders(&self) -> Vec<(String, u64)> {
        self.unknown_senders
            .iter()
            .map(|(a, n)| (a.clone(), *n))
            .collect()
    }

    /// Number of sessions held, offline ones included.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// True when no remote session is held.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Whether a value whose heartbeat is `hb` is older than the one held for
/// its key (`held`): an old value put again, which must not replace the
/// current one. An equal `hb` is not a replay (the snapshot after a
/// reconnect repeats what is held). What is held went through [`cap_hb`]
/// when it was stored, so a value dated far in the future shuts its key for
/// [`MAX_CLOCK_SKEW_SECS`] at most, not for good.
///
/// A publisher whose clock steps back has its writes ignored until its
/// clock passes the last heartbeat held again; with heartbeats every 60 s
/// and the small corrections NTP makes, that is a heartbeat or two.
pub(super) fn is_replay(hb: i64, held: Option<i64>) -> bool {
    held.is_some_and(|held| hb < held)
}

/// A publisher's heartbeat as it is stored: at most [`MAX_CLOCK_SKEW_SECS`]
/// past this clock's `now`.
pub(super) fn cap_hb(hb: i64, now: i64) -> i64 {
    hb.min(now.saturating_add(MAX_CLOCK_SKEW_SECS))
}

/// How many keys of `map` start with `prefix` (`<alias>.s.`, `<alias>.p.`).
/// The keys sort together, so only those are visited.
fn count_prefixed<V>(map: &BTreeMap<String, V>, prefix: &str) -> usize {
    use std::ops::Bound;
    map.range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
        .take_while(|(k, _)| k.starts_with(prefix))
        .count()
}

/// Holds a project to what a publisher sends ([`ProjectPayload::new`]),
/// whether heard live, loaded from the file or restored from a backup: the
/// name cut to [`MAX_PROJECT_NAME`] characters; the first
/// [`MAX_PROJECT_MODELS`] models kept in the order sent, their names cut to
/// [`MAX_REMOTE_MODEL`] characters, and the tokens of the rest (and of any
/// [`OTHER_MODELS`] entry) summed under one [`OTHER_MODELS`] entry at the
/// end, so nothing is lost; `last` and `hb` cut to [`MAX_CLOCK_SKEW_SECS`]
/// past `now`, since a future `last` would keep the project from ever
/// expiring. A value from a real publisher comes out as it went in, apart
/// from the timestamps.
///
/// [`MAX_PROJECT_NAME`]: cc_core::projects::MAX_PROJECT_NAME
/// [`MAX_PROJECT_MODELS`]: cc_core::projects::MAX_PROJECT_MODELS
/// [`OTHER_MODELS`]: cc_core::projects::OTHER_MODELS
/// [`MAX_REMOTE_MODEL`]: cc_core::session::MAX_REMOTE_MODEL
pub(super) fn bound_project(payload: &mut ProjectPayload, now: i64) {
    use cc_core::projects::{
        ModelTokens, Tokens, MAX_PROJECT_MODELS, MAX_PROJECT_NAME, OTHER_MODELS,
    };
    use cc_core::session::MAX_REMOTE_MODEL;
    if payload.name.chars().count() > MAX_PROJECT_NAME {
        payload.name = payload.name.chars().take(MAX_PROJECT_NAME).collect();
    }
    let mut kept: Vec<ModelTokens> = Vec::with_capacity(MAX_PROJECT_MODELS + 1);
    let mut rest: Option<Tokens> = None;
    for mut m in std::mem::take(&mut payload.models) {
        if m.model != OTHER_MODELS && kept.len() < MAX_PROJECT_MODELS {
            if m.model.chars().count() > MAX_REMOTE_MODEL {
                m.model = m.model.chars().take(MAX_REMOTE_MODEL).collect();
            }
            kept.push(m);
        } else {
            rest.get_or_insert_with(Tokens::default).add(&m.tokens());
        }
    }
    if let Some(t) = rest {
        kept.push(ModelTokens {
            model: OTHER_MODELS.to_string(),
            input: t.input,
            output: t.output,
            cache_creation: t.cache_creation,
            cache_read: t.cache_read,
        });
    }
    payload.models = kept;
    payload.last = payload.last.map(|t| cap_hb(t, now));
    payload.hb = cap_hb(payload.hb, now);
}

pub(super) fn effective_state(e: &SessionEntry, now: i64) -> SessionState {
    if now.saturating_sub(e.heard) > OFFLINE_AFTER_SECS {
        SessionState::Offline
    } else {
        e.payload.state
    }
}
