//! Command line: argument parsing and the small subcommands (`init`,
//! `keygen`, `status`, `clear`). `run` and `check` live in their own modules.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::check::{self, Report};
use crate::config::Config;
use crate::keygen::{
    check_confirmation, identity_warning, pair_kid, publisher_keygen, replaces_identity,
    viewer_keygen, Generated, IfExists,
};
use crate::run::{run, RunOptions};
use crate::state::{InUse, PublishState};
use crate::util::{ago, format_utc, now_secs};

/// The program version, `cc-publish 0.1.0`.
pub const VERSION_LINE: &str = concat!("cc-publish ", env!("CARGO_PKG_VERSION"));

const USAGE: &str = "\
Publishes this machine's Claude Code sessions, encrypted, to the cc-sessions
NATS KV bucket so the cc-usage widget on another machine can show them.

Usage:
  cc-publish init --alias ALIAS [--config PATH] [--force]
                 write the config file (default <data_dir>/publish.json)
  cc-publish keygen [--if-missing | --force | --rotate] [--key PATH]
                    [--config PATH] [--confirm ALIAS]
                 create this machine's key pair, its identity (default
                 <data_dir>/keys/publisher.key), and print its public key
                 line, which the viewer asks to accept; --if-missing keeps
                 an existing key and prints its line. --rotate (adds a newer
                 key) and --force (a single new key) replace an existing key
                 only when confirmed with the alias of the config:
                 --confirm ALIAS, or typed at the prompt in a terminal
  cc-publish viewer-keygen [--if-missing | --force | --rotate]
                 on the viewer machine: create <data_dir>/keys/viewer.key and
                 viewer.pub, and print the public key line (copy viewer.pub
                 to every publisher)
  cc-publish check [--config PATH]
                 validate this machine end to end; exits 1 on any FAIL
  cc-publish run [--config PATH] [--force] [--for SECONDS]
                 publish until Ctrl-C, SIGTERM or `stop` (or for SECONDS)
  cc-publish status [--config PATH]
                 config, publisher state, the keys and creds it uses, last
                 publish time and counters;
                 exits 0 only while a publisher runs and wrote within 150 s
  cc-publish clear [--config PATH] [--force]
                 delete this machine's keys from the bucket now (the TTL
                 would remove them within 3 minutes anyway)
  cc-publish stop [--wait SECONDS]
                 ask the running publisher to stop cleanly and wait for it
                 (default 15 s); exits 0 when it stopped or was not running
  cc-publish --version

Every command also takes --data-dir DIR, the data directory (same as
CC_USAGE_DATA_DIR, which it overrides).

Environment:
  CC_USAGE_DATA_DIR  data directory, an absolute path (default:
                     %LOCALAPPDATA%\\cc-usage on Windows, $XDG_DATA_HOME/cc-usage
                     or ~/.local/share/cc-usage); check reports a relative one
  CC_PUBLISH_LOG     log level: error, warn, info (default), debug, trace
";

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `--help`.
    Help,
    /// `--version`.
    Version,
    /// `init`.
    Init {
        /// Machine alias.
        alias: String,
        /// Config file to write.
        config: Option<PathBuf>,
        /// Overwrite an existing config.
        force: bool,
    },
    /// `keygen`.
    Keygen {
        /// Key file to write.
        key: Option<PathBuf>,
        /// Config whose alias confirms replacing an existing key.
        config: Option<PathBuf>,
        /// `--confirm ALIAS`.
        confirm: Option<String>,
        /// What to do with an existing key file.
        exists: IfExists,
    },
    /// `viewer-keygen`.
    ViewerKeygen {
        /// What to do with an existing `viewer.key`.
        exists: IfExists,
    },
    /// `check`.
    Check {
        /// Config file.
        config: Option<PathBuf>,
    },
    /// `run`.
    Run {
        /// Config file.
        config: Option<PathBuf>,
        /// Start even if another run loop looks alive.
        force: bool,
        /// Stop after this many seconds.
        duration: Option<u64>,
    },
    /// `status`.
    Status {
        /// Config file.
        config: Option<PathBuf>,
    },
    /// `clear`.
    Clear {
        /// Config file.
        config: Option<PathBuf>,
        /// Clear even while a run loop is alive.
        force: bool,
    },
    /// `stop`.
    Stop {
        /// Seconds to wait for the run loop to stop.
        wait: Option<u64>,
    },
}

/// Removes `--data-dir DIR` (or `--data-dir=DIR`) from anywhere in `args`
/// and returns DIR, made absolute against the current directory.
pub fn take_data_dir(args: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
        let value = if args[i] == "--data-dir" {
            if i + 1 >= args.len() {
                return Err("--data-dir needs a value".into());
            }
            let v = args.remove(i + 1);
            args.remove(i);
            v
        } else if let Some(v) = args[i].strip_prefix("--data-dir=") {
            let v = v.to_string();
            args.remove(i);
            v
        } else {
            i += 1;
            continue;
        };
        if value.trim().is_empty() {
            return Err("--data-dir needs a value".into());
        }
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map_err(|e| format!("cannot resolve --data-dir: {e}"))?
                .join(path)
        };
        found = Some(path);
    }
    Ok(found)
}

/// Parses the arguments after the program name.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter().map(String::as_str);
    let Some(first) = it.next() else {
        return Ok(Command::Help);
    };
    let rest: Vec<&str> = it.collect();
    let mut config = None;
    let mut key = None;
    let mut alias = None;
    let mut confirm = None;
    let mut force = false;
    let mut if_missing = false;
    let mut rotate = false;
    let mut duration = None;
    let mut wait = None;
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        rest.get(*i)
            .map(|v| v.to_string())
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    while i < rest.len() {
        let arg = rest[i];
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };
        let mut take = |name: &str| match &inline {
            Some(v) => Ok(v.clone()),
            None => value(&mut i, name),
        };
        match flag {
            "--config" | "-c" => config = Some(PathBuf::from(take("--config")?)),
            "--key" => key = Some(PathBuf::from(take("--key")?)),
            "--alias" => alias = Some(take("--alias")?),
            "--confirm" => confirm = Some(take("--confirm")?),
            "--for" => {
                let v = take("--for")?;
                duration = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("--for takes whole seconds, not {v:?}"))?,
                );
            }
            "--wait" => {
                let v = take("--wait")?;
                wait = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("--wait takes whole seconds, not {v:?}"))?,
                );
            }
            "--force" | "-f" => force = true,
            "--if-missing" => if_missing = true,
            "--rotate" => rotate = true,
            "--help" | "-h" => return Ok(Command::Help),
            other => return Err(format!("unknown argument {other:?}")),
        }
        i += 1;
    }
    let only = |allowed: &[&str]| -> Result<(), String> {
        let used = [
            ("--config", config.is_some()),
            ("--key", key.is_some()),
            ("--alias", alias.is_some()),
            ("--confirm", confirm.is_some()),
            ("--force", force),
            ("--if-missing", if_missing),
            ("--rotate", rotate),
            ("--for", duration.is_some()),
            ("--wait", wait.is_some()),
        ];
        match used.iter().find(|(n, set)| *set && !allowed.contains(n)) {
            Some((n, _)) => Err(format!("{n} does not apply to {first}")),
            None => Ok(()),
        }
    };
    let exists = || -> Result<IfExists, String> {
        match (force, if_missing, rotate) {
            (false, false, false) => Ok(IfExists::Refuse),
            (true, false, false) => Ok(IfExists::Replace),
            (false, true, false) => Ok(IfExists::Keep),
            (false, false, true) => Ok(IfExists::Rotate),
            _ => Err("use only one of --force, --if-missing and --rotate".into()),
        }
    };
    match first {
        "--help" | "-h" | "help" => Ok(Command::Help),
        "--version" | "-V" | "version" => Ok(Command::Version),
        "init" => {
            only(&["--config", "--alias", "--force"])?;
            let alias = alias.ok_or("init needs --alias ALIAS")?;
            Ok(Command::Init {
                alias,
                config,
                force,
            })
        }
        "keygen" => {
            only(&[
                "--key",
                "--config",
                "--confirm",
                "--force",
                "--if-missing",
                "--rotate",
            ])?;
            Ok(Command::Keygen {
                key,
                config,
                confirm,
                exists: exists()?,
            })
        }
        "viewer-keygen" => {
            only(&["--force", "--if-missing", "--rotate"])?;
            Ok(Command::ViewerKeygen { exists: exists()? })
        }
        "check" => {
            only(&["--config"])?;
            Ok(Command::Check { config })
        }
        "run" => {
            only(&["--config", "--force", "--for"])?;
            Ok(Command::Run {
                config,
                force,
                duration,
            })
        }
        "status" => {
            only(&["--config"])?;
            Ok(Command::Status { config })
        }
        "clear" => {
            only(&["--config", "--force"])?;
            Ok(Command::Clear { config, force })
        }
        "stop" => {
            only(&["--wait"])?;
            Ok(Command::Stop { wait })
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

/// Entry point used by `main`.
pub fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    match take_data_dir(&mut args) {
        // Before anything reads a path; nothing else runs yet.
        Ok(Some(dir)) => std::env::set_var("CC_USAGE_DATA_DIR", dir),
        Ok(None) => {}
        Err(err) => {
            eprintln!("cc-publish: {err}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    let command = match parse_args(&args) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("cc-publish: {err}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match command {
        Command::Help => {
            println!("{VERSION_LINE}\n\n{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("{VERSION_LINE}");
            ExitCode::SUCCESS
        }
        Command::Init {
            alias,
            config,
            force,
        } => cmd_init(&alias, &config.unwrap_or_else(Config::default_path), force),
        Command::Keygen {
            key,
            config,
            confirm,
            exists,
        } => cmd_keygen(
            &key.unwrap_or_else(cc_core::paths::publisher_key_file),
            &config.unwrap_or_else(Config::default_path),
            confirm.as_deref(),
            exists,
        ),
        Command::ViewerKeygen { exists } => cmd_viewer_keygen(exists),
        Command::Check { config } => {
            let path = config.unwrap_or_else(Config::default_path);
            println!("{VERSION_LINE} check");
            let mut report = Report::new();
            check::check(&path, &mut report);
            let failures = report.failures();
            if failures == 0 {
                println!("RESULT  all checks passed");
                ExitCode::SUCCESS
            } else {
                println!("RESULT  {failures} check(s) failed");
                ExitCode::FAILURE
            }
        }
        Command::Run {
            config,
            force,
            duration,
        } => {
            let path = config.unwrap_or_else(Config::default_path);
            let config = match Config::load(&path) {
                Ok(c) => c,
                Err(err) => {
                    eprintln!("cc-publish: {err}");
                    return ExitCode::FAILURE;
                }
            };
            crate::logging::init(
                config
                    .log_file
                    .then(crate::logging::default_log_path)
                    .as_deref(),
            );
            let opts = RunOptions {
                force,
                duration: duration.map(Duration::from_secs),
            };
            match run(config, opts) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    tracing::error!("{err}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Status { config } => cmd_status(&config.unwrap_or_else(Config::default_path)),
        Command::Clear { config, force } => {
            cmd_clear(&config.unwrap_or_else(Config::default_path), force)
        }
        Command::Stop { wait } => cmd_stop(
            wait.map(Duration::from_secs)
                .unwrap_or(crate::stop::DEFAULT_WAIT),
        ),
    }
}

fn cmd_init(alias: &str, path: &Path, force: bool) -> ExitCode {
    if path.exists() && !force {
        eprintln!(
            "cc-publish: {} already exists; pass --force to replace it",
            path.display()
        );
        return ExitCode::FAILURE;
    }
    let config = match Config::new_default(alias, path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("cc-publish: {err}");
            return ExitCode::from(2);
        }
    };
    if let Err(err) = crate::util::write_atomic(path, config.to_json().as_bytes()) {
        eprintln!("cc-publish: cannot write {}: {err}", path.display());
        return ExitCode::FAILURE;
    }
    println!("wrote {}", path.display());
    println!("  alias  {} (user {})", config.alias, config.user());
    println!("  creds          {}", config.creds.display());
    println!("  publisher_key  {}", config.publisher_key.display());
    println!("  viewer_pub     {}", config.viewer_pub.display());
    ExitCode::SUCCESS
}

/// Prints the public key line alone on stdout (scripts capture it) and what
/// happened on stderr.
fn report_generated(what: &str, path: &Path, generated: &Generated) {
    if generated.changed {
        eprintln!(
            "cc-publish: wrote {what} {} to {}",
            generated.public.kid(),
            path.display()
        );
    } else {
        eprintln!(
            "cc-publish: {} already holds {what} {}; kept",
            path.display(),
            generated.public.kid()
        );
    }
    println!("{}", generated.public.to_line());
}

/// Confirms replacing the existing publisher key at `path`: `--confirm`, or
/// the alias typed at a prompt when stdin is a terminal, must be the alias
/// of `config`. Err is the exit code (2) after the reason is printed.
fn confirm_replacement(path: &Path, config: &Path, confirm: Option<&str>) -> Result<(), ExitCode> {
    let alias = match Config::load(config) {
        Ok(c) => c.alias,
        Err(err) => {
            eprintln!(
                "cc-publish: replacing {} needs this machine's alias, read from the config: {err} \
                 (pass --config PATH); nothing changed",
                path.display()
            );
            return Err(ExitCode::from(2));
        }
    };
    let typed = || {
        use std::io::{BufRead, IsTerminal, Write};
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            return None;
        }
        eprintln!("cc-publish: {}", identity_warning(&alias));
        eprint!("Type the alias ({alias}) to confirm: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok()?;
        Some(line)
    };
    check_confirmation(&alias, confirm, typed).map_err(|err| {
        eprintln!("cc-publish: {err}");
        ExitCode::from(2)
    })
}

fn cmd_keygen(path: &Path, config: &Path, confirm: Option<&str>, exists: IfExists) -> ExitCode {
    let replacing = replaces_identity(path, exists);
    if replacing {
        if let Err(code) = confirm_replacement(path, config, confirm) {
            return code;
        }
    }
    let result = pair_kid("p", now_secs()).and_then(|kid| publisher_keygen(path, &kid, exists));
    match result {
        Ok(generated) => {
            report_generated("publisher key", path, &generated);
            if replacing {
                eprintln!(
                    "cc-publish: this machine's identity changed: the viewer holds back its values \
                     until the new key is accepted there (compare the line above)"
                );
            } else if generated.changed {
                eprintln!(
                    "cc-publish: this key is this machine's identity; the viewer asks to accept it \
                     with the first value it receives (compare the line above)"
                );
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("cc-publish: {err}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_viewer_keygen(exists: IfExists) -> ExitCode {
    let key = cc_core::paths::viewer_key_file();
    let public = cc_core::paths::viewer_pub_file();
    let result =
        pair_kid("v", now_secs()).and_then(|kid| viewer_keygen(&key, &public, &kid, exists));
    match result {
        Ok(generated) => {
            report_generated("viewer key", &key, &generated);
            eprintln!(
                "cc-publish: publishers seal to {}; copy it (not viewer.key) to each of them",
                public.display()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("cc-publish: {err}");
            ExitCode::FAILURE
        }
    }
}

/// How the publisher is usually supervised on this OS.
fn service_hints() -> &'static [&'static str] {
    if cfg!(windows) {
        &[
            "schtasks /Query /TN \"cc-usage publisher\" /V /FO LIST",
            "log: %LOCALAPPDATA%\\cc-usage\\cc-publish.log",
            "stop: cc-publish stop",
        ]
    } else {
        &[
            "systemctl --user status cc-publish.service",
            "journalctl --user -u cc-publish.service -f",
        ]
    }
}

/// The envelope and creds rows of `status`. With a running publisher that
/// records what it uses (`in_use`), that, plus what the files on disk hold
/// where it differs; otherwise what the files on disk would give (`disk`,
/// the sealer's description or why there is none, and `disk_user`, the
/// creds file's user).
fn key_rows(
    disk: &Result<String, String>,
    disk_user: Option<&str>,
    running: bool,
    in_use: Option<&InUse>,
) -> Vec<(&'static str, String)> {
    let on_disk = match disk {
        Ok(d) => d.clone(),
        Err(e) => format!("NONE: {e}"),
    };
    let Some(used) = in_use.filter(|_| running) else {
        let note = if running {
            " (the files on disk; this publisher does not report what it uses)"
        } else {
            ""
        };
        return vec![("envelope", format!("{on_disk}{note}"))];
    };
    let recheck = crate::reload::RECHECK_EVERY.as_secs();
    let used_line = used.describe();
    let mut rows = vec![("envelope", format!("{used_line} (in use)"))];
    match disk {
        Ok(d) if *d == used_line => {}
        Ok(d) => rows.push((
            "disk keys",
            format!("{d}: not in use yet (picked up within {recheck} s)"),
        )),
        Err(_) => rows.push((
            "disk keys",
            format!("{on_disk}; the publisher keeps the keys in use"),
        )),
    }
    if let Some(user) = &used.creds_user {
        rows.push(("creds user", format!("{user} (in use)")));
    }
    // A file of the user in use may still have failed (expired, or its trial
    // did not connect): the error says so.
    if disk_user != used.creds_user.as_deref() || used.creds_error.is_some() {
        let found = disk_user.map_or_else(
            || "missing or not NATS credentials".to_string(),
            |n| format!("for {n}"),
        );
        let state = match &used.creds_error {
            Some(why) => format!("not usable ({why}); the connection keeps the credentials in use"),
            None => format!("not in use yet (picked up within {recheck} s)"),
        };
        rows.push(("disk creds", format!("{found}: {state}")));
    }
    rows
}

fn cmd_status(path: &Path) -> ExitCode {
    let now = now_secs();
    println!("{VERSION_LINE}");
    let row = |k: &str, v: String| println!("{k:<12}{v}");
    let state_path = PublishState::default_path();
    let state = PublishState::load(&state_path);
    let config = match Config::load(path) {
        Ok(c) => {
            row("config", path.display().to_string());
            Some(c)
        }
        Err(err) => {
            row("config", format!("{err}"));
            row("hint", "cc-publish init --alias <alias>".into());
            None
        }
    };
    if let Some(c) = &config {
        row("alias", format!("{} (user {})", c.alias, c.user()));
        row("servers", c.servers.join(", "));
        row("bucket", c.bucket.clone());
        let found = |p: &Path| if p.is_file() { "found" } else { "MISSING" };
        row(
            "creds",
            format!("{} ({})", c.creds.display(), found(&c.creds)),
        );
        let disk = crate::sealing::select(c)
            .map(|s| s.describe())
            .map_err(|e| e.to_string());
        let disk_user = crate::check::creds_user_name(&c.creds);
        let running = state
            .as_ref()
            .filter(|s| s.is_alive(now) && s.alias == c.alias);
        for (k, v) in key_rows(
            &disk,
            disk_user.as_deref(),
            running.is_some(),
            running.and_then(|s| s.in_use.as_ref()),
        ) {
            row(k, v);
        }
        row(
            "keys",
            format!(
                "{} ({}), {} ({})",
                c.publisher_key.display(),
                found(&c.publisher_key),
                c.viewer_pub.display(),
                found(&c.viewer_pub)
            ),
        );
    }
    let mut health = match (&config, &state) {
        (None, _) => Some("no valid config".to_string()),
        (Some(_), None) => Some("the publisher has not run yet".to_string()),
        (Some(_), Some(s)) => s.unhealthy_reason(now),
    };
    if let (Some(c), Some(s), None) = (&config, &state, &health) {
        if s.alias != c.alias {
            health = Some(format!("the running publisher is for alias {}", s.alias));
        }
    }
    match &state {
        None => row("publisher", "has not run yet".into()),
        Some(s) => {
            let status = if s.is_alive(now) {
                format!("running, pid {}, up {}", s.pid, ago(now - s.started_at))
            } else if s.is_live(now) {
                format!("not running (pid {} has exited)", s.pid)
            } else if s.running {
                format!(
                    "not running (pid {} stopped updating {} ago)",
                    s.pid,
                    ago(now - s.updated_at)
                )
            } else {
                format!("stopped {} ago", ago(now - s.updated_at))
            };
            row("publisher", status);
            if s.is_alive(now) {
                row(
                    "connection",
                    match (&s.server, s.connected) {
                        (Some(server), true) => format!("connected via {server}"),
                        (_, _) => "disconnected (retrying)".into(),
                    },
                );
            }
            row("sessions", format!("{} published", s.keys.len()));
            row(
                "projects",
                format!("{} in the last 30 days, published", s.projects),
            );
            row(
                "last write",
                match s.last_publish_at {
                    Some(t) => format!("{} ago ({})", ago(now - t), format_utc(t)),
                    None => "never".into(),
                },
            );
            row(
                "writes",
                format!(
                    "{} puts, {} deletes, {} host, {} project, {} failed flushes, {} pending",
                    s.puts, s.deletes, s.host_puts, s.project_puts, s.failures, s.pending
                ),
            );
            if let (Some(err), Some(t)) = (&s.last_error, s.last_error_at) {
                row("last error", format!("{} ago: {err}", ago(now - t)));
            }
            if let Some(err) = &s.agents_error {
                row("claude CLI", err.clone());
            }
            if let Some(why) = s.exit_reason.as_ref().filter(|_| !s.running) {
                row("exited", why.clone());
            }
        }
    }
    row("state file", state_path.display().to_string());
    for (i, hint) in service_hints().iter().enumerate() {
        row(if i == 0 { "service" } else { "" }, hint.to_string());
    }
    match health {
        None => {
            row("health", "ok: publishing".into());
            ExitCode::SUCCESS
        }
        Some(reason) => {
            row("health", format!("NOT PUBLISHING: {reason}"));
            ExitCode::FAILURE
        }
    }
}

fn cmd_stop(wait: Duration) -> ExitCode {
    use crate::stop::{self, StopOutcome};
    match stop::stop(&PublishState::default_path(), &stop::default_path(), wait) {
        Ok(StopOutcome::NotRunning) => {
            println!("no publisher is running");
            ExitCode::SUCCESS
        }
        Ok(StopOutcome::Stopped(pid)) => {
            println!("stopped the publisher (pid {pid})");
            ExitCode::SUCCESS
        }
        Ok(StopOutcome::TimedOut(pid)) => {
            eprintln!(
                "cc-publish: the publisher (pid {pid}) did not stop within {} s",
                wait.as_secs()
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("cc-publish: cannot write the stop request: {err}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_clear(path: &Path, force: bool) -> ExitCode {
    let config = match Config::load(path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("cc-publish: {err}");
            return ExitCode::FAILURE;
        }
    };
    let state_path = PublishState::default_path();
    let state = PublishState::load(&state_path).unwrap_or_default();
    if state.is_alive(now_secs()) && !force {
        eprintln!(
            "cc-publish: the publisher is running (pid {}); stop it first, or pass --force",
            state.pid
        );
        return ExitCode::FAILURE;
    }
    let mut keys: Vec<String> = state
        .keys
        .iter()
        .filter(|k| k.starts_with(&format!("{}.", config.alias)))
        .cloned()
        .collect();
    keys.push(cc_envelope::host_key(&config.alias));
    keys.push(format!("{}.check", config.alias));
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("cc-publish: {err}");
            return ExitCode::FAILURE;
        }
    };
    let ok = rt.block_on(async {
        let client = match cc_remote::connect(config.remote()).await {
            Ok(c) => c,
            Err(err) => {
                eprintln!("cc-publish: connect failed: {err}");
                return false;
            }
        };
        let mut ok = true;
        match cc_remote::Publisher::bind(&client, &config.bucket).await {
            Ok(publisher) => {
                for key in &keys {
                    match publisher.delete(key).await {
                        Ok(()) => println!("deleted {key}"),
                        Err(err) => {
                            println!("failed  {key}: {err}");
                            ok = false;
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("cc-publish: bind failed: {err}");
                ok = false;
            }
        }
        client.close().await;
        ok
    });
    if ok {
        let mut state = state;
        state.keys.clear();
        let _ = state.save(&state_path);
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn commands_and_flags_parse() {
        assert_eq!(p(&[]).unwrap(), Command::Help);
        assert_eq!(p(&["--version"]).unwrap(), Command::Version);
        assert_eq!(p(&["-V"]).unwrap(), Command::Version);
        assert_eq!(
            p(&["run", "--config", "/x/p.json", "--for=60"]).unwrap(),
            Command::Run {
                config: Some("/x/p.json".into()),
                force: false,
                duration: Some(60)
            }
        );
        assert_eq!(
            p(&["keygen", "--force"]).unwrap(),
            Command::Keygen {
                key: None,
                config: None,
                confirm: None,
                exists: IfExists::Replace
            }
        );
        assert_eq!(
            p(&["init", "--alias", "lab-01"]).unwrap(),
            Command::Init {
                alias: "lab-01".into(),
                config: None,
                force: false
            }
        );
        assert_eq!(
            p(&["check", "-c", "a.json"]).unwrap(),
            Command::Check {
                config: Some("a.json".into())
            }
        );
        assert_eq!(p(&["status", "--help"]).unwrap(), Command::Help);
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(p(&["publish"]).is_err());
        assert!(p(&["run", "--config"])
            .unwrap_err()
            .contains("needs a value"));
        assert!(p(&["run", "--for", "1m"]).is_err());
        assert!(p(&["check", "--force"])
            .unwrap_err()
            .contains("does not apply"));
        assert!(p(&["init"]).unwrap_err().contains("--alias"));
        assert!(p(&["status", "--bogus"]).is_err());
        assert!(p(&["stop", "--wait", "soon"]).is_err());
        assert!(p(&["run", "--wait", "5"])
            .unwrap_err()
            .contains("does not apply"));
    }

    #[test]
    fn keygen_flags_parse() {
        let keygen = |key: Option<&str>, exists| Command::Keygen {
            key: key.map(PathBuf::from),
            config: None,
            confirm: None,
            exists,
        };
        assert_eq!(p(&["keygen"]).unwrap(), keygen(None, IfExists::Refuse));
        assert_eq!(
            p(&["keygen", "--if-missing", "--key", "/k/p.key"]).unwrap(),
            keygen(Some("/k/p.key"), IfExists::Keep)
        );
        assert_eq!(
            p(&["keygen", "--rotate"]).unwrap(),
            keygen(None, IfExists::Rotate)
        );
        assert_eq!(
            p(&[
                "keygen",
                "--rotate",
                "--confirm=lab-01",
                "--config",
                "/k/publish.json"
            ])
            .unwrap(),
            Command::Keygen {
                key: None,
                config: Some("/k/publish.json".into()),
                confirm: Some("lab-01".into()),
                exists: IfExists::Rotate
            }
        );
        assert!(p(&["keygen", "--confirm"])
            .unwrap_err()
            .contains("needs a value"));
        for args in [
            &["viewer-keygen", "--confirm", "x"][..],
            &["clear", "--confirm", "x"],
        ] {
            assert!(p(args).unwrap_err().contains("does not apply"), "{args:?}");
        }
        assert!(p(&["keygen", "--shared"])
            .unwrap_err()
            .contains("unknown argument"));
        assert!(p(&["keygen", "--force", "--if-missing"])
            .unwrap_err()
            .contains("only one"));
        assert_eq!(
            p(&["viewer-keygen", "--if-missing"]).unwrap(),
            Command::ViewerKeygen {
                exists: IfExists::Keep
            }
        );
        assert!(p(&["viewer-keygen", "--key", "x"])
            .unwrap_err()
            .contains("does not apply"));
        assert!(p(&["run", "--if-missing"])
            .unwrap_err()
            .contains("does not apply"));
    }

    #[test]
    fn stop_takes_an_optional_wait() {
        assert_eq!(p(&["stop"]).unwrap(), Command::Stop { wait: None });
        assert_eq!(
            p(&["stop", "--wait=30"]).unwrap(),
            Command::Stop { wait: Some(30) }
        );
    }

    #[test]
    fn data_dir_is_taken_from_anywhere_and_made_absolute() {
        let mut a: Vec<String> = ["run", "--data-dir", "rel dir", "--for", "5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let dir = take_data_dir(&mut a).unwrap().unwrap();
        assert!(dir.is_absolute());
        assert!(dir.ends_with("rel dir"));
        assert_eq!(a, ["run", "--for", "5"]);
        let abs = std::env::temp_dir().join("x");
        let mut a = vec![
            format!("--data-dir={}", abs.display()),
            "status".to_string(),
        ];
        assert_eq!(take_data_dir(&mut a).unwrap(), Some(abs));
        assert_eq!(a, ["status"]);
        let mut a = vec!["status".to_string()];
        assert_eq!(take_data_dir(&mut a).unwrap(), None);
        let mut a = vec!["status".to_string(), "--data-dir".to_string()];
        assert!(take_data_dir(&mut a).is_err());
        let mut a = vec!["--data-dir=".to_string()];
        assert!(take_data_dir(&mut a).is_err());
    }

    #[test]
    fn status_shows_the_keys_in_use_and_what_differs_on_disk() {
        let used = InUse {
            publisher_kid: "p1".into(),
            viewer_kid: "k1".into(),
            creds_user: Some("cc-sessions.pub-lab-01".into()),
            ..Default::default()
        };
        let same = Ok(used.describe());
        let user = Some("cc-sessions.pub-lab-01");
        fn labels(rows: &[(&'static str, String)]) -> Vec<&'static str> {
            rows.iter().map(|(k, _)| *k).collect()
        }

        // Not running: the files on disk, as before.
        let rows = key_rows(&same, user, false, Some(&used));
        assert_eq!(rows, [("envelope", used.describe())]);
        let rows = key_rows(&Err("keys/viewer.pub is missing".into()), user, false, None);
        assert_eq!(rows[0].1, "NONE: keys/viewer.pub is missing");

        // Running, files as in use: only what is in use.
        let rows = key_rows(&same, user, true, Some(&used));
        assert_eq!(labels(&rows), ["envelope", "creds user"]);
        assert!(rows[0].1.ends_with("(in use)") && rows[0].1.contains("viewer key k1"));

        // A rotated viewer key not picked up yet, and broken files.
        let rotated = Ok("publisher key p1, sealed to viewer key k2".to_string());
        let rows = key_rows(&rotated, user, true, Some(&used));
        assert_eq!(labels(&rows), ["envelope", "disk keys", "creds user"]);
        assert!(
            rows[1].1.contains("viewer key k2") && rows[1].1.contains("not in use yet"),
            "{rows:?}"
        );
        let rows = key_rows(
            &Err("cannot load viewer.pub".into()),
            user,
            true,
            Some(&used),
        );
        assert!(
            rows[1].1.contains("NONE") && rows[1].1.contains("keeps the keys in use"),
            "{rows:?}"
        );

        // Creds for another user on disk, rejected by the publisher.
        let rejected = InUse {
            creds_error: Some("for another user".into()),
            ..used.clone()
        };
        let rows = key_rows(&same, Some("cc-sessions.pub-other"), true, Some(&rejected));
        assert_eq!(labels(&rows), ["envelope", "creds user", "disk creds"]);
        assert!(
            rows[2].1.contains("cc-sessions.pub-other") && rows[2].1.contains("not usable"),
            "{rows:?}"
        );

        // Creds of the same user whose trial failed: the row says why.
        let failed = InUse {
            creds_error: Some("u.creds did not connect: authorization violation".into()),
            ..used.clone()
        };
        let rows = key_rows(&same, user, true, Some(&failed));
        assert_eq!(labels(&rows), ["envelope", "creds user", "disk creds"]);
        assert!(
            rows[2]
                .1
                .contains("did not connect: authorization violation"),
            "{rows:?}"
        );

        // A running publisher of a version that does not report it.
        let rows = key_rows(&same, user, true, None);
        assert!(rows[0].1.contains("does not report"), "{rows:?}");
    }

    #[test]
    fn version_line_names_the_crate_version() {
        assert_eq!(
            VERSION_LINE,
            format!("cc-publish {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
