//! Command-line dispatch.
//!
//! - no arguments: the hook receiver ([`crate::receiver::run`]), silent, exit 0;
//! - `statusline [--settings PATH]`: the status line chain
//!   ([`crate::statusline::run`]): silent itself, exits with the chained
//!   command's code (0 when there is none);
//! - `install [--settings PATH] [--command CMD] [--no-statusline]`,
//!   `uninstall [--settings PATH]`, `status [--settings PATH]`: the settings
//!   installer, which talks normally on stdout and stderr and uses exit codes
//!   (1 failure, 2 usage error);
//! - `help`, `--help`, `-h`, `--version`, `-V`: what they say.
//!
//! Anything else is treated as a hook call when stdin is piped (so a stray
//! argument in a hook command can never make Claude Code show an error) and as
//! a usage error when stdin is a terminal.

use crate::install::{self, Action, Error, Outcome, StatusLineSetup};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

const USAGE: &str = "\
cc-hook: Claude Code hook receiver for cc-usage

USAGE:
    cc-hook                                         read one hook payload on stdin (what Claude Code runs)
    cc-hook statusline [--settings PATH]            status line: export the context window, then run the
                                                    status line command install replaced
    cc-hook install   [--settings PATH] [--command CMD] [--no-statusline]
    cc-hook uninstall [--settings PATH]
    cc-hook status    [--settings PATH]

OPTIONS:
    --settings PATH   settings file to edit (default: $CLAUDE_CONFIG_DIR/settings.json,
                      or ~/.claude/settings.json when CLAUDE_CONFIG_DIR is not an absolute path)
    --command CMD     hook command to register (default: this executable's path, quoted)
    --no-statusline   install the hooks only; leave statusLine alone

install also puts cc-hook in front of statusLine.command and records the
replaced statusLine in <data dir>/statusline-chain.json; uninstall puts it
back, unless statusLine was changed since.

ENVIRONMENT:
    CLAUDE_CONFIG_DIR  Claude Code's folder instead of ~/.claude (the default settings file
                       of install, uninstall, status and statusline); absolute paths only
    CC_USAGE_DATA_DIR  data directory override (spool, live files and the status line chain);
                       absolute paths only
    CC_HOOK_DEBUG      when set, the receiver logs to <data dir>/cc-hook.log
";

/// Runs cc-hook with the given arguments (without the program name) and
/// returns the process exit code.
pub fn run(args: &[OsString]) -> i32 {
    let Some(first) = args.first() else {
        crate::receiver::run();
        return 0;
    };
    match first.to_str() {
        Some("statusline") => crate::statusline::run(&args[1..]),
        Some("install") => cmd_install(&args[1..]),
        Some("uninstall") => cmd_uninstall(&args[1..]),
        Some("status") => cmd_status(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print!("{USAGE}");
            0
        }
        Some("--version" | "-V") => {
            println!("cc-hook {}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ if !std::io::stdin().is_terminal() => {
            crate::receiver::run();
            0
        }
        _ => {
            eprintln!("cc-hook: unknown command {first:?}\n\n{USAGE}");
            2
        }
    }
}

/// Parsed installer options.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `--settings PATH`.
    pub settings: Option<PathBuf>,
    /// `--command CMD`.
    pub command: Option<String>,
    /// `--no-statusline`.
    pub no_statusline: bool,
}

/// Parses `--settings PATH`, `--settings=PATH`, `--command CMD`,
/// `--command=CMD` and `--no-statusline`. `install_flags` is false for
/// subcommands that take neither a command nor `--no-statusline`.
pub fn parse_options(args: &[OsString], install_flags: bool) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let text = arg.to_string_lossy();
        let (flag, inline) = match text.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(OsString::from(v))),
            _ => (text.to_string(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--settings" => opts.settings = Some(PathBuf::from(value()?)),
            "--command" if install_flags => {
                let v = value()?;
                let v = v
                    .into_string()
                    .map_err(|_| "--command must be valid Unicode".to_string())?;
                opts.command = Some(v);
            }
            "--no-statusline" if install_flags && inline.is_none() => opts.no_statusline = true,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(opts)
}

fn settings_path(opts: &Options) -> Result<PathBuf, String> {
    opts.settings
        .clone()
        .or_else(install::default_settings_path)
        .ok_or_else(|| "cannot find Claude Code's folder (no CLAUDE_CONFIG_DIR or home directory); pass --settings PATH".to_string())
}

/// Whether `path` is the default settings file (the one `cc-hook statusline`
/// without `--settings` chains for).
fn is_default_settings(path: &Path) -> bool {
    install::default_settings_path()
        .is_some_and(|d| crate::chain::settings_key(&d) == crate::chain::settings_key(path))
}

fn usage_error(msg: &str) -> i32 {
    eprintln!("cc-hook: {msg}\n\n{USAGE}");
    2
}

fn fail(e: &Error) -> i32 {
    eprintln!("cc-hook: {e}");
    1
}

fn action_label(a: Action) -> &'static str {
    match a {
        Action::Added => "added",
        Action::Updated => "updated",
        Action::Unchanged => "unchanged",
        Action::Removed => "removed",
    }
}

fn print_outcome(outcome: &Outcome, path: &std::path::Path) {
    for (event, action) in &outcome.report.actions {
        println!("  {event:<20} {}", action_label(*action));
    }
    for note in &outcome.notes {
        println!("note: {note}");
    }
    if let Some(b) = &outcome.backup {
        println!("backup: {}", b.display());
    }
    if outcome.written {
        println!("wrote {}", path.display());
    } else {
        println!("nothing to change in {}", path.display());
    }
}

fn cmd_install(args: &[OsString]) -> i32 {
    let opts = match parse_options(args, true) {
        Ok(o) => o,
        Err(e) => return usage_error(&e),
    };
    let path = match settings_path(&opts) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    let command = match opts
        .command
        .clone()
        .map(Ok)
        .unwrap_or_else(install::default_command)
    {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let statusline = if opts.no_statusline {
        None
    } else {
        match install::statusline_command(
            &command,
            (!is_default_settings(&path)).then_some(path.as_path()),
        ) {
            Ok(command) => Some(StatusLineSetup {
                command,
                chain_file: cc_core::paths::statusline_chain_file(),
            }),
            Err(e) => return fail(&e),
        }
    };
    println!("installing {command} into {}", path.display());
    if let Some(sl) = &statusline {
        println!(
            "status line: {} (record in {})",
            sl.command,
            sl.chain_file.display()
        );
    }
    match install::install_file_with(&path, &command, statusline.as_ref()) {
        Ok(outcome) => {
            print_outcome(&outcome, &path);
            0
        }
        Err(e) => fail(&e),
    }
}

fn cmd_uninstall(args: &[OsString]) -> i32 {
    let opts = match parse_options(args, false) {
        Ok(o) => o,
        Err(e) => return usage_error(&e),
    };
    let path = match settings_path(&opts) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    println!("removing cc-hook from {}", path.display());
    let chain_file = cc_core::paths::statusline_chain_file();
    match install::uninstall_file_with(&path, Some(&chain_file)) {
        Ok(outcome) => {
            print_outcome(&outcome, &path);
            0
        }
        Err(e) => fail(&e),
    }
}

fn cmd_status(args: &[OsString]) -> i32 {
    let opts = match parse_options(args, false) {
        Ok(o) => o,
        Err(e) => return usage_error(&e),
    };
    let path = match settings_path(&opts) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    println!("settings: {}", path.display());
    match install::status_file(&path) {
        Ok(None) => {
            println!("file does not exist; cc-hook is not installed");
            return 0;
        }
        Ok(Some(events)) => {
            for s in events {
                let state = match (s.expected, s.commands.len()) {
                    (true, 0) => "missing",
                    (true, 1) => "installed",
                    (true, _) => "duplicated",
                    (false, _) => "stray (not a cc-hook event; install or uninstall removes it)",
                };
                println!("  {:<20} {state}", s.event);
                for c in &s.commands {
                    println!("  {:<20}   {c}", "");
                }
            }
        }
        Err(e) => return fail(&e),
    }
    let chain_file = cc_core::paths::statusline_chain_file();
    match install::statusline_status_file(&path, &chain_file) {
        Ok(Some(s)) => {
            let label = install::STATUS_LINE;
            match (&s.command, s.ours, &s.chain) {
                (None, _, _) => println!("  {label:<20} not chained (no statusLine command)"),
                (Some(c), false, _) => println!("  {label:<20} not chained (runs {c})"),
                (Some(c), true, Some(chain)) => {
                    println!("  {label:<20} chained: {c}");
                    match chain.command() {
                        Some(orig) => println!("  {:<20}   then runs {orig}", ""),
                        None => println!(
                            "  {:<20}   then runs nothing (there was no status line command)",
                            ""
                        ),
                    }
                }
                (Some(c), true, None) => {
                    println!("  {label:<20} chained: {c}");
                    println!(
                        "  {:<20}   no record in {}; runs nothing after the export",
                        "",
                        chain_file.display()
                    );
                }
            }
            0
        }
        Ok(None) => 0,
        Err(e) => fail(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_both_option_spellings() {
        let o = parse_options(
            &os(&["--settings", "a.json", "--command=\"x/cc-hook\""]),
            true,
        )
        .unwrap();
        assert_eq!(o.settings, Some(PathBuf::from("a.json")));
        assert_eq!(o.command.as_deref(), Some("\"x/cc-hook\""));
        assert!(!o.no_statusline);
        let o = parse_options(&os(&["--settings=b.json"]), false).unwrap();
        assert_eq!(o.settings, Some(PathBuf::from("b.json")));
        let o = parse_options(&os(&["--no-statusline"]), true).unwrap();
        assert!(o.no_statusline);
    }

    #[test]
    fn rejects_missing_values_and_unknown_flags() {
        assert!(parse_options(&os(&["--settings"]), false).is_err());
        assert!(parse_options(&os(&["--command", "x"]), false).is_err());
        assert!(parse_options(&os(&["--force"]), true).is_err());
        assert!(parse_options(&os(&["--no-statusline"]), false).is_err());
        assert!(parse_options(&os(&["--no-statusline=1"]), true).is_err());
    }
}
