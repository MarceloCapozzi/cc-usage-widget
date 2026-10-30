//! What Claude Code's status line command leaves on disk: the only place the
//! context window percentage is known.
//!
//! `cc-hook statusline` writes one file per session,
//! `<data_dir>/live/<session_id>.json` (`cc_hook::statusline::LiveExport`,
//! camelCase keys), read by [`read_sessions_from`] as [`SessionLive`]. The
//! reader is lenient: it needs `sessionId` and takes the rest when present.
//!
//! It is live data: a file only means something while Claude Code keeps
//! rewriting it, so the reader filters by age. Times are Unix seconds.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;

/// A per-session file is used while it is at most this old (seconds). Claude
/// Code re-renders the status line on every assistant message, so an idle
/// session keeps its last value for a while, then the row stops claiming it.
pub const SESSION_FRESHNESS_SECS: f64 = 10.0 * 60.0;

/// Per-session files older than this (seconds) are deleted, whatever their
/// session.
pub const SESSION_MAX_AGE_SECS: f64 = 24.0 * 3600.0;

/// Files larger than this are skipped unread (real ones are well under 1 KiB).
const MAX_FILE_BYTES: u64 = 64 * 1024;

// ---------------------------------------------------------------------------
// Per-session files from `cc-hook statusline`
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionLiveFile {
    ts: Option<f64>,
    session_id: Option<String>,
    context_used_percent: Option<f64>,
    context_window_size: Option<u64>,
    context_used_tokens: Option<u64>,
    model_id: Option<String>,
    model_display_name: Option<String>,
    five_hour_percent: Option<f64>,
    seven_day_percent: Option<f64>,
    cost_usd: Option<f64>,
}

/// One session's status line data.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionLive {
    /// The Claude Code session id (the file name).
    pub session_id: String,
    /// Unix seconds when the file was written.
    pub written_at: f64,
    /// `context_window.used_percentage`, as Claude Code computed it, 0 to
    /// 100. `None` early in a session.
    pub context_percent: Option<f64>,
    /// The context window size, tokens.
    pub context_window_size: Option<u64>,
    /// Tokens of the context window in use.
    pub context_used_tokens: Option<u64>,
    /// The model id in use now (it follows `/model`).
    pub model_id: Option<String>,
    /// The model's display name.
    pub model_display_name: Option<String>,
    /// The 5-hour limit, percent used.
    pub five_hour_pct: Option<f64>,
    /// The weekly limit, percent used.
    pub seven_day_pct: Option<f64>,
    /// The session's cost so far, USD.
    pub cost_usd: Option<f64>,
}

impl SessionLive {
    /// The context percentage rounded for a row or a payload, 0 to 100.
    pub fn percent_u8(&self) -> Option<u8> {
        self.context_percent.map(round_percent)
    }

    /// The model to show: the id, else the display name.
    pub fn model(&self) -> Option<&str> {
        self.model_id
            .as_deref()
            .or(self.model_display_name.as_deref())
    }
}

/// A percentage as the integer a row shows, clamped to 0..=100.
pub fn round_percent(p: f64) -> u8 {
    p.round().clamp(0.0, 100.0) as u8
}

/// Whether a session id is one `cc-hook statusline` would have written:
/// `[A-Za-z0-9._-]`, 1 to 128 characters, not starting with a dot.
pub fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A file's modification time as Unix seconds.
fn mtime_secs(meta: &std::fs::Metadata) -> Option<f64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

/// `<id>.json` files with a valid id: id, path, modification time, size.
fn entries(dir: &Path) -> Vec<(String, PathBuf, Option<f64>, u64)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".json")?;
            if !valid_session_id(id) {
                return None;
            }
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            Some((id.to_string(), e.path(), mtime_secs(&meta), meta.len()))
        })
        .collect()
}

/// Per-session files in `dir` written within [`SESSION_FRESHNESS_SECS`] of
/// `now`. Files already too old by modification time, or too large, are not
/// opened; files that do not parse, or whose `sessionId` does not match the
/// name, are skipped. A missing folder is just empty.
pub fn read_sessions_from(dir: &Path, now: f64) -> Vec<SessionLive> {
    let fresh = |t: f64| now - t <= SESSION_FRESHNESS_SECS;
    entries(dir)
        .into_iter()
        .filter(|(_, _, mtime, len)| mtime.is_none_or(fresh) && *len <= MAX_FILE_BYTES)
        .filter_map(|(id, path, mtime, _)| {
            let raw = std::fs::read(&path).ok()?;
            let f: SessionLiveFile = serde_json::from_slice(&raw).ok()?;
            if f.session_id.as_deref() != Some(id.as_str()) {
                return None;
            }
            let written_at = f.ts.filter(|t| t.is_finite()).or(mtime)?;
            if !fresh(written_at) {
                return None;
            }
            let pct = |p: Option<f64>| p.filter(|p| p.is_finite() && *p >= 0.0);
            Some(SessionLive {
                session_id: id,
                written_at,
                context_percent: pct(f.context_used_percent).map(|p| p.min(100.0)),
                context_window_size: f.context_window_size,
                context_used_tokens: f.context_used_tokens,
                model_id: f.model_id,
                model_display_name: f.model_display_name,
                five_hour_pct: pct(f.five_hour_percent),
                seven_day_pct: pct(f.seven_day_percent),
                cost_usd: f.cost_usd,
            })
        })
        .collect()
}

/// Deletes the per-session files of `ended` sessions and every per-session
/// file older than [`SESSION_MAX_AGE_SECS`] at `now`. Other files in `dir`
/// are left alone. Returns how many were deleted.
pub fn prune(dir: &Path, ended: &[String], now: f64) -> usize {
    entries(dir)
        .into_iter()
        .filter(|(id, _, mtime, _)| {
            ended.contains(id) || mtime.is_some_and(|t| now - t > SESSION_MAX_AGE_SECS)
        })
        .filter(|(_, path, _, _)| std::fs::remove_file(path).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::now_secs;

    /// Exactly what `cc-hook statusline` writes (`LiveExport`), nulls included.
    fn cc_hook_file(id: &str, ts: f64, pct: &str) -> String {
        format!(
            r#"{{"v":1,"ts":{ts},"sessionId":"{id}","contextUsedPercent":{pct},"contextWindowSize":1000000,
                "contextUsedTokens":425000,"modelId":"claude-opus-5-5","modelDisplayName":"Opus",
                "fiveHourPercent":23.5,"fiveHourResetsAt":1791900000,"sevenDayPercent":null,
                "sevenDayResetsAt":null,"costUsd":0.5,"cwd":"C:/w","transcriptPath":"C:/t.jsonl"}}"#
        )
    }

    #[test]
    fn per_session_files_are_read_with_exact_percentages() {
        let dir = tempfile::tempdir().unwrap();
        let now = now_secs();
        std::fs::write(
            dir.path().join("s-1.json"),
            cc_hook_file("s-1", now, "42.5"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("s-2.json"),
            cc_hook_file("s-2", now - 1.0, "null"),
        )
        .unwrap();
        // Ignored: stale by its own timestamp, a temp file, a name that is not
        // its sessionId, garbage, and a name cc-hook would never write.
        std::fs::write(
            dir.path().join("old.json"),
            cc_hook_file("old", now - 3600.0, "10"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".s-3.json.tmp-1"),
            cc_hook_file("s-3", now, "10"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("liar.json"),
            cc_hook_file("other", now, "10"),
        )
        .unwrap();
        std::fs::write(dir.path().join("bad.json"), "{ nope").unwrap();
        std::fs::write(dir.path().join("a b.json"), cc_hook_file("a b", now, "10")).unwrap();

        let mut got = read_sessions_from(dir.path(), now_secs());
        got.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        let ids: Vec<&str> = got.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, ["s-1", "s-2"]);
        assert_eq!(got[0].context_percent, Some(42.5));
        assert_eq!(got[0].context_window_size, Some(1_000_000));
        assert_eq!(got[0].context_used_tokens, Some(425_000));
        assert_eq!(got[0].model_id.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(got[0].model_display_name.as_deref(), Some("Opus"));
        assert_eq!(got[0].five_hour_pct, Some(23.5));
        assert_eq!(got[0].seven_day_pct, None);
        assert_eq!(got[0].cost_usd, Some(0.5));
        assert!((got[0].written_at - now).abs() < 0.01);
        assert_eq!(got[1].context_percent, None, "null early in a session");
    }

    #[test]
    fn reads_fresh_files_and_skips_bad_ones() {
        let dir = tempfile::tempdir().unwrap();
        let now = now_secs();
        let write = |name: &str, body: String| std::fs::write(dir.path().join(name), body).unwrap();
        write(
            "s1.json",
            format!(
                r#"{{"ts":{now},"sessionId":"s1","contextUsedPercent":41.6,"modelId":"claude-opus-5-5","extra":1}}"#
            ),
        );
        write(
            "old.json",
            format!(
                r#"{{"ts":{},"sessionId":"old","contextUsedPercent":5}}"#,
                now - 3600.0
            ),
        );
        write(
            "wrong.json",
            format!(r#"{{"ts":{now},"sessionId":"other"}}"#),
        );
        write("broken.json", "{".into());
        write(
            ".hidden.json",
            format!(r#"{{"ts":{now},"sessionId":".hidden"}}"#),
        );
        write(
            "s2.json",
            format!(
                r#"{{"ts":{now},"sessionId":"s2","modelDisplayName":"Opus","contextUsedPercent":250}}"#
            ),
        );
        write(
            "big.json",
            format!(
                r#"{{"ts":{now},"sessionId":"big","pad":"{}"}}"#,
                "x".repeat(70_000)
            ),
        );

        let mut got = read_sessions_from(dir.path(), now);
        got.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].session_id, "s1");
        assert_eq!(got[0].percent_u8(), Some(42));
        assert_eq!(got[0].model(), Some("claude-opus-5-5"));
        assert_eq!(got[1].percent_u8(), Some(100), "clamped");
        assert_eq!(got[1].model(), Some("Opus"));
    }

    #[test]
    fn a_missing_live_folder_is_just_empty() {
        assert!(read_sessions_from(Path::new("Z:/does/not/exist"), now_secs()).is_empty());
    }

    #[test]
    fn pruning_drops_ended_sessions_and_day_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let now = now_secs();
        for id in ["ended", "alive", "ancient"] {
            std::fs::write(
                dir.path().join(format!("{id}.json")),
                cc_hook_file(id, now, "1"),
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "keep").unwrap();
        let n = prune(dir.path(), &["ended".to_string()], now_secs());
        assert_eq!(n, 1);
        assert!(!dir.path().join("ended.json").exists());
        // A day later by the clock, everything old goes, other files stay.
        let later = now_secs() + SESSION_MAX_AGE_SECS + 60.0;
        assert_eq!(prune(dir.path(), &[], later), 2);
        assert!(dir.path().join("notes.txt").exists());
    }

    #[test]
    fn prune_removes_ended_sessions_only() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["a", "b"] {
            std::fs::write(dir.path().join(format!("{id}.json")), "{}").unwrap();
        }
        assert_eq!(prune(dir.path(), &["a".to_string()], now_secs()), 1);
        assert!(!dir.path().join("a.json").exists());
        assert!(dir.path().join("b.json").exists());
        assert_eq!(
            prune(dir.path(), &[], now_secs() + 2.0 * SESSION_MAX_AGE_SECS),
            1
        );
    }

    #[test]
    fn session_ids_match_what_cc_hook_writes() {
        for good in [
            "abc",
            "0b3c-4d5e",
            "A.b_c-1",
            "0f8e7c1a-1234-4bcd-9abc-1234567890ab",
        ] {
            assert!(valid_session_id(good));
        }
        for bad in ["", ".x", "a/b", "a b", "..", "é", "../x"] {
            assert!(!valid_session_id(bad), "{bad:?}");
        }
    }
}
