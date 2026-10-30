//! Reads the OAuth token that Claude Code leaves in `~/.claude/.credentials.json`
//! (under `CLAUDE_CONFIG_DIR` when that is set).
//!
//! # Why this module does NOT refresh the token
//!
//! The file belongs to Claude Code: the CLI rewrites it when it renews. If the
//! widget also wrote there, two processes would compete for the same file, and
//! a lost race logs the user out of their work tool. The risk is not worth it:
//! the token lasts ~8 h and Claude Code renews it on its next request; the
//! poll loop then notices within seconds (`poller::wait_for_renewal`).
//!
//! The stance is therefore **read-only**. If the token expired, the widget says
//! so and keeps showing the last known data, marked as stale. It never writes
//! to `.credentials.json`.

use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;

/// The `Display` strings reach the flyout through `Snapshot::usage_error`, so
/// they are user-facing text, in English like the rest of the UI.
#[derive(Debug)]
pub enum CredentialError {
    NotFound(PathBuf),
    Unreadable(String),
    Malformed(String),
    NoToken,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(p) => write!(
                f,
                "{} does not exist. Did you sign in to Claude Code on this machine?",
                p.display()
            ),
            Self::Unreadable(e) => write!(f, "could not read the file: {e}"),
            Self::Malformed(e) => write!(f, "invalid JSON: {e}"),
            Self::NoToken => write!(f, "claudeAiOauth.accessToken is missing"),
        }
    }
}

impl std::error::Error for CredentialError {}

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OauthBlock>,
    #[serde(rename = "organizationUuid")]
    organization_uuid: Option<String>,
}

#[derive(Deserialize)]
struct OauthBlock {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    /// Epoch in milliseconds.
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
}

/// Token and plan metadata. `Debug` is implemented by hand so the token does
/// not end up in a log by accident (see the impl below).
#[derive(Clone)]
pub struct Credentials {
    access_token: String,
    pub expires_at_ms: Option<i64>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
    pub organization_uuid: Option<String>,
}

impl Credentials {
    /// Explicit access to the secret. It is called this, and not `token()`, so
    /// that the call site makes it obvious sensitive material is being touched.
    pub fn expose_token(&self) -> &str {
        &self.access_token
    }

    /// Which login these credentials are: a hash of the token and the
    /// organisation, kept only in memory, so a cached profile is used only
    /// with the credentials it was fetched with. A logout and a login with
    /// another account give another key, and so does a renewal of the
    /// token (one more profile request, every few hours). The organisation
    /// alone would not tell apart two accounts of one organisation.
    pub fn account_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.access_token.hash(&mut hasher);
        self.organization_uuid.hash(&mut hasher);
        hasher.finish()
    }

    pub fn is_expired(&self) -> bool {
        match self.expires_at_ms {
            Some(ms) => ms <= now_ms(),
            None => false, // no expiry data: let the API answer
        }
    }

    /// Plan label for the header: the organisation plan from `subscriptionType`
    /// (Team, Enterprise) followed by the usage tier from `rateLimitTier`, e.g.
    /// "Team · Max 5x". Individual plans (Pro, Max) are already named by their
    /// tier, so only the tier shows. Unknown values are left out: showing
    /// nothing is better than showing the wrong label.
    pub fn plan_label(&self) -> Option<String> {
        let organisation = self.organisation_plan();
        let tier = self.tier_label();
        match (organisation, tier) {
            (Some(org), Some(tier)) if tier != org => Some(format!("{org} · {tier}")),
            (Some(org), _) => Some(org.to_string()),
            (None, tier) => tier.map(String::from),
        }
    }

    /// `subscriptionType` when it names an organisation plan.
    fn organisation_plan(&self) -> Option<&'static str> {
        match self.subscription_type.as_deref()?.to_lowercase().as_str() {
            "team" => Some("Team"),
            "enterprise" => Some("Enterprise"),
            _ => None,
        }
    }

    /// The usage tier, from fragments of `rateLimitTier`.
    fn tier_label(&self) -> Option<&'static str> {
        let tier = self.rate_limit_tier.as_deref()?.to_lowercase();
        let label = if tier.contains("max_20x") {
            "Max 20x"
        } else if tier.contains("max_5x") {
            "Max 5x"
        } else if tier.contains("max") {
            "Max"
        } else if tier.contains("team") {
            "Team"
        } else if tier.contains("enterprise") {
            "Enterprise"
        } else if tier.contains("pro") {
            "Pro"
        } else if tier.contains("free") || tier == "default" {
            "Free"
        } else {
            return None;
        };
        Some(label)
    }
}

/// Redacts the token. Without this, a `dbg!` or a `{:?}` in a log would leak it.
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_token", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("subscription_type", &self.subscription_type)
            .field("rate_limit_tier", &self.rate_limit_tier)
            .field("organization_uuid", &self.organization_uuid)
            .finish()
    }
}

/// `.credentials.json` in Claude Code's folder: `CLAUDE_CONFIG_DIR` when set,
/// as for the sessions and transcripts, else `~/.claude`.
pub fn default_path() -> PathBuf {
    cc_local::claude_dir().join(".credentials.json")
}

pub fn load() -> Result<Credentials, CredentialError> {
    load_from(&default_path())
}

pub fn load_from(path: &PathBuf) -> Result<Credentials, CredentialError> {
    if !path.exists() {
        return Err(CredentialError::NotFound(path.clone()));
    }
    let raw =
        std::fs::read_to_string(path).map_err(|e| CredentialError::Unreadable(e.to_string()))?;
    let parsed: CredentialsFile =
        serde_json::from_str(&raw).map_err(|e| CredentialError::Malformed(e.to_string()))?;

    let oauth = parsed.oauth.ok_or(CredentialError::NoToken)?;
    let access_token = oauth
        .access_token
        .filter(|t| !t.is_empty())
        .ok_or(CredentialError::NoToken)?;

    Ok(Credentials {
        access_token,
        expires_at_ms: oauth.expires_at,
        subscription_type: oauth.subscription_type,
        rate_limit_tier: oauth.rate_limit_tier,
        organization_uuid: parsed.organization_uuid,
    })
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(tier: Option<&str>, expires_at_ms: Option<i64>) -> Credentials {
        Credentials {
            access_token: "secret-that-must-not-appear".into(),
            expires_at_ms,
            subscription_type: None,
            rate_limit_tier: tier.map(String::from),
            organization_uuid: None,
        }
    }

    /// The reason the `Debug` impl is hand-written. If someone derives it, this
    /// fails.
    #[test]
    fn debug_does_not_leak_the_token() {
        let output = format!("{:?}", creds(None, None));
        assert!(!output.contains("secret-that-must-not-appear"));
        assert!(output.contains("<redacted>"));
    }

    #[test]
    fn plan_label_from_the_tier() {
        assert_eq!(
            creds(Some("default_claude_max_5x"), None).plan_label(),
            Some("Max 5x".into())
        );
        assert_eq!(
            creds(Some("claude_max_20x"), None).plan_label(),
            Some("Max 20x".into())
        );
        // max_20x has to beat max_5x and max: the order of the ifs matters.
        assert_eq!(
            creds(Some("enterprise_tier"), None).plan_label(),
            Some("Enterprise".into())
        );
        // Unknown tier: nothing, instead of a made-up label.
        assert_eq!(
            creds(Some("tier_that_does_not_exist"), None).plan_label(),
            None
        );
        assert_eq!(creds(None, None).plan_label(), None);
    }

    #[test]
    fn plan_label_names_the_organisation_plan_first() {
        let with = |sub: Option<&str>, tier: Option<&str>| {
            Credentials {
                subscription_type: sub.map(String::from),
                ..creds(tier, None)
            }
            .plan_label()
        };
        // This machine's account: a Team seat on the Max 5x tier.
        assert_eq!(
            with(Some("team"), Some("default_claude_max_5x")),
            Some("Team · Max 5x".into())
        );
        assert_eq!(
            with(Some("enterprise"), Some("claude_max_20x")),
            Some("Enterprise · Max 20x".into())
        );
        // The tier already says it; no "Enterprise · Enterprise".
        assert_eq!(
            with(Some("enterprise"), Some("enterprise_tier")),
            Some("Enterprise".into())
        );
        assert_eq!(with(Some("team"), None), Some("Team".into()));
        // Individual plans are named by the tier alone.
        assert_eq!(
            with(Some("max"), Some("default_claude_max_5x")),
            Some("Max 5x".into())
        );
        assert_eq!(
            with(Some("pro"), Some("default_claude_pro")),
            Some("Pro".into())
        );
        assert_eq!(with(Some("something_new"), None), None);
    }

    #[test]
    fn another_login_is_another_account_key() {
        let one = creds(None, None);
        assert_eq!(one.account_key(), creds(None, None).account_key());
        let other = Credentials {
            access_token: "the-token-of-another-login".into(),
            ..creds(None, None)
        };
        assert_ne!(one.account_key(), other.account_key());
        let other_org = Credentials {
            organization_uuid: Some("00000000-0000-4000-8000-000000000001".into()),
            ..creds(None, None)
        };
        assert_ne!(one.account_key(), other_org.account_key());
    }

    #[test]
    fn expiry() {
        let now = now_ms();
        assert!(creds(None, Some(now - 1000)).is_expired());
        assert!(!creds(None, Some(now + 3_600_000)).is_expired());
        // Without expiry data we do not assume it expired: let the API answer.
        assert!(!creds(None, None).is_expired());
    }

    #[test]
    fn parses_the_real_file() {
        let json = r#"{
            "claudeAiOauth": {
                "accessToken": "tok-abc",
                "expiresAt": 1789962118318,
                "subscriptionType": "team",
                "rateLimitTier": "default_claude_max_5x",
                "scopes": ["user:profile"]
            },
            "organizationUuid": "00000000-0000-4000-8000-000000000000"
        }"#;
        let f: CredentialsFile = serde_json::from_str(json).unwrap();
        let o = f.oauth.unwrap();
        assert_eq!(o.access_token.unwrap(), "tok-abc");
        assert_eq!(o.rate_limit_tier.unwrap(), "default_claude_max_5x");
        assert_eq!(
            f.organization_uuid.unwrap(),
            "00000000-0000-4000-8000-000000000000"
        );
    }
}
