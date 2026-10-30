//! Widget preferences, in `settings.json` in the app config dir.
//!
//! Every field has a default, and the file is read one field at a time
//! ([`parse`]): an incomplete or hand-edited file still loads, each
//! missing or unreadable field filled in with its default, rather than
//! failing as a whole and leaving the widget with no settings.
//!
//! The file may also hold a `"nats"` object: where the viewer and the
//! Publisher role connect, see [`nats`]. The ⚙ view's "Remote connection"
//! group edits it ([`nats_form`], [`check_nats`], [`write_with_nats`]); it
//! can be edited by hand too, and every other save keeps it as it is.
//!
//! ```json
//! "nats": { "servers": ["wss://nats.example:443", "tls://nats.example:4222"], "bucket": "cc-sessions" }
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = "settings.json";

/// The app's Tauri identifier (`tauri.conf.json`), the name of its config
/// folder under `%APPDATA%`.
pub const APP_IDENTIFIER: &str = "ar.cc.usage";

/// Overrides `nats.servers`: a comma-separated server list.
pub const NATS_SERVERS_ENV: &str = "CC_USAGE_NATS_SERVERS";

/// Overrides `nats.bucket`.
pub const NATS_BUCKET_ENV: &str = "CC_USAGE_NATS_BUCKET";

/// The member of `settings.json` that holds the NATS connection: an object
/// with the optional members `servers` (URLs: transport, host and optional
/// port) and `bucket`.
const NATS_MEMBER: &str = "nats";

/// The global show/hide shortcut out of the box. Checked free on the
/// development machine (`RegisterHotKey` succeeds); if another application
/// holds it, the ⚙ view says so and the widget keeps working from the tray.
pub const DEFAULT_SHORTCUT: &str = "Alt+Shift+U";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Global show/hide shortcut, as typed or captured. Empty means none.
    pub shortcut: String,
    /// When to get out of the way of something full screen. Never by
    /// default: always on top means always on top until asked.
    pub hide_on_fullscreen: HideWhenFullscreen,
    /// "Hide idle sessions after", minutes; 0 is Never (the default). One
    /// of [`HIDE_IDLE_CHOICES`].
    pub hide_idle_after_mins: u32,
    /// "Remove offline sessions after", minutes after a remote row's last
    /// heartbeat; 0 is Never (the default: an offline row stays until the
    /// clear button). One of [`REMOVE_OFFLINE_CHOICES`].
    pub remove_offline_after_mins: u32,
    /// "This machine's name", the tag on local rows. `None` (the default,
    /// also for a file without it) follows the
    /// Windows computer name, see [`default_machine_name`]. Same rules as a
    /// remote machine's alias.
    pub machine_name: Option<String>,
    /// "Refresh usage every", minutes between two reads of the plan usage.
    /// One of [`REFRESH_CHOICES`]; 5 by default.
    pub refresh_mins: u32,
    /// "Theme": System (follow the Windows app theme, the default), Light
    /// or Dark.
    pub theme: Theme,
    /// "Role": Viewer (the default) or Publisher, see [`Role`]. Changed
    /// only through the role switch, which restarts the app.
    pub role: Role,
}

/// What this machine does with the `cc-sessions` bucket. Exactly one
/// machine is the viewer; a machine with the app that must publish its own
/// sessions is a Publisher (it never runs `cc-publish` next to the app).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// Reads the bucket and shows the other machines' sessions, projects
    /// and activity.
    #[default]
    Viewer,
    /// Publishes this machine's sessions, host record, projects and
    /// activity for the viewer on another PC; shows this machine only.
    Publisher,
}

/// The panel's colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    /// Follow the Windows app theme (light or dark), live.
    #[default]
    System,
    Light,
    Dark,
}

/// "When something is full screen".
///
/// Windows reports a game or a projected deck, and any other full-screen
/// application (a remote desktop client someone works in all day, a
/// maximised browser), through two different notification states, so the
/// widget can tell them apart; one switch could not say which was meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HideWhenFullscreen {
    /// Never get out of the way.
    #[default]
    Never,
    /// Only for Direct3D full screen and presentation mode: games, video
    /// players, someone projecting.
    GamesAndPresentations,
    /// Also for any ordinary application running full screen.
    Always,
}

/// Reads the three names; an unknown name is the default.
impl<'de> Deserialize<'de> for HideWhenFullscreen {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "gamesAndPresentations" => Self::GamesAndPresentations,
            "always" => Self::Always,
            _ => Self::Never,
        })
    }
}

/// The local rows' tag when the computer name gives nothing usable.
pub const FALLBACK_MACHINE_NAME: &str = "this-pc";

/// This machine's name out of the box: the computer name (`COMPUTERNAME`
/// on Windows, `HOSTNAME` elsewhere) reduced to the alias rules, so
/// `OFFICE-PC-07` shows as `office-pc-07`.
pub fn default_machine_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok();
    machine_name_from(raw.as_deref())
}

/// [`default_machine_name`] for a given computer name.
pub fn machine_name_from(computer_name: Option<&str>) -> String {
    computer_name
        .and_then(cc_envelope::normalize_alias)
        .unwrap_or_else(|| FALLBACK_MACHINE_NAME.to_string())
}

/// Checks a machine name typed in the ⚙ view. Blank means "back to the
/// default" (`Ok(None)`); anything else must follow the alias rules, and the
/// error says how, with the nearest valid name as an example.
pub fn check_machine_name(typed: &str) -> Result<Option<String>, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Ok(None);
    }
    match cc_envelope::validate_alias(name) {
        Ok(()) => Ok(Some(name.to_string())),
        Err(err) => {
            let rule = match err {
                cc_envelope::AliasError::TooLong => format!(
                    "\"{name}\" is too long: {} characters at most.",
                    cc_envelope::MAX_ALIAS_LEN
                ),
                _ => format!(
                    "\"{name}\" is not a valid name: use lowercase letters, digits and dashes, \
                     starting with a letter or a digit."
                ),
            };
            Err(match cc_envelope::normalize_alias(name) {
                Some(hint) => format!("{rule} Try {hint}."),
                None => rule,
            })
        }
    }
}

/// The ⚙ view's choices for "Hide idle sessions after", minutes (0: Never).
pub const HIDE_IDLE_CHOICES: [u32; 5] = [0, 15, 30, 60, 240];

/// The ⚙ view's choices for "Remove offline sessions after", minutes (0: Never).
pub const REMOVE_OFFLINE_CHOICES: [u32; 5] = [0, 5, 10, 30, 60];

/// The ⚙ view's choices for "Refresh usage every", minutes.
pub const REFRESH_CHOICES: [u32; 5] = [2, 5, 10, 15, 30];

/// Plan usage is read every 5 minutes out of the box.
pub const DEFAULT_REFRESH_MINS: u32 = 5;

/// Idle rows are never hidden out of the box.
pub const DEFAULT_HIDE_IDLE_AFTER_MINS: u32 = 0;

/// Offline remote rows stay until the clear button out of the box.
pub const DEFAULT_REMOVE_OFFLINE_AFTER_MINS: u32 = 0;

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: DEFAULT_SHORTCUT.to_string(),
            hide_on_fullscreen: HideWhenFullscreen::Never,
            hide_idle_after_mins: DEFAULT_HIDE_IDLE_AFTER_MINS,
            remove_offline_after_mins: DEFAULT_REMOVE_OFFLINE_AFTER_MINS,
            machine_name: None,
            refresh_mins: DEFAULT_REFRESH_MINS,
            theme: Theme::System,
            role: Role::Viewer,
        }
    }
}

impl Settings {
    /// Brings every value back into shape. Applied on load as well as on
    /// save: the file can be hand-edited. A delay that is not one of the
    /// choices becomes the nearest one, so the ⚙ view can always show it.
    pub fn sanitize(mut self) -> Self {
        self.shortcut = tidy(&self.shortcut);
        self.hide_idle_after_mins = nearest(&HIDE_IDLE_CHOICES, self.hide_idle_after_mins);
        self.remove_offline_after_mins =
            nearest(&REMOVE_OFFLINE_CHOICES, self.remove_offline_after_mins);
        self.refresh_mins = nearest(&REFRESH_CHOICES, self.refresh_mins);
        // A hand-edited name that breaks the rules is reduced to them rather
        // than dropped; a blank one means the default.
        self.machine_name = self
            .machine_name
            .as_deref()
            .and_then(cc_envelope::normalize_alias);
        self
    }

    /// The tag on this machine's rows: the saved name, or `default` (see
    /// [`default_machine_name`]).
    pub fn machine_name_or(&self, default: &str) -> String {
        self.machine_name
            .clone()
            .unwrap_or_else(|| default.to_string())
    }

    /// Seconds after its last heartbeat an offline remote row is removed;
    /// `None` (Never) keeps it.
    pub fn remove_offline_after_secs(&self) -> Option<i64> {
        (self.remove_offline_after_mins > 0).then(|| i64::from(self.remove_offline_after_mins) * 60)
    }

    /// Time between two reads of the plan usage.
    pub fn refresh_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.refresh_mins) * 60)
    }

    /// The shortcut in the spelling the hotkey library understands.
    ///
    /// It knows `Ctrl`, `Alt`, `Shift` and `Super`, and **not** `Win`, while
    /// the key on a Windows keyboard has a Windows logo on it and is called
    /// the Windows key everywhere in the operating system. Anyone reaching
    /// for it writes `Win`, which the parser then takes for a main key, fails
    /// to find, and rejects the whole combination. Translating here costs one
    /// line and removes a trap with no upside.
    pub fn accelerator(&self) -> String {
        self.shortcut
            .split('+')
            .map(|token| match token.trim().to_ascii_lowercase().as_str() {
                "win" | "windows" | "meta" => "Super",
                _ => token.trim(),
            })
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// Trims the whole combination and the spaces around each `+`, so
/// `" alt + shift + u "` is stored as `"alt+shift+u"`. Case is left alone:
/// the parser does not care and the person typed it that way.
fn tidy(shortcut: &str) -> String {
    let trimmed = shortcut.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .split('+')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("+")
}

/// The choice closest to `value` (the smaller one on a tie).
fn nearest(choices: &[u32], value: u32) -> u32 {
    choices
        .iter()
        .copied()
        .min_by_key(|c| c.abs_diff(value))
        .unwrap_or(value)
}

/// Turns the hotkey library's error into something that says what to do
/// about it.
///
/// The two failures want opposite remedies. A combination the parser does
/// not understand has to be rewritten; one that another application holds
/// has to be exchanged for a different one. Telling someone "another app may
/// be using it" when they have actually written `Win` instead of `Super`
/// sends them off hunting through their startup programs.
pub fn shortcut_problem(shortcut: &str, error: &str) -> String {
    if error.contains("already registered") {
        // Windows itself holds a good number of these: Win+Ctrl+C toggles
        // the colour filters, for one.
        format!(
            "{shortcut} is already taken by Windows or another application. Try a different combination."
        )
    } else if error.contains("recognize")
        || error.contains("Couldn't")
        || error.contains("Unexpected")
        || error.contains("Unknown VKCode")
    {
        format!(
            "{shortcut} is not a combination this understands. Build it from Ctrl, Alt, Shift and \
             Super (the Windows key) plus one more key, like {DEFAULT_SHORTCUT}."
        )
    } else {
        format!("Could not register {shortcut}: {error}")
    }
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// Where `settings.json` lives without a Tauri handle: `CC_USAGE_CONFIG_DIR`
/// when set, else the app config dir Tauri uses (`%APPDATA%\ar.cc.usage`
/// on Windows, `$XDG_CONFIG_HOME/ar.cc.usage` or `~/.config/ar.cc.usage`
/// elsewhere), else the data folder. The same folder as the binary's
/// `state::config_dir`.
pub fn default_dir() -> PathBuf {
    let os = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    crate::paths::config_dir_override()
        .or_else(|| {
            if cfg!(windows) {
                os("APPDATA").map(PathBuf::from)
            } else {
                os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| os("HOME").map(|h| PathBuf::from(h).join(".config")))
            }
            .map(|base| base.join(APP_IDENTIFIER))
        })
        .unwrap_or_else(crate::paths::data_dir)
}

/// The NATS connection in force: servers and bucket, each from
/// [`NATS_SERVERS_ENV`] / [`NATS_BUCKET_ENV`] when set, else from
/// `settings.json`, else cc-remote's defaults (Synadia Cloud, WebSocket
/// first, and `cc-sessions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nats {
    /// Checked server URLs.
    pub servers: Vec<String>,
    /// Checked bucket name.
    pub bucket: String,
    /// What was set but could not be used, user-facing, one line each:
    /// the value was replaced by the default, and the footer's tooltip and
    /// the log say so. Empty when every setting applied.
    pub problems: Vec<String>,
}

impl Default for Nats {
    fn default() -> Self {
        Self {
            servers: cc_remote::default_servers(),
            bucket: cc_remote::DEFAULT_BUCKET.to_string(),
            problems: Vec::new(),
        }
    }
}

impl Nats {
    /// True when servers and bucket are cc-remote's defaults.
    pub fn is_default(&self) -> bool {
        self.servers == cc_remote::default_servers() && self.bucket == cc_remote::DEFAULT_BUCKET
    }
}

/// The NATS connection for the `settings.json` in `dir` and this process's
/// environment, see [`resolve_nats`]. Reads the file; call it off the
/// executor.
pub fn nats(dir: &Path) -> Nats {
    let text = std::fs::read_to_string(path(dir)).ok();
    resolve_nats(
        text.as_deref(),
        env_var(NATS_SERVERS_ENV).as_deref(),
        env_var(NATS_BUCKET_ENV).as_deref(),
    )
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Whether an override variable's value counts: set and not blank.
fn is_set(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// What the ⚙ view's "Remote connection" group shows. Serialized for the
/// frontend (camelCase).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NatsForm {
    /// The servers saved in `settings.json`, as written there (trimmed,
    /// blank entries left out). Empty: none saved, the defaults apply; also
    /// when `nats.servers` is not a list of strings, which `problems` names.
    pub servers: Vec<String>,
    /// The bucket saved there, trimmed; empty when none is.
    pub bucket: String,
    /// cc-remote's defaults, for the placeholders.
    pub default_servers: Vec<String>,
    pub default_bucket: String,
    /// What the connection uses now ([`resolve_nats`]): the variables, the
    /// file, or the defaults.
    pub effective_servers: Vec<String>,
    pub effective_bucket: String,
    /// [`NATS_SERVERS_ENV`] when it is set (not blank): it wins over the
    /// file, so the view shows the servers in force read-only.
    pub servers_env: Option<&'static str>,
    /// [`NATS_BUCKET_ENV`] when it is set, likewise.
    pub bucket_env: Option<&'static str>,
    /// What was set but could not be used ([`Nats::problems`]).
    pub problems: Vec<String>,
}

/// The "Remote connection" group for the `settings.json` in `dir` and this
/// process's environment. Reads the file.
pub fn nats_form(dir: &Path) -> NatsForm {
    let text = std::fs::read_to_string(path(dir)).ok();
    nats_form_from(
        text.as_deref(),
        env_var(NATS_SERVERS_ENV).as_deref(),
        env_var(NATS_BUCKET_ENV).as_deref(),
    )
}

/// [`nats_form`] from the text of `settings.json` and the two variables.
pub fn nats_form_from(
    settings_text: Option<&str>,
    env_servers: Option<&str>,
    env_bucket: Option<&str>,
) -> NatsForm {
    let resolved = resolve_nats(settings_text, env_servers, env_bucket);
    let saved = settings_text
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| value.get(NATS_MEMBER).cloned());
    let servers = saved
        .as_ref()
        .and_then(|n| n.get("servers"))
        .and_then(|v| Vec::<String>::deserialize(v).ok())
        .map(|list| tidy_servers(&list))
        .unwrap_or_default();
    let bucket = saved
        .as_ref()
        .and_then(|n| n.get("bucket"))
        .and_then(serde_json::Value::as_str)
        .map(|b| b.trim().to_string())
        .unwrap_or_default();
    NatsForm {
        servers,
        bucket,
        default_servers: cc_remote::default_servers(),
        default_bucket: cc_remote::DEFAULT_BUCKET.to_string(),
        effective_servers: resolved.servers,
        effective_bucket: resolved.bucket,
        servers_env: is_set(env_servers).then_some(NATS_SERVERS_ENV),
        bucket_env: is_set(env_bucket).then_some(NATS_BUCKET_ENV),
        problems: resolved.problems,
    }
}

/// The servers trimmed, blank entries left out.
fn tidy_servers(list: &[String]) -> Vec<String> {
    list.iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// What the "Remote connection" group sends: the servers, one per entry,
/// and the bucket. Blank entries are skipped; no servers, or a blank
/// bucket, means that setting's default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NatsInput {
    pub servers: Vec<String>,
    pub bucket: String,
}

/// A checked change of the `"nats"` object. For each member, `None` leaves
/// it as the file has it (a variable overrides that setting, so the view
/// could not edit it), `Some(None)` removes it (the default applies), and
/// `Some(Some(_))` writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsChange {
    pub servers: Option<Option<Vec<String>>>,
    pub bucket: Option<Option<String>>,
}

/// Checks what the "Remote connection" group sent, with cc-remote's rules
/// ([`cc_remote::validate_servers`], [`cc_remote::validate_bucket`]), the
/// ones the connection applies. A setting its variable overrides
/// (`keep_servers`, `keep_bucket`) is not looked at and stays as saved. A
/// value equal to the default is stored as none, so it keeps following the
/// default. The error, user-facing, names every value refused (a password
/// in a URL shows as `***`); nothing is saved then.
pub fn check_nats(
    input: &NatsInput,
    keep_servers: bool,
    keep_bucket: bool,
) -> Result<NatsChange, String> {
    let mut errors = Vec::new();
    let servers = if keep_servers {
        None
    } else {
        let list = tidy_servers(&input.servers);
        if list.is_empty() || list == cc_remote::default_servers() {
            Some(None)
        } else {
            match cc_remote::validate_servers(&list) {
                Ok(()) => Some(Some(list)),
                Err(err) => {
                    errors.push(format!("server {}", remote_message(err)));
                    None
                }
            }
        }
    };
    let bucket = if keep_bucket {
        None
    } else {
        let name = input.bucket.trim();
        if name.is_empty() || name == cc_remote::DEFAULT_BUCKET {
            Some(None)
        } else {
            match cc_remote::validate_bucket(name) {
                Ok(()) => Some(Some(name.to_string())),
                Err(err) => {
                    errors.push(remote_message(err));
                    None
                }
            }
        }
    };
    if errors.is_empty() {
        Ok(NatsChange { servers, bucket })
    } else {
        Err(errors.join("; "))
    }
}

/// Whether the variables override the servers and the bucket now: what
/// [`check_nats`] keeps.
pub fn nats_overridden() -> (bool, bool) {
    (
        is_set(env_var(NATS_SERVERS_ENV).as_deref()),
        is_set(env_var(NATS_BUCKET_ENV).as_deref()),
    )
}

/// The `"nats"` object after `change`, from the one in the file (`old`).
/// A kept member is copied as it is, even unusable (its variable wins
/// anyway); unknown members are dropped. `None` when nothing is left.
fn nats_member(old: Option<&serde_json::Value>, change: &NatsChange) -> Option<serde_json::Value> {
    let mut map = serde_json::Map::new();
    let kept = |name: &str| {
        old.and_then(|o| o.get(name))
            .filter(|v| !v.is_null())
            .cloned()
    };
    let servers = match &change.servers {
        None => kept("servers"),
        Some(list) => list.as_ref().map(|l| serde_json::json!(l)),
    };
    let bucket = match &change.bucket {
        None => kept("bucket"),
        Some(name) => name.as_ref().map(|b| serde_json::json!(b)),
    };
    if let Some(v) = servers {
        map.insert("servers".into(), v);
    }
    if let Some(v) = bucket {
        map.insert("bucket".into(), v);
    }
    (!map.is_empty()).then_some(serde_json::Value::Object(map))
}

/// The NATS connection from the text of `settings.json` (`None`: no file)
/// and the two environment overrides. A blank variable counts as unset. A
/// value that is set but unusable (a plaintext or host-less URL, an empty
/// list, a bucket JetStream refuses, a `"nats"` that is not an object) is
/// never half used: that setting falls back to the default and a line in
/// [`Nats::problems`] says which value was ignored and why.
pub fn resolve_nats(
    settings_text: Option<&str>,
    env_servers: Option<&str>,
    env_bucket: Option<&str>,
) -> Nats {
    let mut nats = Nats::default();
    let mut problems = Vec::new();
    let file = settings_text
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| value.get(NATS_MEMBER).cloned());
    let mut file_servers: Option<Vec<String>> = None;
    let mut file_bucket: Option<String> = None;
    if let Some(value) = file {
        match value {
            serde_json::Value::Object(map) => {
                for key in map.keys() {
                    if key != "servers" && key != "bucket" {
                        problems.push(format!(
                            "settings.json: nats.{key} is not a setting (servers, bucket); ignored"
                        ));
                    }
                }
                match map.get("servers") {
                    None | Some(serde_json::Value::Null) => {}
                    Some(v) => match Vec::<String>::deserialize(v) {
                        Ok(list) => file_servers = Some(list),
                        Err(_) => problems.push(
                            "settings.json: nats.servers is not a list of URLs; using the default servers"
                                .into(),
                        ),
                    },
                }
                match map.get("bucket") {
                    None | Some(serde_json::Value::Null) => {}
                    Some(serde_json::Value::String(b)) => file_bucket = Some(b.clone()),
                    Some(_) => problems.push(
                        "settings.json: nats.bucket is not a name; using the default bucket".into(),
                    ),
                }
            }
            serde_json::Value::Null => {}
            _ => problems.push(
                "settings.json: nats is not an object; using the default servers and bucket".into(),
            ),
        }
    }

    let set = |v: Option<&str>| {
        v.map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let servers = match set(env_servers) {
        Some(list) => Some((
            NATS_SERVERS_ENV.to_string(),
            list.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>(),
        )),
        None => file_servers.map(|list| ("settings.json: nats.servers".to_string(), list)),
    };
    if let Some((source, list)) = servers {
        let list: Vec<String> = list
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        match cc_remote::validate_servers(&list) {
            Ok(()) => nats.servers = list,
            Err(err) => problems.push(format!(
                "{source} ignored ({}); using the default servers",
                remote_message(err)
            )),
        }
    }
    let bucket = match set(env_bucket) {
        Some(b) => Some((NATS_BUCKET_ENV.to_string(), b)),
        None => file_bucket.map(|b| {
            (
                "settings.json: nats.bucket".to_string(),
                b.trim().to_string(),
            )
        }),
    };
    if let Some((source, bucket)) = bucket {
        match cc_remote::validate_bucket(&bucket) {
            Ok(()) => nats.bucket = bucket,
            Err(err) => problems.push(format!(
                "{source} ignored ({}); using the default bucket {}",
                remote_message(err),
                cc_remote::DEFAULT_BUCKET
            )),
        }
    }
    nats.problems = problems;
    nats
}

/// The message of a cc-remote config error without its prefix.
fn remote_message(err: cc_remote::RemoteError) -> String {
    match err {
        cc_remote::RemoteError::Config(msg) => msg,
        other => other.to_string(),
    }
}

/// The saved settings, or the defaults if there is no readable file. Each
/// setting is read on its own ([`parse`]).
pub fn read(dir: &Path) -> Settings {
    std::fs::read_to_string(path(dir))
        .map(|text| parse(&text))
        .unwrap_or_default()
        .sanitize()
}

/// The settings in `text`, one field at a time: a field that is missing or
/// holds the wrong type (`"refreshMins": "5"` from a hand edit) takes its
/// default and the others keep their values. Deserializing the struct as a
/// whole would fail on that one field, every setting would reset, and the
/// next save would write the defaults over the file. Text that is not a
/// JSON object at all is the defaults. Not sanitized; [`read`] does that.
///
/// Every field of [`Settings`] must be listed here; `settings_round_trip`
/// fails for one that is not.
pub fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text) else {
        return s;
    };
    fn take<T: serde::de::DeserializeOwned>(
        map: &serde_json::Map<String, serde_json::Value>,
        name: &str,
        into: &mut T,
    ) {
        if let Some(value) = map.get(name).and_then(|v| T::deserialize(v).ok()) {
            *into = value;
        }
    }
    take(&map, "shortcut", &mut s.shortcut);
    take(&map, "hideOnFullscreen", &mut s.hide_on_fullscreen);
    take(&map, "hideIdleAfterMins", &mut s.hide_idle_after_mins);
    take(
        &map,
        "removeOfflineAfterMins",
        &mut s.remove_offline_after_mins,
    );
    take(&map, "machineName", &mut s.machine_name);
    take(&map, "refreshMins", &mut s.refresh_mins);
    take(&map, "theme", &mut s.theme);
    take(&map, "role", &mut s.role);
    s
}

/// One read and rewrite of `settings.json` at a time: each keeps the
/// `"nats"` object it read, and two at once could put back one the other
/// just changed.
static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Saves the settings, atomically: a crash halfway leaves the previous file
/// whole rather than a truncated one that would load as the defaults. The
/// file's `"nats"` object, which [`Settings`] does not hold, is written
/// back as it was.
pub fn write(dir: &Path, settings: &Settings) -> Result<(), String> {
    write_nats_with(dir, settings, |old| old.cloned())
}

/// [`write`], with the `"nats"` object changed by `change` (see
/// [`check_nats`]): the object is removed when no member is left.
pub fn write_with_nats(dir: &Path, settings: &Settings, change: &NatsChange) -> Result<(), String> {
    write_nats_with(dir, settings, |old| nats_member(old, change))
}

fn write_nats_with(
    dir: &Path,
    settings: &Settings,
    nats: impl FnOnce(Option<&serde_json::Value>) -> Option<serde_json::Value>,
) -> Result<(), String> {
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut value = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    let old = std::fs::read_to_string(path(dir))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|old| old.get(NATS_MEMBER).cloned());
    if let (Some(nats), serde_json::Value::Object(map)) = (nats(old.as_ref()), &mut value) {
        map.insert(NATS_MEMBER.to_string(), nats);
    }
    let json = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&path(dir), json.as_bytes())
        .map_err(|e| format!("could not save: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_incomplete_file_is_filled_in_with_defaults() {
        let partial: Settings =
            serde_json::from_str(r#"{"hideOnFullscreen":"gamesAndPresentations"}"#)
                .expect("should parse");
        assert_eq!(
            partial.hide_on_fullscreen,
            HideWhenFullscreen::GamesAndPresentations
        );
        assert_eq!(partial.shortcut, DEFAULT_SHORTCUT);
    }

    /// One wrong-typed field costs that field only: the rest of the file
    /// keeps its values, and saving again does not reset them.
    #[test]
    fn a_wrong_typed_field_resets_that_field_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            r#"{"shortcut":"Ctrl+Alt+K","refreshMins":"5","hideIdleAfterMins":30,
                "removeOfflineAfterMins":-1,"machineName":42,"hideOnFullscreen":"always"}"#,
        )
        .unwrap();
        let s = read(dir.path());
        assert_eq!(s.shortcut, "Ctrl+Alt+K");
        assert_eq!(s.refresh_mins, DEFAULT_REFRESH_MINS);
        assert_eq!(s.hide_idle_after_mins, 30);
        assert_eq!(
            s.remove_offline_after_mins,
            DEFAULT_REMOVE_OFFLINE_AFTER_MINS
        );
        assert_eq!(s.machine_name, None);
        assert_eq!(s.hide_on_fullscreen, HideWhenFullscreen::Always);
        write(dir.path(), &s).unwrap();
        assert_eq!(read(dir.path()), s);
    }

    #[test]
    fn text_that_is_not_an_object_is_the_defaults() {
        assert_eq!(parse("[1,2]"), Settings::default());
        assert_eq!(parse("\"x\""), Settings::default());
        assert_eq!(parse(""), Settings::default());
        // Unknown fields are ignored.
        assert_eq!(parse(r#"{"future":true}"#), Settings::default());
        // A full-screen level that is not a name is the default.
        assert_eq!(
            parse(r#"{"hideOnFullscreen":true}"#).hide_on_fullscreen,
            HideWhenFullscreen::Never
        );
        assert_eq!(
            parse(r#"{"hideOnFullscreen":3}"#).hide_on_fullscreen,
            HideWhenFullscreen::Never
        );
    }

    #[test]
    fn a_garbage_file_breaks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "{ not json }").unwrap();
        assert_eq!(read(dir.path()), Settings::default());
    }

    #[test]
    fn no_file_means_the_defaults_and_hiding_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let settings = read(dir.path());
        assert_eq!(settings.shortcut, "Alt+Shift+U");
        assert_eq!(settings.hide_on_fullscreen, HideWhenFullscreen::Never);
        assert_eq!(settings.refresh_mins, 5);
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mine = Settings {
            shortcut: "Ctrl+Alt+K".into(),
            hide_on_fullscreen: HideWhenFullscreen::Always,
            hide_idle_after_mins: 30,
            remove_offline_after_mins: 60,
            machine_name: Some("office-pc".into()),
            refresh_mins: 15,
            theme: Theme::Dark,
            role: Role::Publisher,
        };
        write(dir.path(), &mine).unwrap();
        assert_eq!(read(dir.path()), mine);
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(text.contains(r#""hideIdleAfterMins": 30"#), "{text}");
        assert!(text.contains(r#""removeOfflineAfterMins": 60"#), "{text}");
        assert!(text.contains(r#""machineName": "office-pc""#), "{text}");
        assert!(text.contains(r#""hideOnFullscreen": "always""#), "{text}");
        assert!(text.contains(r#""refreshMins": 15"#), "{text}");
        assert!(text.contains(r#""theme": "dark""#), "{text}");
        assert!(text.contains(r#""role": "publisher""#), "{text}");
    }

    /// Viewer out of the box and for a file without the setting; an
    /// unknown or wrong-typed role is Viewer and costs no other field.
    #[test]
    fn the_role_defaults_to_viewer_and_parses_leniently() {
        assert_eq!(Settings::default().role, Role::Viewer);
        assert_eq!(parse(r#"{"refreshMins":15}"#).role, Role::Viewer);
        assert_eq!(parse(r#"{"role":"publisher"}"#).role, Role::Publisher);
        assert_eq!(parse(r#"{"role":"viewer"}"#).role, Role::Viewer);
        let s = parse(r#"{"role":"both","refreshMins":15}"#);
        assert_eq!((s.role, s.refresh_mins), (Role::Viewer, 15));
        assert_eq!(parse(r#"{"role":1}"#).role, Role::Viewer);
        assert_eq!(
            parse(r#"{"role":"Publisher"}"#).role,
            Role::Viewer,
            "names are exact"
        );
    }

    /// An unknown theme name is that one setting's default, not a reset.
    #[test]
    fn an_unknown_theme_is_system() {
        let s = parse(r#"{"theme":"sepia","refreshMins":15}"#);
        assert_eq!(s.theme, Theme::System);
        assert_eq!(s.refresh_mins, 15);
        assert_eq!(parse(r#"{"theme":"light"}"#).theme, Theme::Light);
    }

    /// The three names round-trip; any other name is Never rather than a
    /// failed file.
    #[test]
    fn the_full_screen_setting_reads_the_names() {
        let level = |json: &str| {
            serde_json::from_str::<Settings>(&format!(r#"{{"hideOnFullscreen":{json}}}"#))
                .expect("should parse")
                .hide_on_fullscreen
        };
        assert_eq!(level(r#""never""#), HideWhenFullscreen::Never);
        assert_eq!(
            level(r#""gamesAndPresentations""#),
            HideWhenFullscreen::GamesAndPresentations
        );
        assert_eq!(level(r#""always""#), HideWhenFullscreen::Always);
        assert_eq!(level(r#""sometimes""#), HideWhenFullscreen::Never);
    }

    #[test]
    fn the_refresh_interval_snaps_to_a_choice() {
        let s = Settings {
            refresh_mins: 1,
            ..Default::default()
        }
        .sanitize();
        assert_eq!(s.refresh_mins, 2, "never faster than 2 minutes");
        let s = Settings {
            refresh_mins: 600,
            ..Default::default()
        }
        .sanitize();
        assert_eq!(s.refresh_mins, 30);
        assert_eq!(Settings::default().refresh_interval().as_secs(), 300);
    }

    #[test]
    fn a_settings_file_without_the_machine_name_loads_with_the_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            "{\n  \"shortcut\": \"Ctrl+Alt+K\",\n  \"hideOnFullscreen\": \"never\",\n  \
             \"hideIdleAfterMins\": 30,\n  \"removeOfflineAfterMins\": 5\n}",
        )
        .unwrap();
        let s = read(dir.path());
        assert_eq!(s.machine_name, None);
        assert_eq!(s.hide_idle_after_mins, 30);
        assert_eq!(s.machine_name_or("office-pc-07"), "office-pc-07");
        // Saved again, it says so explicitly and loads the same.
        write(dir.path(), &s).unwrap();
        assert_eq!(read(dir.path()), s);
    }

    #[test]
    fn the_default_machine_name_is_the_computer_name_by_the_alias_rules() {
        assert_eq!(machine_name_from(Some("OFFICE-PC-07")), "office-pc-07");
        assert_eq!(machine_name_from(Some("Ada's PC")), "ada-s-pc");
        assert_eq!(machine_name_from(Some(&"LONGNAME".repeat(5))).len(), 31);
        assert_eq!(machine_name_from(Some("???")), FALLBACK_MACHINE_NAME);
        assert_eq!(machine_name_from(None), FALLBACK_MACHINE_NAME);
        // Whatever the environment holds, the result is a valid alias.
        assert_eq!(cc_envelope::validate_alias(&default_machine_name()), Ok(()));
    }

    #[test]
    fn a_hand_edited_machine_name_is_brought_into_shape_on_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), r#"{"machineName":"Office PC"}"#).unwrap();
        assert_eq!(read(dir.path()).machine_name.as_deref(), Some("office-pc"));
        std::fs::write(path(dir.path()), r#"{"machineName":"   "}"#).unwrap();
        assert_eq!(read(dir.path()).machine_name, None);
        std::fs::write(path(dir.path()), r#"{"machineName":null}"#).unwrap();
        assert_eq!(read(dir.path()).machine_name, None);
    }

    #[test]
    fn a_typed_machine_name_follows_the_alias_rules() {
        assert_eq!(check_machine_name("devbox"), Ok(Some("devbox".into())));
        assert_eq!(check_machine_name("  lab-01 "), Ok(Some("lab-01".into())));
        assert_eq!(check_machine_name(""), Ok(None), "blank is the default");
        assert_eq!(check_machine_name("  "), Ok(None));
        let err = check_machine_name("Office PC").unwrap_err();
        assert!(
            err.contains("lowercase letters, digits and dashes"),
            "{err}"
        );
        assert!(err.ends_with("Try office-pc."), "{err}");
        let err = check_machine_name(&"x".repeat(32)).unwrap_err();
        assert!(err.contains("31 characters at most"), "{err}");
        let err = check_machine_name("-lab").unwrap_err();
        assert!(err.ends_with("Try lab."), "{err}");
        let err = check_machine_name("é").unwrap_err();
        assert!(!err.contains("Try"), "{err}");
    }

    #[test]
    fn an_incomplete_settings_file_loads_the_missing_options_with_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            "{\n  \"shortcut\": \"Ctrl+Alt+K\",\n  \"hideOnFullscreen\": \"gamesAndPresentations\"\n}",
        )
        .unwrap();
        let s = read(dir.path());
        assert_eq!(s.shortcut, "Ctrl+Alt+K");
        assert_eq!(
            s.hide_on_fullscreen,
            HideWhenFullscreen::GamesAndPresentations
        );
        assert_eq!(s.hide_idle_after_mins, 0, "Never");
        assert_eq!(s.remove_offline_after_mins, 0, "Never");
        assert_eq!(s.remove_offline_after_secs(), None);
        let s = Settings {
            remove_offline_after_mins: 10,
            ..Default::default()
        };
        assert_eq!(s.remove_offline_after_secs(), Some(600));
    }

    #[test]
    fn delays_outside_the_choices_snap_to_the_nearest_one() {
        let s = Settings {
            hide_idle_after_mins: 20,
            remove_offline_after_mins: 1000,
            ..Default::default()
        }
        .sanitize();
        assert_eq!(s.hide_idle_after_mins, 15);
        assert_eq!(s.remove_offline_after_mins, 60);
        let s = Settings {
            hide_idle_after_mins: 3,
            remove_offline_after_mins: 0,
            ..Default::default()
        }
        .sanitize();
        assert_eq!(s.hide_idle_after_mins, 0);
        assert_eq!(s.remove_offline_after_mins, 0);
        let s = Settings {
            remove_offline_after_mins: 4,
            ..Default::default()
        }
        .sanitize();
        assert_eq!(s.remove_offline_after_mins, 5);
        for c in HIDE_IDLE_CHOICES {
            let s = Settings {
                hide_idle_after_mins: c,
                ..Default::default()
            }
            .sanitize();
            assert_eq!(s.hide_idle_after_mins, c);
        }
        for c in REMOVE_OFFLINE_CHOICES {
            let s = Settings {
                remove_offline_after_mins: c,
                ..Default::default()
            }
            .sanitize();
            assert_eq!(s.remove_offline_after_mins, c);
        }
    }

    #[test]
    fn an_empty_shortcut_stays_empty() {
        let none = Settings {
            shortcut: "   ".into(),
            ..Default::default()
        }
        .sanitize();
        assert_eq!(none.shortcut, "");
    }

    #[test]
    fn stray_spaces_are_tidied_on_load() {
        let messy = Settings {
            shortcut: " alt + Shift +u ".into(),
            ..Default::default()
        }
        .sanitize();
        assert_eq!(messy.shortcut, "alt+Shift+u");
    }

    #[test]
    fn the_windows_key_is_spelled_the_way_the_keyboard_spells_it() {
        let win = Settings {
            shortcut: "Ctrl+Win+C".into(),
            ..Default::default()
        };
        assert_eq!(win.accelerator(), "Ctrl+Super+C");

        // Case and stray spaces are what people actually type.
        let messy = Settings {
            shortcut: " ctrl + WINDOWS + k ".into(),
            ..Default::default()
        };
        assert_eq!(messy.accelerator(), "ctrl+Super+k");

        let meta = Settings {
            shortcut: "Meta+Shift+U".into(),
            ..Default::default()
        };
        assert_eq!(meta.accelerator(), "Super+Shift+U");

        // Anything already correct passes through untouched.
        let plain = Settings::default();
        assert_eq!(plain.accelerator(), "Alt+Shift+U");
    }

    #[test]
    fn a_taken_shortcut_says_to_pick_another() {
        let msg = shortcut_problem(
            "Alt+Shift+U",
            "HotKey already registered: HotKey { mods: ALT | SHIFT, key: KeyU, id: 1 }",
        );
        assert!(msg.starts_with("Alt+Shift+U is already taken by Windows or another application"));
    }

    #[test]
    fn an_unparseable_shortcut_says_how_to_build_one() {
        let msg = shortcut_problem(
            "Ctrl+Banana",
            "Couldn't recognize \"Banana\" as a valid HotKey Code, if you feel like it should be, please report this",
        );
        assert!(
            msg.contains("is not a combination this understands"),
            "{msg}"
        );
        let msg = shortcut_problem(
            "U+Alt",
            "Unexpected hotkey string format: \"U+Alt\", a hotkey should have the modifiers first",
        );
        assert!(
            msg.contains("is not a combination this understands"),
            "{msg}"
        );
    }

    /// Without a file, a `"nats"` object or the variables, the connection
    /// is cc-remote's default: Synadia Cloud over WebSocket first, and
    /// `cc-sessions`.
    #[test]
    fn nats_defaults_to_synadia_and_cc_sessions() {
        for text in [
            None,
            Some(""),
            Some("{}"),
            Some(r#"{"nats":null}"#),
            Some("[1]"),
        ] {
            let n = resolve_nats(text, None, Some("  "));
            assert_eq!(n, Nats::default(), "{text:?}");
            assert!(n.is_default());
        }
        let n = Nats::default();
        assert_eq!(n.servers[0], "wss://connect.ngs.global");
        assert_eq!(n.servers[1], "tls://connect.ngs.global");
        assert_eq!(n.bucket, "cc-sessions");
    }

    #[test]
    fn nats_comes_from_the_file_and_the_variables_win() {
        let text = r#"{"refreshMins":15,"nats":{"servers":[" tls://nats.example:4222 ","wss://nats.example"],"bucket":"team_sessions"}}"#;
        let n = resolve_nats(Some(text), None, None);
        assert_eq!(n.servers, ["tls://nats.example:4222", "wss://nats.example"]);
        assert_eq!(n.bucket, "team_sessions");
        assert!(n.problems.is_empty(), "{:?}", n.problems);
        assert!(!n.is_default());

        let n = resolve_nats(
            Some(text),
            Some("wss://other.example:8443, ,tls://other.example"),
            Some("other"),
        );
        assert_eq!(
            n.servers,
            ["wss://other.example:8443", "tls://other.example"]
        );
        assert_eq!(n.bucket, "other");
        assert!(n.problems.is_empty(), "{:?}", n.problems);

        // Only the bucket in the file: the default servers.
        let n = resolve_nats(Some(r#"{"nats":{"bucket":"b2"}}"#), None, None);
        assert_eq!(
            (n.servers, n.bucket.as_str()),
            (cc_remote::default_servers(), "b2")
        );
    }

    /// A value that cannot be used is never half used or silently dropped:
    /// that setting is the default and a problem names what was ignored.
    #[test]
    fn unusable_nats_values_fall_back_to_the_defaults_and_say_so() {
        let n = resolve_nats(
            Some(
                r#"{"nats":{"servers":["tls://ok.example","nats://plain.example:4222"],"bucket":"team"}}"#,
            ),
            None,
            None,
        );
        assert_eq!(n.servers, cc_remote::default_servers());
        assert_eq!(n.bucket, "team", "the valid bucket still applies");
        assert_eq!(n.problems.len(), 1, "{:?}", n.problems);
        assert!(
            n.problems[0].contains("nats.servers ignored"),
            "{:?}",
            n.problems
        );
        assert!(n.problems[0].contains("plaintext"), "{:?}", n.problems);

        let n = resolve_nats(None, Some("tls://"), Some("cc.sessions"));
        assert!(n.is_default());
        assert_eq!(n.problems.len(), 2, "{:?}", n.problems);
        assert!(
            n.problems[0].starts_with(NATS_SERVERS_ENV),
            "{:?}",
            n.problems
        );
        assert!(
            n.problems[1].starts_with(NATS_BUCKET_ENV),
            "{:?}",
            n.problems
        );

        let n = resolve_nats(Some(r#"{"nats":{"servers":[]}}"#), None, None);
        assert!(n.is_default());
        assert!(n.problems[0].contains("no servers"), "{:?}", n.problems);

        for (text, needle) in [
            (r#"{"nats":"wss://x"}"#, "nats is not an object"),
            (r#"{"nats":{"servers":"wss://x"}}"#, "not a list of URLs"),
            (r#"{"nats":{"bucket":7}}"#, "nats.bucket is not a name"),
            (
                r#"{"nats":{"server":["wss://x"]}}"#,
                "nats.server is not a setting",
            ),
        ] {
            let n = resolve_nats(Some(text), None, None);
            assert!(n.is_default(), "{text}");
            assert_eq!(n.problems.len(), 1, "{text}: {:?}", n.problems);
            assert!(n.problems[0].contains(needle), "{text}: {:?}", n.problems);
        }

        // A user in a URL is refused without repeating the password.
        let n = resolve_nats(None, Some("wss://me:hunter2@nats.example"), None);
        assert!(n.is_default());
        assert!(!n.problems[0].contains("hunter2"), "{:?}", n.problems);
    }

    /// [`Settings`] does not hold the `"nats"` object; saving the other
    /// settings must not lose it.
    #[test]
    fn a_save_keeps_the_hand_edited_nats_object() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            r#"{"refreshMins":15,"nats":{"servers":["tls://nats.example:4222"],"bucket":"team"}}"#,
        )
        .unwrap();
        let mut s = read(dir.path());
        s.refresh_mins = 30;
        write(dir.path(), &s).unwrap();
        assert_eq!(read(dir.path()).refresh_mins, 30);
        let n = nats(dir.path());
        assert_eq!(n.servers, ["tls://nats.example:4222"]);
        assert_eq!(n.bucket, "team");
        // Without one, none is written.
        let other = tempfile::tempdir().unwrap();
        write(other.path(), &s).unwrap();
        let text = std::fs::read_to_string(path(other.path())).unwrap();
        assert!(!text.contains("nats"), "{text}");
    }

    /// The form shows what is saved, what is in force and the defaults; a
    /// variable makes its setting read-only and wins.
    #[test]
    fn the_nats_form_shows_saved_effective_and_overridden_values() {
        let f = nats_form_from(None, None, None);
        assert!(f.servers.is_empty() && f.bucket.is_empty());
        assert_eq!(f.default_servers, cc_remote::default_servers());
        assert_eq!(f.default_bucket, "cc-sessions");
        assert_eq!(f.effective_servers, f.default_servers);
        assert_eq!((f.servers_env, f.bucket_env), (None, None));

        let text = r#"{"nats":{"servers":[" tls://nats.example:4222 ",""],"bucket":" team "}}"#;
        let f = nats_form_from(Some(text), Some("  "), None);
        assert_eq!(f.servers, ["tls://nats.example:4222"]);
        assert_eq!(f.bucket, "team");
        assert_eq!(f.effective_servers, ["tls://nats.example:4222"]);
        assert_eq!(f.effective_bucket, "team");
        assert_eq!(f.servers_env, None, "a blank variable is unset");

        let f = nats_form_from(Some(text), Some("wss://other.example"), Some("other"));
        assert_eq!(f.servers, ["tls://nats.example:4222"], "the file's, kept");
        assert_eq!(f.effective_servers, ["wss://other.example"]);
        assert_eq!(f.effective_bucket, "other");
        assert_eq!(f.servers_env, Some(NATS_SERVERS_ENV));
        assert_eq!(f.bucket_env, Some(NATS_BUCKET_ENV));

        // An unusable saved value shows as typed, with the problem.
        let f = nats_form_from(
            Some(r#"{"nats":{"servers":["nats://plain:4222"],"bucket":7}}"#),
            None,
            None,
        );
        assert_eq!(f.servers, ["nats://plain:4222"]);
        assert_eq!(f.bucket, "");
        assert_eq!(f.effective_servers, cc_remote::default_servers());
        assert_eq!(f.problems.len(), 2, "{:?}", f.problems);
    }

    #[test]
    fn typed_nats_values_follow_cc_remote_rules() {
        let input = |servers: &[&str], bucket: &str| NatsInput {
            servers: servers.iter().map(|s| s.to_string()).collect(),
            bucket: bucket.into(),
        };
        let change = check_nats(
            &input(
                &[" tls://nats.example:4222", "", "wss://nats.example "],
                " team ",
            ),
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            change.servers,
            Some(Some(vec![
                "tls://nats.example:4222".to_string(),
                "wss://nats.example".to_string()
            ]))
        );
        assert_eq!(change.bucket, Some(Some("team".into())));

        // Blank, or the default itself, is stored as none.
        let defaults = cc_remote::default_servers();
        let defaults: Vec<&str> = defaults.iter().map(String::as_str).collect();
        for change in [
            check_nats(&input(&["", " "], ""), false, false).unwrap(),
            check_nats(&input(&defaults, "cc-sessions"), false, false).unwrap(),
        ] {
            assert_eq!(change.servers, Some(None));
            assert_eq!(change.bucket, Some(None));
        }

        // Every refused value is named; a password never repeated.
        let err = check_nats(
            &input(&["wss://me:hunter2@nats.example"], "cc.sessions"),
            false,
            false,
        )
        .unwrap_err();
        assert!(
            err.starts_with("server \"wss://***@nats.example\""),
            "{err}"
        );
        assert!(err.contains("no user or password"), "{err}");
        assert!(err.contains("bucket \"cc.sessions\""), "{err}");
        assert!(!err.contains("hunter2"), "{err}");
        let err = check_nats(&input(&["nats://plain:4222"], ""), false, false).unwrap_err();
        assert!(err.contains("plaintext"), "{err}");

        // A setting its variable overrides is not looked at.
        let change = check_nats(&input(&["nats://plain"], "bad.name"), true, true).unwrap();
        assert_eq!((change.servers, change.bucket), (None, None));
    }

    #[test]
    fn a_nats_save_writes_the_object_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            r#"{"refreshMins":15,"nats":{"servers":["nats://kept:4222"],"bucket":"old","server":1}}"#,
        )
        .unwrap();
        let s = read(dir.path());
        // The servers are overridden (kept as they are), the bucket set.
        let change = NatsChange {
            servers: None,
            bucket: Some(Some("team".into())),
        };
        write_with_nats(dir.path(), &s, &change).unwrap();
        assert_eq!(read(dir.path()).refresh_mins, 15);
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["nats"],
            serde_json::json!({"servers": ["nats://kept:4222"], "bucket": "team"}),
            "unknown members dropped"
        );

        // Back to the defaults: no object at all.
        let change = NatsChange {
            servers: Some(None),
            bucket: Some(None),
        };
        write_with_nats(dir.path(), &s, &change).unwrap();
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(!text.contains("nats"), "{text}");
        assert!(nats(dir.path()).is_default());

        // A new file gets the settings and the object.
        let other = tempfile::tempdir().unwrap();
        let change = NatsChange {
            servers: Some(Some(vec!["tls://nats.example".into()])),
            bucket: Some(None),
        };
        write_with_nats(other.path(), &Settings::default(), &change).unwrap();
        assert_eq!(read(other.path()), Settings::default());
        let n = nats(other.path());
        assert_eq!(n.servers, ["tls://nats.example"]);
        assert_eq!(n.bucket, "cc-sessions");
    }

    #[test]
    fn the_config_dir_is_named_after_the_tauri_identifier() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], APP_IDENTIFIER);
        if crate::paths::config_dir_override().is_none() && std::env::var_os("APPDATA").is_some() {
            assert!(default_dir().ends_with(APP_IDENTIFIER));
        }
    }

    #[test]
    fn any_other_failure_is_passed_through() {
        let msg = shortcut_problem("Alt+Shift+U", "Access is denied. (os error 5)");
        assert_eq!(
            msg,
            "Could not register Alt+Shift+U: Access is denied. (os error 5)"
        );
    }
}
