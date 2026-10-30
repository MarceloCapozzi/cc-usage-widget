//! The publisher's config file, `<data_dir>/publish.json` by default:
//!
//! ```json
//! {
//!   "alias": "lab-01",
//!   "servers": ["wss://connect.ngs.global", "tls://connect.ngs.global"],
//!   "bucket": "cc-sessions",
//!   "creds": "/home/me/.local/share/cc-usage/nats/cc-sessions.pub-lab-01.creds",
//!   "publisher_key": "/home/me/.local/share/cc-usage/keys/publisher.key",
//!   "viewer_pub": "/home/me/.local/share/cc-usage/keys/viewer.pub"
//! }
//! ```
//!
//! Only `alias` is required. `servers` defaults to
//! [`cc_remote::DEFAULT_SERVERS`], WebSocket first (port 443 gets through
//! corporate networks), then native TLS; each URL carries the transport,
//! the host and an optional port, and only `tls://` and `wss://` are
//! accepted (plaintext `nats://` and `ws://` are refused, see
//! [`cc_remote::validate_servers`]); `bucket` to
//! [`cc_remote::DEFAULT_BUCKET`], `cc-sessions` (the keys this machine
//! writes are `$KV.<bucket>.<alias>.>`; it must be the viewer's bucket);
//! `creds` to
//! `<data_dir>/nats/cc-sessions.pub-<alias>.creds`; `publisher_key` and
//! `viewer_pub` (this machine's private key and the viewer's public key,
//! which every value is sealed with, see [`crate::sealing`]) to
//! `<data_dir>/keys/publisher.key` and `<data_dir>/keys/viewer.pub`. Paths
//! may start with `~/` or `<data_dir>/`; relative
//! paths are resolved against the config file's folder. `log_file` (bool)
//! turns the rotating `<data_dir>/cc-publish.log` on or off; it defaults to on
//! for Windows (no journal there) and off elsewhere. Any other member is
//! rejected as a typo.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name of the config inside the data directory.
pub const CONFIG_FILE: &str = "publish.json";

/// Config files larger than this are rejected unread.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// Why a config could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read config {}: {source}", path.display())]
    Read {
        /// The config file.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is larger than any sensible config.
    #[error("config {} is too large", .0.display())]
    TooLarge(PathBuf),
    /// The JSON is malformed or has unknown fields.
    #[error("invalid config: {0}")]
    Parse(String),
    /// The alias is not a valid machine alias.
    #[error("invalid alias {alias:?}: {source}")]
    Alias {
        /// The rejected alias.
        alias: String,
        /// Why it was rejected.
        #[source]
        source: cc_envelope::AliasError,
    },
    /// The server list is empty or holds an unsupported URL.
    #[error("invalid servers: {0}")]
    Servers(String),
    /// The bucket name is not one JetStream accepts.
    #[error("invalid bucket: {0}")]
    Bucket(String),
}

/// The file as written on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    servers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    creds: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publisher_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    viewer_pub: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    log_file: Option<bool>,
}

/// A validated config with every default filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Machine alias: the first token of every KV key this machine writes.
    pub alias: String,
    /// Server URLs, tried in order by transport.
    pub servers: Vec<String>,
    /// The KV bucket it writes to, the viewer's.
    pub bucket: String,
    /// The publisher user's `.creds` file.
    pub creds: PathBuf,
    /// This machine's private key file, which seals every value.
    pub publisher_key: PathBuf,
    /// The viewer's public key file every value is sealed to.
    pub viewer_pub: PathBuf,
    /// Whether to also log to `<data_dir>/cc-publish.log`.
    pub log_file: bool,
    /// The file this config was read from (or will be written to).
    pub path: PathBuf,
}

impl Config {
    /// `<data_dir>/publish.json`.
    pub fn default_path() -> PathBuf {
        cc_core::paths::data_dir().join(CONFIG_FILE)
    }

    /// Reads and validates a config file.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let read = |source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        };
        let len = std::fs::metadata(path).map_err(read)?.len();
        if len > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge(path.to_path_buf()));
        }
        let text = std::fs::read_to_string(path).map_err(read)?;
        Self::parse(&text, path)
    }

    /// Parses and validates config text that lives at `path` (used to resolve
    /// relative paths and recorded in [`Config::path`]).
    pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
        let file: ConfigFile = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|e| ConfigError::Parse(e.to_string()))?;
        let alias = file.alias.trim().to_string();
        cc_envelope::validate_alias(&alias).map_err(|source| ConfigError::Alias {
            alias: alias.clone(),
            source,
        })?;
        let servers: Vec<String> = match file.servers {
            Some(list) => list
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            None => cc_remote::default_servers(),
        };
        validate_servers(&servers)?;
        let bucket = match file.bucket {
            Some(b) => b.trim().to_string(),
            None => cc_remote::DEFAULT_BUCKET.to_string(),
        };
        cc_remote::validate_bucket(&bucket).map_err(|e| ConfigError::Bucket(config_message(e)))?;
        let base = path.parent().unwrap_or(Path::new("."));
        let creds = match file.creds.as_deref() {
            Some(p) => resolve(p, base),
            None => default_creds(&alias),
        };
        let publisher_key = match file.publisher_key.as_deref() {
            Some(p) => resolve(p, base),
            None => cc_core::paths::publisher_key_file(),
        };
        let viewer_pub = match file.viewer_pub.as_deref() {
            Some(p) => resolve(p, base),
            None => cc_core::paths::viewer_pub_file(),
        };
        Ok(Config {
            alias,
            servers,
            bucket,
            creds,
            publisher_key,
            viewer_pub,
            log_file: file.log_file.unwrap_or(cfg!(windows)),
            path: path.to_path_buf(),
        })
    }

    /// The config `cc-publish init` writes for `alias`: default servers and
    /// bucket and the default creds and key paths, spelled out.
    pub fn new_default(alias: &str, path: &Path) -> Result<Config, ConfigError> {
        cc_envelope::validate_alias(alias).map_err(|source| ConfigError::Alias {
            alias: alias.to_string(),
            source,
        })?;
        Ok(Config {
            alias: alias.to_string(),
            servers: cc_remote::default_servers(),
            bucket: cc_remote::DEFAULT_BUCKET.to_string(),
            creds: default_creds(alias),
            publisher_key: cc_core::paths::publisher_key_file(),
            viewer_pub: cc_core::paths::viewer_pub_file(),
            log_file: cfg!(windows),
            path: path.to_path_buf(),
        })
    }

    /// The config as pretty JSON, every field explicit.
    pub fn to_json(&self) -> String {
        let file = ConfigFile {
            alias: self.alias.clone(),
            servers: Some(self.servers.clone()),
            bucket: Some(self.bucket.clone()),
            creds: Some(self.creds.to_string_lossy().into_owned()),
            publisher_key: Some(self.publisher_key.to_string_lossy().into_owned()),
            viewer_pub: Some(self.viewer_pub.to_string_lossy().into_owned()),
            log_file: Some(self.log_file),
        };
        let mut text = serde_json::to_string_pretty(&file).unwrap_or_default();
        text.push('\n');
        text
    }

    /// The NATS user, `cc-sessions.pub-<alias>`.
    pub fn user(&self) -> String {
        publisher_user(&self.alias)
    }

    /// The cc-remote connection config for this publisher.
    pub fn remote(&self) -> cc_remote::Config {
        cc_remote::Config {
            servers: self.servers.clone(),
            bucket: self.bucket.clone(),
            creds_path: self.creds.clone(),
            user: self.user(),
            name: format!("cc-publish-{}", self.alias),
        }
    }
}

/// The NATS user of a publisher: `cc-sessions.pub-<alias>`.
pub fn publisher_user(alias: &str) -> String {
    cc_core::nats_user(&format!("pub-{alias}"))
}

/// `<data_dir>/nats/cc-sessions.pub-<alias>.creds`.
pub fn default_creds(alias: &str) -> PathBuf {
    cc_core::paths::nats_dir().join(format!("{}.creds", publisher_user(alias)))
}

/// cc-remote's checks, the ones its `connect` applies.
fn validate_servers(servers: &[String]) -> Result<(), ConfigError> {
    cc_remote::validate_servers(servers).map_err(|e| ConfigError::Servers(config_message(e)))
}

/// The message of a cc-remote config error, without its "invalid config"
/// prefix (the [`ConfigError`] says what is invalid).
fn config_message(err: cc_remote::RemoteError) -> String {
    match err {
        cc_remote::RemoteError::Config(msg) => msg,
        other => other.to_string(),
    }
}

/// Expands `~/` and `<data_dir>/`, and resolves relative paths against `base`.
fn resolve(raw: &str, base: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw == "<data_dir>" {
        return cc_core::paths::data_dir();
    }
    for prefix in ["<data_dir>/", "<data_dir>\\"] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return cc_core::paths::data_dir().join(rest);
        }
    }
    for prefix in ["~/", "~\\"] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return cc_local::home_dir().join(rest);
        }
    }
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> PathBuf {
        // An absolute path on both Windows and Unix.
        if cfg!(windows) {
            PathBuf::from(format!("C:{p}"))
        } else {
            PathBuf::from(p)
        }
    }

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::parse(text, &abs("/etc/cc/publish.json"))
    }

    #[test]
    fn minimal_config_fills_the_defaults() {
        let c = parse(r#"{"alias":"lab-01"}"#).unwrap();
        assert_eq!(c.alias, "lab-01");
        assert_eq!(c.servers, cc_remote::default_servers());
        assert_eq!(c.servers[0], "wss://connect.ngs.global", "WebSocket first");
        assert_eq!(c.bucket, "cc-sessions");
        assert_eq!(c.user(), "cc-sessions.pub-lab-01");
        assert_eq!(
            c.creds,
            cc_core::paths::nats_dir().join("cc-sessions.pub-lab-01.creds")
        );
        assert_eq!(c.publisher_key, cc_core::paths::publisher_key_file());
        assert_eq!(c.viewer_pub, cc_core::paths::viewer_pub_file());
        assert_eq!(c.log_file, cfg!(windows));
        let r = c.remote();
        assert_eq!(r.user, "cc-sessions.pub-lab-01");
        assert_eq!(r.inbox_prefix(), "_INBOX.cc-sessions.pub-lab-01");
        assert_eq!(r.name, "cc-publish-lab-01");
        assert_eq!((r.servers, r.bucket), (c.servers, c.bucket));
    }

    #[test]
    fn full_config_is_taken_as_written() {
        let creds = abs("/x/u.creds");
        let text = serde_json::json!({
            "alias": "test",
            "servers": ["tls://a.example:4443", " wss://b.example "],
            "bucket": " other_bucket ",
            "creds": creds,
            "publisher_key": "keys/publisher.key",
            "viewer_pub": "<data_dir>/keys/viewer.pub",
            "log_file": true,
        })
        .to_string();
        let c = parse(&text).unwrap();
        assert_eq!(c.servers, ["tls://a.example:4443", "wss://b.example"]);
        assert_eq!(c.bucket, "other_bucket");
        assert_eq!(c.remote().bucket, "other_bucket");
        assert_eq!(c.creds, creds);
        assert_eq!(
            c.publisher_key,
            abs("/etc/cc").join("keys/publisher.key"),
            "relative to the config"
        );
        assert_eq!(
            c.viewer_pub,
            cc_core::paths::data_dir().join("keys/viewer.pub")
        );
        assert!(c.log_file);
    }

    #[test]
    fn placeholders_expand() {
        let c =
            parse(r#"{"alias":"a","creds":"<data_dir>/nats/x.creds","viewer_pub":"~/k"}"#).unwrap();
        assert_eq!(c.creds, cc_core::paths::data_dir().join("nats/x.creds"));
        assert_eq!(c.viewer_pub, cc_local::home_dir().join("k"));
    }

    #[test]
    fn bad_configs_are_rejected_with_a_reason() {
        assert!(matches!(parse("{"), Err(ConfigError::Parse(_))));
        assert!(
            matches!(parse("{}"), Err(ConfigError::Parse(_))),
            "alias is required"
        );
        assert!(
            matches!(
                parse(r#"{"alias":"a","server":["tls://x"]}"#),
                Err(ConfigError::Parse(_))
            ),
            "unknown fields are typos"
        );
        match parse(r#"{"alias":"a","key":"keys/k"}"#) {
            Err(ConfigError::Parse(msg)) => assert!(msg.contains("unknown field `key`"), "{msg}"),
            other => panic!("a key member: {other:?}"),
        }
        for bad in [
            "Lab",
            "lab.01",
            "",
            "-lab",
            "a_b",
            "abcdefghijklmnopqrstuvwxyz0123456",
        ] {
            let text = format!(r#"{{"alias":"{bad}"}}"#);
            assert!(
                matches!(parse(&text), Err(ConfigError::Alias { .. })),
                "{bad:?}"
            );
        }
        assert!(matches!(
            parse(r#"{"alias":"a","servers":[]}"#),
            Err(ConfigError::Servers(_))
        ));
        assert!(matches!(
            parse(r#"{"alias":"a","servers":["https://x"]}"#),
            Err(ConfigError::Servers(_))
        ));
        assert!(matches!(
            parse(r#"{"alias":"a","servers":["wss://"]}"#),
            Err(ConfigError::Servers(_))
        ));
        for plaintext in ["nats://x:4222", "ws://x", "NATS://x"] {
            let text = format!(r#"{{"alias":"a","servers":["tls://ok","{plaintext}"]}}"#);
            match parse(&text) {
                Err(ConfigError::Servers(msg)) => assert!(msg.contains("plaintext"), "{msg}"),
                other => panic!("{plaintext}: {other:?}"),
            }
        }
        assert!(matches!(
            parse(r#"{"alias":"a","servers":["tls://:4222"]}"#),
            Err(ConfigError::Servers(_))
        ));
        for bad in ["", "cc.sessions", "a b", "x>"] {
            let text = format!(r#"{{"alias":"a","bucket":"{bad}"}}"#);
            match parse(&text) {
                Err(ConfigError::Bucket(msg)) => assert!(msg.starts_with("bucket"), "{msg}"),
                other => panic!("{bad:?}: {other:?}"),
            }
        }
        assert!(
            matches!(
                parse(r#"{"alias":"a","buckets":"x"}"#),
                Err(ConfigError::Parse(_))
            ),
            "a misspelt bucket is a typo"
        );
    }

    #[test]
    fn written_config_reads_back_identically() {
        let path = abs("/etc/cc/publish.json");
        let c = Config::new_default("lab-01", &path).unwrap();
        let back = Config::parse(&c.to_json(), &path).unwrap();
        assert_eq!(back, c);
        assert!(c.to_json().contains(r#""bucket": "cc-sessions""#));
        assert!(Config::new_default("Bad", &path).is_err());
    }

    #[test]
    fn load_reports_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let err = Config::load(&dir.path().join("nope.json")).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }));
        let p = dir.path().join("publish.json");
        std::fs::write(&p, "\u{feff}{\"alias\":\"x1\"}").unwrap();
        assert_eq!(Config::load(&p).unwrap().alias, "x1");
    }
}
