//! Usage API client.
//!
//! # Why `api.anthropic.com` and not `claude.ai`
//!
//! The browser extension uses `claude.ai/api/organizations/{org}/usage` with the
//! session cookie. From a native process that does not work: `claude.ai` sits
//! behind Cloudflare and returns `403` with a bot challenge — verified on
//! 2026-09-20 with two auth schemes. The request dies at the edge and the token
//! is never even evaluated. The extension does not suffer from this because it
//! runs inside the browser, which already passed the challenge.
//!
//! The working endpoint was taken from the Claude Code 2.1.227 binary:
//!
//! ```text
//! fetchUtilization: GET /api/oauth/usage
//! await Pi.get("/api/oauth/usage", { timeout: 5000, ... })
//! ```
//!
//! It lives on `api.anthropic.com`, with no Cloudflare in between, and a single
//! call returns buckets, `limits[]` and `spend` — against the five calls the
//! extension makes.
//!
//! Headers: `Authorization: Bearer` + `Accept` is enough. Four combinations
//! were tried and all four return 200, including the minimal one, so the widget
//! identifies itself with its own name instead of pretending to be `claude-cli`.

use crate::model::UsageResponse;
use serde::Deserialize;
use std::time::Duration;

const API_BASE: &str = "https://api.anthropic.com/api";
const USER_AGENT: &str = concat!("cc-usage/", env!("CARGO_PKG_VERSION"));

/// Claude Code uses 5 s for the same call. We copy the choice.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The `Display` strings are rendered in the flyout and the tray tooltip, so
/// they are user-facing text, in English like the rest of the UI.
#[derive(Debug)]
pub enum ApiError {
    /// The token was not accepted. This is where "re-authenticate" is the right
    /// message.
    Unauthorized(u16),
    /// A bot challenge swallowed the request: the token was never evaluated.
    /// It is the opposite diagnosis of `Unauthorized`, and mixing them up leads
    /// to redesigning the architecture for nothing.
    BotChallenge,
    /// The path changed between versions. It has to be re-extracted from the
    /// CLI binary.
    NotFound,
    Http(u16),
    Network(String),
    Malformed(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized(c) => {
                write!(f, "HTTP {c}: the token was not accepted. Claude Code renews it on its next request.")
            }
            Self::BotChallenge => write!(
                f,
                "Cloudflare bot challenge: the request never reached the API and the token was not checked."
            ),
            Self::NotFound => write!(
                f,
                "HTTP 404: the endpoint changed. Extract the path from the Claude Code binary again."
            ),
            Self::Http(429) => write!(f, "HTTP 429: too many requests; the next refresh retries"),
            Self::Http(c) => write!(f, "HTTP {c}"),
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::Malformed(e) => write!(f, "unreadable response: {e}"),
        }
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub account: Option<Account>,
    #[serde(default)]
    pub organization: Option<Organization>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub email: Option<String>,
    pub full_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Organization {
    pub name: Option<String>,
    pub rate_limit_tier: Option<String>,
    pub organization_type: Option<String>,
}

pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new() -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        Ok(Self { http })
    }

    pub async fn fetch_usage(&self, token: &str) -> Result<UsageResponse, ApiError> {
        let body = self.get(&format!("{API_BASE}/oauth/usage"), token).await?;
        serde_json::from_str(&body).map_err(|e| ApiError::Malformed(e.to_string()))
    }

    /// The email in the mockup's header comes from here. It rarely changes, so
    /// it is worth fetching once per start rather than on every poll.
    pub async fn fetch_profile(&self, token: &str) -> Result<Profile, ApiError> {
        let body = self
            .get(&format!("{API_BASE}/oauth/profile"), token)
            .await?;
        serde_json::from_str(&body).map_err(|e| ApiError::Malformed(e.to_string()))
    }

    async fn get(&self, url: &str, token: &str) -> Result<String, ApiError> {
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| ApiError::Network(redact(&e.to_string(), token)))?;

        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| ApiError::Network(redact(&e.to_string(), token)))?;

        // The challenge is checked BEFORE the status: it arrives as a 403, and
        // without this it would read as "credential rejected", which is the
        // opposite conclusion.
        if is_bot_challenge(&body) {
            return Err(ApiError::BotChallenge);
        }

        match status {
            200 => Ok(body),
            401 | 403 => Err(ApiError::Unauthorized(status)),
            404 => Err(ApiError::NotFound),
            other => Err(ApiError::Http(other)),
        }
    }
}

fn is_bot_challenge(body: &str) -> bool {
    body.contains("challenges.cloudflare.com")
        || body.contains("Just a moment")
        || body.contains("cf-browser-verification")
        || body.contains("__cf_chl")
}

/// Safety net: reqwest may include the URL in the error message, and if the
/// token ever travelled in the query string it would end up in a log.
///
/// The placeholder ends up inside `ApiError::Network`, which is rendered in the
/// UI.
fn redact(message: &str, token: &str) -> String {
    if token.len() > 8 {
        message.replace(token, "<redacted>")
    } else {
        message.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_cloudflare_challenge() {
        let cf = r#"<!DOCTYPE html><html><head><title>Just a moment...</title>
                    <script src="https://challenges.cloudflare.com/turnstile"></script>"#;
        assert!(is_bot_challenge(cf));
        // A real API error is JSON and must not be mistaken for a challenge.
        assert!(!is_bot_challenge(
            r#"{"type":"error","error":{"type":"not_found_error"}}"#
        ));
        assert!(!is_bot_challenge(r#"{"five_hour":{"utilization":11.0}}"#));
    }

    #[test]
    fn redacts_the_token_in_errors() {
        let t = "tok-very-long-secret";
        assert!(!redact(&format!("GET failed ?auth={t}"), t).contains(t));
        // A short token is not redacted, but it does not break the message either.
        assert_eq!(redact("hello", "abc"), "hello");
    }

    #[test]
    fn the_user_agent_tells_the_truth() {
        assert!(USER_AGENT.starts_with("cc-usage/"));
        assert!(!USER_AGENT.contains("claude-cli"));
    }

    /// The shape the API returns, with made-up values.
    #[test]
    fn parses_the_profile_the_api_returns() {
        let json = r#"{"account":{"uuid":"00000000-0000-4000-8000-000000000001",
                                  "full_name":"Ada Lovelace","email":"ada@example.com"},
                       "organization":{"name":"Example Org","rate_limit_tier":"default_claude_pro",
                                       "organization_type":"claude_enterprise"}}"#;
        let p: Profile = serde_json::from_str(json).unwrap();
        assert_eq!(p.account.unwrap().email.unwrap(), "ada@example.com");
        assert_eq!(p.organization.unwrap().name.unwrap(), "Example Org");
    }
}
