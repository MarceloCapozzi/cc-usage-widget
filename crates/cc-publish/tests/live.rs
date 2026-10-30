//! Live test against Synadia Cloud, ignored by default:
//!
//! ```text
//! cargo test -p cc-publish --test live -- --ignored --nocapture
//! ```
//!
//! Uses the existing `cc-sessions.pub-test` credentials (alias `test`) and
//! the `cc-sessions.viewer` credentials from the real data directory, copied
//! into a temporary `CC_USAGE_DATA_DIR` that also holds freshly generated
//! publisher and viewer key pairs. It runs `init`,
//! `keygen`, `viewer-keygen`, `check`, a 60 s `run` with one synthetic
//! session from the spool, then checks from the viewer side that the host
//! record, the session, one synthetic project and that machine's activity
//! are in the bucket and open with the temporary key, and finally `clear`s every session key it wrote
//! (the project and activity keys expire with the bucket's TTL). `CLAUDE_CONFIG_DIR` points into
//! the temporary directory, so none of this machine's real sessions are
//! published. At most one connection is open at a time.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cc_core::events::SpoolEvent;
use cc_envelope::{KvKey, OpenKeys, SecretKeys, ViewerKeys};

const BIN: &str = env!("CARGO_BIN_EXE_cc-publish");
const SESSION_ID: &str = "cc-publish-live-test-0001";
const PROJECT_FOLDER: &str = "-tmp-cc-publish-live-project";

/// `secs` as an RFC 3339 UTC timestamp, the transcripts' format.
fn chrono_like_rfc3339(secs: f64) -> String {
    format!(
        "{}Z",
        cc_publish::util::format_utc(secs)
            .trim_end_matches(" UTC")
            .replacen(' ', "T", 1)
    )
}

fn cc_publish(data: &Path, claude: &Path, args: &[&str]) -> Output {
    let out = Command::new(BIN)
        .args(args)
        .env("CC_USAGE_DATA_DIR", data)
        .env("CLAUDE_CONFIG_DIR", claude)
        .env("CC_PUBLISH_LOG", "debug")
        .output()
        .expect("run cc-publish");
    println!("$ cc-publish {}", args.join(" "));
    print!("{}", String::from_utf8_lossy(&out.stdout));
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    out
}

fn copy_creds(user: &str, to: &Path) -> PathBuf {
    let from = cc_core::paths::nats_dir().join(format!("{user}.creds"));
    assert!(from.is_file(), "missing {}", from.display());
    let dest = to.join(format!("{user}.creds"));
    std::fs::copy(&from, &dest).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    dest
}

#[test]
#[ignore = "talks to Synadia Cloud with the pub-test and viewer credentials"]
fn check_run_and_view_against_synadia() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let claude = tmp.path().join("claude");
    let nats = data.join("nats");
    std::fs::create_dir_all(&nats).unwrap();
    std::fs::create_dir_all(&claude).unwrap();
    copy_creds("cc-sessions.pub-test", &nats);
    // The viewer's creds stay outside the publisher's data dir.
    let viewer_creds = copy_creds("cc-sessions.viewer", tmp.path());

    assert!(cc_publish(&data, &claude, &["init", "--alias", "test"])
        .status
        .success());
    assert!(cc_publish(&data, &claude, &["keygen"]).status.success());
    assert!(cc_publish(&data, &claude, &["viewer-keygen"])
        .status
        .success());
    let refused = cc_publish(&data, &claude, &["keygen"]);
    assert!(!refused.status.success(), "keygen must refuse to overwrite");

    let check = cc_publish(&data, &claude, &["check"]);
    let text = String::from_utf8_lossy(&check.stdout);
    assert!(check.status.success(), "check failed");
    for line in [
        "PASS  connect",
        "PASS  bind",
        "PASS  put probe",
        "PASS  delete probe",
    ] {
        assert!(text.contains(line), "missing {line:?}");
    }

    // One synthetic hooked session in the spool.
    let spool = data.join("spool");
    std::fs::create_dir_all(&spool).unwrap();
    let now = cc_publish::util::now_secs();
    let mut start = SpoolEvent::new("SessionStart", SESSION_ID, now);
    start.model = Some("claude-opus-5-5".into());
    let mut prompt = SpoolEvent::new("UserPromptSubmit", SESSION_ID, now + 0.001);
    prompt.prompt_id = Some("p1".into());
    for (i, e) in [start, prompt].iter().enumerate() {
        let name = e.file_name(&format!("live{i}"));
        std::fs::write(spool.join(name), serde_json::to_vec(e).unwrap()).unwrap();
    }

    // One synthetic project transcript, under the temporary CLAUDE_CONFIG_DIR.
    let project = claude.join("projects").join(PROJECT_FOLDER);
    std::fs::create_dir_all(&project).unwrap();
    let ts = chrono_like_rfc3339(now);
    std::fs::write(
        project.join("s1.jsonl"),
        format!(
            r#"{{"timestamp":"{ts}","cwd":"/tmp/cc-publish-live-project","message":{{"model":"claude-opus-5-5","usage":{{"input_tokens":1234,"output_tokens":56}}}}}}"#
        ),
    )
    .unwrap();

    // From here on every key written is cleared, even if an assertion fails.
    let guard = ClearOnDrop {
        data: data.clone(),
        claude: claude.clone(),
        cleared: false,
    };

    // 65 s: the first puts plus one 60 s heartbeat of the session and host.
    let run = cc_publish(&data, &claude, &["run", "--for", "65"]);
    assert!(run.status.success(), "run failed");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("publish-state.json")).unwrap()).unwrap();
    println!("state: {state:#}");
    assert_eq!(state["running"], false);
    assert!(
        state["puts"].as_u64().unwrap() >= 2,
        "put, then a heartbeat"
    );
    assert!(
        state["host_puts"].as_u64().unwrap() >= 2,
        "host, then a heartbeat"
    );
    assert_eq!(state["failures"], 0);
    let sid = cc_envelope::sid_for(SESSION_ID);
    let session_key = cc_envelope::session_key("test", &sid);
    assert_eq!(state["keys"], serde_json::json!([session_key]));

    // The viewer sees the session and it opens with the temporary key.
    // The viewer key made by viewer-keygen above, with
    // this publisher's key pinned up front, since other test publishers
    // share the alias "test" and must not be pinned first.
    let keys = ViewerKeys::new(
        SecretKeys::load(data.join("keys").join("viewer.key")).unwrap(),
        None,
    );
    let publisher = SecretKeys::load(data.join("keys").join("publisher.key")).unwrap();
    keys.pin("test", &publisher.current().public_key()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let config = cc_remote::Config {
            servers: vec![
                "wss://connect.ngs.global".into(),
                "tls://connect.ngs.global".into(),
            ],
            bucket: cc_core::BUCKET.into(),
            creds_path: viewer_creds.clone(),
            user: "cc-sessions.viewer".into(),
            name: "cc-publish-live-test-viewer".into(),
        };
        let client = cc_remote::connect(config).await.expect("viewer connects");
        let viewer = cc_remote::Viewer::bind(&client, cc_core::BUCKET)
            .await
            .unwrap();
        let snapshot = viewer.snapshot().await.unwrap();
        client.close().await;
        let mine: Vec<_> = snapshot
            .iter()
            .filter(|(k, _)| KvKey::parse(k).is_some_and(|k| k.alias() == "test"))
            .collect();
        // Other test publishers share the alias "test" and may have
        // overwritten test.host with their own key since; only ours must open.
        match mine.iter().find(|(k, _)| k == "test.host") {
            Some((_, bytes)) => {
                match open_as::<cc_envelope::HostPayload>(&keys, bytes, "test.host") {
                    Ok(host) => {
                        println!("viewer opened test.host: {host:?}");
                        assert_eq!(host.machine, "test");
                        assert_eq!(host.os, std::env::consts::OS);
                    }
                    Err(err) => println!("test.host was rewritten by another publisher: {err}"),
                }
            }
            None => println!("test.host was deleted by another publisher"),
        }
        let session = mine
            .iter()
            .find(|(k, _)| *k == session_key)
            .expect("the session in the bucket");
        let payload =
            open_as::<cc_envelope::RemotePayload>(&keys, &session.1, &session_key).unwrap();
        println!("viewer opened {session_key}: {payload:?}");
        assert_eq!(payload.machine, "test");
        assert_eq!(payload.sid, sid);
        assert_eq!(payload.state, cc_core::session::SessionState::Working);
        assert_eq!(payload.model.as_deref(), Some("claude-opus-5-5"));
        assert!(
            payload.hb as f64 >= now + 55.0,
            "the heartbeat refreshed hb"
        );
        assert!(
            open_as::<cc_envelope::RemotePayload>(&keys, &session.1, "test.s.00000000").is_err(),
            "bound to its key"
        );

        let project_key = cc_envelope::project_key("test", &cc_envelope::pid_for(PROJECT_FOLDER));
        let value = mine
            .iter()
            .find(|(k, _)| *k == project_key)
            .expect("the project in the bucket");
        let p = open_as::<cc_envelope::ProjectPayload>(&keys, &value.1, &project_key).unwrap();
        println!("viewer opened {project_key}: {p:?}");
        assert_eq!(p.machine, "test");
        assert_eq!(
            p.name, "cc-publish-live-project",
            "the folder name, not the path"
        );
        assert_eq!(p.sessions, 1);
        assert_eq!(p.tokens().input, 1234);
        assert_eq!(p.models[0].model, "claude-opus-5-5");

        // The same transcript is today's activity.
        let activity_key = cc_envelope::activity_key("test");
        let value = mine
            .iter()
            .find(|(k, _)| *k == activity_key)
            .expect("the activity in the bucket");
        let a = open_as::<cc_envelope::ActivityPayload>(&keys, &value.1, &activity_key).unwrap();
        println!("viewer opened {activity_key}: {a:?}");
        assert_eq!(a.machine, "test");
        assert_eq!(a.days.len(), 1);
        assert_eq!(a.days[0].tokens, 1234 + 56);
    });

    let mut guard = guard;
    let text = guard.clear();
    assert!(text.contains(&format!("deleted {session_key}")), "{text}");
    assert!(text.contains("deleted test.host"), "{text}");

    // The run has stopped, so status reports it and exits 1.
    let status = cc_publish(&data, &claude, &["status"]);
    let out = String::from_utf8_lossy(&status.stdout);
    assert_eq!(status.status.code(), Some(1), "{out}");
    assert!(
        out.contains("NOT PUBLISHING: the publisher is not running"),
        "{out}"
    );
}

/// Runs `cc-publish clear` when dropped, unless it already ran.
struct ClearOnDrop {
    data: PathBuf,
    claude: PathBuf,
    cleared: bool,
}

impl ClearOnDrop {
    fn clear(&mut self) -> String {
        self.cleared = true;
        let out = cc_publish(&self.data, &self.claude, &["clear", "--force"]);
        assert!(out.status.success(), "clear failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        if !self.cleared {
            let _ = cc_publish(&self.data, &self.claude, &["clear", "--force"]);
        }
    }
}

/// Opens a value with the viewer's keys and returns its payload.
fn open_as<T: serde::de::DeserializeOwned>(
    keys: &ViewerKeys,
    bytes: &[u8],
    kv_key: &str,
) -> Result<T, cc_envelope::EnvelopeError> {
    keys.open_any(bytes, kv_key).map(|o| o.value)
}
