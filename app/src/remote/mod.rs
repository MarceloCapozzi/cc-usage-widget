//! Remote sessions: rows published by the other machines (`cc-publish`, or
//! the app in its Publisher role, see [`crate::publisher`]), read from the
//! `cc-sessions` NATS KV bucket on Synadia Cloud. This side runs only when
//! the app's role is Viewer.
//!
//! Publishers write one sealed [`RemotePayload`] per session under
//! `<alias>.s.<sid>` (on every state change and as a 60 s heartbeat, deleted
//! when the session ends) and a sealed [`HostPayload`] under `<alias>.host`
//! every 60 s. This module is the viewer side:
//!
//! - [`RemoteMap`] holds what the bucket says, opened with the viewer's
//!   [`ViewerKeys`] (the KV key is the associated data): the envelope,
//!   with the viewer's key pair (`keys/viewer.key`, created on first start)
//!   and each publisher's pinned key (`keys/publishers/<alias>.pub`, written
//!   only with the user's consent). It takes the clock as an argument, so
//!   every rule below is unit tested.
//! - [`PendingKeys`] holds the publisher keys waiting for that consent, a
//!   new machine's first key included, for the flyout's notice (Accept,
//!   Reject: [`accept_key_of`], [`reject_key_of`]); see `review.rs`.
//! - [`RemoteHub`] adds the connection state and builds what the flyout
//!   shows: [`RemoteRow`]s and the footer's [`RemoteStatus`], which carries
//!   the waiting keys too.
//! - [`run`] is the background task: it connects as `cc-sessions.viewer`
//!   (WebSocket on 443 first, for corporate networks, then native TLS),
//!   binds the bucket, consumes `watch_all` (the initial snapshot, then live
//!   changes) and reconnects with backoff. [`run_ticker`] applies the
//!   time-based rules while nothing arrives. The servers and the bucket
//!   are cc-remote's defaults unless `settings.json` (`"nats"`) or the
//!   `CC_USAGE_NATS_SERVERS` / `CC_USAGE_NATS_BUCKET` variables name others
//!   ([`crate::settings::nats`]); they are read before every connection and
//!   every [`KEY_RECHECK`], and at once when Settings saved them
//!   ([`reload_nats`]); a change reconnects, and a value that cannot be
//!   used falls back to the default and is named in the footer's tooltip.
//! - [`merge_rows`] puts local and remote rows in one list, in panel order,
//!   in the JSON shape `ui/scripts/sessions.js` renders.
//!
//! Rules:
//!
//! - A value that fails to open (wrong or missing key, tampering, a value
//!   moved to another key, an unsupported envelope version) is
//!   counted, logged once per key and never shown. A value whose sender key
//!   is not pinned for its alias (a new machine, or a new key of a known
//!   one) is not shown either: its key waits for review, with when it was
//!   first and last seen and how many values it sent
//!   (`keys/publishers/<alias>.pending`), and its values open from the
//!   first write after Accept. One from a rejected key
//!   (`<alias>.rejected`) is dropped unopened and counted in the footer's
//!   tooltip ("devbox: 3 value(s) from a rejected key dropped"); one whose
//!   pin file is unusable (a revoked machine) is named there as "devbox:
//!   unknown sender key — values rejected".
//! - A delete (or purge, or a server TTL marker: [`KvChange::Delete`] does
//!   not tell them apart) removes the row at once: the session ended.
//! - A row last heard of more than [`OFFLINE_AFTER_SECS`] ago shows
//!   *offline* ("no heartbeat since HH:MM"). By default it stays, offline,
//!   until the clear button; the ⚙ view can drop it a set delay after that
//!   last heartbeat instead ([`RemoteMap::set_drop_after`]). The bucket's
//!   own TTL (3 minutes) removes the key silently, so the row lives in
//!   memory only.
//! - The widget's clear button dismisses every row
//!   ([`RemoteMap::dismiss_all`]): a dismissed key stays hidden until a
//!   write with a newer KV revision arrives. A session that is still
//!   publishing comes back with its next heartbeat (60 s at most); a dead
//!   one does not, even when a reconnect replays its last value.
//! - A session that moves into *needs you* on a live write toasts, once
//!   ([`RemoteMap::take_needs_you`], [`needs_you_toast`]): not one found
//!   so in the initial snapshot (the viewer starting or reconnecting), and
//!   not one that needed the user before a clear and still does when it
//!   comes back.
//! - "Last heard" is the local time a live write arrived. Values from the
//!   initial snapshot were written before the viewer connected, so for them
//!   it is the publisher's `hb`, capped at the local time. A publisher whose
//!   clock runs ahead therefore cannot keep its rows fresh forever, and one
//!   that runs behind only affects rows found in the snapshot.
//!
//! - Project values (`<alias>.p.<pid>`, a machine's token usage per
//!   project) are kept apart from the rows: they feed the usage view, not the
//!   session list. A delete or the TTL does not remove one, since a machine
//!   that is switched off still used what it used; the latest value per key
//!   is saved to [`REMOTE_PROJECTS_FILE`] so it survives a restart, and one
//!   whose last activity is older than the window is dropped. The watch and
//!   the ticker write that file on tokio's blocking pool, at most every
//!   [`PROJECTS_SAVE_INTERVAL`]; [`flush_projects_of`] writes what is
//!   pending at once (a restore, the app's exit).
//!
//! - An activity value (`<alias>.activity`) is held per machine, also
//!   through a delete; the usage view saves its days in `activity.json`.
//!
//! - Values under this machine's own name ([`RemoteMap::set_own_alias`])
//!   are ignored, sessions, host, projects and activity alike: a viewer
//!   that published before (as a Publisher, or with `cc-publish`) may find
//!   them in the bucket, and its own sessions are shown as local already.
//!
//! - The envelope proves which machine wrote a value, not that its contents
//!   make sense: a broken or compromised publisher can only write its own
//!   alias, but anything there. So a value whose `hb` is older than the one
//!   held for its key is a replay and ignored ([`is_replay`]); heartbeats
//!   and a project's last activity are cut to [`MAX_CLOCK_SKEW_SECS`] ahead
//!   of this clock when stored; and
//!   a machine gets at most [`MAX_SESSIONS_PER_ALIAS`] sessions and
//!   [`MAX_PROJECTS_PER_ALIAS`] projects, further keys ignored. At most
//!   [`MAX_ALIASES`] machines are held; values of further ones are ignored
//!   unopened (no pin read, no key noted) and counted in the footer's tooltip.
//!   A row goes by the sid of its KV key, a value whose payload names
//!   another machine than its key is rejected like one that does not open,
//!   texts (tool and model included) and projects are cut to what a
//!   publisher sends, and a [`REMOTE_PROJECTS_FILE`] over 8 MB is kept
//!   aside instead of loaded.
//!
//! - Key and pin files are read ahead of time, on the blocking pool and
//!   with the hub's lock let go: the pin files of machines not seen yet
//!   before a batch is applied ([`ViewerKeys::load_pins`]), the ones held
//!   again every [`KEY_RECHECK`] ([`ViewerKeys::reload_pins`]), and the
//!   viewer key file when it changed. The projects file and the `.pending`
//!   files are written with the lock let go as well, and so are the pin
//!   and rejected files of an answer, by the command that gives it; then
//!   the running viewer's keys read that alias's files again at once
//!   ([`ViewerKeys::refresh_alias`]). Nothing is written under the lock.
//!
//! Nothing here knows about Tauri; `main.rs` spawns the tasks and
//! `poller.rs` turns the notifications into `sessions` and `remote-status`
//! events.
//!
//! The code is split by concern: `map.rs` ([`RemoteMap`] and its rules),
//! `view.rs` (rows and their order), `hub.rs` ([`RemoteHub`] and the
//! footer status), `store.rs` (the projects file), `review.rs` (the keys
//! waiting for review), `task.rs` ([`run`] and [`run_ticker`]); the tests
//! are in `tests.rs`.
//!
//! [`RemotePayload`]: cc_core::session::RemotePayload
//! [`HostPayload`]: cc_envelope::HostPayload
//! [`ViewerKeys`]: cc_envelope::ViewerKeys
//! [`ViewerKeys::load_pins`]: cc_envelope::ViewerKeys::load_pins
//! [`ViewerKeys::reload_pins`]: cc_envelope::ViewerKeys::reload_pins
//! [`ViewerKeys::refresh_alias`]: cc_envelope::ViewerKeys::refresh_alias
//! [`KvChange::Delete`]: cc_remote::KvChange::Delete
//! [`is_replay`]: map::is_replay

mod hub;
mod map;
mod review;
mod store;
mod task;
mod view;

pub use hub::{
    activity_of, demo_status, dismiss_all_of, needs_you_toast, projects_of, restore_projects_of,
    rows_of, set_drop_after_of, set_own_alias_of, status_of, status_text, take_needs_you_of,
    tick_of, ConnState, Notify, RemoteHub, RemoteStatus, SharedHub,
};
pub use map::{RemoteMap, StoredProject};
pub use review::{
    accept_key_of, fingerprint, flush_pending_of, pending_count_of, pending_keys_of, read_pending,
    reject_key_of, review_text, PendingKey, PendingKeys, PendingWrite, MAX_PENDING_ALIASES,
    MAX_PENDING_PER_ALIAS, PENDING_EXT, PENDING_SAVE_INTERVAL,
};
pub use store::{flush_projects_of, projects_file_hub, ProjectsWrite};
pub use task::{run, run_ticker, Setup};
pub use view::{merge_rows, merge_rows_with, name_local_rows, origin_json, RemoteRow};

use std::time::{Duration, SystemTime};

use cc_core::projects::PROJECT_WINDOW_DAYS;

/// A row (or a machine) last heard of longer ago than this is offline.
pub const OFFLINE_AFTER_SECS: i64 = 150;

/// The file in the data folder holding the last project values of other
/// machines.
pub const REMOTE_PROJECTS_FILE: &str = "remote-projects.json";

/// A project value older than the window it covers is no longer worth
/// showing: dropped this long after its last activity (or after it was last
/// heard of, when the publisher did not know the activity).
pub const PROJECT_KEEP_SECS: i64 = PROJECT_WINDOW_DAYS as i64 * 24 * 3600;

/// How long a dismissal is remembered. Far longer than the bucket's 3-minute
/// TTL: by then the dismissed value is gone from the bucket, so no reconnect
/// can replay it.
pub const DISMISS_RETENTION_SECS: i64 = 3600;

/// How far ahead of this clock a publisher's timestamps are believed. Every
/// `hb`, and a project's `last`, is cut to this when stored ([`cap_hb`]),
/// so a project dated in the future cannot outlive [`PROJECT_KEEP_SECS`],
/// and a value held with a future `hb` stops blocking newer ones
/// ([`is_replay`]) a day later at most.
///
/// [`cap_hb`]: map::cap_hb
/// [`is_replay`]: map::is_replay
pub const MAX_CLOCK_SKEW_SECS: i64 = 24 * 3600;

/// Sessions held per machine. Far above what one machine runs at once; a
/// publisher writing more (broken, or compromised: it can only write its
/// own alias, but as many keys as it likes) gets the extra keys ignored, so
/// it cannot fill the flyout or the memory.
pub const MAX_SESSIONS_PER_ALIAS: usize = 200;

/// Projects held (and saved) per machine, for the same reason.
pub const MAX_PROJECTS_PER_ALIAS: usize = 200;

/// Machines held at once: sessions, host records, projects, activity,
/// rejected senders and waiting keys alike, and so the publisher pin files
/// read. Far above what one viewer follows; the values of further machines
/// are ignored unopened (counted in the footer's tooltip) until one of
/// those held goes.
pub const MAX_ALIASES: usize = 64;

/// Keys remembered as already logged. Past this the memory is cleared, so a
/// key may be logged again after this many others.
pub const MAX_LOGGED_KEYS: usize = 1024;

/// Least time between two writes of [`REMOTE_PROJECTS_FILE`] by the watch
/// and the ticker. A change held back is written by a later tick.
pub const PROJECTS_SAVE_INTERVAL: Duration = Duration::from_secs(5);

/// How often [`run_ticker`] re-evaluates the offline and drop rules.
pub const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// While the creds or the key file are missing, how often [`run`] looks again.
pub const SETUP_RECHECK: Duration = Duration::from_secs(30);

/// How often a live watch checks whether the viewer key file changed
/// (rotation, a restored backup) and re-reads the pin files.
pub const KEY_RECHECK: Duration = Duration::from_secs(30);

/// Shortest wait between two connection attempts of [`run`].
const MIN_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Prefix of the row ids of remote sessions; local ids are Claude Code UUIDs,
/// so the two never collide and the focus commands never match a remote row.
pub const REMOTE_ID_PREFIX: &str = "remote:";

/// Wakes [`run`] to read the connection settings now.
static NATS_RELOAD: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();

pub(crate) fn nats_reload() -> &'static tokio::sync::Notify {
    NATS_RELOAD.get_or_init(tokio::sync::Notify::new)
}

/// Settings saved new servers or a new bucket: the viewer task reads them
/// at once instead of at its next [`KEY_RECHECK`], and reconnects when they
/// changed; while it waits to retry or for its creds, it tries again now.
/// Nothing else is affected when nothing changed.
pub fn reload_nats() {
    nats_reload().notify_one();
}

/// The NATS user of the viewer role, `cc-sessions.viewer`.
pub fn viewer_user() -> String {
    cc_core::nats_user("viewer")
}

/// Current time as Unix seconds.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
