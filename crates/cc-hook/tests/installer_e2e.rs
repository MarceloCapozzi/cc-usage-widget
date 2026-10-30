//! End-to-end checks of `cc-hook install | uninstall | status` on temp copies
//! of a settings file shaped like the real one on this machine: five Google
//! Chat notifier hooks, a status line, model settings and a model.

use cc_core::events::HOOK_EVENTS;
use cc_hook::install::{self, Action};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "../src/testutil.rs"]
mod testutil;
use testutil::TestDir;

const CMD: &str = "\"C:/Users/me/AppData/Local/cc-usage/bin/cc-hook.exe\"";

const REAL_SHAPE: &str = r#"{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "node \"C:\\Users\\me\\Projects\\chat-notifier\\notify.js\" user-prompt-submit"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "Bash|Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          {
            "type": "command",
            "command": "node \"C:\\Users\\me\\Projects\\chat-notifier\\notify.js\" pre-tool-use"
          }
        ]
      }
    ],
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "node \"C:\\Users\\me\\Projects\\chat-notifier\\notify.js\" session-start"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "node \"C:\\Users\\me\\Projects\\chat-notifier\\notify.js\" stop"
          }
        ]
      }
    ],
    "Notification": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "node \"C:\\Users\\me\\Projects\\chat-notifier\\notify.js\" notification"
          }
        ]
      }
    ]
  },
  "statusLine": {
    "type": "command",
    "command": "node \"C:\\Users\\me\\.claude\\statusline.js\"",
    "padding": 0
  },
  "modelSettings": {
    "claude-opus-5-5": {
      "effortLevel": "high"
    }
  },
  "model": "opus"
}"#;

/// A fresh directory with `settings.json` holding `content` (or nothing).
fn fixture(tag: &str, content: Option<&str>) -> (TestDir, PathBuf) {
    let dir = TestDir::new(&format!("inst-{tag}"));
    let path = dir.join("settings.json");
    if let Some(c) = content {
        std::fs::write(&path, c).unwrap();
    }
    (dir, path)
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn backups(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("settings.json.bak-cc-hook-")
        })
        .collect();
    v.sort();
    v
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with('.'))
        .collect()
}

/// Points everything a cc-hook process resolves a real path from at folders
/// under `root`: the data folder (`data`, where the status line chain record
/// goes), the home folder (`USERPROFILE` and `HOME`, `home`) and
/// `CLAUDE_CONFIG_DIR` (`claude-config`). Every spawned cc-hook gets this, so
/// a call without `--settings` can never reach the real `~/.claude`.
fn isolate<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("CC_USAGE_DATA_DIR", root.join("data"))
        .env("USERPROFILE", root.join("home"))
        .env("HOME", root.join("home"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude-config"))
}

/// Runs the built cc-hook isolated under `root` ([`isolate`]), with `env`
/// set on top.
fn cli_in(root: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cc-hook"));
    isolate(&mut cmd, root);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(args).output().unwrap()
}

/// Runs the built cc-hook isolated under the test's own folder `root`
/// ([`isolate`]). The status line chain record of an install lives in its
/// data folder, so the uninstall that follows must use the same `root`.
fn cli(root: &Path, args: &[&str]) -> Output {
    cli_in(root, &[], args)
}

fn ok(out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Key paths in document order, to compare ordering, not just content.
fn key_order(v: &Value, prefix: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, child) in m {
                let p = format!("{prefix}/{k}");
                out.push(p.clone());
                key_order(child, &p, out);
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                key_order(child, &format!("{prefix}/{i}"), out);
            }
        }
        _ => {}
    }
}

fn keys(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    key_order(v, "", &mut out);
    out
}

#[test]
fn install_appends_after_existing_hooks_and_keeps_everything_else() {
    let (dir, path) = fixture("install", Some(REAL_SHAPE));
    let before = read_json(&path);
    let outcome = install::install_file(&path, CMD).unwrap();
    assert!(outcome.written);
    let after = read_json(&path);

    // Other keys untouched and in their original order.
    for key in ["statusLine", "modelSettings", "model"] {
        assert_eq!(after[key], before[key], "{key}");
    }
    let top: Vec<&String> = after.as_object().unwrap().keys().collect();
    assert_eq!(top, ["hooks", "statusLine", "modelSettings", "model"]);

    // Existing events keep their order and their notifier group first; new
    // events are appended after them.
    let events: Vec<&String> = after["hooks"].as_object().unwrap().keys().collect();
    let mut expected: Vec<&str> = vec![
        "UserPromptSubmit",
        "PreToolUse",
        "SessionStart",
        "Stop",
        "Notification",
    ];
    expected.extend(
        HOOK_EVENTS
            .iter()
            .filter(|e| !expected.contains(e))
            .copied()
            .collect::<Vec<_>>(),
    );
    assert_eq!(events, expected);
    for event in [
        "UserPromptSubmit",
        "PreToolUse",
        "SessionStart",
        "Stop",
        "Notification",
    ] {
        let groups = after["hooks"][event].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{event}");
        assert_eq!(groups[0], before["hooks"][event][0], "{event}");
        assert_eq!(
            groups[1],
            serde_json::json!({"hooks": [{"type": "command", "command": CMD}]})
        );
    }
    install::verify_installed(&after, CMD).unwrap();

    // The backup holds the original bytes; no temp files remain.
    let b = backups(&dir);
    assert_eq!(b, vec![outcome.backup.unwrap()]);
    assert_eq!(std::fs::read_to_string(&b[0]).unwrap(), REAL_SHAPE);
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

#[test]
fn install_twice_changes_nothing_the_second_time() {
    let (dir, path) = fixture("twice", Some(REAL_SHAPE));
    install::install_file(&path, CMD).unwrap();
    let once = std::fs::read(&path).unwrap();
    let second = install::install_file(&path, CMD).unwrap();
    assert!(!second.written);
    assert!(second
        .report
        .actions
        .iter()
        .all(|(_, a)| *a == Action::Unchanged));
    assert_eq!(std::fs::read(&path).unwrap(), once);
    assert_eq!(backups(&dir).len(), 1);
}

#[test]
fn install_then_uninstall_restores_the_original() {
    for (tag, original) in [
        ("lf", REAL_SHAPE.to_string()),
        ("lf-newline", format!("{REAL_SHAPE}\n")),
        ("crlf", format!("{REAL_SHAPE}\n").replace('\n', "\r\n")),
        ("four-spaces", REAL_SHAPE.replace("  ", "    ")),
    ] {
        let (dir, path) = fixture(tag, Some(&original));
        let before = read_json(&path);
        install::install_file(&path, CMD).unwrap();
        let outcome = install::uninstall_file(&path).unwrap();
        assert!(outcome.written, "{tag}");
        let after = read_json(&path);
        assert_eq!(after, before, "{tag}");
        assert_eq!(keys(&after), keys(&before), "{tag}");
        // Same formatting conventions, so the bytes come back identical too.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original, "{tag}");
        install::verify_uninstalled(&after).unwrap();
        assert_eq!(backups(&dir).len(), 2, "{tag}");
    }
}

#[test]
fn uninstall_without_cc_hook_does_not_write() {
    let (dir, path) = fixture("noop", Some(REAL_SHAPE));
    let outcome = install::uninstall_file(&path).unwrap();
    assert!(!outcome.written);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), REAL_SHAPE);
    assert!(backups(&dir).is_empty());
}

#[test]
fn install_creates_a_missing_file_and_uninstall_empties_it() {
    let (dir, _) = fixture("missing", None);
    let path = dir.join("nested").join("settings.json");
    let outcome = install::install_file(&path, CMD).unwrap();
    assert!(outcome.written && outcome.backup.is_none());
    install::verify_installed(&read_json(&path), CMD).unwrap();
    install::uninstall_file(&path).unwrap();
    assert_eq!(read_json(&path), serde_json::json!({}));
}

#[test]
fn invalid_json_is_refused_untouched() {
    let broken = "{ \"hooks\": { \"Stop\": [ ] , }";
    let (dir, path) = fixture("broken", Some(broken));
    assert!(matches!(
        install::install_file(&path, CMD),
        Err(install::Error::InvalidJson { .. })
    ));
    assert!(matches!(
        install::uninstall_file(&path),
        Err(install::Error::InvalidJson { .. })
    ));
    let p = path.to_str().unwrap();
    for args in [
        vec!["install", "--settings", p, "--command", CMD],
        vec!["uninstall", "--settings", p],
        vec!["status", "--settings", p],
    ] {
        let out = cli(&dir, &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("not valid JSON"));
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    assert!(backups(&dir).is_empty());
    assert!(leftovers(&dir).is_empty());
}

#[test]
fn wrong_shapes_are_refused_untouched() {
    for (tag, content) in [
        ("array", "[]"),
        ("hooks-array", r#"{"hooks": []}"#),
        ("event-object", r#"{"hooks": {"Stop": {}}}"#),
    ] {
        let (dir, path) = fixture(tag, Some(content));
        assert!(
            matches!(
                install::install_file(&path, CMD),
                Err(install::Error::Shape(_))
            ),
            "{tag}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        assert!(backups(&dir).is_empty());
    }
}

#[test]
fn cli_install_status_uninstall_round_trip() {
    let (dir, path) = fixture("cli", Some(REAL_SHAPE));
    let p = path.to_str().unwrap();

    let out = cli(&dir, &["status", "--settings", p]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.matches("missing").count(), HOOK_EVENTS.len(), "{text}");

    // Default command: this test binary's cc-hook, quoted with forward slashes.
    let out = cli(&dir, &["install", "--settings", p]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected_cmd = install::command_for_exe(Path::new(env!("CARGO_BIN_EXE_cc-hook"))).unwrap();
    if cfg!(windows) {
        assert!(expected_cmd.starts_with('"') && !expected_cmd.contains('\\'));
    } else {
        assert!(expected_cmd.starts_with('\''));
    }
    install::verify_installed(&read_json(&path), &expected_cmd).unwrap();

    let out = cli(&dir, &["status", "--settings", p]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.matches("installed").count(),
        HOOK_EVENTS.len(),
        "{text}"
    );
    assert!(text.contains(&expected_cmd));

    let out = cli(&dir, &["uninstall", "--settings", p]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        read_json(&path),
        serde_json::from_str::<Value>(REAL_SHAPE).unwrap()
    );
}

#[test]
fn cli_rejects_bad_usage() {
    let (dir, path) = fixture("usage", Some(REAL_SHAPE));
    let out = cli(
        &dir,
        &[
            "install",
            "--command",
            "notmine.exe",
            "--settings",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not run an executable named"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), REAL_SHAPE);
    assert!(backups(&dir).is_empty());
    let out = cli(&dir, &["uninstall", "--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    let out = cli(&dir, &["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("cc-hook install"));
}

/// The hook command `cc-hook install` registers by default: the built cc-hook.
fn exe_command() -> String {
    install::command_for_exe(Path::new(env!("CARGO_BIN_EXE_cc-hook"))).unwrap()
}

#[test]
fn without_settings_the_cli_edits_the_file_in_claude_config_dir() {
    let (root, _) = fixture("config-dir", None);
    let config = root.join("claude-config");
    std::fs::create_dir_all(&config).unwrap();
    let path = config.join("settings.json");
    std::fs::write(&path, REAL_SHAPE).unwrap();

    let out = cli_in(&root, &[], &["install"]);
    ok(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&path.display().to_string()));
    let exe = exe_command();
    let after = read_json(&path);
    install::verify_installed(&after, &exe).unwrap();
    // It is the default settings file, so the status line command names no
    // --settings: `cc-hook statusline` finds the file the same way.
    assert_eq!(after["statusLine"]["command"], format!("{exe} statusline"));
    assert!(
        !root.join("home").join(".claude").exists(),
        "the home folder is left alone"
    );

    let out = cli_in(&root, &[], &["status"]);
    ok(&out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("settings: {}", path.display())),
        "{text}"
    );
    assert_eq!(
        text.matches("installed").count(),
        HOOK_EVENTS.len(),
        "{text}"
    );

    ok(&cli_in(&root, &[], &["uninstall"]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), REAL_SHAPE);
}

#[test]
fn an_empty_claude_config_dir_means_the_home_folder() {
    let (root, _) = fixture("empty-config-dir", None);
    let claude = root.join("home").join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let path = claude.join("settings.json");
    std::fs::write(&path, REAL_SHAPE).unwrap();
    let empty = [("CLAUDE_CONFIG_DIR", "")];

    let out = cli_in(&root, &empty, &["install", "--no-statusline"]);
    ok(&out);
    install::verify_installed(&read_json(&path), &exe_command()).unwrap();
    assert!(!root.join("claude-config").exists());

    ok(&cli_in(&root, &empty, &["uninstall"]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), REAL_SHAPE);
}
