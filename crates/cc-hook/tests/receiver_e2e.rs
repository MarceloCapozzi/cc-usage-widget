//! End-to-end checks of the hook receiver: the built `cc-hook` binary is run
//! with sample payloads on stdin and `CC_USAGE_DATA_DIR` pointing to a temp
//! dir. Every run must exit 0 with empty stdout and stderr.

use cc_core::events::{SpoolEvent, MAX_TEXT_CHARS};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "../src/testutil.rs"]
mod testutil;
use testutil::TestDir;

fn temp_data_dir(tag: &str) -> TestDir {
    TestDir::new(&format!("e2e-{tag}"))
}

/// Runs the receiver with `input` on stdin (None: stdin closed) and asserts silence and exit 0.
fn run_hook(data_dir: &Path, input: Option<&[u8]>, args: &[&str], debug: bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cc-hook"));
    cmd.args(args)
        .env("CC_USAGE_DATA_DIR", data_dir)
        .env("TERM_PROGRAM", "vscode")
        .env_remove("CC_HOOK_DEBUG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if debug {
        cmd.env("CC_HOOK_DEBUG", "1");
    }
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd.spawn().unwrap();
    if let Some(bytes) = input {
        let mut stdin = child.stdin.take().unwrap();
        // The receiver may legitimately stop early; ignore broken pipes.
        let _ = stdin.write_all(bytes);
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "exit code");
    assert!(
        out.stdout.is_empty(),
        "stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn spool_files(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(data_dir.join("spool")) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd.map(|e| e.unwrap().path()).collect();
    v.sort();
    v
}

fn only_event(data_dir: &Path) -> (PathBuf, SpoolEvent) {
    let files = spool_files(data_dir);
    assert_eq!(files.len(), 1, "spool: {files:?}");
    let path = files[0].clone();
    let e = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    (path, e)
}

#[test]
fn pre_tool_use_is_spooled_with_its_fields() {
    let dir = temp_data_dir("pre");
    let payload = br#"{
        "session_id": "0b5f1c9e-0000-4000-8000-000000000001",
        "transcript_path": "C:\\Users\\me\\.claude\\projects\\x\\0b5f.jsonl",
        "cwd": "C:\\repo",
        "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_use_id": "toolu_01",
        "tool_input": {"file_path": "C:\\repo\\src\\main.rs", "old_string": "a", "new_string": "b"}
    }"#;
    run_hook(&dir, Some(payload), &[], false);
    let (path, e) = only_event(&dir);
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(name.ends_with(".json") && !name.starts_with('.'));
    assert_eq!(
        name,
        e.file_name(name.split_once('-').unwrap().1.trim_end_matches(".json"))
    );
    assert_eq!(e.v, cc_core::events::SPOOL_SCHEMA_VERSION);
    assert_eq!(e.event, "PreToolUse");
    assert_eq!(e.session_id, "0b5f1c9e-0000-4000-8000-000000000001");
    assert_eq!(e.cwd.as_deref(), Some("C:\\repo"));
    assert_eq!(e.tool_name.as_deref(), Some("Edit"));
    assert_eq!(e.tool_use_id.as_deref(), Some("toolu_01"));
    assert_eq!(e.tool_summary.as_deref(), Some("main.rs"));
    assert_eq!(e.term_program.as_deref(), Some("vscode"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    assert!((now - e.ts).abs() < 60.0);
}

#[test]
fn prompt_and_stop_texts_are_truncated() {
    let dir = temp_data_dir("texts");
    let long = "word ".repeat(200);
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit", "session_id": "s", "prompt": long
    });
    run_hook(&dir, Some(payload.to_string().as_bytes()), &[], false);
    let (_, e) = only_event(&dir);
    let input = e.user_input.unwrap();
    assert_eq!(input.chars().count(), MAX_TEXT_CHARS + 1);
    assert!(input.ends_with('…'));
}

#[test]
fn a_renewal_only_claude_process_is_never_spooled() {
    let dir = temp_data_dir("nospool");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cc-hook"))
        .env("CC_USAGE_DATA_DIR", &dir)
        .env(cc_core::paths::NO_SPOOL_ENV, "1")
        .env_remove("CC_HOOK_DEBUG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ =
        child.stdin.take().unwrap().write_all(
            br#"{"hook_event_name":"SessionStart","session_id":"s","source":"startup"}"#,
        );
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(spool_files(&dir).is_empty(), "nothing spooled");
}

#[test]
fn garbage_input_is_silent_and_writes_nothing() {
    let dir = temp_data_dir("garbage");
    let inputs: [&[u8]; 8] = [
        b"",
        b"   \r\n",
        b"not json at all",
        b"{\"hook_event_name\": \"Stop\", ",
        b"\x00\xff\xfe\x01binary",
        b"[1, 2, 3]",
        b"{\"hook_event_name\": \"Stop\"}",
        b"{\"session_id\": 42, \"hook_event_name\": \"Stop\"}",
    ];
    for input in inputs {
        run_hook(&dir, Some(input), &[], false);
    }
    run_hook(&dir, None, &[], false);
    assert!(spool_files(&dir).is_empty());
}

#[test]
fn a_large_payload_is_handled() {
    let dir = temp_data_dir("large");
    let content = "x".repeat(8 * 1024 * 1024);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Write",
        "tool_input": {"file_path": "/tmp/big.txt", "content": content}
    });
    run_hook(&dir, Some(payload.to_string().as_bytes()), &[], false);
    let (path, e) = only_event(&dir);
    assert_eq!(e.tool_summary.as_deref(), Some("big.txt"));
    assert!(std::fs::metadata(path).unwrap().len() < 4096);
}

#[test]
fn a_payload_past_the_stdin_limit_is_skipped_silently() {
    let dir = temp_data_dir("too-large");
    let limit = cc_hook::receiver::MAX_STDIN_BYTES as usize;
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Write",
        "tool_input": {"file_path": "/tmp/big.txt", "content": "x".repeat(limit)}
    });
    run_hook(&dir, Some(payload.to_string().as_bytes()), &[], false);
    assert!(spool_files(&dir).is_empty());
}

#[test]
fn an_unwritable_spool_is_silent() {
    let dir = temp_data_dir("unwritable");
    // The spool path is a regular file, so creating the directory fails.
    std::fs::write(dir.join("spool"), b"in the way").unwrap();
    run_hook(
        &dir,
        Some(br#"{"hook_event_name":"Stop","session_id":"s"}"#),
        &[],
        false,
    );
    assert!(dir.join("spool").is_file());
}

#[test]
fn stray_arguments_with_piped_stdin_still_act_as_the_receiver() {
    let dir = temp_data_dir("stray");
    run_hook(
        &dir,
        Some(br#"{"hook_event_name":"Stop","session_id":"s"}"#),
        &["--unexpected"],
        false,
    );
    assert_eq!(only_event(&dir).1.event, "Stop");
}

#[test]
fn debug_mode_logs_to_a_file_and_stays_silent() {
    let dir = temp_data_dir("debug");
    run_hook(
        &dir,
        Some(br#"{"hook_event_name":"Stop","session_id":"s"}"#),
        &[],
        true,
    );
    run_hook(&dir, Some(b"garbage"), &[], true);
    let log = std::fs::read_to_string(dir.join("cc-hook.log")).unwrap();
    assert_eq!(log.lines().count(), 3, "{log}");
    assert!(log.contains("spool write"));
    assert!(log.contains("Written"));
    assert!(log.contains("not JSON"));
}

#[test]
fn many_calls_produce_distinct_files() {
    let dir = temp_data_dir("many");
    for i in 0..20 {
        let p = format!(
            r#"{{"hook_event_name":"Notification","session_id":"s{i}","notification_type":"idle_prompt","message":"Claude is waiting"}}"#
        );
        run_hook(&dir, Some(p.as_bytes()), &[], false);
    }
    let files = spool_files(&dir);
    assert_eq!(files.len(), 20);
    assert!(files
        .iter()
        .all(|f| !f.file_name().unwrap().to_str().unwrap().starts_with('.')));
}
