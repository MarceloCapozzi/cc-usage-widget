//! Core of the Claude usage widget.
//!
//! All the logic lives here, with no dependency on Tauri, so it can be exercised
//! from a console (`cargo run -p cc-usage --example probe`) and from tests without starting
//! WebView2. The binary (`main.rs` and its modules) is the only part that
//! knows about Tauri: its poller calls [`refresh`] and publishes the result to the window. Local sessions live in
//! [`sessions`], remote ones (other machines, through the NATS KV bucket) in
//! [`remote`], which runs in the Viewer role; in the Publisher role
//! [`publisher`] sends this machine's sessions to the bucket instead. The floating widget's geometry, preferences and full-screen
//! rule live in [`window_state`], [`settings`] and [`fullscreen`]; the tray
//! menu's "Start with Windows" logic in [`autostart`]; Settings → Data's
//! backup and restore in [`backup`]; one running app per data folder in
//! [`instance`].

pub mod activity;
pub mod api;
pub mod autostart;
pub mod backup;
pub mod credentials;
pub mod demo;
pub mod fullscreen;
pub mod history;
pub mod instance;
pub mod model;
pub mod notifications;
pub mod paths;
pub mod projects;
pub mod publisher;
pub mod remote;
pub mod sessions;
pub mod settings;
pub mod statusline;
pub mod window_state;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Everything the UI needs to draw itself once.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub usage: Option<model::UsageSnapshot>,
    /// Why there is no API data, if there is none.
    pub usage_error: Option<String>,
    /// Context window and cost, from the status line files. `None` if Claude
    /// Code is not running or its status line is not chained to cc-hook.
    pub live: Option<statusline::LiveSnapshot>,
    /// Freshness depends on the clock, so it is evaluated when the snapshot is
    /// built and travels already resolved: the frontend should not reimplement
    /// the 5-minute rule and risk the two versions drifting apart.
    pub live_fresh: bool,
    pub plan_label: Option<String>,
    pub email: Option<String>,
    /// The token expired. The widget keeps showing the last thing it knows,
    /// marked as stale; renewing it is Claude Code's job, not ours.
    pub token_expired: bool,
    /// While the token is expired: how the last request to Claude Code to
    /// renew it went ("Asked Claude Code to renew it at 18:20"), for the
    /// footer's tooltip. Filled in by the app's poller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renew_note: Option<String>,
    /// Folder overrides set to a relative path and ignored
    /// ([`paths::ignored_overrides`]), one line each, for the footer's
    /// tooltips. Filled in by the app's poller.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path_problems: Vec<String>,
    pub at: DateTime<Utc>,
    /// The login `email` belongs to ([`credentials::Credentials::account_key`]),
    /// for the poller's [`CachedProfile`]; never sent to the frontend.
    #[serde(skip)]
    pub account: Option<u64>,
}

impl Snapshot {
    /// Can what is shown be trusted?
    pub fn is_stale(&self) -> bool {
        self.usage.is_none() || self.token_expired
    }
}

/// The account's email, kept with the login it was fetched for: the
/// profile rarely changes, so it is fetched once per login instead of on
/// every poll, and a login with another account fetches it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedProfile {
    /// [`credentials::Credentials::account_key`] of the login.
    pub account: u64,
    pub email: String,
}

impl CachedProfile {
    /// The email to show for the login `account`, if this cache is that
    /// login's; `None` asks for the profile again.
    pub fn email_for(cache: Option<&Self>, account: u64) -> Option<String> {
        cache
            .filter(|c| c.account == account)
            .map(|c| c.email.clone())
    }
}

/// One full pass: credentials, API and the status line files. The email
/// comes from `profile_cache` whatever the login (the console probe's
/// single account); the app uses [`refresh_cached`].
pub async fn refresh(client: &api::Client, profile_cache: Option<&api::Profile>) -> Snapshot {
    refresh_with(client, |_| profile_cache.map(email_of)).await
}

/// One full pass, taking the email from `cache` only when it belongs to
/// the login in the credentials file now.
pub async fn refresh_cached(client: &api::Client, cache: Option<&CachedProfile>) -> Snapshot {
    refresh_with(client, |account| {
        CachedProfile::email_for(cache, account).map(Some)
    })
    .await
}

/// One full pass. `cached` gives, for the login's account key, the email
/// already known (`Some`, possibly none) or `None` to fetch the profile.
///
/// It never fails as a whole. Each source fails on its own and whatever
/// survives is shown: if the API does not answer but Claude Code is running,
/// the context window is still there; if the status line is missing, the
/// limits are still visible.
async fn refresh_with(
    client: &api::Client,
    cached: impl Fn(u64) -> Option<Option<String>>,
) -> Snapshot {
    let at = Utc::now();

    // The status line files are local and cannot fail because of the
    // network: they are always read, even when the credentials are missing.
    // Both are read on a blocking thread, so the poll never holds up the
    // executor.
    let (live, creds) = read_local_files().await;
    let live_fresh = live.as_ref().is_some_and(|l| l.is_fresh());

    let creds = match creds {
        Ok(c) => c,
        Err(e) => {
            return Snapshot {
                usage: None,
                usage_error: Some(e.to_string()),
                live,
                live_fresh,
                plan_label: None,
                email: None,
                token_expired: false,
                renew_note: None,
                path_problems: Vec::new(),
                at,
                account: None,
            }
        }
    };

    let token_expired = creds.is_expired();
    let plan_label = creds.plan_label();
    let account = creds.account_key();
    let known = cached(account);

    // With an expired token we do not even try: it would be a guaranteed 401
    // and a wasted poll. The old data stays on screen, marked as such.
    if token_expired {
        return Snapshot {
            usage: None,
            // User-facing: rendered in the flyout and the tray tooltip.
            usage_error: Some(
                "the token expired; Claude Code renews it on its next request".into(),
            ),
            live,
            live_fresh,
            plan_label,
            email: known.flatten(),
            token_expired,
            renew_note: None,
            path_problems: Vec::new(),
            at,
            account: Some(account),
        };
    }

    let (usage, usage_error) = match client.fetch_usage(creds.expose_token()).await {
        Ok(raw) => (Some(raw.normalize()), None),
        Err(e) => (None, Some(e.to_string())),
    };

    // The profile rarely changes: it is fetched once per login and cached.
    // If it fails, the widget still works, just without the email in the
    // header, and the next poll tries again.
    let email = match known {
        Some(email) => email,
        None => client
            .fetch_profile(creds.expose_token())
            .await
            .ok()
            .and_then(|p| email_of(&p)),
    };

    Snapshot {
        usage,
        usage_error,
        live,
        live_fresh,
        plan_label,
        email,
        token_expired,
        renew_note: None,
        path_problems: Vec::new(),
        at,
        account: Some(account),
    }
}

/// The status line files and the credentials, read on tokio's blocking pool.
/// A read that panicked counts as no status line data and unreadable
/// credentials.
async fn read_local_files() -> (
    Option<statusline::LiveSnapshot>,
    Result<credentials::Credentials, credentials::CredentialError>,
) {
    tokio::task::spawn_blocking(|| (statusline::read(), credentials::load()))
        .await
        .unwrap_or_else(|e| {
            (
                None,
                Err(credentials::CredentialError::Unreadable(e.to_string())),
            )
        })
}

fn email_of(p: &api::Profile) -> Option<String> {
    p.account.as_ref()?.email.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_email_is_used_only_for_its_own_login() {
        let cache = CachedProfile {
            account: 7,
            email: "me@example.com".into(),
        };
        assert_eq!(
            CachedProfile::email_for(Some(&cache), 7),
            Some("me@example.com".into())
        );
        // Another account logged in (or the token was renewed): fetched again.
        assert_eq!(CachedProfile::email_for(Some(&cache), 8), None);
        assert_eq!(CachedProfile::email_for(None, 7), None);
    }
}
