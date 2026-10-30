//! `cc-publish stop` and `--data-dir` against the real binary, offline: the
//! config points at a closed local port, so `run` keeps retrying the connect
//! (never fatal) until it is asked to stop. `CLAUDE_CONFIG_DIR` is a temporary
//! folder, so no real session is read.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use cc_publish::state::PublishState;

const BIN: &str = env!("CARGO_BIN_EXE_cc-publish");

const CONFIG: &str = r#"{
  "alias": "stoptest",
  "servers": ["tls://127.0.0.1:9"],
  "creds": "nats/cc-sessions.pub-stoptest.creds",
  "log_file": true
}
"#;

struct Setup {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    claude: PathBuf,
    config: PathBuf,
}

fn setup() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    // A space in the path, like most Windows profiles.
    let data = tmp.path().join("data dir");
    let claude = tmp.path().join("claude");
    std::fs::create_dir_all(data.join("nats")).unwrap();
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(
        data.join("nats").join("cc-sessions.pub-stoptest.creds"),
        "not real\n",
    )
    .unwrap();
    let config = data.join("publish.json");
    std::fs::write(&config, CONFIG).unwrap();
    let s = Setup {
        _tmp: tmp,
        data,
        claude,
        config,
    };
    // This machine's key pair, and a viewer key pair in
    // the same data dir so that viewer.pub is there.
    let out = cmd(&s, &["keygen"]);
    assert!(out.status.success(), "keygen: {out:?}");
    let keys = s.data.join("keys");
    assert!(
        keys.join("publisher.key").is_file(),
        "--data-dir applies to keygen"
    );
    let line = String::from_utf8(out.stdout).unwrap();
    let public =
        cc_envelope::PublicKey::parse_line(line.trim()).expect("stdout is the public key line");
    assert!(public.kid().starts_with('p'));
    let out = cmd(&s, &["viewer-keygen"]);
    assert!(out.status.success(), "viewer-keygen: {out:?}");
    assert!(keys.join("viewer.key").is_file() && keys.join("viewer.pub").is_file());
    s
}

fn public_line(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn keygen_refuses_to_overwrite_and_if_missing_prints_the_same_key() {
    let s = setup();
    let key = s.data.join("keys").join("publisher.key");
    let before = std::fs::read(&key).unwrap();
    let first = cmd(&s, &["keygen", "--if-missing"]);
    assert!(first.status.success(), "{first:?}");
    let line = public_line(&first);
    assert!(cc_envelope::PublicKey::parse_line(&line).is_ok(), "{line}");

    let again = cmd(&s, &["keygen"]);
    assert!(!again.status.success(), "a second keygen must refuse");
    assert!(String::from_utf8_lossy(&again.stderr).contains("--force"));
    assert!(again.stdout.is_empty());
    assert_eq!(std::fs::read(&key).unwrap(), before, "untouched");

    let kept = cmd(&s, &["keygen", "--if-missing"]);
    assert!(kept.status.success());
    assert_eq!(
        public_line(&kept),
        line,
        "--if-missing prints the existing key"
    );
    assert_eq!(std::fs::read(&key).unwrap(), before);

    // viewer-keygen --if-missing prints the viewer's line, which matches viewer.pub.
    let viewer = cmd(&s, &["viewer-keygen", "--if-missing"]);
    assert!(viewer.status.success());
    let pubs = cc_envelope::PublicKeys::load(s.data.join("keys").join("viewer.pub")).unwrap();
    assert_eq!(public_line(&viewer), pubs.current().to_line());
    assert!(!cmd(&s, &["viewer-keygen"]).status.success());

    // status names the key ids it seals with.
    let status = cmd(&s, &["status", "--config", s.config.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("envelope    publisher key p"), "{text}");

    // A member the config does not know is an error, like any typo.
    let with_key = CONFIG.replace("\"log_file\"", "\"key\": \"key\",\n  \"log_file\"");
    std::fs::write(&s.config, with_key).unwrap();
    let status = cmd(&s, &["status", "--config", s.config.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("unknown field `key`"), "{text}");
}

fn command(s: &Setup, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.arg("--data-dir")
        .arg(&s.data)
        .args(args)
        // A different CC_USAGE_DATA_DIR loses against --data-dir.
        .env("CC_USAGE_DATA_DIR", s.data.join("ignored"))
        .env("CLAUDE_CONFIG_DIR", &s.claude)
        .env("CC_PUBLISH_LOG", "debug");
    c
}

fn cmd(s: &Setup, args: &[&str]) -> Output {
    command(s, args).output().expect("run cc-publish")
}

fn spawn_run(s: &Setup) -> Child {
    command(s, &["run", "--config", s.config.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cc-publish run")
}

fn state(s: &Setup) -> Option<PublishState> {
    PublishState::load(&s.data.join(cc_publish::state::STATE_FILE))
}

fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("cc-publish run did not exit");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn log(data: &Path) -> String {
    std::fs::read_to_string(data.join("cc-publish.log")).unwrap_or_default()
}

#[test]
fn stop_ends_a_running_publisher_cleanly() {
    let s = setup();
    let mut child = spawn_run(&s);
    let pid = child.id();
    wait_for("the state file", Duration::from_secs(20), || {
        state(&s).is_some_and(|st| st.pid == pid && st.running)
    });
    assert!(!s.data.join("ignored").exists(), "CC_USAGE_DATA_DIR lost");

    let status = cmd(&s, &["status", "--config", s.config.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains(&format!("running, pid {pid}")), "{text}");
    assert_eq!(status.status.code(), Some(1), "never wrote, so not healthy");

    let out = cmd(&s, &["stop", "--wait", "20"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(
        text.contains(&format!("stopped the publisher (pid {pid})")),
        "{text}"
    );
    assert!(wait_exit(&mut child, Duration::from_secs(10)).success());
    let st = state(&s).unwrap();
    assert!(!st.running, "marked stopped");
    assert!(!s.data.join(cc_publish::stop::STOP_FILE).exists());
    assert!(log(&s.data).contains("stop requested"), "{}", log(&s.data));

    let out = cmd(&s, &["stop"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("no publisher is running"));
}

#[test]
fn a_stale_request_does_not_stop_a_new_run() {
    let s = setup();
    cc_publish::stop::request(&s.data.join(cc_publish::stop::STOP_FILE), 1).unwrap();
    let mut child = spawn_run(&s);
    let pid = child.id();
    wait_for("the state file", Duration::from_secs(20), || {
        state(&s).is_some_and(|st| st.pid == pid && st.running)
    });
    std::thread::sleep(Duration::from_millis(1500));
    assert!(child.try_wait().unwrap().is_none(), "still running");
    assert!(
        !s.data.join(cc_publish::stop::STOP_FILE).exists(),
        "cleared at start"
    );
    assert!(cmd(&s, &["stop", "--wait", "20"]).status.success());
    assert!(wait_exit(&mut child, Duration::from_secs(10)).success());
}

/// Two starters at once (the scheduled task and a manual `run`): the second
/// finds the lock taken and exits 1, even with `--force`, and the first
/// keeps publishing.
#[test]
fn a_second_run_is_refused_while_one_holds_the_lock() {
    let s = setup();
    let mut first = spawn_run(&s);
    let pid = first.id();
    wait_for("the state file", Duration::from_secs(20), || {
        state(&s).is_some_and(|st| st.pid == pid && st.running)
    });
    assert!(s.data.join(cc_publish::instance::LOCK_FILE).is_file());

    let second = cmd(
        &s,
        &[
            "run",
            "--config",
            s.config.to_str().unwrap(),
            "--force",
            "--for",
            "5",
        ],
    );
    assert_eq!(second.status.code(), Some(1), "{second:?}");
    let text = log(&s.data);
    assert!(
        text.contains(&format!("another publisher (pid {pid}) is active")),
        "{text}"
    );
    assert!(
        first.try_wait().unwrap().is_none(),
        "the first one still runs"
    );
    assert_eq!(
        state(&s).unwrap().pid,
        pid,
        "the state file is still the first one's"
    );

    assert!(cmd(&s, &["stop", "--wait", "20"]).status.success());
    assert!(wait_exit(&mut first, Duration::from_secs(10)).success());
    // Released on exit: the next run starts.
    let next = cmd(
        &s,
        &["run", "--config", s.config.to_str().unwrap(), "--for", "1"],
    );
    assert!(next.status.success(), "{next:?}");
}

/// Windows and Linux: a publisher killed outright (ending its task, a forced
/// stop, a crash under systemd) leaves a fresh state file saying it runs; the
/// next run must start at once instead of refusing for 30 s.
#[cfg(any(windows, target_os = "linux"))]
#[test]
fn a_killed_publisher_does_not_block_the_next_run() {
    let s = setup();
    let mut first = spawn_run(&s);
    let pid = first.id();
    wait_for("the state file", Duration::from_secs(20), || {
        state(&s).is_some_and(|st| st.pid == pid && st.running)
    });
    first.kill().unwrap();
    first.wait().unwrap();
    let st = state(&s).unwrap();
    assert!(
        st.running && st.is_live(cc_publish::util::now_secs()),
        "fresh, says running"
    );

    let out = cmd(&s, &["status", "--config", s.config.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(&format!("pid {pid} has exited")), "{text}");
    let out = cmd(&s, &["stop"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("no publisher is running"));

    let mut second = spawn_run(&s);
    let pid2 = second.id();
    wait_for("the second run", Duration::from_secs(20), || {
        state(&s).is_some_and(|st| st.pid == pid2 && st.running)
    });
    assert!(cmd(&s, &["stop", "--wait", "20"]).status.success());
    assert!(wait_exit(&mut second, Duration::from_secs(10)).success());
}

/// A panic in the gathering thread must end the process with a failure (so
/// systemd's `Restart=on-failure` or the scheduled task starts a new one),
/// log why, and leave a state file that does not block the next run.
#[test]
fn a_gather_panic_ends_the_process_with_a_failure() {
    let s = setup();
    let mut child = command(&s, &["run", "--config", s.config.to_str().unwrap()])
        .env(cc_publish::run::FAULT_ENV, "gather-panic")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cc-publish run");
    let pid = child.id();
    let status = wait_exit(&mut child, Duration::from_secs(30));
    assert!(!status.success(), "{status:?}");
    if cfg!(debug_assertions) {
        // Unwinding build: the supervisor's exit code. A release build aborts.
        assert_eq!(status.code(), Some(cc_publish::run::EXIT_GATHER_PANIC));
        let st = state(&s).unwrap();
        assert_eq!(st.pid, pid);
        assert!(!st.running, "marked stopped");
        assert!(
            st.exit_reason
                .as_deref()
                .is_some_and(|e| e.contains("gathering thread panicked")),
            "{:?}",
            st.exit_reason
        );
    }
    let text = log(&s.data);
    assert!(text.contains("injected fault"), "{text}");
    let out = cmd(&s, &["status", "--config", s.config.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    if cfg!(debug_assertions) {
        assert!(text.contains("the gathering thread panicked"), "{text}");
    }
    assert!(state(&s).is_some_and(|st| !st.blocks_run(1, cc_publish::util::now_secs())));
}
