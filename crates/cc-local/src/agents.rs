//! `claude agents --json`: the list of live sessions as Claude Code reports it.
//!
//! [`snapshot`] runs the CLI and [`parse_agents_json`] turns its output into
//! [`RegistryEntry`] values. The output is an undocumented JSON array of
//! objects such as:
//!
//! ```json
//! [{"pid":21728,"cwd":"c:\\...","kind":"interactive","startedAt":1791855985658,
//!   "sessionId":"...","name":"my-repo-1","status":"busy"}]
//! ```
//!
//! plus `waitingFor` on waiting sessions and `id` on background ones. Unknown
//! fields are ignored, missing ones are left empty, and an item without any
//! session id is skipped rather than failing the whole snapshot.
//!
//! Spawned directly, the CLI answered in about 0.5 s on the development
//! machine (from Git Bash it took around 5 s, close to [`DEFAULT_TIMEOUT`]).
//! If it times out, [`crate::registry::snapshot`] gives a cheaper, slightly
//! less reliable list from the registry files.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cc_core::process_tree::{self, ProcessTree};
use cc_core::session::{RegistryEntry, RegistryStatus};
use serde_json::Value;

use crate::{fields, LocalError};

/// How long [`snapshot`] waits for `claude agents --json` before killing it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Output beyond this many bytes is ignored (the real output is about 300
/// bytes per session).
const MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;

/// How long the output is still waited for when the CLI exits at the deadline.
const OUTPUT_GRACE: Duration = Duration::from_millis(200);

/// `waitingFor` values longer than this are truncated.
const MAX_WAITING_FOR_CHARS: usize = 120;

/// Runs `claude agents --json` with [`DEFAULT_TIMEOUT`] and parses its output.
pub fn snapshot() -> Result<Vec<RegistryEntry>, LocalError> {
    snapshot_with_timeout(DEFAULT_TIMEOUT)
}

/// Runs `claude agents --json`, killing it after `timeout`, and parses its output.
pub fn snapshot_with_timeout(timeout: Duration) -> Result<Vec<RegistryEntry>, LocalError> {
    let exe = find_claude().ok_or(LocalError::ClaudeNotFound)?;
    let output = run_with_timeout(&exe, &["agents", "--json"], &[], timeout)?;
    parse_agents_json(&String::from_utf8_lossy(&output))
}

/// Parses the output of `claude agents --json`.
///
/// Accepts the bare array, or an object wrapping it under `agents` or
/// `sessions`, and tolerates text printed before or after the JSON (update
/// notices and the like). Items that are not objects or have neither
/// `sessionId` nor `id` are skipped. A missing `status` becomes
/// `RegistryStatus::Other("unknown")`.
pub fn parse_agents_json(text: &str) -> Result<Vec<RegistryEntry>, LocalError> {
    let items = match parse_lenient(text)? {
        Value::Array(items) => items,
        Value::Object(mut obj) => match obj.remove("agents").or_else(|| obj.remove("sessions")) {
            Some(Value::Array(items)) => items,
            _ => {
                return Err(LocalError::Parse(
                    "expected a JSON array of sessions".into(),
                ))
            }
        },
        _ => {
            return Err(LocalError::Parse(
                "expected a JSON array of sessions".into(),
            ))
        }
    };
    Ok(items.iter().filter_map(entry_from_value).collect())
}

/// Parses `text` as JSON, falling back to the outermost `[...]` span when
/// other text surrounds it.
fn parse_lenient(text: &str) -> Result<Value, LocalError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(LocalError::Parse("empty output".into()));
    }
    if let Ok(v) = serde_json::from_str(trimmed) {
        return Ok(v);
    }
    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
        if start < end {
            if let Ok(v) = serde_json::from_str(&trimmed[start..=end]) {
                return Ok(v);
            }
        }
    }
    Err(LocalError::Parse("output is not valid JSON".into()))
}

fn entry_from_value(item: &Value) -> Option<RegistryEntry> {
    let obj = item.as_object()?;
    let job_id = fields::string(obj, "id");
    let (session_id, background_id) = match fields::string(obj, "sessionId") {
        Some(sid) => (sid, job_id.filter(|id| !id.is_empty())),
        None => (job_id?, None),
    };
    Some(RegistryEntry {
        session_id,
        pid: fields::u32_field(obj, "pid"),
        cwd: fields::string(obj, "cwd"),
        name: fields::string(obj, "name"),
        kind: fields::string(obj, "kind"),
        status: fields::status(obj, "status")
            .unwrap_or_else(|| RegistryStatus::Other("unknown".into())),
        waiting_for: fields::loose_text(obj, "waitingFor", MAX_WAITING_FOR_CHARS),
        entrypoint: fields::string(obj, "entrypoint"),
        started_at_ms: fields::i64_field(obj, "startedAt"),
        background_id,
    })
}

/// How long [`ask_to_renew`] waits for `claude -p` (it may renew a
/// token over the network first).
pub const RENEW_TIMEOUT: Duration = Duration::from_secs(20);

/// Runs `claude -p` with no prompt and an empty stdin, discarding its
/// output: Claude Code loads (and, when its access token expired, renews)
/// its credentials when it starts, before it reads its input, then stops
/// with "Input must be provided…" and a non-zero exit, which is the expected
/// one: nothing is sent. Its SessionStart and SessionEnd hooks still run;
/// they inherit `NO_SPOOL_ENV`, so cc-hook records neither. (`claude auth
/// status` did not renew an expired token when tried.) The widget never
/// renews the token itself, since a renewal can rotate the refresh token
/// Claude Code holds and sign it out; asking the owner of the file avoids
/// that. Ok when
/// the CLI ran to an exit, whatever its code; whether the token was renewed
/// shows in `.credentials.json`.
pub fn ask_to_renew() -> Result<(), LocalError> {
    let exe = find_claude().ok_or(LocalError::ClaudeNotFound)?;
    // Its hooks inherit NO_SPOOL_ENV, so its start and end are not spooled.
    match run_with_timeout(
        &exe,
        &["-p"],
        &[(cc_core::paths::NO_SPOOL_ENV, "1")],
        RENEW_TIMEOUT,
    ) {
        Ok(_) | Err(LocalError::ExitStatus(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Finds the `claude` executable: the first `PATH` directory holding
/// `claude.exe`, `claude.cmd` or `claude.bat` (on Windows, in that order of
/// preference within a directory) or `claude` (elsewhere), then the native
/// installer's `~/.local/bin`.
pub fn find_claude() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd", "claude.bat"]
    } else {
        &["claude"]
    };
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    let fallback = cc_core::paths::home_dir().map(|home| home.join(".local").join("bin"));
    path_dirs
        .into_iter()
        .chain(fallback)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| names.iter().map(|n| dir.join(n)).find(|p| p.is_file()))
}

/// Runs `exe args...` (with `envs` added to its environment) with no
/// console window and no stdin, returning stdout.
///
/// A `.cmd` or `.bat` shim is run through `cmd.exe` by the standard library,
/// which escapes the arguments for it. The child runs contained with
/// everything it starts ([`cc_core::process_tree`]: a job object on Windows,
/// its own process group on Unix), so a timeout kills the whole tree: the
/// shim's `cmd.exe` and the `node.exe` it started, or whatever the CLI left
/// running.
///
/// Nothing here waits longer than `timeout`, counted from the spawn. The
/// output is read on its own thread and waited for only until the same
/// deadline, because the pipe only reaches end-of-file when every process
/// holding it has closed it: a grandchild that inherited stdout would
/// otherwise block the read, and the caller's gathering thread with it. On
/// either timeout (the CLI still running, or its output still open) the tree
/// is killed, which also closes the pipe and ends the reader thread. When the
/// CLI exits in time with its output complete, or with an error status,
/// whatever it left running is left alone.
fn run_with_timeout(
    exe: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> Result<Vec<u8>, LocalError> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Keeps the child from opening (and flashing) a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    process_tree::own_group(&mut cmd);
    let deadline = Instant::now() + timeout;
    let mut child = cmd.spawn()?;
    let tree = ProcessTree::adopt(&child);
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("claude-agents-stdout".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let result = (&mut stdout).take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
            let _ = tx.send(result.map(|_| buf));
            // Drain anything beyond the cap so the child never blocks on a full pipe.
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
    if let Err(e) = reader {
        tree.kill();
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.into());
    }

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                tree.kill();
                let _ = child.kill();
                let _ = child.wait();
                return Err(LocalError::Timeout(timeout));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                tree.kill();
                let _ = child.kill();
                return Err(e.into());
            }
        }
    };
    if !status.success() {
        tree.leave();
        return Err(LocalError::ExitStatus(status.code()));
    }
    // A child that exited right at the deadline still gets a moment for the
    // output already in the pipe.
    let wait = deadline
        .saturating_duration_since(Instant::now())
        .max(OUTPUT_GRACE);
    match rx.recv_timeout(wait) {
        Ok(output) => {
            tree.leave();
            Ok(output?)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Something the CLI started still holds its stdout.
            tree.kill();
            Err(LocalError::Timeout(timeout))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            tree.leave();
            Err(LocalError::Parse("output reader stopped".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../fixtures/agents.json");

    #[test]
    fn parses_the_real_shape() {
        let entries = parse_agents_json(FIXTURE).unwrap();
        assert_eq!(entries.len(), 4);
        let e = &entries[0];
        assert_eq!(e.session_id, "0f3c2a8e-1b7d-4c55-9e0a-6d2f8b4e1a01");
        assert_eq!(e.pid, Some(16808));
        assert_eq!(
            e.cwd.as_deref(),
            Some("c:\\Users\\dev\\Projects\\web-dashboard")
        );
        assert_eq!(e.kind.as_deref(), Some("interactive"));
        assert_eq!(e.name.as_deref(), Some("web-dashboard-12"));
        assert_eq!(e.status, RegistryStatus::Idle);
        assert_eq!(e.started_at_ms, Some(1_791_850_492_786));
        assert_eq!(e.waiting_for, None);
        assert_eq!(e.entrypoint, None);
    }

    #[test]
    fn keeps_waiting_for_busy_and_unknown_statuses() {
        let entries = parse_agents_json(FIXTURE).unwrap();
        assert_eq!(entries[1].status, RegistryStatus::Waiting);
        assert_eq!(entries[1].waiting_for.as_deref(), Some("permission"));
        assert_eq!(entries[2].status, RegistryStatus::Busy);
        assert_eq!(
            entries[3].status,
            RegistryStatus::Other("compacting".into())
        );
        assert_eq!(entries[3].kind.as_deref(), Some("background"));
        assert_eq!(
            entries[3].session_id,
            "e58b2c1d-9f4a-4b67-8d3e-1c6a5f2b7d04"
        );
        assert_eq!(entries[3].background_id.as_deref(), Some("bg_01ab"));
        assert_eq!(entries[2].background_id, None);
    }

    #[test]
    fn empty_array_is_no_sessions() {
        assert_eq!(parse_agents_json("[]\n").unwrap(), vec![]);
    }

    #[test]
    fn tolerates_missing_fields_and_falls_back_to_id() {
        let entries =
            parse_agents_json(r#"[{"id":"bg_1"},{"sessionId":"s2","pid":"oops","status":7}]"#)
                .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].session_id, "bg_1");
        assert_eq!(entries[0].status, RegistryStatus::Other("unknown".into()));
        assert_eq!(entries[1].pid, None);
        assert_eq!(entries[1].cwd, None);
        assert_eq!(entries[1].status, RegistryStatus::Other("unknown".into()));
    }

    #[test]
    fn skips_items_without_identity_or_not_objects() {
        let entries =
            parse_agents_json(r#"[42, "x", null, {"pid":1,"status":"busy"}, {"sessionId":"ok"}]"#)
                .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "ok");
    }

    #[test]
    fn tolerates_surrounding_text_and_wrapped_arrays() {
        let noisy = "Update available: 2.2.0\n[{\"sessionId\":\"a\",\"status\":\"idle\"}]\n";
        assert_eq!(parse_agents_json(noisy).unwrap().len(), 1);
        let wrapped = r#"{"agents":[{"sessionId":"a","status":"busy"}]}"#;
        assert_eq!(
            parse_agents_json(wrapped).unwrap()[0].status,
            RegistryStatus::Busy
        );
    }

    #[test]
    fn rejects_non_json_and_wrong_shapes() {
        assert!(matches!(parse_agents_json(""), Err(LocalError::Parse(_))));
        assert!(matches!(
            parse_agents_json("error: not logged in"),
            Err(LocalError::Parse(_))
        ));
        assert!(matches!(
            parse_agents_json(r#"{"count":0}"#),
            Err(LocalError::Parse(_))
        ));
    }

    #[test]
    fn waiting_for_objects_are_kept_as_compact_json() {
        let entries = parse_agents_json(
            r#"[{"sessionId":"a","status":"waiting","waitingFor":{"type":"permission","tool":"Bash"}}]"#,
        )
        .unwrap();
        // Key order depends on whether serde_json's preserve_order feature is
        // enabled anywhere in the workspace (cc-hook enables it), so compare the
        // compact text as JSON rather than byte for byte.
        let text = entries[0].waiting_for.as_deref().expect("waitingFor kept");
        assert!(!text.contains(' '), "compact JSON: {text}");
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"type": "permission", "tool": "Bash"})
        );
    }

    /// A shell that prints `[]`, then leaves a grandchild running that
    /// inherited stdout, and exits at once.
    fn shell_leaving_a_grandchild() -> (PathBuf, Vec<&'static str>) {
        if cfg!(windows) {
            (
                PathBuf::from("cmd.exe"),
                vec!["/C", "echo [] & start /b ping -n 15 127.0.0.1"],
            )
        } else {
            (
                PathBuf::from("/bin/sh"),
                vec!["-c", "echo '[]'; sleep 15 &"],
            )
        }
    }

    #[test]
    fn a_grandchild_holding_stdout_cannot_block_past_the_timeout() {
        let (exe, args) = shell_leaving_a_grandchild();
        let started = Instant::now();
        let result = run_with_timeout(&exe, &args, &[], Duration::from_millis(1500));
        let took = started.elapsed();
        assert!(took < Duration::from_secs(4), "blocked for {took:?}");
        assert!(matches!(result, Err(LocalError::Timeout(_))), "{result:?}");
    }

    #[test]
    fn a_normal_run_returns_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("agents.json");
        std::fs::write(&json, FIXTURE).unwrap();
        let json = json.to_str().unwrap();
        let (exe, args): (PathBuf, Vec<&str>) = if cfg!(windows) {
            (PathBuf::from("cmd.exe"), vec!["/C", "type", json])
        } else {
            (PathBuf::from("/bin/cat"), vec![json])
        };
        let out = run_with_timeout(&exe, &args, &[], Duration::from_secs(10)).unwrap();
        assert_eq!(
            parse_agents_json(&String::from_utf8_lossy(&out))
                .unwrap()
                .len(),
            4
        );

        let (exe, args): (PathBuf, Vec<&str>) = if cfg!(windows) {
            (PathBuf::from("cmd.exe"), vec!["/C", "exit 3"])
        } else {
            (PathBuf::from("/bin/sh"), vec!["-c", "exit 3"])
        };
        assert!(matches!(
            run_with_timeout(&exe, &args, &[], Duration::from_secs(10)),
            Err(LocalError::ExitStatus(Some(3)))
        ));
    }

    #[test]
    fn a_hung_cli_is_killed_at_the_timeout() {
        let (exe, args): (PathBuf, Vec<&str>) = if cfg!(windows) {
            (
                PathBuf::from("cmd.exe"),
                vec!["/C", "ping -n 15 127.0.0.1 >nul"],
            )
        } else {
            (PathBuf::from("/bin/sh"), vec!["-c", "sleep 15"])
        };
        let started = Instant::now();
        let result = run_with_timeout(&exe, &args, &[], Duration::from_millis(500));
        assert!(matches!(result, Err(LocalError::Timeout(_))), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[cfg(windows)]
    fn running(pid: u32) -> bool {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
    }

    #[cfg(unix)]
    fn running(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// The case of a `.cmd` shim that hangs: the shell is not the process
    /// doing the work, its child is, and both go at the timeout.
    #[test]
    fn a_timeout_kills_what_the_cli_started_too() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let script;
        let (exe, args): (PathBuf, Vec<&str>) = if cfg!(windows) {
            script = format!(
                "$p = Start-Process -PassThru -WindowStyle Hidden ping -ArgumentList '-n','60','127.0.0.1'; \
                 Set-Content -LiteralPath '{}' $p.Id; Wait-Process -Id $p.Id",
                pid_file.display()
            );
            (
                PathBuf::from("powershell.exe"),
                vec!["-NoProfile", "-NonInteractive", "-Command", &script],
            )
        } else {
            script = format!("sleep 60 & echo $! > '{}'; wait", pid_file.display());
            (PathBuf::from("/bin/sh"), vec!["-c", &script])
        };
        let result = run_with_timeout(&exe, &args, &[], Duration::from_secs(8));
        assert!(matches!(result, Err(LocalError::Timeout(_))), "{result:?}");
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .expect("the grandchild started before the timeout")
            .trim()
            .parse()
            .unwrap();
        let gone = (0..100).any(|_| {
            let gone = !running(pid);
            if !gone {
                std::thread::sleep(Duration::from_millis(100));
            }
            gone
        });
        assert!(gone, "grandchild {pid} survived the timeout");
    }

    #[test]
    #[ignore = "runs the real claude CLI"]
    fn live_snapshot() {
        let started = Instant::now();
        let entries = snapshot_with_timeout(Duration::from_secs(20)).expect("claude agents --json");
        let count = |f: &dyn Fn(&RegistryEntry) -> bool| entries.iter().filter(|e| f(e)).count();
        println!(
            "agents: {} entries in {} ms; with pid {}, cwd {}, name {}, kind {}, startedAt {}, \
             waitingFor {}; busy {}, waiting {}, idle {}, other {}",
            entries.len(),
            started.elapsed().as_millis(),
            count(&|e| e.pid.is_some()),
            count(&|e| e.cwd.is_some()),
            count(&|e| e.name.is_some()),
            count(&|e| e.kind.is_some()),
            count(&|e| e.started_at_ms.is_some()),
            count(&|e| e.waiting_for.is_some()),
            count(&|e| e.status == RegistryStatus::Busy),
            count(&|e| e.status == RegistryStatus::Waiting),
            count(&|e| e.status == RegistryStatus::Idle),
            count(&|e| matches!(e.status, RegistryStatus::Other(_))),
        );
        assert!(entries.iter().all(|e| !e.session_id.is_empty()));
    }
}
