//! Data locations shared by every cc-usage binary, on Windows and Linux, and
//! the home and Claude Code folders they are derived from.
//!
//! - Windows: `%LOCALAPPDATA%\cc-usage`
//! - elsewhere: `$XDG_DATA_HOME/cc-usage`, or `~/.local/share/cc-usage`
//!
//! `CC_USAGE_DATA_DIR` overrides both, for tests and unusual setups.
//!
//! Every folder taken from the environment, the explicit overrides
//! (`CC_USAGE_DATA_DIR`, `CLAUDE_CONFIG_DIR`) as well as the bases
//! (`LOCALAPPDATA`, `XDG_DATA_HOME`, `USERPROFILE`, `HOME`), must be an
//! absolute path, or it counts as unset and the next rule applies; so does
//! an empty value. cc-hook runs in the user's project folder, and a relative
//! value would put the spool, the live files or the keys inside it, a
//! different folder per project. [`relative_overrides`] names the overrides
//! ignored that way, which `cc-publish check` reports.
//!
//! Every resolver has a `_from` twin that reads the environment through a
//! function, so the rules are tested without touching the process
//! environment.

use std::ffi::OsString;
use std::path::PathBuf;

pub const APP_DIR: &str = "cc-usage";

/// Set (to any value) in the environment of a Claude Code process that only
/// runs to renew its token (the widget's `claude -p` with no prompt): its
/// hooks inherit it and cc-hook writes nothing to the spool, so the start
/// and end of that process never show as a session.
pub const NO_SPOOL_ENV: &str = "CC_USAGE_NO_SPOOL";

/// Overrides [`data_dir`].
pub const DATA_DIR_ENV: &str = "CC_USAGE_DATA_DIR";

/// Overrides [`claude_config_dir`]; Claude Code honours it too.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

fn env_var(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// A variable's value when it is an absolute path (so never empty).
fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// The explicit overrides ([`DATA_DIR_ENV`], [`CLAUDE_CONFIG_DIR_ENV`]) that
/// are set to a relative path, and so ignored: each variable's name with its
/// value, in that order. Empty when both are unset, empty or absolute.
pub fn relative_overrides() -> Vec<(&'static str, PathBuf)> {
    relative_overrides_from(env_var)
}

/// [`relative_overrides`] from the environment alone, read through `var`.
pub fn relative_overrides_from(
    var: impl Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, PathBuf)> {
    [DATA_DIR_ENV, CLAUDE_CONFIG_DIR_ENV]
        .into_iter()
        .filter_map(|name| {
            let value = var(name).filter(|v| !v.is_empty()).map(PathBuf::from)?;
            (!value.is_absolute()).then_some((name, value))
        })
        .collect()
}

/// The user's home folder: `USERPROFILE` on Windows, `HOME` elsewhere, each
/// falling back to the other, then to what the OS reports for the user
/// ([`std::env::home_dir`]). Only absolute paths count; `None` when there
/// is none.
pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(env_var).or_else(|| std::env::home_dir().filter(|p| p.is_absolute()))
}

/// [`home_dir`] from the environment alone, read through `var`.
pub fn home_dir_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let names: [&str; 2] = if cfg!(windows) {
        ["USERPROFILE", "HOME"]
    } else {
        ["HOME", "USERPROFILE"]
    };
    names.iter().find_map(|name| absolute(var(name)))
}

/// Claude Code's configuration folder, the rule Claude Code itself follows:
/// `CLAUDE_CONFIG_DIR` when set to an absolute path, else `.claude` in
/// [`home_dir`]. `None` without either.
pub fn claude_config_dir() -> Option<PathBuf> {
    absolute(env_var(CLAUDE_CONFIG_DIR_ENV)).or_else(|| home_dir().map(|h| h.join(".claude")))
}

/// [`claude_config_dir`] from the environment alone, read through `var`.
pub fn claude_config_dir_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    absolute(var(CLAUDE_CONFIG_DIR_ENV)).or_else(|| home_dir_from(var).map(|h| h.join(".claude")))
}

/// The data directory. Not created here; writers create what they need.
///
/// `CC_USAGE_DATA_DIR` when absolute, else `%LOCALAPPDATA%\cc-usage` on Windows, else
/// `$XDG_DATA_HOME/cc-usage`, else `~/.local/share/cc-usage` ([`home_dir`]).
/// With none of those usable, `cc-usage` in the system temp folder, so the
/// result is never a path relative to the current folder.
pub fn data_dir() -> PathBuf {
    data_dir_with(env_var, home_dir()).unwrap_or_else(|| std::env::temp_dir().join(APP_DIR))
}

/// [`data_dir`] from the environment alone, read through `var`: `None`
/// instead of the temp folder fallback.
pub fn data_dir_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let home = home_dir_from(&var);
    data_dir_with(var, home)
}

fn data_dir_with(var: impl Fn(&str) -> Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(dir) = absolute(var(DATA_DIR_ENV)) {
        return Some(dir);
    }
    if cfg!(windows) {
        if let Some(base) = absolute(var("LOCALAPPDATA")) {
            return Some(base.join(APP_DIR));
        }
    }
    if let Some(base) = absolute(var("XDG_DATA_HOME")) {
        return Some(base.join(APP_DIR));
    }
    home.map(|h| h.join(".local").join("share").join(APP_DIR))
}

/// Where cc-hook drops one JSON file per hook call.
pub fn spool_dir() -> PathBuf {
    data_dir().join("spool")
}

/// NATS credentials, one `<user>.creds` per user.
pub fn nats_dir() -> PathBuf {
    data_dir().join("nats")
}

/// The key pairs the envelope is sealed and opened with.
///
/// On the viewer: `viewer.key` (its private keys), `viewer.pub` (the
/// matching public keys) and `publishers/<alias>.pub` (each publisher's
/// pinned public keys). On a publisher: `publisher.key` (its own private
/// keys) and `viewer.pub` (the viewer's public keys it seals to).
pub fn keys_dir() -> PathBuf {
    data_dir().join("keys")
}

/// The viewer's private keys, `keys/viewer.key`.
pub fn viewer_key_file() -> PathBuf {
    keys_dir().join("viewer.key")
}

/// The viewer's public keys, `keys/viewer.pub`: on the viewer, derived from
/// [`viewer_key_file`]; on a publisher, the keys it seals to.
pub fn viewer_pub_file() -> PathBuf {
    keys_dir().join("viewer.pub")
}

/// A publisher's own private keys, `keys/publisher.key`.
pub fn publisher_key_file() -> PathBuf {
    keys_dir().join("publisher.key")
}

/// Where the viewer pins publishers' public keys, `keys/publishers`.
pub fn publishers_dir() -> PathBuf {
    keys_dir().join("publishers")
}

/// The pinned public keys of one publisher, `keys/publishers/<alias>.pub`.
/// `alias` must already be a valid machine alias.
pub fn publisher_pin_file(alias: &str) -> PathBuf {
    publishers_dir().join(format!("{alias}.pub"))
}

/// Where `cc-hook statusline` writes one `<session_id>.json` per session with
/// the context window and limits Claude Code passed to the status line.
pub fn live_dir() -> PathBuf {
    data_dir().join("live")
}

/// The status line commands `cc-hook install` replaced, so `cc-hook
/// statusline` can run them and `cc-hook uninstall` can put them back.
pub fn statusline_chain_file() -> PathBuf {
    data_dir().join("statusline-chain.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// An absolute path on this platform.
    fn abs(rest: &str) -> String {
        if cfg!(windows) {
            format!(r"C:\{rest}")
        } else {
            format!("/{rest}")
        }
    }

    fn env(pairs: &[(&str, String)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn the_home_folder_skips_empty_and_relative_values() {
        let own = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let other = if cfg!(windows) { "HOME" } else { "USERPROFILE" };
        let both = env(&[(own, abs("own")), (other, abs("other"))]);
        assert_eq!(home_dir_from(both), Some(PathBuf::from(abs("own"))));
        // Each falls back to the other, skipping empty and relative values.
        let empty = env(&[(own, String::new()), (other, abs("other"))]);
        assert_eq!(home_dir_from(empty), Some(PathBuf::from(abs("other"))));
        let relative = env(&[(own, "home".into()), (other, abs("other"))]);
        assert_eq!(home_dir_from(relative), Some(PathBuf::from(abs("other"))));
        assert_eq!(
            home_dir_from(env(&[(own, String::new()), (other, "x".into())])),
            None
        );
        assert_eq!(home_dir_from(env(&[])), None);
    }

    #[test]
    fn the_claude_folder_follows_claude_config_dir_then_the_home_folder() {
        let own = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let custom = env(&[("CLAUDE_CONFIG_DIR", abs("cfg")), (own, abs("u"))]);
        assert_eq!(
            claude_config_dir_from(custom),
            Some(PathBuf::from(abs("cfg")))
        );
        // Empty counts as unset, like Claude Code does.
        let empty = env(&[("CLAUDE_CONFIG_DIR", String::new()), (own, abs("u"))]);
        assert_eq!(
            claude_config_dir_from(empty),
            Some(Path::new(&abs("u")).join(".claude"))
        );
        // No usable home: no folder, never a relative `.claude`.
        assert_eq!(
            claude_config_dir_from(env(&[("CLAUDE_CONFIG_DIR", String::new())])),
            None
        );
        assert_eq!(claude_config_dir_from(env(&[(own, String::new())])), None);
        // A relative value counts as unset too.
        let relative = env(&[("CLAUDE_CONFIG_DIR", "cfg".into()), (own, abs("u"))]);
        assert_eq!(
            claude_config_dir_from(relative),
            Some(Path::new(&abs("u")).join(".claude"))
        );
    }

    #[test]
    fn a_relative_override_is_ignored_and_reported() {
        let own = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let relative = env(&[
            ("CC_USAGE_DATA_DIR", "data".into()),
            ("CLAUDE_CONFIG_DIR", r".\claude".into()),
            ("XDG_DATA_HOME", abs("xdg")),
            ("LOCALAPPDATA", abs("xdg")),
            (own, abs("u")),
        ]);
        assert_eq!(
            data_dir_from(&relative),
            Some(Path::new(&abs("xdg")).join(APP_DIR))
        );
        assert_eq!(
            relative_overrides_from(&relative),
            vec![
                ("CC_USAGE_DATA_DIR", PathBuf::from("data")),
                ("CLAUDE_CONFIG_DIR", PathBuf::from(r".\claude")),
            ]
        );

        // Unset, empty and absolute overrides are not reported.
        assert!(relative_overrides_from(env(&[])).is_empty());
        let fine = env(&[
            ("CC_USAGE_DATA_DIR", abs("data")),
            ("CLAUDE_CONFIG_DIR", String::new()),
        ]);
        assert!(relative_overrides_from(fine).is_empty());
    }

    #[test]
    fn the_data_folder_is_never_relative() {
        let own = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let set = env(&[
            ("CC_USAGE_DATA_DIR", abs("data")),
            ("LOCALAPPDATA", abs("lad")),
            (own, abs("u")),
        ]);
        assert_eq!(data_dir_from(set), Some(PathBuf::from(abs("data"))));

        // Empty or relative bases are skipped down to the home folder.
        let blank = env(&[
            ("CC_USAGE_DATA_DIR", String::new()),
            ("LOCALAPPDATA", String::new()),
            ("XDG_DATA_HOME", "relative".into()),
            (own, abs("u")),
        ]);
        let home_based = Path::new(&abs("u"))
            .join(".local")
            .join("share")
            .join(APP_DIR);
        assert_eq!(data_dir_from(blank), Some(home_based));

        let xdg = env(&[("XDG_DATA_HOME", abs("xdg")), (own, abs("u"))]);
        assert_eq!(
            data_dir_from(xdg),
            Some(Path::new(&abs("xdg")).join(APP_DIR))
        );
        if cfg!(windows) {
            let lad = env(&[("LOCALAPPDATA", abs("lad")), ("XDG_DATA_HOME", abs("xdg"))]);
            assert_eq!(
                data_dir_from(lad),
                Some(Path::new(&abs("lad")).join(APP_DIR))
            );
        }

        // Nothing usable: None here, the temp folder from data_dir.
        assert_eq!(
            data_dir_from(env(&[
                (own, String::new()),
                ("XDG_DATA_HOME", String::new())
            ])),
            None
        );
        assert!(data_dir().is_absolute());
    }
}
