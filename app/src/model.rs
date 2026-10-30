//! Shapes of `GET /api/oauth/usage` and their normalization.
//!
//! # Golden rule: iterate `limits[]`, never hardcode buckets
//!
//! The response carries ~15 flat buckets with code names (`tangelo`,
//! `nimbus_quill`, `cinder_cove`, `harbor_lantern`, `amber_gauge`…) that are
//! `null` today: they are unreleased features that will be switched on over
//! time. A fixed list of names leaves the widget blind to any new limit.
//!
//! `limits[]` is also more complete than the flat buckets: on a Team account a
//! Fable `weekly_scoped` shows up at 11% while `seven_day_opus` and
//! `seven_day_sonnet` are `null`. That is why the array is the authoritative
//! source, and the flat buckets are only a fallback for `five_hour` / `seven_day`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Raw: what the API returns ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UsageResponse {
    #[serde(default)]
    pub limits: Vec<RawLimit>,
    #[serde(default)]
    pub five_hour: Option<RawBucket>,
    #[serde(default)]
    pub seven_day: Option<RawBucket>,
    #[serde(default)]
    pub spend: Option<RawSpend>,
}

#[derive(Debug, Deserialize)]
pub struct RawBucket {
    pub utilization: Option<f64>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RawLimit {
    pub kind: Option<String>,
    pub group: Option<String>,
    pub percent: Option<f64>,
    pub severity: Option<String>,
    pub resets_at: Option<String>,
    #[serde(default)]
    pub scope: Option<RawScope>,
    #[serde(default)]
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RawScope {
    #[serde(default)]
    pub model: Option<RawModel>,
}

#[derive(Debug, Deserialize)]
pub struct RawModel {
    pub display_name: Option<String>,
}

/// `spend` carries the exponent explicitly instead of assuming cents, so
/// there is no blind division by 100.
#[derive(Debug, Deserialize)]
pub struct RawSpend {
    #[serde(default)]
    pub used: Option<RawMoney>,
    #[serde(default)]
    pub limit: Option<RawMoney>,
    pub percent: Option<f64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RawMoney {
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub exponent: Option<u32>,
}

// ── Normalized: what the UI consumes ────────────────────────────────────────

/// Severity as the server rates it. We prefer this to pinning our own
/// thresholds: if Anthropic changes the criteria, the widget follows without
/// touching anything. `Unknown` keeps the raw string so a future value is not
/// lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Severity {
    Normal,
    Warning,
    Critical,
    Unknown(String),
}

impl Severity {
    fn parse(s: Option<&str>) -> Self {
        match s.map(str::to_lowercase).as_deref() {
            Some("normal") | None => Self::Normal,
            Some("warning") | Some("warn") => Self::Warning,
            Some("critical") | Some("crit") => Self::Critical,
            Some(other) => Self::Unknown(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum LimitKind {
    Session,
    WeeklyAll,
    WeeklyScoped,
    Other(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Limit {
    pub kind: LimitKind,
    /// Label ready to display: "5-hour session", "All models · weekly",
    /// "Fable · weekly".
    /// The text is user-facing and doubles as the series key stored in
    /// `history.json` and `notification-state.json`, so changing a label
    /// splits the stored history into two series.
    pub label: String,
    pub percent: f64,
    pub severity: Severity,
    pub resets_at: Option<DateTime<Utc>>,
    /// The window that is actually limiting right now, according to the server.
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Spend {
    pub used: f64,
    pub limit: Option<f64>,
    pub currency: String,
    pub percent: Option<f64>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSnapshot {
    pub limits: Vec<Limit>,
    pub spend: Option<Spend>,
    pub fetched_at: DateTime<Utc>,
}

impl UsageSnapshot {
    /// The number in the tray icon: the 5 h session.
    pub fn session(&self) -> Option<&Limit> {
        self.limits.iter().find(|l| l.kind == LimitKind::Session)
    }

    pub fn weekly(&self) -> Option<&Limit> {
        self.limits.iter().find(|l| l.kind == LimitKind::WeeklyAll)
    }

    /// The highest limit of all: the one worth watching even if it is not the
    /// one the user looks at by default.
    pub fn highest(&self) -> Option<&Limit> {
        self.limits.iter().max_by(|a, b| {
            a.percent
                .partial_cmp(&b.percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

// ── Normalization ───────────────────────────────────────────────────────────

impl UsageResponse {
    pub fn normalize(&self) -> UsageSnapshot {
        let mut limits: Vec<Limit> = self.limits.iter().filter_map(map_limit).collect();

        // Fallback in case `limits[]` arrives empty (old account, different
        // shape): only then do we fall back to the flat buckets, and only for
        // the two that exist.
        if limits.is_empty() {
            if let Some(l) =
                bucket_as_limit(self.five_hour.as_ref(), LimitKind::Session, LABEL_SESSION)
            {
                limits.push(l);
            }
            if let Some(l) =
                bucket_as_limit(self.seven_day.as_ref(), LimitKind::WeeklyAll, LABEL_WEEKLY)
            {
                limits.push(l);
            }
        }

        // Session first, then overall weekly, then the per-model ones.
        limits.sort_by_key(|l| match l.kind {
            LimitKind::Session => 0,
            LimitKind::WeeklyAll => 1,
            LimitKind::WeeklyScoped => 2,
            LimitKind::Other(_) => 3,
        });

        UsageSnapshot {
            limits,
            spend: self.spend.as_ref().and_then(map_spend),
            fetched_at: Utc::now(),
        }
    }
}

fn map_limit(raw: &RawLimit) -> Option<Limit> {
    // Without a percentage there is nothing to show. Note: `null` is not 0 — a
    // null bucket means "does not apply to this plan", and drawing it as 0%
    // would be a lie.
    let percent = normalize_pct(raw.percent)?;

    let kind = match raw.kind.as_deref() {
        Some("session") => LimitKind::Session,
        Some("weekly_all") => LimitKind::WeeklyAll,
        Some("weekly_scoped") => LimitKind::WeeklyScoped,
        Some(other) => LimitKind::Other(other.to_string()),
        None => return None,
    };

    let model = raw
        .scope
        .as_ref()
        .and_then(|s| s.model.as_ref())
        .and_then(|m| m.display_name.as_deref());

    let label = match (&kind, model) {
        (LimitKind::Session, _) => LABEL_SESSION.to_string(),
        (LimitKind::WeeklyAll, _) => LABEL_WEEKLY.to_string(),
        // The display_name comes from the server; if a new model shows up
        // tomorrow it is displayed on its own, without touching code.
        (LimitKind::WeeklyScoped, Some(m)) => format!("{m}{SCOPED_WEEKLY_SUFFIX}"),
        (LimitKind::WeeklyScoped, None) => LABEL_WEEKLY_PER_MODEL.to_string(),
        (LimitKind::Other(k), Some(m)) => format!("{m} · {k}"),
        (LimitKind::Other(k), None) => k.clone(),
    };

    Some(Limit {
        kind,
        label,
        percent,
        severity: Severity::parse(raw.severity.as_deref()),
        resets_at: parse_time(raw.resets_at.as_deref()),
        is_active: raw.is_active.unwrap_or(false),
    })
}

/// Label of the 5-hour window.
pub const LABEL_SESSION: &str = "5-hour session";
/// Label of the overall weekly window, worded like the per-model ones it
/// sits above ("Fable · weekly").
pub const LABEL_WEEKLY: &str = "All models · weekly";
/// Label of a per-model weekly window whose model has no name.
pub const LABEL_WEEKLY_PER_MODEL: &str = "Weekly (per model)";
/// Appended to the model name of a per-model weekly window.
const SCOPED_WEEKLY_SUFFIX: &str = " · weekly";

fn bucket_as_limit(raw: Option<&RawBucket>, kind: LimitKind, label: &str) -> Option<Limit> {
    let b = raw?;
    let percent = normalize_pct(b.utilization)?;
    Some(Limit {
        kind,
        label: label.to_string(),
        percent,
        severity: Severity::Normal,
        resets_at: parse_time(b.resets_at.as_deref()),
        is_active: false,
    })
}

fn map_spend(raw: &RawSpend) -> Option<Spend> {
    let used_money = raw.used.as_ref()?;
    let used = money_to_major(used_money)?;
    Some(Spend {
        used,
        limit: raw.limit.as_ref().and_then(money_to_major),
        currency: used_money.currency.clone().unwrap_or_else(|| "USD".into()),
        percent: raw.percent,
        enabled: raw.enabled.unwrap_or(false),
    })
}

/// `amount_minor` + `exponent` → major units. With `exponent: 2`, 1050 is
/// 10.50. The exponent is explicit precisely so cents are not assumed: some
/// currencies have no decimals (JPY, exponent 0).
fn money_to_major(m: &RawMoney) -> Option<f64> {
    let minor = m.amount_minor?;
    let exp = m.exponent.unwrap_or(2);
    Some(minor as f64 / 10f64.powi(exp as i32))
}

/// `null`/negative → `None` ("does not apply"), never 0. On the high end it is
/// clamped to 100: with overage the server can go past it, and losing the
/// bucket right when the limit is reached is the worst moment to be left
/// without data.
fn normalize_pct(value: Option<f64>) -> Option<f64> {
    let v = value?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    Some(v.min(100.0))
}

fn parse_time(value: Option<&str>) -> Option<DateTime<Utc>> {
    let s = value?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real response captured on 2026-09-20 (Team / max_5x account), trimmed.
    const REAL: &str = r#"{
      "five_hour": { "utilization": 11.0, "resets_at": "2026-09-21T05:10:00.324386+00:00" },
      "seven_day": { "utilization": 19.0, "resets_at": "2026-09-21T03:00:00.324408+00:00" },
      "seven_day_opus": null, "seven_day_sonnet": null, "tangelo": null,
      "nimbus_quill": { "utilization": 0.0, "resets_at": null },
      "limits": [
        { "kind": "session", "group": "session", "percent": 11, "severity": "normal",
          "resets_at": "2026-09-21T05:10:00.324386+00:00", "scope": null, "is_active": false },
        { "kind": "weekly_all", "group": "weekly", "percent": 19, "severity": "normal",
          "resets_at": "2026-09-21T03:00:00.324408+00:00", "scope": null, "is_active": true },
        { "kind": "weekly_scoped", "group": "weekly", "percent": 11, "severity": "normal",
          "resets_at": "2026-09-21T02:59:59.778615+00:00",
          "scope": { "model": { "id": null, "display_name": "Fable" }, "surface": null },
          "is_active": false }
      ],
      "spend": { "used": { "amount_minor": 0, "currency": "USD", "exponent": 2 },
                 "limit": null, "percent": 0, "severity": "normal", "enabled": false }
    }"#;

    fn snapshot() -> UsageSnapshot {
        serde_json::from_str::<UsageResponse>(REAL)
            .unwrap()
            .normalize()
    }

    #[test]
    fn parses_the_real_response() {
        let s = snapshot();
        assert_eq!(s.limits.len(), 3);
        assert_eq!(s.session().unwrap().percent, 11.0);
        assert_eq!(s.weekly().unwrap().percent, 19.0);
        assert!(s.weekly().unwrap().is_active);
    }

    /// The case that justifies the whole module: Fable comes from `limits[]`,
    /// not from a flat bucket. With `seven_day_opus`/`seven_day_sonnet` null,
    /// reading only the flat buckets would lose this limit.
    #[test]
    fn captures_the_per_model_limit_that_has_no_flat_bucket() {
        let s = snapshot();
        let fable = s
            .limits
            .iter()
            .find(|l| l.label.starts_with("Fable"))
            .unwrap();
        assert_eq!(fable.kind, LimitKind::WeeklyScoped);
        assert_eq!(fable.percent, 11.0);
        assert_eq!(fable.label, "Fable · weekly");
    }

    /// A `kind` that does not exist yet has to survive parsing and be shown,
    /// not discarded.
    #[test]
    fn an_unknown_kind_is_not_lost() {
        let json = r#"{ "limits": [
          { "kind": "monthly_secret", "percent": 42, "severity": "elevated",
            "resets_at": null, "scope": { "model": { "display_name": "Tangelo" } } }
        ]}"#;
        let s = serde_json::from_str::<UsageResponse>(json)
            .unwrap()
            .normalize();
        assert_eq!(s.limits.len(), 1);
        assert_eq!(s.limits[0].label, "Tangelo · monthly_secret");
        assert_eq!(s.limits[0].severity, Severity::Unknown("elevated".into()));
    }

    #[test]
    fn null_is_not_zero() {
        let json = r#"{ "limits": [
          { "kind": "session", "percent": null, "resets_at": null },
          { "kind": "weekly_all", "percent": 5, "resets_at": null }
        ]}"#;
        let s = serde_json::from_str::<UsageResponse>(json)
            .unwrap()
            .normalize();
        // The null bucket is dropped instead of showing up as 0%.
        assert_eq!(s.limits.len(), 1);
        assert_eq!(s.limits[0].kind, LimitKind::WeeklyAll);
    }

    #[test]
    fn falls_back_to_flat_buckets_when_there_are_no_limits() {
        let json = r#"{ "five_hour": { "utilization": 33.0, "resets_at": null },
                        "seven_day": { "utilization": 44.0, "resets_at": null },
                        "limits": [] }"#;
        let s = serde_json::from_str::<UsageResponse>(json)
            .unwrap()
            .normalize();
        assert_eq!(s.limits.len(), 2);
        assert_eq!(s.session().unwrap().percent, 33.0);
    }

    #[test]
    fn money_uses_the_declared_exponent() {
        let j = r#"{"limits":[],"spend":{"used":{"amount_minor":1050,"currency":"USD","exponent":2},
                     "limit":{"amount_minor":10000,"currency":"USD","exponent":2},"percent":10.5,"enabled":true}}"#;
        let s = serde_json::from_str::<UsageResponse>(j)
            .unwrap()
            .normalize();
        let sp = s.spend.unwrap();
        assert_eq!(sp.used, 10.50);
        assert_eq!(sp.limit, Some(100.0));

        // Currency without decimals: 1050 JPY is 1050, not 10.50.
        let j0 = r#"{"limits":[],"spend":{"used":{"amount_minor":1050,"currency":"JPY","exponent":0},
                      "limit":null,"percent":null,"enabled":true}}"#;
        let s0 = serde_json::from_str::<UsageResponse>(j0)
            .unwrap()
            .normalize();
        assert_eq!(s0.spend.unwrap().used, 1050.0);
    }

    #[test]
    fn the_highest_limit_wins() {
        let s = snapshot();
        assert_eq!(s.highest().unwrap().percent, 19.0);
    }

    #[test]
    fn labels_are_english() {
        let s = snapshot();
        let labels: Vec<_> = s.limits.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(
            labels,
            ["5-hour session", "All models · weekly", "Fable · weekly"]
        );
    }

    #[test]
    fn overage_is_clamped_to_100() {
        let json = r#"{ "limits": [{ "kind": "session", "percent": 137.5, "resets_at": null }] }"#;
        let s = serde_json::from_str::<UsageResponse>(json)
            .unwrap()
            .normalize();
        assert_eq!(s.limits[0].percent, 100.0);
    }
}
