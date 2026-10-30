//! Tests for the hook and statusline installer: the settings.json edits,
//! the commands it writes and how they are quoted, and the safe rewrites of
//! the file (backups, concurrent changes, symlinks, indentation).

use super::*;
use crate::testutil::TestDir;
use serde_json::json;

const CMD: &str = "\"C:/Users/x/AppData/Local/cc-usage/bin/cc-hook.exe\"";

#[test]
fn install_into_empty_document_adds_every_event_in_order() {
    let mut doc = json!({});
    let r = apply_install(&mut doc, CMD).unwrap();
    assert!(r.actions.iter().all(|(_, a)| *a == Action::Added));
    let keys: Vec<&str> = doc["hooks"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, HOOK_EVENTS);
    assert_eq!(
        doc["hooks"]["Stop"],
        json!([{ "hooks": [{ "type": "command", "command": CMD }] }])
    );
    verify_installed(&doc, CMD).unwrap();
}

#[test]
fn install_is_idempotent_and_updates_in_place() {
    let mut doc = json!({});
    apply_install(&mut doc, CMD).unwrap();
    let once = doc.clone();
    let r = apply_install(&mut doc, CMD).unwrap();
    assert!(!r.changed());
    assert_eq!(doc, once);

    let other = "\"D:/tools/cc-hook.exe\"";
    let r = apply_install(&mut doc, other).unwrap();
    assert!(r.actions.iter().all(|(_, a)| *a == Action::Updated));
    verify_installed(&doc, other).unwrap();
    assert_eq!(doc["hooks"]["Stop"].as_array().unwrap().len(), 1);
}

#[test]
fn install_collapses_duplicates_and_drops_stale_events() {
    let mut doc = json!({ "hooks": {
        "Stop": [
            { "hooks": [{ "type": "command", "command": "cc-hook" }, { "type": "command", "command": "keep-me" }] },
            { "hooks": [{ "type": "command", "command": "old/cc-hook.exe" }] }
        ],
        "PostToolUse": [{ "hooks": [{ "type": "command", "command": "cc-hook" }] }]
    }});
    let r = apply_install(&mut doc, CMD).unwrap();
    assert!(r.actions.contains(&("Stop".into(), Action::Updated)));
    assert!(r.actions.contains(&("PostToolUse".into(), Action::Removed)));
    assert_eq!(
        doc["hooks"]["Stop"],
        json!([{ "hooks": [{ "type": "command", "command": CMD }, { "type": "command", "command": "keep-me" }] }])
    );
    assert!(doc["hooks"].get("PostToolUse").is_none());
    verify_installed(&doc, CMD).unwrap();
}

#[test]
fn install_rejects_unrecognisable_commands_and_bad_shapes() {
    assert!(matches!(
        apply_install(&mut json!({}), "my-hook.exe"),
        Err(Error::BadCommand(_))
    ));
    assert!(matches!(
        apply_install(&mut json!([]), CMD),
        Err(Error::Shape(_))
    ));
    assert!(matches!(
        apply_install(&mut json!({"hooks": []}), CMD),
        Err(Error::Shape(_))
    ));
    assert!(matches!(
        apply_install(&mut json!({"hooks": {"Stop": {}}}), CMD),
        Err(Error::Shape(_))
    ));
}

#[test]
fn uninstall_keeps_other_tools_and_shared_groups() {
    let mut doc = json!({ "hooks": {
        "Stop": [{ "matcher": "", "hooks": [{ "type": "command", "command": CMD }, { "type": "command", "command": "other" }] }],
        "Notification": [{ "hooks": [{ "type": "command", "command": CMD }] }],
        "Weird": [{ "hooks": [] }, { "nohooks": true }]
    }});
    let r = apply_uninstall(&mut doc).unwrap();
    assert_eq!(r.actions.len(), 2);
    assert_eq!(
        doc,
        json!({ "hooks": {
            "Stop": [{ "matcher": "", "hooks": [{ "type": "command", "command": "other" }] }],
            "Weird": [{ "hooks": [] }, { "nohooks": true }]
        }})
    );
    verify_uninstalled(&doc).unwrap();
    assert!(!apply_uninstall(&mut doc).unwrap().changed());
}

#[test]
fn uninstall_keeps_an_empty_hooks_object_it_did_not_empty() {
    let mut doc = json!({ "hooks": {}, "model": "opus" });
    apply_uninstall(&mut doc).unwrap();
    assert_eq!(doc, json!({ "hooks": {}, "model": "opus" }));
    let mut doc = json!({ "model": "opus" });
    apply_install(&mut doc, CMD).unwrap();
    apply_uninstall(&mut doc).unwrap();
    assert_eq!(doc, json!({ "model": "opus" }));
}

#[test]
fn status_lists_expected_and_stray_events() {
    let doc = json!({ "hooks": {
        "Stop": [{ "hooks": [{ "type": "command", "command": CMD }] }],
        "PostToolUse": [{ "hooks": [{ "type": "command", "command": CMD }] }],
        "Other": [{ "hooks": [{ "type": "command", "command": "x" }] }]
    }});
    let s = hook_status(&doc);
    assert_eq!(s.len(), HOOK_EVENTS.len() + 1);
    assert_eq!(
        s.iter().find(|e| e.event == "Stop").unwrap().commands,
        vec![CMD.to_string()]
    );
    assert!(s
        .iter()
        .find(|e| e.event == "SessionStart")
        .unwrap()
        .commands
        .is_empty());
    let stray = s.last().unwrap();
    assert_eq!(
        (stray.event.as_str(), stray.expected),
        ("PostToolUse", false)
    );
    assert!(verify_installed(&doc, CMD).is_err());
}

#[test]
fn command_for_exe_quotes_for_the_platform_shell() {
    if cfg!(windows) {
        assert_eq!(
            command_for_exe(Path::new(
                r"C:\Users\x\AppData\Local\cc-usage\bin\cc-hook.exe"
            ))
            .unwrap(),
            CMD
        );
        assert_eq!(
            command_for_exe(Path::new(r"\\?\C:\a\cc-hook.exe")).unwrap(),
            "\"C:/a/cc-hook.exe\""
        );
        assert!(matches!(
            command_for_exe(Path::new(r"C:\100%\cc-hook.exe")),
            Err(Error::UnsafePath { .. })
        ));
    } else {
        assert_eq!(
            command_for_exe(Path::new("/usr/local/bin/cc-hook")).unwrap(),
            "'/usr/local/bin/cc-hook'"
        );
        assert_eq!(
            command_for_exe(Path::new("/opt/$x/cc-hook")).unwrap(),
            "'/opt/$x/cc-hook'"
        );
    }
}

#[test]
fn posix_quoting_survives_any_character_but_control_ones() {
    assert_eq!(
        quote_posix("/home/a b/cc-hook").unwrap(),
        "'/home/a b/cc-hook'"
    );
    let odd = quote_posix("/tmp/it's $HOME `x` \"q\"/cc-hook").unwrap();
    assert_eq!(odd, r#"'/tmp/it'\''s $HOME `x` "q"/cc-hook'"#);
    assert!(is_cc_hook_command(&odd));
    assert_eq!(
        crate::shell::program_of(&odd),
        "/tmp/it's $HOME `x` \"q\"/cc-hook"
    );
    assert!(matches!(
        quote_posix("/tmp/a\nb"),
        Err(Error::UnsafePath { .. })
    ));
}

#[test]
fn windows_quoting_refuses_what_it_cannot_carry() {
    assert_eq!(
        quote_windows("C:/Users/O'Brien/a b/cc-hook.exe").unwrap(),
        "\"C:/Users/O'Brien/a b/cc-hook.exe\""
    );
    for bad in [
        "C:/a\"b/cc-hook.exe",
        "C:/50%/cc-hook.exe",
        "C:/$x/cc-hook.exe",
        "C:/a`b/cc-hook.exe",
        "C:/a\tb",
    ] {
        let err = quote_windows(bad).unwrap_err();
        assert!(matches!(err, Error::UnsafePath { .. }), "{bad}");
        assert!(err.to_string().contains("safely"), "{err}");
    }
    assert!(is_cc_hook_command(
        &quote_windows("C:/Users/O'Brien/cc-hook.exe").unwrap()
    ));
}

fn temp_dir(tag: &str) -> TestDir {
    TestDir::new(&format!("install-{tag}"))
}

fn backups(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("settings.json.bak-cc-hook-"))
        .collect();
    v.sort();
    v
}

#[test]
fn only_the_newest_backups_are_kept() {
    let dir = temp_dir("backups");
    let path = dir.join("settings.json");
    std::fs::write(&path, b"{}").unwrap();
    // Older backups, including an unrelated file that must survive.
    for i in 0..7 {
        std::fs::write(
            dir.join(format!("settings.json.bak-cc-hook-2020010{i}-000000")),
            b"{}",
        )
        .unwrap();
    }
    std::fs::write(dir.join("other.json.bak-cc-hook-20200101-000000"), b"{}").unwrap();
    let newest = write_backup(&path, b"{\"a\":1}").unwrap();
    let left = backups(&dir);
    assert_eq!(left.len(), MAX_BACKUPS, "{left:?}");
    assert!(left.contains(&newest.file_name().unwrap().to_string_lossy().into_owned()));
    assert!(!left.contains(&"settings.json.bak-cc-hook-20200100-000000".to_string()));
    assert!(dir.join("other.json.bak-cc-hook-20200101-000000").exists());
}

#[test]
fn a_file_changed_since_it_was_read_is_left_alone() {
    let dir = temp_dir("changed");
    let path = dir.join("settings.json");
    std::fs::write(&path, b"{\"theirs\":true}").unwrap();
    let err = replace_file(&path, b"{}", Some(Some(b"{\"mine\":1}"))).unwrap_err();
    assert!(matches!(err, Error::Changed(_)), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"theirs\":true}");
    // Created meanwhile, where none was read.
    assert!(matches!(
        replace_file(&path, b"{}", Some(None)),
        Err(Error::Changed(_))
    ));
    // Only the settings file is in the folder: no temp file left behind.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    replace_file(&path, b"{}", Some(Some(b"{\"theirs\":true}"))).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{}");
}

#[cfg(unix)]
#[test]
fn rewrites_keep_the_mode_and_write_through_symlinks() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("unix");
    let real = dir.join("real-settings.json");
    std::fs::write(&real, b"{}\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = dir.join("settings.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let out = install_file(&link, "'/x/cc-hook'").unwrap();
    assert!(out.written);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "still a link"
    );
    assert!(std::fs::read_to_string(&real).unwrap().contains("cc-hook"));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&real), 0o600);
    assert_eq!(mode(&out.backup.unwrap()), 0o600);
}

#[test]
fn utc_timestamps() {
    assert_eq!(timestamp_utc(0), "19700101-000000");
    assert_eq!(timestamp_utc(951_782_400), "20000229-000000");
    assert_eq!(timestamp_utc(1_791_856_373), "20261013-015253");
}

#[test]
fn only_the_cc_hook_executable_is_ours() {
    for ours in [
        CMD,
        "cc-hook",
        "old/cc-hook.exe",
        "\"D:\\a b\\CC-HOOK.EXE\" --x",
        "  /usr/bin/cc-hook",
        "'/home/u/.local/bin/cc-hook' statusline",
        "& \"C:/x/cc-hook.exe\"",
    ] {
        assert!(is_cc_hook_command(ours), "{ours}");
    }
    for theirs in [
        "/opt/my-cc-hooks/notify.sh",
        "node \"C:/tools/cc-hook-relay.js\"",
        "bash -c 'echo cc-hook'",
        "\"C:/cc-hook/notify.exe\"",
        "cc-hooker",
    ] {
        assert!(!is_cc_hook_command(theirs), "{theirs}");
    }
}

#[test]
fn install_and_uninstall_leave_foreign_commands_mentioning_cc_hook_alone() {
    let foreign = json!({ "type": "command", "command": "/opt/my-cc-hooks/notify.sh" });
    let original = json!({ "hooks": { "Stop": [{ "hooks": [foreign.clone()] }] } });
    let mut doc = original.clone();
    apply_install(&mut doc, CMD).unwrap();
    assert_eq!(doc["hooks"]["Stop"][0], json!({ "hooks": [foreign] }));
    verify_installed(&doc, CMD).unwrap();
    apply_uninstall(&mut doc).unwrap();
    assert_eq!(doc, original);
}

const SL: &str = "\"C:/Users/x/AppData/Local/cc-usage/bin/cc-hook.exe\" statusline";

fn user_shape() -> Value {
    json!({
        "hooks": {},
        "statusLine": { "type": "command", "command": "node \"C:\\Users\\me\\.claude\\statusline.js\"", "padding": 0 },
        "model": "opus"
    })
}

#[test]
fn statusline_install_keeps_other_keys_and_records_the_original_verbatim() {
    let mut doc = user_shape();
    let before = doc.clone();
    let (action, record) = apply_statusline_install(&mut doc, SL, None, "s.json", 7).unwrap();
    assert_eq!(action, Action::Added);
    let record = record.unwrap();
    assert!(record.had_status_line);
    assert_eq!(record.original, before["statusLine"]);
    assert_eq!(
        record.command(),
        Some("node \"C:\\Users\\me\\.claude\\statusline.js\"")
    );
    assert_eq!(
        doc["statusLine"],
        json!({ "type": "command", "command": SL, "padding": 0 })
    );
    let order: Vec<&String> = doc["statusLine"].as_object().unwrap().keys().collect();
    assert_eq!(order, ["type", "command", "padding"]);
    let top: Vec<&String> = doc.as_object().unwrap().keys().collect();
    assert_eq!(top, ["hooks", "statusLine", "model"]);
    verify_statusline_installed(&doc, SL).unwrap();

    assert_eq!(
        apply_statusline_uninstall(&mut doc, Some(&record)).unwrap(),
        Action::Removed
    );
    assert_eq!(doc, before);
    verify_statusline_uninstalled(&doc).unwrap();
}

#[test]
fn statusline_install_is_idempotent_and_follows_the_executable() {
    let mut doc = user_shape();
    let (_, record) = apply_statusline_install(&mut doc, SL, None, "s.json", 7).unwrap();
    let once = doc.clone();
    let (action, again) =
        apply_statusline_install(&mut doc, SL, record.as_ref(), "s.json", 8).unwrap();
    assert_eq!((action, again), (Action::Unchanged, None));
    assert_eq!(doc, once);

    let moved = "\"D:/tools/cc-hook.exe\" statusline";
    let (action, again) =
        apply_statusline_install(&mut doc, moved, record.as_ref(), "s.json", 9).unwrap();
    assert_eq!(
        (action, again),
        (Action::Updated, None),
        "the original record is kept"
    );
    assert_eq!(doc["statusLine"]["command"], moved);
    assert_eq!(doc["statusLine"]["padding"], 0);

    // Ours but the record was lost: record "nothing to chain" once.
    let (action, rec) = apply_statusline_install(&mut doc, moved, None, "s.json", 9).unwrap();
    assert_eq!(action, Action::Unchanged);
    assert!(!rec.unwrap().had_status_line);
}

#[test]
fn without_a_statusline_ours_is_added_and_removed_again() {
    let mut doc = json!({ "model": "opus" });
    let (action, record) = apply_statusline_install(&mut doc, SL, None, "s.json", 1).unwrap();
    assert_eq!(action, Action::Added);
    let record = record.unwrap();
    assert!(!record.had_status_line && record.command().is_none());
    assert_eq!(
        doc,
        json!({ "model": "opus", "statusLine": { "type": "command", "command": SL } })
    );
    apply_statusline_uninstall(&mut doc, Some(&record)).unwrap();
    assert_eq!(doc, json!({ "model": "opus" }));
}

#[test]
fn a_statusline_the_user_changed_since_install_is_left_alone() {
    let mut doc = user_shape();
    let (_, record) = apply_statusline_install(&mut doc, SL, None, "s.json", 1).unwrap();
    doc["statusLine"]["command"] = json!("bash ~/.claude/new-line.sh");
    let changed = doc.clone();
    assert_eq!(
        apply_statusline_uninstall(&mut doc, record.as_ref()).unwrap(),
        Action::Unchanged
    );
    assert_eq!(doc, changed);
    // Reinstalling records the new one.
    let (action, rec) =
        apply_statusline_install(&mut doc, SL, record.as_ref(), "s.json", 2).unwrap();
    assert_eq!(action, Action::Added);
    assert_eq!(rec.unwrap().command(), Some("bash ~/.claude/new-line.sh"));
}

#[test]
fn odd_statusline_values_round_trip_and_bad_commands_are_refused() {
    for odd in [
        json!(null),
        json!("echo hi"),
        json!({ "type": "static", "text": "x" }),
    ] {
        let mut doc = json!({ "statusLine": odd.clone(), "z": 1 });
        let before = doc.clone();
        let (_, record) = apply_statusline_install(&mut doc, SL, None, "s.json", 1).unwrap();
        assert_eq!(doc["statusLine"]["command"], SL);
        assert_eq!(doc["statusLine"]["type"], "command");
        apply_statusline_uninstall(&mut doc, record.as_ref()).unwrap();
        assert_eq!(doc, before, "{odd}");
    }
    assert!(matches!(
        apply_statusline_install(&mut json!({}), "node x.js statusline", None, "s", 1),
        Err(Error::BadCommand(_))
    ));
    assert!(matches!(
        apply_statusline_install(&mut json!([]), SL, None, "s", 1),
        Err(Error::Shape(_))
    ));
    // No record and ours: removed.
    let mut doc = json!({ "statusLine": { "type": "command", "command": SL } });
    assert_eq!(
        apply_statusline_uninstall(&mut doc, None).unwrap(),
        Action::Removed
    );
    assert_eq!(doc, json!({}));
}

#[test]
fn statusline_commands_name_non_default_settings() {
    assert_eq!(
        statusline_command("\"C:/a/cc-hook.exe\"", None).unwrap(),
        "\"C:/a/cc-hook.exe\" statusline"
    );
    if cfg!(windows) {
        assert_eq!(
            statusline_command(
                "\"C:/a/cc-hook.exe\"",
                Some(Path::new(r"C:\tmp\x y\settings.json"))
            )
            .unwrap(),
            "\"C:/a/cc-hook.exe\" statusline --settings \"C:/tmp/x y/settings.json\""
        );
        let odd = statusline_command("cc-hook", Some(Path::new(r"C:\tmp\it's\s.json"))).unwrap();
        assert_eq!(odd, "cc-hook statusline --settings \"C:/tmp/it's/s.json\"");
        assert!(statusline_command("cc-hook", Some(Path::new(r"C:\$x\s.json"))).is_err());
    } else {
        let odd = statusline_command("cc-hook", Some(Path::new("/tmp/it's/s.json"))).unwrap();
        assert_eq!(odd, r"cc-hook statusline --settings '/tmp/it'\''s/s.json'");
    }
    assert!(is_cc_hook_command(&statusline_command(CMD, None).unwrap()));
}

#[test]
fn indentation_detection() {
    assert_eq!(detect_indent("{\n    \"a\": 1\n}"), "    ");
    assert_eq!(detect_indent("{\n\t\"a\": 1\n}"), "\t");
    assert_eq!(detect_indent("{}"), "  ");
}
