//! Where cc-usage keeps its files.
//!
//! Everything lives under `%LOCALAPPDATA%\cc-usage`, the folder
//! [`cc_core::paths::data_dir`] resolves, the one cc-hook and cc-publish
//! use: `CC_USAGE_DATA_DIR` overrides it when absolute, so a test instance
//! never touches the real history. A relative `CC_USAGE_DATA_DIR` or
//! `CLAUDE_CONFIG_DIR` is ignored, and [`ignored_overrides`] says so (the
//! footer's tooltips, the ⚙ view and the log).
//! The widget's window and shortcut preferences live in the app config dir
//! instead (`%APPDATA%\ar.cc.usage`, resolved by Tauri in `state.rs`), which
//! `CC_USAGE_CONFIG_DIR` overrides.

use std::path::{Path, PathBuf};

/// Overrides [`data_dir`].
pub use cc_core::paths::DATA_DIR_ENV;
/// Overrides the app config dir that holds `window.json` and `settings.json`.
pub const CONFIG_DIR_ENV: &str = "CC_USAGE_CONFIG_DIR";

pub const HISTORY: &str = "history.json";
pub const PRICING: &str = "pricing.json";
pub const NOTIFICATION_STATE: &str = "notification-state.json";
/// The app's diagnostics log, both roles: rotated to `cc-usage.log.1` at
/// 1 MB. Not `cc-publish.log`, which the standalone publisher writes in the
/// same folder.
pub const LOG: &str = "cc-usage.log";

/// [`cc_core::paths::data_dir`]: `%LOCALAPPDATA%\cc-usage`, or
/// `CC_USAGE_DATA_DIR` when it is an absolute path.
pub fn data_dir() -> PathBuf {
    cc_core::paths::data_dir()
}

/// `CC_USAGE_CONFIG_DIR`, when set: where the app binary (`state.rs`) keeps the widget's
/// `window.json` and `settings.json` instead of the app config dir.
pub fn config_dir_override() -> Option<PathBuf> {
    override_from(std::env::var_os(CONFIG_DIR_ENV))
}

/// An environment override counts only when it is an absolute path, like
/// cc-core's: a relative one would depend on the folder the app started in.
/// [`ignored_overrides`] names one that is ignored.
fn override_from(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// The folder overrides set to a relative path, and so ignored
/// ([`cc_core::paths::relative_overrides`]), one user-facing line each:
/// "CC_USAGE_DATA_DIR is relative (data) and is ignored; set an absolute
/// path. Using …". The environment does not change while the app runs, so
/// they are worked out once, and logged as warnings then.
pub fn ignored_overrides() -> &'static [String] {
    static LINES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LINES.get_or_init(|| {
        let lines = ignored_overrides_from(|name| std::env::var_os(name));
        for line in &lines {
            tracing::warn!("{line}");
        }
        lines
    })
}

/// [`ignored_overrides`] from the environment read through `var`.
fn ignored_overrides_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<String> {
    use cc_core::paths::{claude_config_dir_from, data_dir_from, relative_overrides_from, APP_DIR};
    let config = var(CONFIG_DIR_ENV)
        .filter(|v| !v.is_empty() && !PathBuf::from(v).is_absolute())
        .map(|value| {
            format!(
                "{CONFIG_DIR_ENV} is relative ({}) and is ignored; set an absolute path. \
                 Using the app's config folder.",
                PathBuf::from(value).display()
            )
        });
    relative_overrides_from(&var)
        .into_iter()
        .map(|(name, value)| {
            // What is used instead: the data folder always exists (the temp
            // folder as a last resort), Claude Code's folder may not.
            let used = if name == DATA_DIR_ENV {
                let dir = data_dir_from(&var).unwrap_or_else(|| std::env::temp_dir().join(APP_DIR));
                format!("Using {}.", dir.display())
            } else {
                match claude_config_dir_from(&var) {
                    Some(dir) => format!("Using {}.", dir.display()),
                    None => "No Claude Code folder is found.".to_string(),
                }
            };
            format!(
                "{name} is relative ({}) and is ignored; set an absolute path. {used}",
                value.display()
            )
        })
        .chain(config)
        .collect()
}

/// A file inside [`data_dir`].
pub fn file(name: &str) -> PathBuf {
    data_dir().join(name)
}

/// Writes `contents` to a temporary file next to `path` and renames it into
/// place, creating the folder first. A reader (the flyout, another view, the
/// next start) never finds a half-written file, and a crash or a full disk
/// in the middle leaves the previous file whole instead of truncated.
///
/// The temporary name is unique per call: two threads of the app may save
/// the same file at once, and sharing one `.tmp` would let one rename the
/// other's half-written bytes into place. A rename refused because another
/// process holds the file open for a moment (an antivirus scan) is retried
/// for up to about three seconds. This is [`cc_core::fsutil::write_atomic`].
pub use cc_core::fsutil::write_atomic;

/// Moves a file that exists but cannot be parsed out of the way, to
/// `<name>.corrupt-<YYYYMMDD-HHMMSS>` next to it, and says so in the log.
///
/// Called before a store that only grows (the activity calendar) starts
/// again from empty: saving over the unreadable file would throw away
/// whatever it still holds, and it may be months of days a hand edit or a
/// bad sector made unreadable. Kept aside, it can be repaired by hand.
/// Returns where it went, or `None` if it could not be moved (the caller
/// must then not save over it).
pub fn keep_corrupt(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let aside = path.with_file_name(format!("{name}.corrupt-{stamp}"));
    match std::fs::rename(path, &aside) {
        Ok(()) => {
            tracing::warn!(
                "{} could not be read; kept it as {} and started a new one",
                path.display(),
                aside.display()
            );
            Some(aside)
        }
        Err(err) => {
            tracing::warn!(
                "{} could not be read nor moved aside ({err}); leaving it untouched",
                path.display()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_override_is_no_override() {
        assert_eq!(override_from(None), None);
        assert_eq!(override_from(Some("".into())), None);
        let absolute = if cfg!(windows) { r"C:\tmp\x" } else { "/tmp/x" };
        assert_eq!(
            override_from(Some(absolute.into())),
            Some(PathBuf::from(absolute))
        );
    }

    #[test]
    fn a_relative_config_override_is_ignored_and_named() {
        assert_eq!(override_from(Some(r"cfg\x".into())), None);
        let lines =
            ignored_overrides_from(|name| (name == CONFIG_DIR_ENV).then(|| r"cfg\x".into()));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("CC_USAGE_CONFIG_DIR is relative"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn a_relative_folder_override_is_named_with_what_is_used_instead() {
        let (base, home) = if cfg!(windows) {
            (r"C:\Users\devbox\AppData\Local", r"C:\Users\devbox")
        } else {
            ("/home/devbox/.local/share", "/home/devbox")
        };
        let own = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let env = |pairs: Vec<(&'static str, &'static str)>| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };
        let lines = ignored_overrides_from(env(vec![
            ("CC_USAGE_DATA_DIR", "data"),
            ("CLAUDE_CONFIG_DIR", r".\claude"),
            ("LOCALAPPDATA", base),
            ("XDG_DATA_HOME", base),
            (own, home),
        ]));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            lines[0],
            format!(
                "CC_USAGE_DATA_DIR is relative (data) and is ignored; set an absolute path. Using {}.",
                Path::new(base).join("cc-usage").display()
            )
        );
        assert_eq!(
            lines[1],
            format!(
                "CLAUDE_CONFIG_DIR is relative (.\\claude) and is ignored; set an absolute path. Using {}.",
                Path::new(home).join(".claude").display()
            )
        );

        // Without a home folder there is no Claude Code folder to name.
        let lines = ignored_overrides_from(env(vec![("CLAUDE_CONFIG_DIR", "claude")]));
        assert_eq!(
            lines,
            [
                "CLAUDE_CONFIG_DIR is relative (claude) and is ignored; set an absolute path. \
              No Claude Code folder is found."
            ]
        );

        // Unset, empty and absolute overrides say nothing.
        assert!(ignored_overrides_from(env(vec![])).is_empty());
        assert!(ignored_overrides_from(env(vec![
            ("CC_USAGE_DATA_DIR", ""),
            ("CLAUDE_CONFIG_DIR", home),
        ]))
        .is_empty());
    }

    #[test]
    fn the_data_folder_is_the_one_cc_core_resolves() {
        assert_eq!(data_dir(), cc_core::paths::data_dir());
        assert_eq!(file(HISTORY), data_dir().join(HISTORY));
    }

    #[test]
    fn a_corrupt_file_is_kept_aside_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.json");
        std::fs::write(&path, "{ half").unwrap();
        let aside = keep_corrupt(&path).expect("moved");
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(&aside).unwrap(), "{ half");
        let name = aside.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("activity.json.corrupt-"), "{name}");
        assert_eq!(keep_corrupt(&path), None, "nothing left to move");
    }
}
