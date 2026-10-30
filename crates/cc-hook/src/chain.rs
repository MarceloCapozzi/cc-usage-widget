//! The record of the status line commands cc-hook replaced,
//! `<data_dir>/statusline-chain.json`.
//!
//! `cc-hook install` puts its own command in `statusLine.command` and keeps the
//! previous `statusLine` value here, verbatim, so `cc-hook statusline` can run
//! the original command and `cc-hook uninstall` can put the value back. One
//! entry per settings file, keyed by [`settings_key`], so installing into a
//! test copy with `--settings` never touches the record of the real file:
//!
//! ```json
//! { "v": 1, "chains": { "c:/users/me/.claude/settings.json": {
//!     "settingsPath": "C:\\Users\\me\\.claude\\settings.json",
//!     "hadStatusLine": true,
//!     "original": { "type": "command", "command": "node ...", "padding": 0 },
//!     "installedAt": 1791856373 } } }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// Format version of the chain file.
pub const CHAIN_VERSION: u32 = 1;

/// One replaced status line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chain {
    /// The settings file, as given at install time (for people reading the file).
    pub settings_path: String,
    /// Whether the settings file had a `statusLine` key at all.
    pub had_status_line: bool,
    /// The previous `statusLine` value, verbatim (`null` when there was none).
    #[serde(default)]
    pub original: Value,
    /// Unix seconds of the install that recorded it.
    #[serde(default)]
    pub installed_at: u64,
}

impl Chain {
    /// The command to run after the export: the original `statusLine.command`,
    /// when there was one and it is not empty.
    pub fn command(&self) -> Option<&str> {
        self.original
            .get("command")
            .and_then(Value::as_str)
            .filter(|c| !c.trim().is_empty())
    }
}

/// The whole chain file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainFile {
    /// [`CHAIN_VERSION`].
    pub v: u32,
    /// Entries by [`settings_key`].
    #[serde(default)]
    pub chains: BTreeMap<String, Chain>,
}

impl Default for ChainFile {
    fn default() -> Self {
        Self {
            v: CHAIN_VERSION,
            chains: BTreeMap::new(),
        }
    }
}

/// The key of a settings file: absolute, forward slashes, without a `\\?\`
/// prefix, and lower-cased on Windows, where paths are case-insensitive. Not
/// canonicalized: the file may not exist yet.
pub fn settings_key(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let text = abs.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut key = text.replace('\\', "/");
    // Drop "." segments so `./settings.json` and `settings.json` agree.
    while key.contains("/./") {
        key = key.replace("/./", "/");
    }
    if cfg!(windows) {
        key = key.to_lowercase();
    }
    key
}

/// Reads the chain file. A missing file is an empty record; a file that is
/// not a valid chain file is an error (`InvalidData`), so an install never
/// overwrites a record it cannot read.
pub fn load(path: &Path) -> io::Result<ChainFile> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ChainFile::default()),
        Err(e) => Err(e),
    }
}

/// The entry for one settings file, for the status line runtime: any problem
/// reading the record means there is nothing to chain.
pub fn lookup(chain_file: &Path, settings: &Path) -> Option<Chain> {
    load(chain_file)
        .ok()?
        .chains
        .remove(&settings_key(settings))
}

/// Writes the chain file through a temp file and a rename, creating its folder.
pub fn save(path: &Path, file: &ChainFile) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(file).map_err(io::Error::other)?;
    bytes.push(b'\n');
    cc_core::fsutil::write_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_ignore_separators_dots_and_case_on_windows() {
        let a = settings_key(Path::new(r"C:\Users\Me\.claude\settings.json"));
        let b = settings_key(Path::new(r"\\?\C:\Users\Me\.claude\.\settings.json"));
        assert_eq!(a, b);
        if cfg!(windows) {
            assert_eq!(a, "c:/users/me/.claude/settings.json");
            assert_eq!(
                a,
                settings_key(Path::new("c:/USERS/me/.claude/settings.json"))
            );
        }
        assert!(settings_key(Path::new("rel.json")).ends_with("/rel.json"));
    }

    #[test]
    fn round_trips_and_keeps_the_original_verbatim() {
        let root = crate::testutil::TestDir::new("chain");
        let dir = root.join("data");
        let path = dir.join("statusline-chain.json");
        assert_eq!(load(&path).unwrap(), ChainFile::default());
        let original = json!({ "type": "command", "command": "node \"C:\\x\\s.js\"", "padding": 0, "zzz": [1, 2] });
        let mut file = ChainFile::default();
        file.chains.insert(
            settings_key(Path::new("C:/a/settings.json")),
            Chain {
                settings_path: "C:/a/settings.json".into(),
                had_status_line: true,
                original: original.clone(),
                installed_at: 1,
            },
        );
        save(&path, &file).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back, file);
        let chain = lookup(&path, Path::new(r"C:\a\settings.json")).unwrap();
        assert_eq!(chain.original, original);
        assert_eq!(chain.command(), Some("node \"C:\\x\\s.js\""));
        assert!(lookup(&path, Path::new("C:/b/settings.json")).is_none());

        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(lookup(&path, Path::new("C:/a/settings.json")).is_none());
    }

    #[test]
    fn an_absent_or_empty_original_chains_nothing() {
        let mut c = Chain {
            settings_path: String::new(),
            had_status_line: false,
            original: Value::Null,
            installed_at: 0,
        };
        assert_eq!(c.command(), None);
        c.original = json!({ "type": "command", "command": "  " });
        assert_eq!(c.command(), None);
    }
}
