//! The NATS project file template, `infra/nats/cc-sessions.example.yaml`,
//! against the code: the bucket's limits are tied to constants of the
//! envelope and the publisher, and drifting apart breaks publishing in ways
//! no other test sees (a value the server refuses as too large, keys that
//! expire between two heartbeats). The viewer's side of the timing (rows
//! offline before their key expires) is checked in the app's remote tests,
//! from the same file.
//!
//! A small line parser reads the file: the template is flat, hand-written
//! YAML, and the workspace has no YAML crate.

use std::path::PathBuf;
use std::time::Duration;

use cc_publish::{planner::HEARTBEAT_SECS, projects::REPUBLISH_EVERY};

fn example_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../infra/nats/cc-sessions.example.yaml")
}

fn example() -> String {
    let path = example_path();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A `key: value` line, its value without a trailing comment or quotes.
fn key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.trim().trim_start_matches("- ").split_once(':')?;
    let value = value.trim();
    let value = if let Some(quoted) = value.strip_prefix('\'') {
        quoted.strip_suffix('\'').unwrap_or(quoted)
    } else if let Some(quoted) = value.strip_prefix('"') {
        quoted.strip_suffix('"').unwrap_or(quoted)
    } else {
        value.split(" #").next().unwrap_or(value).trim()
    };
    Some((key.trim(), value))
}

/// The members of the top-level section `section` (`buckets`, `check`),
/// in order: the lines indented under it, comments and blank lines left
/// out. For a list, every entry's members follow one another.
fn section<'a>(text: &'a str, section: &str) -> Vec<(&'a str, &'a str)> {
    let mut inside = false;
    let mut members = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') {
            inside = line.trim_end() == format!("{section}:");
            continue;
        }
        if inside {
            if let Some(kv) = key_value(line) {
                members.push(kv);
            }
        }
    }
    members
}

/// The members of the first bucket entry of `buckets:`.
fn first_bucket(text: &str) -> Vec<(&str, &str)> {
    let mut entry = Vec::new();
    for (key, value) in section(text, "buckets") {
        if key == "name" && !entry.is_empty() {
            break;
        }
        entry.push((key, value));
    }
    entry
}

fn member<'a>(members: &[(&str, &'a str)], key: &str) -> &'a str {
    members
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("{key} missing from {members:?}"))
}

/// A NATS duration as toolctl writes it: `90s`, `3m`, `1h`, `1d`.
fn duration(text: &str) -> Duration {
    let (number, unit) = text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len()),
    );
    let n: u64 = number
        .parse()
        .unwrap_or_else(|_| panic!("duration {text:?}"));
    let secs = match unit {
        "s" | "" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => panic!("duration unit {text:?}"),
    };
    Duration::from_secs(secs)
}

/// A size: plain bytes, or with `KiB`, `MiB` or `GiB`.
fn size(text: &str) -> u64 {
    let (number, unit) = text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len()),
    );
    let n: u64 = number.parse().unwrap_or_else(|_| panic!("size {text:?}"));
    n * match unit {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        _ => panic!("size unit {text:?}"),
    }
}

#[test]
fn the_parser_reads_what_the_template_writes() {
    assert_eq!(duration("3m"), Duration::from_secs(180));
    assert_eq!(duration("90s"), Duration::from_secs(90));
    assert_eq!(size("16MiB"), 16 * 1024 * 1024);
    assert_eq!(size("4096"), 4096);
    assert_eq!(key_value("    history: 1 # one"), Some(("history", "1")));
    assert_eq!(
        key_value(r#"  probe_value: '{"a":1}'"#),
        Some(("probe_value", r#"{"a":1}"#))
    );
    assert_eq!(key_value("  - name: x"), Some(("name", "x")));
}

#[test]
fn bucket_limits_match_the_code() {
    let text = example();
    let project = text
        .lines()
        .find_map(|l| l.strip_prefix("project:"))
        .map(str::trim)
        .expect("project:");
    assert_eq!(
        project,
        cc_core::PROJECT,
        "users are named <project>.<role>"
    );

    let bucket = first_bucket(&text);
    assert_eq!(member(&bucket, "name"), cc_remote::DEFAULT_BUCKET);

    // Every value is one envelope; the server refuses a larger one.
    let max_value_size = size(member(&bucket, "max_value_size"));
    assert_eq!(max_value_size, cc_envelope::MAX_ENVELOPE_BYTES as u64);
    assert!(size(member(&bucket, "max_bytes")) >= 1024 * max_value_size);

    // Only the latest value of a key matters, and a replay must not find
    // an older one.
    assert_eq!(member(&bucket, "history"), "1");

    // A live session or machine is re-put every heartbeat, its projects
    // every REPUBLISH_EVERY: its keys must outlive at least two missed
    // heartbeats' worth of delay, so one slow pass never empties the
    // bucket of a machine that is still there.
    let max_age = duration(member(&bucket, "max_age"));
    let heartbeat = Duration::from_secs_f64(HEARTBEAT_SECS);
    assert!(
        max_age >= heartbeat * 3,
        "max_age {max_age:?} must cover three heartbeats of {heartbeat:?}"
    );
    assert!(
        max_age >= REPUBLISH_EVERY + heartbeat,
        "max_age {max_age:?} must outlive the project re-put every {REPUBLISH_EVERY:?}"
    );
}

/// The probe `toolctl check` writes goes under a key the viewer does not
/// open: `toolctl-probe` is no session id (8 hex characters), so the
/// viewer logs it once as "not a cc-sessions key" and ignores its value,
/// whatever it holds. The value claims no envelope version either.
#[test]
fn the_check_probe_is_ignored_by_the_viewer() {
    let text = example();
    let check = section(&text, "check");
    let key = member(&check, "probe_key").replace("{alias}", "test");
    assert!(cc_envelope::KvKey::parse(&key).is_none(), "{key}");
    let value = member(&check, "probe_value");
    let json: serde_json::Value = serde_json::from_str(value).expect("the probe is JSON");
    assert!(json.get("v").is_none(), "no envelope version in {value}");
    assert!(value.len() as u64 <= size(member(&first_bucket(&text), "max_value_size")));
}
