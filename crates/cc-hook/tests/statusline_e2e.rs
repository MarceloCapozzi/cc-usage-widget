//! End-to-end checks of the status line chain: `cc-hook install` on temp
//! copies of a settings file shaped like the real one, and the built
//! `cc-hook statusline` run with sample payloads, `CC_USAGE_DATA_DIR` on a temp
//! folder and the replaced command run through the shell Claude Code uses.
//! Every spawned cc-hook has its home folder and `CLAUDE_CONFIG_DIR` in the
//! fixture too. The real `~/.claude/settings.json` (or the one under
//! `CLAUDE_CONFIG_DIR`) is only read, by one ignored test, and never written.

use cc_hook::install;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[path = "../src/testutil.rs"]
mod testutil;
use testutil::{skip_without_shell, TestDir};

const USER_LINE: &str = r#"node \"C:\\Users\\me\\.claude\\statusline.js\""#;

fn real_shape() -> String {
    format!(
        r#"{{
  "hooks": {{
    "Stop": [
      {{
        "hooks": [
          {{
            "type": "command",
            "command": "node \"C:\\Users\\me\\Projects\\chat-notifier\\notify.js\" stop"
          }}
        ]
      }}
    ]
  }},
  "statusLine": {{
    "type": "command",
    "command": "{USER_LINE}",
    "padding": 0
  }},
  "modelSettings": {{
    "claude-opus-5-5": {{
      "effortLevel": "high"
    }}
  }},
  "model": "opus"
}}"#
    )
}

const SAMPLE: &str = r#"{"session_id":"e2e-0b3c","transcript_path":"C:/t.jsonl","model":{"id":"claude-opus-5-5","display_name":"Opus"},"workspace":{"current_dir":"C:/w"},"cost":{"total_cost_usd":0.5},"context_window":{"context_window_size":1000000,"used_percentage":42.5,"current_usage":{"input_tokens":1000,"cache_creation_input_tokens":4000,"cache_read_input_tokens":420000}},"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1791900000},"seven_day":{"used_percentage":41.2,"resets_at":1792300000}}}"#;

/// A fresh folder with a `data` subfolder and `settings.json` holding
/// `content`, removed when the fixture is dropped.
struct Fixture {
    _root: TestDir,
    dir: PathBuf,
    settings: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(tag: &str, content: Option<&str>) -> Self {
        let root = TestDir::new(&format!("sl-e2e-{tag}"));
        let dir = root.to_path_buf();
        let settings = dir.join("settings.json");
        if let Some(c) = content {
            std::fs::write(&settings, c).unwrap();
        }
        let data = dir.join("data");
        Self {
            _root: root,
            dir,
            settings,
            data,
        }
    }

    fn p(&self) -> &str {
        self.settings.to_str().unwrap()
    }

    /// The built cc-hook with everything it resolves a real path from pointed
    /// into this fixture: the data folder (`data`), the home folder
    /// (`USERPROFILE` and `HOME`, `home`) and `CLAUDE_CONFIG_DIR`
    /// (`claude-config`). So a call without `--settings` can never reach the
    /// real `~/.claude`.
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cc-hook"));
        cmd.env("CC_USAGE_DATA_DIR", &self.data)
            .env("USERPROFILE", self.home())
            .env("HOME", self.home())
            .env("CLAUDE_CONFIG_DIR", self.claude_config());
        cmd
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    fn claude_config(&self) -> PathBuf {
        self.dir.join("claude-config")
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    /// `cc-hook statusline --settings <this file>` with `input` on stdin.
    fn statusline(&self, input: &str) -> Output {
        self.statusline_with(&["--settings", self.p()], &[], input)
    }

    /// `cc-hook statusline <args>` with `env` on top of [`Self::command`]'s
    /// and `input` on stdin.
    fn statusline_with(&self, args: &[&str], env: &[(&str, &Path)], input: &str) -> Output {
        let mut cmd = self.command();
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .arg("statusline")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.settings).unwrap()).unwrap()
    }

    fn chain_file(&self) -> PathBuf {
        self.data.join("statusline-chain.json")
    }

    fn live(&self, id: &str) -> Option<Value> {
        let bytes = std::fs::read(self.data.join("live").join(format!("{id}.json"))).ok()?;
        Some(serde_json::from_slice(&bytes).unwrap())
    }

    /// Points the settings' status line at `command` (before install).
    fn set_status_command(&self, command: &str) {
        let mut doc = self.json();
        doc["statusLine"]["command"] = Value::from(command);
        std::fs::write(&self.settings, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }
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

fn our_command(f: &Fixture) -> String {
    install::statusline_command(
        &install::command_for_exe(Path::new(env!("CARGO_BIN_EXE_cc-hook"))).unwrap(),
        Some(&f.settings),
    )
    .unwrap()
}

/// The shell-form runtime tests need the shell Claude Code would use: Git Bash
/// on Windows (always there on a machine with Claude Code and Git), sh
/// elsewhere. Without it a test prints `skipped: no Git Bash` and passes, or
/// fails under `CC_TEST_REQUIRE_SHELL=1`.
fn have_shell() -> bool {
    if !cfg!(windows) || cc_hook::shell::find_git_bash().is_some() {
        return true;
    }
    skip_without_shell("no Git Bash");
    false
}

#[test]
fn install_chains_the_status_line_and_uninstall_restores_the_bytes() {
    let original = real_shape();
    let f = Fixture::new("roundtrip", Some(&original));
    let before = f.json();

    let out = f.cli(&["install", "--settings", f.p()]);
    ok(&out);
    let after = f.json();
    let ours = our_command(&f);
    assert_eq!(after["statusLine"]["command"], ours.as_str());
    assert_eq!(after["statusLine"]["type"], "command");
    assert_eq!(after["statusLine"]["padding"], 0);
    for key in ["modelSettings", "model"] {
        assert_eq!(after[key], before[key]);
    }
    let top: Vec<&String> = after.as_object().unwrap().keys().collect();
    assert_eq!(top, ["hooks", "statusLine", "modelSettings", "model"]);

    // The record holds the original object verbatim.
    let chains: Value = serde_json::from_slice(&std::fs::read(f.chain_file()).unwrap()).unwrap();
    let entry = chains["chains"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(entry["original"], before["statusLine"]);
    assert_eq!(entry["hadStatusLine"], true);

    // Idempotent: nothing to change the second time.
    let once = std::fs::read(&f.settings).unwrap();
    let out = f.cli(&["install", "--settings", f.p()]);
    ok(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to change"));
    assert_eq!(std::fs::read(&f.settings).unwrap(), once);

    let status = f.cli(&["status", "--settings", f.p()]);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("chained:"), "{text}");
    assert!(
        text.contains(r#"then runs node "C:\Users\me\.claude\statusline.js""#),
        "{text}"
    );

    let out = f.cli(&["uninstall", "--settings", f.p()]);
    ok(&out);
    assert_eq!(
        std::fs::read_to_string(&f.settings).unwrap(),
        original,
        "byte-identical"
    );
    assert!(
        !f.chain_file().exists(),
        "the last record goes with the file"
    );
}

#[test]
fn no_statusline_flag_leaves_it_alone() {
    let f = Fixture::new("flag", Some(&real_shape()));
    let before = f.json();
    ok(&f.cli(&["install", "--no-statusline", "--settings", f.p()]));
    assert_eq!(f.json()["statusLine"], before["statusLine"]);
    assert!(!f.chain_file().exists());
}

#[test]
fn a_missing_status_line_is_added_and_removed_again() {
    let original = "{\n  \"model\": \"opus\"\n}\n";
    let f = Fixture::new("none", Some(original));
    ok(&f.cli(&["install", "--settings", f.p()]));
    assert_eq!(f.json()["statusLine"]["command"], our_command(&f).as_str());
    // Nothing chained: the status line prints nothing and exits 0.
    let out = f.statusline(SAMPLE);
    ok(&out);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert_eq!(f.live("e2e-0b3c").unwrap()["contextUsedPercent"], 42.5);
    ok(&f.cli(&["uninstall", "--settings", f.p()]));
    // Hooks and status line gone; same bytes as before.
    assert_eq!(std::fs::read_to_string(&f.settings).unwrap(), original);
}

#[test]
fn uninstall_leaves_a_status_line_the_user_changed() {
    let f = Fixture::new("changed", Some(&real_shape()));
    ok(&f.cli(&["install", "--settings", f.p()]));
    f.set_status_command("bash ~/.claude/other.sh");
    let out = f.cli(&["uninstall", "--settings", f.p()]);
    ok(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("no longer runs cc-hook"));
    let after = f.json();
    assert_eq!(after["statusLine"]["command"], "bash ~/.claude/other.sh");
    install::verify_uninstalled(&after).unwrap();
    assert!(!f.chain_file().exists(), "a stale record is dropped");
}

/// The user's real settings file, copied (read only) into a temp folder.
/// Its cc-hook entries are stripped first, which is what the file looks like
/// before the app's installer runs; install then uninstall must give those
/// bytes back. Ignored: it reads the real file, so it runs only on request
/// (`cargo test -p cc-hook -- --ignored`).
#[test]
#[ignore = "reads the real ~/.claude/settings.json (or $CLAUDE_CONFIG_DIR's)"]
fn a_copy_of_the_real_settings_round_trips() {
    let Some(real) = install::default_settings_path().filter(|p| p.is_file()) else {
        eprintln!("no real settings.json; skipped");
        return;
    };
    let bytes = std::fs::read(&real).unwrap();
    // On a machine where cc-usage is installed the real status line already is
    // cc-hook's, and its original lives in the real data folder's chain record,
    // which this test must not read or touch. The "before install" shape the
    // test needs no longer exists there.
    let current: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let chained = current["statusLine"]["command"]
        .as_str()
        .is_some_and(|c| c.to_ascii_lowercase().contains("cc-hook"));
    if chained {
        eprintln!("the real status line is already chained through cc-hook; skipped");
        return;
    }
    let f = Fixture::new("real", Some(std::str::from_utf8(&bytes).unwrap()));
    install::uninstall_file(&f.settings).unwrap();
    let baseline = std::fs::read(&f.settings).unwrap();
    ok(&f.cli(&["install", "--settings", f.p()]));
    let installed = f.json();
    if let Some(orig) = serde_json::from_slice::<Value>(&baseline)
        .unwrap()
        .get("statusLine")
    {
        // type, padding and anything else kept, only the command replaced.
        let mut expected = orig.clone();
        if expected.is_object() {
            expected["command"] = Value::from(our_command(&f));
            expected["type"] = Value::from("command");
            assert_eq!(installed["statusLine"], expected);
        }
    }
    ok(&f.cli(&["uninstall", "--settings", f.p()]));
    assert_eq!(
        std::fs::read(&f.settings).unwrap(),
        baseline,
        "byte-identical"
    );
}

#[test]
fn the_chained_command_gets_the_same_stdin_and_its_stdout_and_exit_code_come_back() {
    if !have_shell() {
        return;
    }
    let f = Fixture::new("chain", Some(&real_shape()));
    f.set_status_command("printf 'X:'; cat; printf ':Y'; exit 7");
    ok(&f.cli(&["install", "--settings", f.p()]));

    let out = f.statusline(SAMPLE);
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("X:{SAMPLE}:Y")
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let live = f.live("e2e-0b3c").unwrap();
    assert_eq!(live["v"], 1);
    assert_eq!(live["contextUsedPercent"], 42.5);
    assert_eq!(live["contextUsedTokens"], 425_000);
    assert_eq!(live["modelId"], "claude-opus-5-5");
    assert_eq!(live["fiveHourPercent"], 23.5);

    // Exit 0 and multi-line output, bytes untouched (no newline translation).
    let chains_path = f.chain_file();
    let mut chains: Value = serde_json::from_slice(&std::fs::read(&chains_path).unwrap()).unwrap();
    for entry in chains["chains"].as_object_mut().unwrap().values_mut() {
        entry["original"]["command"] = Value::from(r"printf 'a\nb\r\n\033[36mc\033[0m\n'");
    }
    std::fs::write(&chains_path, serde_json::to_vec(&chains).unwrap()).unwrap();
    let out = f.statusline(SAMPLE);
    ok(&out);
    assert_eq!(out.stdout, b"a\nb\r\n\x1b[36mc\x1b[0m\n");
}

#[test]
fn quoted_windows_paths_work_like_the_users_node_command() {
    if !have_shell() {
        return;
    }
    let f = Fixture::new("quoted", Some(&real_shape()));
    let script = f.dir.join("line script.sh");
    std::fs::write(&script, "read -r line; printf 'got %s' \"${#line}\"\n").unwrap();
    // Native separators in double quotes, as in `node "C:\Users\...\statusline.js"`.
    f.set_status_command(&format!("sh \"{}\"", script.display()));
    ok(&f.cli(&["install", "--settings", f.p()]));
    let out = f.statusline(SAMPLE);
    ok(&out);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("got {}", SAMPLE.len())
    );
}

#[test]
fn a_bad_session_id_skips_the_export_but_still_chains() {
    if !have_shell() {
        return;
    }
    let f = Fixture::new("badid", Some(&real_shape()));
    f.set_status_command("printf ok");
    ok(&f.cli(&["install", "--settings", f.p()]));
    let out = f.statusline(r#"{"session_id":"../../evil","context_window":{"used_percentage":5}}"#);
    ok(&out);
    assert_eq!(out.stdout, b"ok");
    assert!(!f.data.join("live").exists());
    // Null context early in a session: exported as null, still chained.
    let out = f.statusline(
        r#"{"session_id":"early","context_window":{"used_percentage":null,"current_usage":null}}"#,
    );
    ok(&out);
    assert_eq!(out.stdout, b"ok");
    assert!(f.live("early").unwrap()["contextUsedPercent"].is_null());
}

#[test]
fn a_failed_export_still_runs_the_chained_command() {
    if !have_shell() {
        return;
    }
    let f = Fixture::new("noexport", Some(&real_shape()));
    f.set_status_command("printf still-here");
    ok(&f.cli(&["install", "--settings", f.p()]));
    // A file where the live folder should be: the export cannot write.
    std::fs::write(f.data.join("live"), b"").unwrap();
    let out = f.statusline(SAMPLE);
    ok(&out);
    assert_eq!(out.stdout, b"still-here");
    assert!(out.stderr.is_empty());
}

#[test]
fn without_any_record_or_input_it_is_silent() {
    let f = Fixture::new("silent", None);
    for input in ["", "not json", SAMPLE] {
        let out = f.statusline(input);
        ok(&out);
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{input}");
    }
}

#[test]
fn without_settings_the_status_line_follows_claude_config_dir() {
    if !have_shell() {
        return;
    }
    let f = Fixture::new("config-dir", None);
    let config = f.claude_config();
    std::fs::create_dir_all(&config).unwrap();
    let settings = config.join("settings.json");
    let mut doc: Value = serde_json::from_str(&real_shape()).unwrap();
    doc["statusLine"]["command"] = Value::from("printf via-config-dir");
    let original = serde_json::to_string_pretty(&doc).unwrap();
    std::fs::write(&settings, &original).unwrap();

    // Install as the app's installer does: no --settings, CLAUDE_CONFIG_DIR set.
    ok(&f.cli(&["install"]));
    let exe = install::command_for_exe(Path::new(env!("CARGO_BIN_EXE_cc-hook"))).unwrap();
    let installed: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(
        installed["statusLine"]["command"],
        format!("{exe} statusline")
    );
    assert!(
        !f.home().join(".claude").exists(),
        "the home folder is left alone"
    );

    // What Claude Code runs: the plain command, in its own CLAUDE_CONFIG_DIR.
    let out = f.statusline_with(&[], &[], SAMPLE);
    ok(&out);
    assert_eq!(out.stdout, b"via-config-dir");
    assert_eq!(f.live("e2e-0b3c").unwrap()["contextUsedPercent"], 42.5);

    // Another Claude Code folder has no record: export only, nothing chained.
    let other = f.dir.join("other-config");
    let out = f.statusline_with(&[], &[("CLAUDE_CONFIG_DIR", other.as_path())], SAMPLE);
    ok(&out);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());

    ok(&f.cli(&["uninstall"]));
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        original,
        "byte-identical"
    );
    assert!(!f.chain_file().exists());
}
