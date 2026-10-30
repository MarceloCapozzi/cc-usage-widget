//! The status line data for the footer: the context window block and the
//! 5h/7d fallback.
//!
//! # Why this module exists
//!
//! The **context window** (the mockup's third block: `330K / 1.0M`, `42% until
//! compact`) is not exposed by any endpoint. It only arrives on stdin to the
//! status line command, which Claude Code runs on every render.
//! `cc-hook statusline` writes what it receives to one file per session,
//! `<data_dir>/live/<session_id>.json` ([`SessionLive`]), and the widget
//! reads them. The reader lives in [`cc_local::live`], shared with the
//! session monitor (which feeds each session's file to its row) and with
//! `cc-publish`.
//!
//! [`read`] takes the newest per-session file as a [`LiveSnapshot`], the
//! shape the frontend receives.
//!
//! # It is live data, not persistent data
//!
//! A file is only updated while Claude Code runs. If it is old, the context
//! window belongs to a finished session and showing it as current is
//! misleading. That is why the snapshot carries its age and `is_fresh()`
//! decides; the UI hides the block when it is not fresh, instead of freezing
//! a number.
//!
//! The 5h/7d limits the hook also carries are a fallback: if the API fails but
//! Claude Code is running, they keep the widget from showing nothing.

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use std::path::PathBuf;

pub use cc_local::live::SessionLive;

/// How long a snapshot is valid before it is considered to come from a
/// finished session. The status line is redrawn on every turn, so a file
/// older than 5 min almost always means nobody is working.
pub const FRESHNESS: chrono::Duration = chrono::Duration::minutes(5);

#[derive(Debug, Clone, Serialize)]
pub struct ContextWindow {
    pub percent: f64,
    pub used_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub model: Option<String>,
}

impl ContextWindow {
    /// "330K / 1.0M", as in the mockup.
    pub fn tokens_label(&self) -> Option<String> {
        let (used, total) = (self.used_tokens?, self.total_tokens?);
        Some(format!("{} / {}", compact(used), compact(total)))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveSnapshot {
    pub written_at: DateTime<Utc>,
    pub context: Option<ContextWindow>,
    pub five_hour_pct: Option<f64>,
    pub seven_day_pct: Option<f64>,
    pub cost_usd: Option<f64>,
    pub session_id: Option<String>,
}

impl LiveSnapshot {
    pub fn age(&self) -> chrono::Duration {
        Utc::now() - self.written_at
    }

    /// Is Claude Code running right now? If not, the context window belongs to
    /// an old session and must not be shown.
    pub fn is_fresh(&self) -> bool {
        self.age() < FRESHNESS
    }
}

/// Unix seconds as a UTC time (the epoch when out of range).
fn utc(secs: f64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt((secs * 1000.0).round() as i64)
        .single()
        .unwrap_or_default()
}

/// One session's file as the footer's snapshot.
pub fn snapshot_of(s: &SessionLive) -> LiveSnapshot {
    LiveSnapshot {
        written_at: utc(s.written_at),
        context: s.context_percent.map(|p| ContextWindow {
            percent: p,
            used_tokens: s.context_used_tokens,
            total_tokens: s.context_window_size,
            model: s.model_id.clone(),
        }),
        five_hour_pct: s.five_hour_pct,
        seven_day_pct: s.seven_day_pct,
        cost_usd: s.cost_usd,
        session_id: Some(s.session_id.clone()),
    }
}

/// `<data_dir>/live`, where `cc-hook statusline` writes.
pub fn live_dir() -> PathBuf {
    cc_core::paths::live_dir()
}

/// The status line data for the footer: the newest fresh per-session file,
/// or `None` when no session wrote one recently (or the status line chain
/// is not installed).
pub fn read() -> Option<LiveSnapshot> {
    cc_local::live::read_sessions_from(&live_dir(), cc_local::now_secs())
        .into_iter()
        .max_by(|a, b| a.written_at.total_cmp(&b.written_at))
        .map(|s| snapshot_of(&s))
}

/// 330000 → "330K", 1000000 → "1.0M". As in the mockup.
fn compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(written_at: f64) -> SessionLive {
        SessionLive {
            session_id: "s-1".into(),
            written_at,
            context_percent: Some(42.5),
            context_window_size: Some(1_000_000),
            context_used_tokens: Some(425_000),
            model_id: Some("claude-opus-5-5".into()),
            model_display_name: Some("Opus".into()),
            five_hour_pct: Some(23.5),
            seven_day_pct: None,
            cost_usd: Some(0.5),
        }
    }

    #[test]
    fn compact_token_format() {
        assert_eq!(compact(330_000), "330K");
        assert_eq!(compact(1_000_000), "1.0M");
        assert_eq!(compact(999), "999");
    }

    #[test]
    fn a_session_file_becomes_the_footer_snapshot() {
        let now = cc_local::now_secs();
        let s = session(now);
        let snap = snapshot_of(&s);
        assert!(snap.is_fresh());
        assert_eq!(
            snap.written_at.timestamp_millis(),
            (now * 1000.0).round() as i64
        );
        let ctx = snap.context.unwrap();
        assert_eq!(ctx.percent, 42.5);
        assert_eq!(ctx.tokens_label().as_deref(), Some("425K / 1.0M"));
        assert_eq!(ctx.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(snap.five_hour_pct, Some(23.5));
        assert_eq!(snap.session_id.as_deref(), Some("s-1"));
        let early = SessionLive {
            context_percent: None,
            ..s
        };
        assert!(
            snapshot_of(&early).context.is_none(),
            "null early in a session"
        );
    }

    #[test]
    fn an_old_file_is_not_taken_as_fresh() {
        let an_hour_ago = cc_local::now_secs() - 3600.0;
        let snap = snapshot_of(&session(an_hour_ago));
        assert!(!snap.is_fresh());
        assert!(snap.age() > chrono::Duration::minutes(50));
    }

    /// The JSON the frontend receives for the footer must not change.
    #[test]
    fn the_snapshot_keeps_its_json_shape() {
        let json = serde_json::to_value(snapshot_of(&session(1_789_962_118.318))).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "written_at": "2026-09-21T03:41:58.318Z",
                "context": {
                    "percent": 42.5,
                    "used_tokens": 425000,
                    "total_tokens": 1000000,
                    "model": "claude-opus-5-5"
                },
                "five_hour_pct": 23.5,
                "seven_day_pct": null,
                "cost_usd": 0.5,
                "session_id": "s-1"
            })
        );
    }
}
