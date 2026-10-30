//! Tests for the backup file: its round trip, the password-sealed secrets
//! and their allowlist, the merge of each section on restore and the default
//! file name.

use super::*;
use crate::history::{Reading, Sample};
use crate::remote::RemoteHub;
use std::collections::HashMap;

/// Argon2id's minimum cost: the format is under test here, not the
/// cost, and the default takes seconds in a debug build.
const FAST: KdfParams = KdfParams { m: 8, t: 1, p: 1 };
const LATEST: &str = "2099-12-31";
const B64_A: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
const B64_B: &str = "Hx4dHBsaGRgXFhUUExIREA8ODQwLCgkIBwYFBAMCAQA=";
/// Not a real credential: a marker to look for in the backup's text.
const CREDS: &str = "-----BEGIN TEST CREDENTIAL MARKER-----";

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn sample(t: i64, p: f64) -> Sample {
    Sample {
        t,
        v: vec![Reading {
            k: "5-hour session".into(),
            p,
            r: None,
        }],
    }
}

fn project(alias: &str, folder: &str, heard: i64) -> StoredProject {
    StoredProject {
        key: cc_envelope::project_key(alias, &cc_envelope::pid_for(folder)),
        alias: alias.into(),
        payload: cc_core::projects::ProjectPayload::new(
            alias,
            folder,
            1,
            &HashMap::new(),
            Some(heard),
            heard,
        ),
        heard,
    }
}

fn target<'a>(dir: &'a Path, this: &'a str) -> Target<'a> {
    Target {
        data_dir: dir,
        this_machine: this,
        now: remote::now_unix(),
        oldest: "2000-01-01".into(),
        latest: LATEST.into(),
    }
}

fn secrets(pairs: &[(&str, &str)]) -> SecretFiles {
    let mut files = SecretFiles::default();
    for (rel, text) in pairs {
        files.insert(rel.to_string(), text.to_string());
    }
    files
}

/// A data folder with every data file and every kind of secret.
fn populated(dir: &Path) {
    let history = History {
        samples: vec![sample(now_ms() - 60_000, 12.0)],
    };
    history.save_to(&dir.join(crate::paths::HISTORY)).unwrap();
    std::fs::write(
        dir.join(activity::FILE),
        r#"{"v":1,"machines":{"":{"2026-10-01":{"t":100,"h":9}},"devbox":{"2026-10-02":{"t":50,"h":3}}}}"#,
    )
    .unwrap();
    let projects = vec![project("devbox", "c--app", remote::now_unix() - 60)];
    std::fs::write(
        dir.join(remote::REMOTE_PROJECTS_FILE),
        serde_json::to_string(&projects).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join(crate::paths::PRICING),
        r#"{"claude-x":{"input":1,"output":2}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("nats")).unwrap();
    std::fs::write(dir.join("nats").join("cc-sessions.viewer.creds"), CREDS).unwrap();
    std::fs::write(dir.join("nats").join("notes.txt"), "not a secret").unwrap();
    std::fs::create_dir_all(dir.join("keys").join("sub")).unwrap();
    std::fs::write(dir.join("keys").join("sub").join("id.nk"), "seed").unwrap();
    // Machine-specific files that never travel.
    std::fs::write(dir.join(crate::paths::LOG), "a local log line").unwrap();
}

#[test]
fn a_backup_round_trips_through_its_file() {
    let src = tempfile::tempdir().unwrap();
    populated(src.path());
    let made = create(
        src.path(),
        "wk-01",
        Some(("correct horse", FAST)),
        Utc::now(),
    )
    .unwrap();
    assert!(made.skipped.is_empty());
    let file = src
        .path()
        .join("out")
        .join(default_file_name("wk-01", chrono::Local::now()));
    write(&file, &made.backup).unwrap();

    let back = read(&file).unwrap();
    assert_eq!(back, made.backup);
    assert_eq!(back.format, FORMAT);
    assert_eq!(back.app_version, env!("CARGO_PKG_VERSION"));
    assert!(back.created.ends_with('Z'), "{}", back.created);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        !text.contains(CREDS) && !text.contains(B64_A),
        "secrets are sealed"
    );
    let data = serde_json::to_string(&back.data).unwrap();
    assert!(
        !data.contains("a local log line"),
        "machine-specific files stay out"
    );

    let c = contents(&back).unwrap();
    let s = summary(&back, &c, &file, "other-pc");
    assert_eq!(
        s.counts,
        Counts {
            history_samples: 1,
            activity_machines: 2,
            activity_days: 2,
            remote_projects: 1,
            has_pricing: true
        }
    );
    assert!(s.has_secrets && !s.same_machine);

    let files = open(back.secrets.as_ref().unwrap(), "correct horse").unwrap();
    assert_eq!(
        files.paths(),
        ["keys/sub/id.nk", "nats/cc-sessions.viewer.creds"]
    );
    assert_eq!(files.files["nats/cc-sessions.viewer.creds"], CREDS);
}

#[test]
fn missing_files_are_null_and_unreadable_ones_are_named() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(activity::FILE), "{ half").unwrap();
    let made = create(dir.path(), "wk-01", None, Utc::now()).unwrap();
    assert_eq!(made.backup.data, Data::default());
    assert_eq!(made.backup.secrets, None);
    assert_eq!(made.skipped, [activity::FILE]);
    let json = String::from_utf8(to_bytes(&made.backup).unwrap()).unwrap();
    assert!(
        json.contains(r#""history":null"#) && json.contains(r#""secrets":null"#),
        "{json}"
    );
}

#[test]
fn secrets_need_a_password_and_something_to_seal() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        create(dir.path(), "wk-01", Some(("short", FAST)), Utc::now()),
        Err(BackupError::Password(_))
    ));
    assert_eq!(
        create(dir.path(), "wk-01", Some(("long enough", FAST)), Utc::now()).unwrap_err(),
        BackupError::NoSecrets
    );
}

#[test]
fn secrets_open_with_the_right_password_only() {
    let files = secrets(&[("keys/viewer.key", "k1:x"), ("nats/a.creds", "c")]);
    let sealed = seal(&files, "password-1", FAST).unwrap();
    assert_eq!((sealed.m, sealed.t, sealed.p), (8, 1, 1));
    assert_eq!(
        open(&sealed, "password-1").unwrap().paths(),
        ["keys/viewer.key", "nats/a.creds"]
    );
    assert_eq!(
        open(&sealed, "password-2").unwrap_err(),
        BackupError::Decrypt
    );
    assert!(BackupError::Decrypt.to_string().contains("wrong password"));
    // A fresh salt and nonce every time.
    assert_ne!(
        seal(&files, "password-1", FAST).unwrap().ciphertext,
        sealed.ciphertext
    );
}

#[test]
fn altered_secrets_do_not_open() {
    let sealed = seal(&secrets(&[("keys/viewer.key", "k1:x")]), "password-1", FAST).unwrap();
    let mut bytes = STANDARD.decode(&sealed.ciphertext).unwrap();
    bytes[0] ^= 1;
    let tampered = Sealed {
        ciphertext: STANDARD.encode(&bytes),
        ..sealed.clone()
    };
    assert_eq!(
        open(&tampered, "password-1").unwrap_err(),
        BackupError::Decrypt
    );
    let short_nonce = Sealed {
        nonce: STANDARD.encode([0u8; 12]),
        ..sealed.clone()
    };
    assert!(matches!(
        open(&short_nonce, "password-1"),
        Err(BackupError::Malformed(_))
    ));
    // The parameters are bound to the key: changing them fails too.
    let other_cost = Sealed {
        t: 2,
        ..sealed.clone()
    };
    assert_eq!(
        open(&other_cost, "password-1").unwrap_err(),
        BackupError::Decrypt
    );
    let greedy = Sealed {
        m: MAX_KDF.m + 1,
        ..sealed
    };
    assert!(matches!(
        open(&greedy, "password-1"),
        Err(BackupError::Malformed(_))
    ));
}

#[test]
fn the_allowlist_takes_keys_and_creds_only() {
    for ok in ["nats/cc-sessions.viewer.creds", "keys/a", "keys/sub/b.nk"] {
        assert!(is_allowed_secret(ok), "{ok}");
    }
    for bad in [
        "key",
        "../x",
        "keys/../key",
        "/key",
        "C:/key",
        r"nats\a.creds",
        "other.txt",
        "nats/.creds",
        "nats/a.txt",
        "nats/sub/a.creds",
        "keys",
        "keys/",
        "./key",
        "",
        "keys/a?b",
    ] {
        assert!(!is_allowed_secret(bad), "{bad}");
    }
}

/// The files of the Publisher role travel too: its private key, the
/// viewer's public key it seals to, and its own creds.
#[test]
fn the_publisher_roles_files_are_secrets_that_travel() {
    for rel in [
        "keys/publisher.key",
        "keys/viewer.pub",
        "nats/cc-sessions.pub-devbox.creds",
    ] {
        assert!(is_allowed_secret(rel), "{rel}");
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("keys")).unwrap();
    std::fs::create_dir_all(d.join("nats")).unwrap();
    std::fs::write(
        d.join("keys").join("publisher.key"),
        format!("p1:{B64_A}\n"),
    )
    .unwrap();
    std::fs::write(d.join("keys").join("viewer.pub"), format!("k1:{B64_B}\n")).unwrap();
    std::fs::write(d.join("nats").join("cc-sessions.pub-devbox.creds"), CREDS).unwrap();
    let files = collect_secrets(d).unwrap();
    assert_eq!(
        files.paths(),
        [
            "keys/publisher.key",
            "keys/viewer.pub",
            "nats/cc-sessions.pub-devbox.creds"
        ]
    );
}

/// A backup that names any other file is refused before anything is
/// written.
#[test]
fn secrets_outside_the_allowlist_are_refused() {
    for bad in ["../x", "/etc/passwd", "other.txt"] {
        let sealed = seal(&secrets(&[("keys/a", "k"), (bad, "x")]), "password-1", FAST).unwrap();
        match open(&sealed, "password-1") {
            Err(BackupError::Malformed(why)) => assert!(why.contains(bad), "{why}"),
            other => panic!("{bad}: {other:?}"),
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let files = secrets(&[("../x", "x")]);
    assert!(restore_secrets(&files, &data).is_err());
    assert!(!dir.path().join("x").exists());
}

#[test]
fn oversized_and_foreign_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.ccbak");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(MAX_BACKUP_BYTES + 1).unwrap();
    drop(f);
    assert!(matches!(read(&big), Err(BackupError::TooLarge(_))));
    assert_eq!(
        parse(b"{\"format\":\"zip\"}").unwrap_err(),
        BackupError::NotABackup
    );
    assert_eq!(parse(b"not json").unwrap_err(), BackupError::NotABackup);
    let newer = br#"{"format":"cc-usage-backup","version":2}"#;
    assert_eq!(parse(newer).unwrap_err(), BackupError::NewerVersion(2));
    let bad_name = br#"{"format":"cc-usage-backup","version":1,"created":"2026-10-14T12:00:00Z","appVersion":"0.1.0","machine":"../x","data":{}}"#;
    assert!(matches!(parse(bad_name), Err(BackupError::Malformed(_))));
    let fine = br#"{"format":"cc-usage-backup","version":1,"created":"2026-10-14T12:00:00Z","appVersion":"0.1.0","machine":"wk-01","data":{}}"#;
    assert_eq!(parse(fine).unwrap().secrets, None);
}

fn backup_with(machine: &str, data: Data) -> Backup {
    Backup {
        format: FORMAT.into(),
        version: VERSION,
        created: "2026-10-14T12:00:00Z".into(),
        app_version: "0.1.0".into(),
        machine: machine.into(),
        data,
        secrets: None,
    }
}

fn activity_data() -> Data {
    Data {
        activity: Some(serde_json::json!({"v": 1, "machines": {
            "": {"2026-10-01": {"t": 100, "h": 9}},
            "devbox": {"2026-10-02": {"t": 50, "h": 3}},
            "pc": {"2026-10-03": {"t": 70, "h": 4}},
        }})),
        ..Data::default()
    }
}

/// Restored from another machine, the backup's own days go under its
/// name, and days it held under this machine's name become this
/// machine's.
#[test]
fn another_machines_days_keep_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(activity::FILE);
    std::fs::write(
        &path,
        r#"{"v":1,"machines":{"":{"2026-10-03":{"t":20,"h":1}}}}"#,
    )
    .unwrap();
    let prepared = prepare(&backup_with("old-viewer", activity_data()), None).unwrap();
    let report = restore(
        &prepared,
        &mut History::default(),
        &RemoteHub::shared(),
        &target(dir.path(), "pc"),
    )
    .unwrap();

    let store = Store::load(&path);
    assert_eq!(store.machines["old-viewer"]["2026-10-01"].t, 100);
    assert_eq!(store.machines["devbox"]["2026-10-02"].t, 50);
    assert_eq!(store.machines[LOCAL]["2026-10-03"].t, 70, "raised");
    assert!(!store.machines[LOCAL].contains_key("2026-10-01"));
    assert!(!store.machines.contains_key("pc"));
    let mine = report.activity.iter().find(|a| a.local).unwrap();
    assert_eq!(
        (mine.machine.as_str(), mine.added, mine.raised),
        ("pc", 0, 1)
    );
    assert_eq!(report.activity.len(), 3);
}

#[test]
fn the_same_machines_days_stay_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let prepared = prepare(&backup_with("pc", activity_data()), None).unwrap();
    restore(
        &prepared,
        &mut History::default(),
        &RemoteHub::shared(),
        &target(dir.path(), "pc"),
    )
    .unwrap();
    let store = Store::load(&dir.path().join(activity::FILE));
    assert_eq!(store.machines[LOCAL].len(), 2, "2026-10-01 and 2026-10-03");
    assert_eq!(store.machines.keys().collect::<Vec<_>>(), ["", "devbox"]);
    // A second restore finds nothing new.
    let again = restore(
        &prepared,
        &mut History::default(),
        &RemoteHub::shared(),
        &target(dir.path(), "pc"),
    )
    .unwrap();
    assert!(again.activity.is_empty());
}

#[test]
fn history_is_merged_into_the_running_one_and_saved() {
    let dir = tempfile::tempdir().unwrap();
    let t = now_ms();
    let mut running = History {
        samples: vec![sample(t - 600_000, 1.0)],
    };
    let data = Data {
        history: Some(serde_json::json!({"samples": [
            {"t": t - 600_000, "v": [{"k": "5-hour session", "p": 9.0}]},
            {"t": t - 300_000, "v": [{"k": "5-hour session", "p": 2.0}]},
        ]})),
        ..Data::default()
    };
    let prepared = prepare(&backup_with("pc", data), None).unwrap();
    let report = restore(
        &prepared,
        &mut running,
        &RemoteHub::shared(),
        &target(dir.path(), "pc"),
    )
    .unwrap();
    assert_eq!(report.history_added, 1);
    assert_eq!(running.samples.len(), 2);
    assert_eq!(running.samples[1].v[0].p, 2.0, "the backup's new sample");
    let saved = History::load_from(&dir.path().join(crate::paths::HISTORY));
    assert_eq!(saved.samples, running.samples);
}

#[test]
fn projects_merge_into_the_hub_and_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(remote::REMOTE_PROJECTS_FILE);
    let now = remote::now_unix();
    let hub = RemoteHub::shared();
    hub.lock().unwrap().keep_projects_in(file.clone(), now);
    let older = project("devbox", "c--app", now - 600);
    remote::restore_projects_of(&hub, vec![older], now);

    let projects = vec![
        project("devbox", "c--app", now - 60),
        project("devbox", "c--web", now - 60),
        project("pc", "c--mine", now - 60),
    ];
    let data = Data {
        remote_projects: Some(serde_json::to_value(&projects).unwrap()),
        ..Data::default()
    };
    let prepared = prepare(&backup_with("old-viewer", data), None).unwrap();
    let report = restore(
        &prepared,
        &mut History::default(),
        &hub,
        &target(dir.path(), "pc"),
    )
    .unwrap();
    assert_eq!(
        (
            report.projects_added,
            report.projects_updated,
            report.projects_skipped
        ),
        (1, 1, 1)
    );
    let saved: Vec<StoredProject> =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved.len(), 2);
    assert!(saved.iter().all(|p| p.heard == now - 60));
}

/// The Publisher role runs no viewer hub: a restore there goes through a
/// hub that holds only the projects file, merged with what the file has,
/// and a later viewer loads them from it.
#[test]
fn a_publishers_restore_keeps_the_projects_for_a_later_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(remote::REMOTE_PROJECTS_FILE);
    let now = remote::now_unix();
    let earlier = vec![project("devbox", "c--app", now - 600)];
    std::fs::write(&file, serde_json::to_string(&earlier).unwrap()).unwrap();

    let projects = vec![
        project("devbox", "c--web", now - 60),
        project("pc", "c--mine", now - 60),
    ];
    let data = Data {
        remote_projects: Some(serde_json::to_value(&projects).unwrap()),
        ..Data::default()
    };
    let prepared = prepare(&backup_with("old-viewer", data), None).unwrap();
    let hub = remote::projects_file_hub(file.clone(), now);
    let report = restore(
        &prepared,
        &mut History::default(),
        &hub,
        &target(dir.path(), "pc"),
    )
    .unwrap();
    assert_eq!((report.projects_added, report.projects_skipped), (1, 1));
    drop(hub);

    let viewer = RemoteHub::shared();
    viewer.lock().unwrap().keep_projects_in(file, now);
    let mut held: Vec<String> = remote::projects_of(&viewer)
        .into_iter()
        .map(|p| p.key)
        .collect();
    held.sort();
    assert_eq!(
        held.len(),
        2,
        "the file's project and the restored one: {held:?}"
    );
}

/// A restored project is cut like one heard live (name, models), and one
/// whose payload names another machine than its key is not restored.
#[test]
fn restored_projects_are_cut_like_live_ones() {
    use cc_core::projects::{ModelTokens, MAX_PROJECT_MODELS, MAX_PROJECT_NAME, OTHER_MODELS};
    let dir = tempfile::tempdir().unwrap();
    let now = remote::now_unix();
    let hub = RemoteHub::shared();
    let mut big = project("devbox", "c--app", now - 60);
    big.payload.name = "x".repeat(500);
    big.payload.models = (0..40)
        .map(|i| ModelTokens {
            model: format!("{i:02}{}", "m".repeat(500)),
            input: 5,
            output: 0,
            cache_creation: 0,
            cache_read: 0,
        })
        .collect();
    let mut forged = project("lab", "c--web", now - 60);
    forged.payload.machine = "devbox".into();
    let data = Data {
        remote_projects: Some(serde_json::to_value(vec![big, forged]).unwrap()),
        ..Data::default()
    };
    let prepared = prepare(&backup_with("old-viewer", data), None).unwrap();
    let report = restore(
        &prepared,
        &mut History::default(),
        &hub,
        &target(dir.path(), "pc"),
    )
    .unwrap();
    assert_eq!((report.projects_added, report.projects_updated), (1, 0));
    let held = remote::projects_of(&hub);
    assert_eq!(held.len(), 1);
    let p = &held[0].payload;
    assert_eq!(p.name.chars().count(), MAX_PROJECT_NAME);
    assert_eq!(p.models.len(), MAX_PROJECT_MODELS + 1);
    assert!(p
        .models
        .iter()
        .all(|m| m.model.chars().count() <= cc_core::session::MAX_REMOTE_MODEL));
    assert_eq!(p.models.last().unwrap().model, OTHER_MODELS);
    assert_eq!(p.tokens().input, 40 * 5, "nothing is lost");
}

#[test]
fn pricing_is_restored_only_where_there_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let data = Data {
        pricing: Some(serde_json::json!({"claude-x": {"input": 1, "output": 2}})),
        ..Data::default()
    };
    let prepared = prepare(&backup_with("pc", data), None).unwrap();
    let hub = RemoteHub::shared();
    let first = restore(
        &prepared,
        &mut History::default(),
        &hub,
        &target(dir.path(), "pc"),
    )
    .unwrap();
    assert_eq!(first.pricing, PricingOutcome::Restored);
    let path = dir.path().join(crate::paths::PRICING);
    assert!(std::fs::read_to_string(&path).unwrap().contains("claude-x"));
    std::fs::write(&path, "{}").unwrap();
    let second = restore(
        &prepared,
        &mut History::default(),
        &hub,
        &target(dir.path(), "pc"),
    )
    .unwrap();
    assert_eq!(second.pricing, PricingOutcome::Kept);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
}

#[test]
fn secrets_are_written_skipped_merged_or_kept() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let pins = d.join("keys").join("publishers");
    std::fs::create_dir_all(&pins).unwrap();
    std::fs::write(pins.join("lab.pub"), format!("p2:{B64_B}\n")).unwrap();
    std::fs::create_dir_all(d.join("nats")).unwrap();
    std::fs::write(d.join("nats").join("same.creds"), "same").unwrap();
    std::fs::write(d.join("nats").join("mine.creds"), "mine").unwrap();
    let pin_a = format!("p1:{B64_A}\n");
    let files = secrets(&[
        ("keys/publishers/lab.pub", pin_a.as_str()),
        ("nats/same.creds", "same"),
        ("nats/mine.creds", "theirs"),
        ("nats/new.creds", "new"),
        ("keys/sub/id.nk", "seed"),
    ]);
    let report = restore_secrets(&files, d).unwrap();
    assert_eq!(report.written, ["keys/sub/id.nk", "nats/new.creds"]);
    assert_eq!(report.merged, ["keys/publishers/lab.pub"]);
    assert_eq!(report.skipped, ["nats/same.creds"]);
    assert_eq!(report.conflicts, ["nats/mine.creds"]);
    assert_eq!(
        std::fs::read_to_string(d.join("nats").join("mine.creds")).unwrap(),
        "mine"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("keys").join("sub").join("id.nk")).unwrap(),
        "seed"
    );
    let merged = cc_envelope::PublicKeys::load(pins.join("lab.pub")).unwrap();
    let kids: Vec<_> = merged.keys().iter().map(|k| k.kid()).collect();
    assert_eq!(kids, ["p2", "p1"], "this machine's newest key stays first");

    // Again: everything is already here.
    let again = restore_secrets(&files, d).unwrap();
    assert_eq!(
        again.skipped,
        [
            "keys/publishers/lab.pub",
            "keys/sub/id.nk",
            "nats/new.creds",
            "nats/same.creds"
        ]
    );
    assert_eq!(again.conflicts, ["nats/mine.creds"]);

    // A key id holding another key is a conflict, and the file is kept.
    std::fs::write(pins.join("lab.pub"), format!("p1:{B64_B}\n")).unwrap();
    let clash =
        restore_secrets(&secrets(&[("keys/publishers/lab.pub", pin_a.as_str())]), d).unwrap();
    assert_eq!(clash.conflicts, ["keys/publishers/lab.pub"]);
    assert_eq!(
        std::fs::read_to_string(pins.join("lab.pub")).unwrap(),
        format!("p1:{B64_B}\n")
    );
}

/// Moving the viewer: its key pair and the pinned publisher keys travel
/// with the secrets, and on a new PC (where the app already made a key
/// pair of its own) they are merged so that values the publishers still
/// seal to the old viewer key open there.
#[test]
fn viewer_keys_and_pins_travel_and_merge_on_a_new_viewer() {
    use cc_envelope::{OpenKeys, PublicKeys, Sealer, SecretKeys, ViewerKeys};
    let old = tempfile::tempdir().unwrap();
    let keys = old.path().join("keys");
    let made = cc_envelope::ensure_viewer_keys(
        &keys.join("viewer.key"),
        &keys.join("viewer.pub"),
        "v-old",
    )
    .unwrap();
    let devbox = SecretKeys::generate("p-devbox").unwrap();
    devbox
        .public_keys()
        .save(keys.join("publishers").join("devbox.pub"))
        .unwrap();
    let files = collect_secrets(old.path()).unwrap();
    assert_eq!(
        files.paths(),
        [
            "keys/publishers/devbox.pub",
            "keys/viewer.key",
            "keys/viewer.pub"
        ]
    );

    let new = tempfile::tempdir().unwrap();
    let nk = new.path().join("keys");
    let own =
        cc_envelope::ensure_viewer_keys(&nk.join("viewer.key"), &nk.join("viewer.pub"), "v-new")
            .unwrap();
    let report = restore_secrets(&files, new.path()).unwrap();
    assert_eq!(report.written, ["keys/publishers/devbox.pub"]);
    assert_eq!(report.merged, ["keys/viewer.key", "keys/viewer.pub"]);
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);

    let merged = SecretKeys::load(nk.join("viewer.key")).unwrap();
    let kids: Vec<_> = merged.keys().iter().map(|k| k.kid()).collect();
    assert_eq!(
        kids,
        ["v-new", "v-old"],
        "this machine's key stays the newest"
    );
    assert_eq!(
        PublicKeys::load(nk.join("viewer.pub")).unwrap(),
        merged.public_keys()
    );
    assert_eq!(
        merged.current().public_key(),
        own.keys.current().public_key()
    );

    // devbox still seals to the old viewer key: the new viewer opens it.
    let sealer = Sealer::new(devbox.clone(), made.keys.public_keys());
    let key = cc_envelope::host_key("devbox");
    let host = cc_envelope::HostPayload {
        machine: "devbox".into(),
        version: "0.1.0".into(),
        os: "linux".into(),
        hb: 1,
    };
    let viewer = ViewerKeys::new(merged, Some(nk.join("publishers")));
    let opened = viewer
        .open_any::<cc_envelope::HostPayload>(&sealer.seal(&host, &key).unwrap(), &key)
        .unwrap();
    assert_eq!(opened.value, host);
    assert!(opened.pinned.is_none(), "devbox came pinned in the backup");

    // Again: nothing left to do.
    let again = restore_secrets(&files, new.path()).unwrap();
    assert_eq!(
        again.skipped,
        [
            "keys/publishers/devbox.pub",
            "keys/viewer.key",
            "keys/viewer.pub"
        ]
    );

    // A pin file with another key under the same key id is a conflict
    // and kept; one with a new key id gains it.
    let other = SecretKeys::generate("p-devbox").unwrap();
    other
        .public_keys()
        .save(nk.join("publishers").join("devbox.pub"))
        .unwrap();
    let clash = restore_secrets(&files, new.path()).unwrap();
    assert_eq!(clash.conflicts, ["keys/publishers/devbox.pub"]);
    let rotated = SecretKeys::generate("p-devbox2").unwrap();
    rotated
        .public_keys()
        .save(nk.join("publishers").join("devbox.pub"))
        .unwrap();
    let gained = restore_secrets(&files, new.path()).unwrap();
    assert_eq!(gained.merged, ["keys/publishers/devbox.pub"]);
    assert_eq!(
        PublicKeys::load(nk.join("publishers").join("devbox.pub"))
            .unwrap()
            .keys()
            .len(),
        2
    );
}

/// Rejected publisher keys travel and merge like pins; the keys waiting
/// for review never do, and a backup naming one is refused.
#[test]
fn rejected_keys_travel_and_waiting_keys_stay_home() {
    let old = tempfile::tempdir().unwrap();
    let pins = old.path().join("keys").join("publishers");
    std::fs::create_dir_all(&pins).unwrap();
    std::fs::write(pins.join("lab.rejected"), format!("p1:{B64_A}\n")).unwrap();
    std::fs::write(pins.join("lab.pending"), "[]").unwrap();
    let files = collect_secrets(old.path()).unwrap();
    assert_eq!(files.paths(), ["keys/publishers/lab.rejected"]);
    assert!(!is_allowed_secret("keys/publishers/lab.pending"));

    let new = tempfile::tempdir().unwrap();
    let report = restore_secrets(&files, new.path()).unwrap();
    assert_eq!(report.written, ["keys/publishers/lab.rejected"]);
    let here = new
        .path()
        .join("keys")
        .join("publishers")
        .join("lab.rejected");
    std::fs::write(&here, format!("p2:{B64_B}\n")).unwrap();
    let merged = restore_secrets(&files, new.path()).unwrap();
    assert_eq!(merged.merged, ["keys/publishers/lab.rejected"]);
    let kids: Vec<String> = cc_envelope::PublicKeys::load(&here)
        .unwrap()
        .keys()
        .iter()
        .map(|k| k.kid().to_string())
        .collect();
    assert_eq!(kids, ["p2", "p1"]);

    let planted = secrets(&[("keys/publishers/lab.pending", "[]")]);
    assert!(matches!(
        restore_secrets(&planted, new.path()),
        Err(BackupError::Malformed(_))
    ));
    assert!(!new
        .path()
        .join("keys")
        .join("publishers")
        .join("lab.pending")
        .exists());
}

/// A wrong password stops the restore before anything is merged; no
/// password restores the data and leaves the secrets out.
#[test]
fn the_password_decides_the_secrets_only() {
    let src = tempfile::tempdir().unwrap();
    populated(src.path());
    let backup = create(
        src.path(),
        "old-viewer",
        Some(("correct horse", FAST)),
        Utc::now(),
    )
    .unwrap()
    .backup;

    let dest = tempfile::tempdir().unwrap();
    assert_eq!(
        prepare(&backup, Some("wrong horse")).unwrap_err(),
        BackupError::Decrypt
    );
    assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none());

    let hub = RemoteHub::shared();
    let withheld = restore(
        &prepare(&backup, Some("")).unwrap(),
        &mut History::default(),
        &hub,
        &target(dest.path(), "pc"),
    )
    .unwrap();
    assert!(withheld.secrets_withheld && withheld.secrets.is_none());
    assert_eq!(withheld.history_added, 1);

    let full = restore(
        &prepare(&backup, Some("correct horse")).unwrap(),
        &mut History::default(),
        &hub,
        &target(dest.path(), "pc"),
    )
    .unwrap();
    let secrets = full.secrets.unwrap();
    assert_eq!(
        secrets.written,
        ["keys/sub/id.nk", "nats/cc-sessions.viewer.creds"]
    );
    assert!(!dest.path().join(crate::paths::LOG).exists());
}

#[test]
fn the_default_file_name_has_the_machine_and_the_minute() {
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-14T09:05:59-03:00").unwrap();
    assert_eq!(
        default_file_name("wk-01", at),
        "cc-usage-backup-wk-01-20261014-0905.ccbak"
    );
}
