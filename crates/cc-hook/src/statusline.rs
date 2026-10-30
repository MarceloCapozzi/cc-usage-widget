//! `cc-hook statusline`: Claude Code's status line command, chained in front
//! of the user's own.
//!
//! Claude Code runs the status line command on every render and passes it a
//! JSON description of the session on stdin, including the exact context
//! window use (`context_window.used_percentage`) that no other source has. This
//! command:
//!
//! 1. reads all of stdin;
//! 2. writes the useful part to `<data_dir>/live/<session_id>.json`
//!    ([`LiveExport`], temp file then rename) for the widget, unless the
//!    session id is missing or not made of `[A-Za-z0-9._-]`;
//! 3. runs the status line command it replaced (from the chain file, see
//!    [`crate::chain`]) through the shell Claude Code would use
//!    ([`crate::shell`]), with the same stdin bytes and its stdout and stderr
//!    inherited, so the original's output reaches Claude Code untouched; and
//!    exits with the original's exit code. A command still running after
//!    [`CHAIN_TIMEOUT`] is killed with every process it started, and cc-hook
//!    exits 0 with whatever it printed by then.
//!
//! The chain record used is the one of the `--settings` file, else of the
//! default settings file ([`crate::install::default_settings_path`]). That
//! default follows `CLAUDE_CONFIG_DIR`, which Claude Code passes on to the
//! command, so it is the file Claude Code read this status line from.
//!
//! stdin is read up to [`MAX_STDIN_BYTES`]. On Unix the live
//! folder is created `0700` and its files `0600`.
//!
//! Silence contract: cc-hook itself never writes to stdout or stderr. When
//! there is no chained command, or it cannot be started, it prints nothing and
//! exits 0. A failed export never stops the chained command. No network.
//! Diagnostics go to `cc-hook.log` only with `CC_HOOK_DEBUG` set.

use crate::{chain, debuglog, shell, spool};
use cc_core::fsutil::rename_retrying;
use cc_core::process_tree;
use serde::Serialize;
use serde_json::Value;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Format version of the live files.
pub const LIVE_VERSION: u32 = 1;

/// Live files not rewritten for this long are deleted by the occasional prune.
pub const LIVE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// One call in this many prunes the live folder.
pub const PRUNE_ONE_IN: u64 = 32;

/// Most bytes read from stdin. A status line payload is a couple of KiB;
/// past this the input is cut (and the export skipped as not JSON).
pub const MAX_STDIN_BYTES: u64 = 4 * 1024 * 1024;

/// What `<data_dir>/live/<session_id>.json` holds. Keys are camelCase; every
/// key is always present, `null` when Claude Code did not send the value
/// (context fields are `null` before the first API response, rate limits for
/// API-key users).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveExport {
    /// [`LIVE_VERSION`].
    pub v: u32,
    /// Unix seconds (with fraction) when cc-hook wrote the file.
    pub ts: f64,
    /// Claude Code's session id.
    pub session_id: String,
    /// `context_window.used_percentage`, 0 to 100.
    pub context_used_percent: Option<f64>,
    /// `context_window.context_window_size`, in tokens.
    pub context_window_size: Option<u64>,
    /// Input tokens of the last API call (input + cache creation + cache read,
    /// the same formula as `used_percentage`), from `context_window.current_usage`.
    pub context_used_tokens: Option<u64>,
    /// `model.id`.
    pub model_id: Option<String>,
    /// `model.display_name`.
    pub model_display_name: Option<String>,
    /// `rate_limits.five_hour.used_percentage`.
    pub five_hour_percent: Option<f64>,
    /// `rate_limits.five_hour.resets_at`, Unix seconds.
    pub five_hour_resets_at: Option<f64>,
    /// `rate_limits.seven_day.used_percentage`.
    pub seven_day_percent: Option<f64>,
    /// `rate_limits.seven_day.resets_at`, Unix seconds.
    pub seven_day_resets_at: Option<f64>,
    /// `cost.total_cost_usd`.
    pub cost_usd: Option<f64>,
    /// `workspace.current_dir`, or `cwd`.
    pub cwd: Option<String>,
    /// `transcript_path`.
    pub transcript_path: Option<String>,
}

/// Whether a session id can name a file: 1 to 128 characters of
/// `[A-Za-z0-9._-]`, not starting with a dot (dotted names are temp files).
pub fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn lookup<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |cur, key| cur.get(key))
}

fn num(v: &Value, path: &[&str]) -> Option<f64> {
    lookup(v, path)?.as_f64().filter(|n| n.is_finite())
}

fn text(v: &Value, path: &[&str]) -> Option<String> {
    lookup(v, path)?.as_str().map(str::to_string)
}

fn tokens(v: &Value, path: &[&str]) -> Option<u64> {
    num(v, path).filter(|n| *n >= 0.0).map(|n| n as u64)
}

/// The export for one status line payload, or `None` when it has no usable
/// session id.
pub fn build_export(input: &Value, ts: f64) -> Option<LiveExport> {
    let session_id = input.get("session_id")?.as_str()?;
    if !valid_session_id(session_id) {
        return None;
    }
    let used_tokens = lookup(input, &["context_window", "current_usage"])
        .filter(|u| u.is_object())
        .map(|u| {
            [
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
            ]
            .iter()
            .filter_map(|k| tokens(u, &[k]))
            .sum()
        });
    Some(LiveExport {
        v: LIVE_VERSION,
        ts,
        session_id: session_id.to_string(),
        context_used_percent: num(input, &["context_window", "used_percentage"]),
        context_window_size: tokens(input, &["context_window", "context_window_size"]),
        context_used_tokens: used_tokens,
        model_id: text(input, &["model", "id"]),
        model_display_name: text(input, &["model", "display_name"]),
        five_hour_percent: num(input, &["rate_limits", "five_hour", "used_percentage"]),
        five_hour_resets_at: num(input, &["rate_limits", "five_hour", "resets_at"]),
        seven_day_percent: num(input, &["rate_limits", "seven_day", "used_percentage"]),
        seven_day_resets_at: num(input, &["rate_limits", "seven_day", "resets_at"]),
        cost_usd: num(input, &["cost", "total_cost_usd"]),
        cwd: text(input, &["workspace", "current_dir"]).or_else(|| text(input, &["cwd"])),
        transcript_path: text(input, &["transcript_path"]),
    })
}

/// What [`export`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum ExportOutcome {
    /// Written to this file.
    Written(PathBuf),
    /// Nothing written, and why.
    Skipped(&'static str),
}

/// Parses a status line payload and writes its live file into `live_dir`.
pub fn export(input: &[u8], live_dir: &Path, now: SystemTime) -> io::Result<ExportOutcome> {
    let input = input.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(input);
    if input.iter().all(u8::is_ascii_whitespace) {
        return Ok(ExportOutcome::Skipped("empty input"));
    }
    let Ok(value) = serde_json::from_slice::<Value>(input) else {
        return Ok(ExportOutcome::Skipped("input is not JSON"));
    };
    let ts = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let Some(live) = build_export(&value, ts) else {
        return Ok(ExportOutcome::Skipped("no valid session_id"));
    };
    spool::create_private_dir(live_dir)?;
    let bytes = serde_json::to_vec(&live).map_err(io::Error::other)?;
    let name = format!("{}.json", live.session_id);
    let tmp = live_dir.join(format!(".{name}.tmp-{}", spool::unique_token()));
    let target = live_dir.join(&name);
    if let Err(e) = spool::write_private(&tmp, &bytes).and_then(|()| rename_retrying(&tmp, &target))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(ExportOutcome::Written(target))
}

/// Deletes files in `live_dir` (live files and leftover temp files) not
/// modified for [`LIVE_MAX_AGE`]. Returns how many were deleted.
pub fn prune(live_dir: &Path, now: SystemTime) -> io::Result<usize> {
    let mut n = 0;
    for entry in std::fs::read_dir(live_dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || !entry.file_name().to_string_lossy().contains(".json") {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > LIVE_MAX_AGE);
        if old && std::fs::remove_file(entry.path()).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

/// Longest the chained status line command may run. Claude Code runs the
/// status line on every render; one that hangs (a network call, a prompt)
/// must not pile up processes, so past this it is killed with everything it
/// started.
pub const CHAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `command` through `shell` with `input` on stdin and stdout and stderr
/// inherited, for at most [`CHAIN_TIMEOUT`]. Returns its exit code, `0` when
/// it timed out (whatever it printed so far stands), or `None` when it could
/// not be started.
pub fn run_chained(shell: &shell::Shell, command: &str, input: &[u8]) -> Option<i32> {
    run_chained_for(shell, command, input, CHAIN_TIMEOUT)
}

/// [`run_chained`] with an explicit timeout.
///
/// The command's process tree is contained so a timeout kills all of it: on
/// Windows the shell is put in a job object that kills every process in it
/// when cc-hook closes the job (on timeout, or when the command ends, so
/// background processes it left die too); on Unix the shell leads its own
/// process group, which gets `SIGKILL` on timeout (background processes of a
/// command that ended in time are left alone).
pub fn run_chained_for(
    shell: &shell::Shell,
    command: &str,
    input: &[u8],
    timeout: Duration,
) -> Option<i32> {
    let mut cmd = shell.command(command);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    no_window(&mut cmd);
    process_tree::own_group(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            debuglog::log(&format!(
                "statusline: cannot start {:?}: {e}",
                shell.program
            ));
            return None;
        }
    };
    let tree = process_tree::ProcessTree::adopt(&child);
    // Feed stdin from a thread: a command that does not read it must not
    // block us, and one that reads it slowly still gets all of it.
    let writer = child.stdin.take().map(|mut stdin| {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let started = Instant::now();
    let mut pause = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() >= timeout => {
                debuglog::log(&format!(
                    "statusline: chained command timed out after {timeout:?}; killed"
                ));
                tree.kill();
                let _ = child.kill();
                let _ = child.wait();
                return Some(0);
            }
            Ok(None) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(20));
            }
            Err(e) => break Err(e),
        }
    };
    // On Windows closing the job kills what the command left running; on
    // Unix its process group is left alone.
    tree.close();
    // A writer still blocked (a grandchild holds stdin open) is left behind:
    // the process exits right after this.
    if let Some(w) = writer.filter(|w| w.is_finished()) {
        let _ = w.join();
    }
    match status {
        Ok(s) => Some(s.code().unwrap_or(1)),
        Err(e) => {
            debuglog::log(&format!("statusline: wait failed: {e}"));
            Some(1)
        }
    }
}

/// No console window for the shell: cc-hook may run without a console, and
/// Windows would open one for a console program started from it.
#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

/// `--settings PATH` or `--settings=PATH` among the arguments; anything else
/// is ignored (this runs inside Claude Code and must never fail on usage).
pub fn settings_arg(args: &[OsString]) -> Option<PathBuf> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        if s == "--settings" {
            return it.next().map(PathBuf::from);
        }
        if let Some(v) = s.strip_prefix("--settings=") {
            return Some(PathBuf::from(v));
        }
    }
    None
}

/// At most [`MAX_STDIN_BYTES`] of stdin.
fn read_stdin() -> Vec<u8> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = stdin.lock().take(MAX_STDIN_BYTES).read_to_end(&mut buf);
    buf
}

/// The whole command against the real stdin, data folder and chain file.
/// Returns the exit code to use.
pub fn run(args: &[OsString]) -> i32 {
    let started = Instant::now();
    std::panic::set_hook(Box::new(|info| debuglog::log(&format!("panic: {info}"))));
    let input = std::panic::catch_unwind(read_stdin).unwrap_or_default();

    let live_dir = cc_core::paths::live_dir();
    let exported = std::panic::catch_unwind(|| {
        let now = SystemTime::now();
        let out = export(&input, &live_dir, now);
        if spool::random_u64().is_multiple_of(PRUNE_ONE_IN) {
            let _ = prune(&live_dir, now);
        }
        out
    });
    let export_time = started.elapsed();
    match &exported {
        Ok(Ok(o)) => debuglog::log(&format!("statusline: {o:?} in {export_time:?}")),
        Ok(Err(e)) => debuglog::log(&format!("statusline: export failed: {e}")),
        Err(_) => debuglog::log("statusline: export panicked"),
    }

    let code = std::panic::catch_unwind(|| {
        let settings = settings_arg(args).or_else(crate::install::default_settings_path)?;
        let chain = chain::lookup(&cc_core::paths::statusline_chain_file(), &settings)?;
        let command = chain.command()?.to_string();
        run_chained(&shell::claude_code_shell(), &command, &input)
    });
    let code = code.ok().flatten().unwrap_or(0);
    debuglog::log(&format!(
        "statusline: exit {code} after {:?}",
        started.elapsed()
    ));
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{skip_without_shell, TestDir};
    use serde_json::json;

    /// A live folder that does not exist yet, inside a fresh temp folder
    /// that goes away with the returned guard.
    fn temp_dir(tag: &str) -> (TestDir, PathBuf) {
        let root = TestDir::new(&format!("sl-{tag}"));
        let dir = root.join("live");
        (root, dir)
    }

    fn sample() -> Value {
        json!({
            "session_id": "0b3c-4d5e_f.6",
            "transcript_path": "C:/t.jsonl",
            "cwd": "C:/w",
            "model": { "id": "claude-opus-5-5", "display_name": "Opus" },
            "workspace": { "current_dir": "C:/w/sub" },
            "cost": { "total_cost_usd": 0.5 },
            "context_window": {
                "context_window_size": 1000000, "used_percentage": 33.4,
                "current_usage": { "input_tokens": 1000, "output_tokens": 50,
                                   "cache_creation_input_tokens": 2000, "cache_read_input_tokens": 330000 }
            },
            "rate_limits": {
                "five_hour": { "used_percentage": 23.5, "resets_at": 1791900000 },
                "seven_day": { "used_percentage": 41.2, "resets_at": 1792300000 }
            }
        })
    }

    #[test]
    fn exports_the_context_model_and_limits() {
        let e = build_export(&sample(), 12.5).unwrap();
        assert_eq!(e.session_id, "0b3c-4d5e_f.6");
        assert_eq!(e.context_used_percent, Some(33.4));
        assert_eq!(e.context_window_size, Some(1_000_000));
        assert_eq!(e.context_used_tokens, Some(333_000));
        assert_eq!(e.model_id.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(e.model_display_name.as_deref(), Some("Opus"));
        assert_eq!(e.five_hour_percent, Some(23.5));
        assert_eq!(e.seven_day_resets_at, Some(1_792_300_000.0));
        assert_eq!(e.cwd.as_deref(), Some("C:/w/sub"));
        // The on-disk keys the widget reads.
        let v = serde_json::to_value(&e).unwrap();
        for key in [
            "v",
            "ts",
            "sessionId",
            "contextUsedPercent",
            "contextWindowSize",
            "contextUsedTokens",
            "modelId",
            "modelDisplayName",
            "fiveHourPercent",
            "fiveHourResetsAt",
            "sevenDayPercent",
            "sevenDayResetsAt",
            "costUsd",
            "cwd",
            "transcriptPath",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
        assert_eq!(v["v"], 1);
    }

    #[test]
    fn null_context_early_in_a_session_is_exported_as_null() {
        let input = json!({
            "session_id": "s1",
            "model": { "id": "claude-opus-5-5", "display_name": "Opus" },
            "context_window": { "context_window_size": 200000, "used_percentage": null, "current_usage": null }
        });
        let e = build_export(&input, 1.0).unwrap();
        assert_eq!(e.context_used_percent, None);
        assert_eq!(e.context_used_tokens, None);
        assert_eq!(e.context_window_size, Some(200_000));
        assert_eq!(e.five_hour_percent, None);
        let v = serde_json::to_value(&e).unwrap();
        assert!(v["contextUsedPercent"].is_null());
        // No context_window at all.
        let e = build_export(&json!({ "session_id": "s1" }), 1.0).unwrap();
        assert_eq!(
            (e.context_used_percent, e.context_window_size),
            (None, None)
        );
    }

    #[test]
    fn session_ids_that_cannot_name_a_file_are_refused() {
        let long = "x".repeat(129);
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "../evil",
            "a/b",
            r"a\b",
            "a b",
            "a:b",
            "é",
            long.as_str(),
        ] {
            assert!(!valid_session_id(bad), "{bad:?}");
            assert!(
                build_export(&json!({ "session_id": bad }), 1.0).is_none(),
                "{bad:?}"
            );
        }
        assert!(build_export(&json!({ "session_id": 42 }), 1.0).is_none());
        assert!(build_export(&json!({}), 1.0).is_none());
        for good in ["abc", "0b3c-4d5e", "A.b_c-1", "x..y"] {
            assert!(valid_session_id(good), "{good:?}");
        }
    }

    #[test]
    fn export_writes_one_file_per_session_and_replaces_it() {
        let (_root, dir) = temp_dir("export");
        let mut input = sample();
        let out = export(input.to_string().as_bytes(), &dir, SystemTime::now()).unwrap();
        let ExportOutcome::Written(path) = out else {
            panic!("{out:?}")
        };
        assert_eq!(path, dir.join("0b3c-4d5e_f.6.json"));
        input["context_window"]["used_percentage"] = json!(51);
        export(input.to_string().as_bytes(), &dir, SystemTime::now()).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["contextUsedPercent"], 51.0);
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["0b3c-4d5e_f.6.json"], "no temp files left");
    }

    #[test]
    fn export_skips_garbage_and_bad_ids_without_creating_the_folder() {
        let (_root, dir) = temp_dir("skip");
        for input in [
            &b""[..],
            b"  ",
            b"not json",
            b"[1]",
            br#"{"session_id":"../x"}"#,
            br#"{"model":{}}"#,
        ] {
            assert!(matches!(
                export(input, &dir, SystemTime::now()).unwrap(),
                ExportOutcome::Skipped(_)
            ));
        }
        assert!(!dir.exists());
    }

    #[test]
    fn prune_drops_only_old_files() {
        let (_root, dir) = temp_dir("prune");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), b"{}").unwrap();
        let later = SystemTime::now() + LIVE_MAX_AGE + Duration::from_secs(60);
        assert_eq!(prune(&dir, SystemTime::now()).unwrap(), 0);
        assert_eq!(prune(&dir, later).unwrap(), 1);
        assert!(!dir.join("a.json").exists());
    }

    #[test]
    fn settings_argument_forms() {
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            settings_arg(&os(&["--settings", "a.json"])),
            Some(PathBuf::from("a.json"))
        );
        assert_eq!(
            settings_arg(&os(&["--x", "--settings=b.json"])),
            Some(PathBuf::from("b.json"))
        );
        assert_eq!(settings_arg(&os(&["--settings"])), None);
        assert_eq!(settings_arg(&os(&[])), None);
    }

    #[test]
    fn a_shell_that_cannot_start_chains_nothing() {
        let sh = shell::Shell {
            program: PathBuf::from("Z:/no/such/shell.exe"),
            args: vec![],
        };
        assert_eq!(run_chained(&sh, "echo hi", b"{}"), None);
    }

    /// `sh -c` on Unix, Git Bash on Windows; `None` (test skipped, or
    /// failed under `CC_TEST_REQUIRE_SHELL=1`) on a Windows machine without Git.
    fn test_shell() -> Option<shell::Shell> {
        let program = if cfg!(windows) {
            shell::find_git_bash()?
        } else {
            PathBuf::from("/bin/sh")
        };
        Some(shell::Shell {
            program,
            args: vec!["-c".into()],
        })
    }

    #[test]
    fn a_chained_command_gets_stdin_and_its_exit_code_is_kept() {
        let Some(sh) = test_shell() else {
            skip_without_shell("no Git Bash");
            return;
        };
        let input = br#"{"session_id":"s"}"#;
        assert_eq!(
            run_chained(
                &sh,
                r#"read -r x; [ "$x" = '{"session_id":"s"}' ] && exit 3"#,
                input
            ),
            Some(3)
        );
        assert_eq!(run_chained(&sh, "exit 0", input), Some(0));
    }

    #[test]
    fn a_slow_chained_command_is_killed_with_its_children() {
        let Some(sh) = test_shell() else {
            skip_without_shell("no Git Bash");
            return;
        };
        let (_root, dir) = temp_dir("slow");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("late.txt");
        let m = marker.to_string_lossy().replace('\\', "/");
        // A background child that would write the marker after the timeout,
        // and a foreground that outlives it.
        let command = format!("(sleep 2; echo late > '{m}') & sleep 30");
        let started = Instant::now();
        let code = run_chained_for(&sh, &command, b"{}", Duration::from_millis(700));
        let took = started.elapsed();
        assert_eq!(code, Some(0));
        assert!(
            took >= Duration::from_millis(700) && took < Duration::from_secs(5),
            "{took:?}"
        );
        std::thread::sleep(Duration::from_secs(3));
        assert!(
            !marker.exists(),
            "the background child survived the timeout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn live_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, dir) = temp_dir("mode");
        let out = export(sample().to_string().as_bytes(), &dir, SystemTime::now()).unwrap();
        let ExportOutcome::Written(path) = out else {
            panic!("{out:?}")
        };
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&dir), mode(&path)), (0o700, 0o600));
    }
}
