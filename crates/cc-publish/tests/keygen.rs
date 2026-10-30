//! `cc-publish keygen` against the real binary: the publisher key is the
//! machine's identity, so replacing an existing one (`--rotate`, `--force`)
//! needs the config's alias. stdin is not a terminal here (`output()` gives
//! the child a closed stdin), so only `--confirm` can confirm.

use std::path::{Path, PathBuf};
use std::process::Output;

use cc_envelope::SecretKeys;

const BIN: &str = env!("CARGO_BIN_EXE_cc-publish");

struct Setup {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    key: PathBuf,
    config: PathBuf,
}

fn setup() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data dir");
    std::fs::create_dir_all(&data).unwrap();
    let config = data.join("publish.json");
    std::fs::write(&config, "{\n  \"alias\": \"lab-01\"\n}\n").unwrap();
    let key = data.join("keys").join("publisher.key");
    Setup {
        _tmp: tmp,
        data,
        key,
        config,
    }
}

fn keygen(s: &Setup, args: &[&str]) -> Output {
    std::process::Command::new(BIN)
        .arg("--data-dir")
        .arg(&s.data)
        .arg("keygen")
        .args(args)
        .env_remove("CC_USAGE_DATA_DIR")
        .output()
        .expect("run cc-publish")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn kids(path: &Path) -> Vec<String> {
    SecretKeys::load(path)
        .unwrap()
        .keys()
        .iter()
        .map(|k| k.kid().to_string())
        .collect()
}

#[test]
fn a_first_key_needs_no_confirmation_and_says_the_viewer_asks() {
    let s = setup();
    let out = keygen(&s, &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(s.key.is_file());
    assert!(
        stderr(&out).contains("the viewer asks to accept it"),
        "{}",
        stderr(&out)
    );
    // --rotate and --force with no key yet make the first one, unconfirmed.
    let other = setup();
    let out = keygen(&other, &["--rotate"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(kids(&other.key).len(), 1);
}

#[test]
fn replacing_a_key_needs_the_alias() {
    let s = setup();
    assert!(keygen(&s, &[]).status.success());
    let before = std::fs::read(&s.key).unwrap();
    let config = s.config.to_str().unwrap();

    for flag in ["--rotate", "--force"] {
        // No --confirm and no terminal: refused with the hint, exit 2.
        let out = keygen(&s, &[flag]);
        assert_eq!(out.status.code(), Some(2), "{flag}: {out:?}");
        let text = stderr(&out);
        assert!(text.contains("This replaces lab-01's identity."), "{text}");
        assert!(text.contains("--confirm lab-01"), "{text}");
        assert!(out.stdout.is_empty());
        // The wrong alias.
        let out = keygen(&s, &[flag, "--confirm", "lab-02"]);
        assert_eq!(out.status.code(), Some(2), "{flag}: {out:?}");
        assert!(
            stderr(&out).contains("not this machine's alias"),
            "{}",
            stderr(&out)
        );
        // A config elsewhere that does not exist.
        let out = keygen(
            &s,
            &[
                flag,
                "--confirm",
                "lab-01",
                "--config",
                "/nowhere/publish.json",
            ],
        );
        assert_eq!(out.status.code(), Some(2), "{flag}: {out:?}");
        assert!(stderr(&out).contains("--config PATH"), "{}", stderr(&out));
        assert_eq!(std::fs::read(&s.key).unwrap(), before, "{flag}: untouched");
    }

    // --if-missing never asks.
    let out = keygen(&s, &["--if-missing"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read(&s.key).unwrap(), before);

    let first = kids(&s.key);
    let out = keygen(&s, &["--rotate", "--confirm", "lab-01", "--config", config]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains("identity changed"),
        "{}",
        stderr(&out)
    );
    let rotated = kids(&s.key);
    assert_eq!(rotated.len(), 2);
    assert_eq!(rotated[1], first[0], "the old key stays behind the new one");
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(line.starts_with(&format!("{}:", rotated[0])), "{line}");

    // The default config is <data_dir>/publish.json.
    let out = keygen(&s, &["--force", "--confirm", "lab-01"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(kids(&s.key).len(), 1);
}
