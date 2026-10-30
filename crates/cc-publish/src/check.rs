//! `cc-publish check`: validates a configured machine end to end and prints
//! one `PASS`, `WARN` or `FAIL` line per check. Any `FAIL` makes the command
//! exit non-zero; `WARN` lines are advice (the local sources, a transport
//! that did not connect while another did).
//!
//! It opens one connection at a time and closes it before returning. The only
//! key written is the probe `<alias>.check`, deleted right after.

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use cc_envelope::{PublicKeys, Sealer, SecretKeys};

use crate::config::Config;
use crate::outbox::host_payload;
use crate::sealing::{PUBLISHER_KEY_HINT, VIEWER_PUB_HINT};
use crate::util::{now_secs, unix_mode};

/// Outcome of one check line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The check passed.
    Pass,
    /// Worth knowing, does not fail the run.
    Warn,
    /// The machine is not ready.
    Fail,
}

/// Collects check lines and prints them as they come.
#[derive(Debug, Default)]
pub struct Report {
    lines: Vec<(Verdict, String)>,
    quiet: bool,
}

impl Report {
    /// A report that prints each line to stdout.
    pub fn new() -> Self {
        Self::default()
    }

    /// A report that prints nothing (tests).
    pub fn quiet() -> Self {
        Report {
            lines: Vec::new(),
            quiet: true,
        }
    }

    /// Records (and prints) one line.
    pub fn line(&mut self, verdict: Verdict, label: &str, detail: impl AsRef<str>) {
        let tag = match verdict {
            Verdict::Pass => "PASS",
            Verdict::Warn => "WARN",
            Verdict::Fail => "FAIL",
        };
        let text = format!("{label}: {}", detail.as_ref());
        if !self.quiet {
            println!("{tag}  {text}");
        }
        self.lines.push((verdict, text));
    }

    /// Number of `FAIL` lines.
    pub fn failures(&self) -> usize {
        self.lines
            .iter()
            .filter(|(v, _)| *v == Verdict::Fail)
            .count()
    }

    /// All lines so far.
    pub fn lines(&self) -> &[(Verdict, String)] {
        &self.lines
    }
}

/// The `name` claim of the user JWT in a `.creds` file, i.e. the NATS user
/// the file authenticates as. Reads nothing but the JWT block (the seed is
/// never parsed) and returns `None` if the file does not look like creds.
pub fn creds_user_name(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 64 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    lines.find(|l| l.contains("BEGIN NATS USER JWT"))?;
    let jwt = lines.map(str::trim).find(|l| !l.is_empty())?.to_string();
    drop(text);
    let claims = jwt.split('.').nth(1)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    value.get("name")?.as_str().map(str::to_string)
}

/// Checks the files: alias, creds and the envelope keys (both key files
/// are required). Returns the sealer `run` would use, `None` when a key file is missing or broken.
pub fn check_files(config: &Config, report: &mut Report) -> Option<Sealer> {
    match cc_envelope::validate_alias(&config.alias) {
        Ok(()) => report.line(
            Verdict::Pass,
            "alias",
            format!("{} (user {})", config.alias, config.user()),
        ),
        Err(err) => report.line(Verdict::Fail, "alias", format!("{:?}: {err}", config.alias)),
    }

    let creds = &config.creds;
    if creds.is_file() {
        report.line(Verdict::Pass, "creds file", creds.display().to_string());
        match unix_mode(creds) {
            Some(mode) if mode & 0o004 != 0 => report.line(
                Verdict::Fail,
                "creds permissions",
                format!(
                    "{mode:o} is world-readable; run: chmod 600 {}",
                    creds.display()
                ),
            ),
            Some(mode) if mode & 0o070 != 0 => report.line(
                Verdict::Warn,
                "creds permissions",
                format!("{mode:o} is group-accessible; chmod 600 is recommended"),
            ),
            Some(mode) => report.line(Verdict::Pass, "creds permissions", format!("{mode:o}")),
            None => {}
        }
        match creds_user_name(creds) {
            Some(name) if name == config.user() => report.line(Verdict::Pass, "creds user", name),
            Some(name) => report.line(
                Verdict::Fail,
                "creds user",
                format!("the file is for {name}, expected {}", config.user()),
            ),
            None => report.line(
                Verdict::Warn,
                "creds user",
                "could not read the user name from the file",
            ),
        }
    } else {
        report.line(
            Verdict::Fail,
            "creds file",
            format!("{} not found", creds.display()),
        );
    }

    // The envelope keys: this machine's private key and the viewer's public
    // key. Both are required.
    let pk = &config.publisher_key;
    let sender = if pk.is_file() {
        match SecretKeys::load(pk) {
            Ok(keys) => {
                report.line(
                    Verdict::Pass,
                    "publisher key",
                    format!(
                        "{} (key id {}, {} key(s))",
                        pk.display(),
                        keys.current().kid(),
                        keys.keys().len()
                    ),
                );
                check_private_mode(report, "publisher key permissions", pk);
                Some(keys)
            }
            Err(err) => {
                report.line(Verdict::Fail, "publisher key", err.to_string());
                None
            }
        }
    } else {
        report.line(
            Verdict::Fail,
            "publisher key",
            format!("{} missing: {PUBLISHER_KEY_HINT}", pk.display()),
        );
        None
    };
    let vp = &config.viewer_pub;
    let recipient = if vp.is_file() {
        match PublicKeys::load(vp) {
            Ok(keys) => {
                report.line(
                    Verdict::Pass,
                    "viewer.pub",
                    format!(
                        "{} present (seals to viewer key {}, {} key(s))",
                        vp.display(),
                        keys.current().kid(),
                        keys.keys().len()
                    ),
                );
                Some(keys)
            }
            Err(err) => {
                report.line(Verdict::Fail, "viewer.pub", err.to_string());
                None
            }
        }
    } else {
        report.line(
            Verdict::Fail,
            "viewer.pub",
            format!("{} missing: {VIEWER_PUB_HINT}", vp.display()),
        );
        None
    };
    let (sender, recipient) = (sender?, recipient?);
    let sealer = Sealer::new(sender, recipient);
    report.line(
        Verdict::Pass,
        "envelope",
        format!("{}; only the viewer can open it", sealer.describe()),
    );
    Some(sealer)
}

/// A `FAIL` line unless only the owner can open `path` (Unix only).
fn check_private_mode(report: &mut Report, label: &str, path: &Path) {
    match unix_mode(path) {
        Some(mode) if mode & 0o077 == 0 => report.line(Verdict::Pass, label, format!("{mode:o}")),
        Some(mode) => report.line(
            Verdict::Fail,
            label,
            format!("{mode:o}; must be 600: chmod 600 {}", path.display()),
        ),
        None => {}
    }
}

/// Advice about the local sources. Never fails the check: a machine can be
/// set up before Claude Code is.
pub fn check_local(report: &mut Report) {
    match cc_local::agents::find_claude() {
        Some(path) => report.line(Verdict::Pass, "claude CLI", path.display().to_string()),
        None => report.line(
            Verdict::Warn,
            "claude CLI",
            "not found on PATH or in ~/.local/bin; only registry files will be used",
        ),
    }
    let settings = cc_local::claude_dir().join("settings.json");
    let hooked = std::fs::read_to_string(&settings)
        .map(|s| s.contains("cc-hook"))
        .unwrap_or(false);
    if hooked {
        report.line(
            Verdict::Pass,
            "hooks",
            format!("cc-hook found in {}", settings.display()),
        );
    } else {
        report.line(
            Verdict::Warn,
            "hooks",
            format!(
                "cc-hook is not in {}; run `cc-hook install` for live states",
                settings.display()
            ),
        );
    }
}

fn ms(d: Duration) -> String {
    format!("{} ms", d.as_millis())
}

/// Transport groups of the configured servers, in first-seen order
/// (WebSocket and native TLS cannot share one connection pool).
fn transports(servers: &[String]) -> Vec<Vec<String>> {
    let is_ws = |s: &str| {
        let l = s.to_ascii_lowercase();
        l.starts_with("ws://") || l.starts_with("wss://")
    };
    let mut groups: Vec<(bool, Vec<String>)> = Vec::new();
    for s in servers {
        let ws = is_ws(s);
        match groups.iter_mut().find(|(k, _)| *k == ws) {
            Some((_, g)) => g.push(s.clone()),
            None => groups.push((ws, vec![s.clone()])),
        }
    }
    groups.into_iter().map(|(_, g)| g).collect()
}

/// Connects (each transport in order until one works), binds the bucket,
/// puts and deletes the probe key, and closes the connection.
pub async fn check_network(config: &Config, sealer: &Sealer, report: &mut Report) {
    let groups = transports(&config.servers);
    let mut client = None;
    let mut failed = Vec::new();
    for group in &groups {
        let mut remote = config.remote();
        remote.servers = group.clone();
        remote.name = format!("cc-publish-check-{}", config.alias);
        let t = Instant::now();
        match cc_remote::connect(remote).await {
            Ok(c) => {
                for (server, err) in failed.drain(..) {
                    report.line(Verdict::Warn, "connect", format!("{server}: {err}"));
                }
                report.line(
                    Verdict::Pass,
                    "connect",
                    format!(
                        "{} with inbox prefix {} in {}",
                        group.join(", "),
                        cc_core::inbox_prefix(&config.user()),
                        ms(t.elapsed())
                    ),
                );
                client = Some(c);
                break;
            }
            Err(err) => failed.push((group.join(", "), err.to_string())),
        }
    }
    let Some(client) = client else {
        for (server, err) in failed {
            report.line(Verdict::Fail, "connect", format!("{server}: {err}"));
        }
        return;
    };

    let t = Instant::now();
    let publisher = match cc_remote::Publisher::bind(&client, &config.bucket).await {
        Ok(p) => {
            report.line(
                Verdict::Pass,
                "bind",
                format!("bucket {} in {}", config.bucket, ms(t.elapsed())),
            );
            p
        }
        Err(err) => {
            report.line(Verdict::Fail, "bind", err.to_string());
            client.close().await;
            return;
        }
    };

    let key = format!("{}.check", config.alias);
    match sealer.seal(&host_payload(&config.alias, now_secs()), &key) {
        Ok(bytes) => {
            let size = bytes.len();
            let t = Instant::now();
            match publisher.put(&key, bytes).await {
                Ok(rev) => report.line(
                    Verdict::Pass,
                    "put probe",
                    format!(
                        "{key} ({size} bytes sealed, revision {rev}) in {}",
                        ms(t.elapsed())
                    ),
                ),
                Err(err) => report.line(Verdict::Fail, "put probe", format!("{key}: {err}")),
            }
            let t = Instant::now();
            match publisher.delete(&key).await {
                Ok(()) => report.line(
                    Verdict::Pass,
                    "delete probe",
                    format!("{key} in {}", ms(t.elapsed())),
                ),
                Err(err) => report.line(Verdict::Fail, "delete probe", format!("{key}: {err}")),
            }
        }
        Err(err) => report.line(Verdict::Fail, "seal probe", err.to_string()),
    }
    client.close().await;
}

/// A `FAIL` line for each folder override (`CC_USAGE_DATA_DIR`,
/// `CLAUDE_CONFIG_DIR`) set to a relative path, which every binary ignores
/// ([`cc_core::paths::relative_overrides`]); nothing when there is none.
pub fn check_overrides(report: &mut Report) {
    check_overrides_from(|name| std::env::var_os(name), report);
}

/// [`check_overrides`] with the environment read through `var`.
fn check_overrides_from(var: impl Fn(&str) -> Option<OsString>, report: &mut Report) {
    for (name, value) in cc_core::paths::relative_overrides_from(&var) {
        // What the binaries fall back to: the data folder is never missing
        // (the temp folder as a last resort), Claude Code's folder can be.
        let used = if name == cc_core::paths::DATA_DIR_ENV {
            let dir = cc_core::paths::data_dir_from(&var)
                .unwrap_or_else(|| std::env::temp_dir().join(cc_core::paths::APP_DIR));
            format!("{} is used", dir.display())
        } else {
            match cc_core::paths::claude_config_dir_from(&var) {
                Some(dir) => format!("{} is used", dir.display()),
                None => "no Claude Code folder is found".to_string(),
            }
        };
        report.line(
            Verdict::Fail,
            name,
            format!(
                "{} is a relative path, so it is ignored and {used}; \
                 set an absolute path or unset it",
                value.display()
            ),
        );
    }
}

/// Runs every check for the config at `path`, recording them in `report`.
pub fn check(path: &Path, report: &mut Report) {
    check_overrides(report);
    let config = match Config::load(path) {
        Ok(c) => {
            report.line(Verdict::Pass, "config", path.display().to_string());
            c
        }
        Err(err) => {
            report.line(Verdict::Fail, "config", err.to_string());
            return;
        }
    };
    let sealer = check_files(&config, report);
    check_local(report);
    if let Some(sealer) = sealer.filter(|_| config.creds.is_file()) {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(check_network(&config, &sealer, report)),
            Err(err) => report.line(Verdict::Fail, "runtime", err.to_string()),
        }
    } else {
        report.line(
            Verdict::Fail,
            "network",
            "skipped: fix the creds and key first",
        );
    }
    if let Some(state) =
        crate::state::PublishState::load(&crate::state::PublishState::default_path())
    {
        if state.is_alive(now_secs()) {
            report.line(
                Verdict::Pass,
                "publisher",
                format!("running (pid {}), {} session(s)", state.pid, state.sessions),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_creds(name: &str) -> String {
        let b64 = |v: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        let jwt = format!(
            "{}.{}.c2ln",
            b64(serde_json::json!({"typ":"JWT","alg":"ed25519-nkey"})),
            b64(serde_json::json!({"name": name, "sub": "UABC"}))
        );
        format!(
            "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
             -----BEGIN USER NKEY SEED-----\nSUAFAKEFAKEFAKE\n------END USER NKEY SEED------\n"
        )
    }

    #[test]
    fn the_creds_user_is_read_from_the_jwt() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("u.creds");
        std::fs::write(&p, fake_creds("cc-sessions.pub-lab-01")).unwrap();
        assert_eq!(
            creds_user_name(&p).as_deref(),
            Some("cc-sessions.pub-lab-01")
        );
        std::fs::write(&p, "not creds").unwrap();
        assert_eq!(creds_user_name(&p), None);
        assert_eq!(creds_user_name(&dir.path().join("missing")), None);
    }

    /// A configured machine in `dir`: creds and both key files.
    fn setup(dir: &Path) -> Config {
        let creds = dir.join("cc-sessions.pub-lab-01.creds");
        std::fs::write(&creds, fake_creds("cc-sessions.pub-lab-01")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = Config {
            alias: "lab-01".into(),
            servers: vec!["wss://connect.ngs.global".into()],
            bucket: cc_remote::DEFAULT_BUCKET.into(),
            creds,
            publisher_key: dir.join("keys").join("publisher.key"),
            viewer_pub: dir.join("keys").join("viewer.pub"),
            log_file: false,
            path: dir.join("publish.json"),
        };
        crate::keygen::publisher_keygen(
            &config.publisher_key,
            "p1",
            crate::keygen::IfExists::Refuse,
        )
        .unwrap();
        SecretKeys::generate("k1")
            .unwrap()
            .public_keys()
            .save(&config.viewer_pub)
            .unwrap();
        config
    }

    fn has(r: &Report, v: Verdict, text: &str) -> bool {
        r.lines()
            .iter()
            .any(|(verdict, line)| *verdict == v && line.contains(text))
    }

    #[test]
    fn file_checks_pass_on_a_good_setup_and_fail_on_a_bad_one() {
        let dir = tempfile::tempdir().unwrap();
        let config = setup(dir.path());
        let mut report = Report::quiet();
        assert!(check_files(&config, &mut report).is_some());
        assert_eq!(report.failures(), 0, "{:?}", report.lines());
        assert!(
            has(
                &report,
                Verdict::Pass,
                "envelope: publisher key p1, sealed to viewer key k1;"
            ),
            "{:?}",
            report.lines()
        );
        assert!(
            has(&report, Verdict::Pass, "viewer.pub: "),
            "{:?}",
            report.lines()
        );

        let wrong_user = Config {
            alias: "lab-02".into(),
            ..config.clone()
        };
        let mut report = Report::quiet();
        check_files(&wrong_user, &mut report);
        assert_eq!(report.failures(), 1, "{:?}", report.lines());

        let missing = Config {
            creds: dir.path().join("nope.creds"),
            publisher_key: dir.path().join("nokey"),
            ..config.clone()
        };
        let mut report = Report::quiet();
        assert!(check_files(&missing, &mut report).is_none());
        assert_eq!(report.failures(), 2, "{:?}", report.lines());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config.creds, std::fs::Permissions::from_mode(0o644))
                .unwrap();
            std::fs::set_permissions(
                &config.publisher_key,
                std::fs::Permissions::from_mode(0o640),
            )
            .unwrap();
            let mut report = Report::quiet();
            check_files(&config, &mut report);
            assert_eq!(report.failures(), 2, "{:?}", report.lines());
        }
    }

    #[test]
    fn missing_or_broken_key_files_fail_and_name_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let config = setup(dir.path());

        std::fs::remove_file(&config.viewer_pub).unwrap();
        let mut report = Report::quiet();
        assert!(check_files(&config, &mut report).is_none());
        assert_eq!(report.failures(), 1, "{:?}", report.lines());
        assert!(
            has(&report, Verdict::Fail, "copy viewer.pub from the viewer"),
            "{:?}",
            report.lines()
        );

        std::fs::remove_file(&config.publisher_key).unwrap();
        let mut report = Report::quiet();
        assert!(check_files(&config, &mut report).is_none());
        assert_eq!(report.failures(), 2, "{:?}", report.lines());
        assert!(
            has(&report, Verdict::Fail, "cc-publish keygen"),
            "{:?}",
            report.lines()
        );

        let dir2 = tempfile::tempdir().unwrap();
        let config = setup(dir2.path());
        std::fs::write(&config.viewer_pub, "junk").unwrap();
        let mut report = Report::quiet();
        assert!(check_files(&config, &mut report).is_none());
        assert_eq!(report.failures(), 1, "{:?}", report.lines());
    }

    #[test]
    fn transports_keep_their_first_seen_order() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            transports(&s(&["wss://a", "tls://b", "wss://c"])),
            vec![s(&["wss://a", "wss://c"]), s(&["tls://b"])]
        );
    }

    #[test]
    fn a_relative_folder_override_fails_and_names_the_folder_used() {
        let base = if cfg!(windows) {
            r"C:\Users\devbox"
        } else {
            "/home/devbox"
        };
        let env = |data: &'static str, claude: &'static str| {
            move |name: &str| -> Option<OsString> {
                match name {
                    "CC_USAGE_DATA_DIR" => Some(data.into()),
                    "CLAUDE_CONFIG_DIR" => Some(claude.into()),
                    "LOCALAPPDATA" | "XDG_DATA_HOME" | "USERPROFILE" | "HOME" => Some(base.into()),
                    _ => None,
                }
            }
        };

        let mut report = Report::quiet();
        check_overrides_from(env("data", ".claude"), &mut report);
        assert_eq!(report.failures(), 2, "{:?}", report.lines());
        let fallback = Path::new(base).join(cc_core::paths::APP_DIR);
        assert!(
            has(
                &report,
                Verdict::Fail,
                &format!(
                    "CC_USAGE_DATA_DIR: data is a relative path, so it is ignored and {} is used",
                    fallback.display()
                )
            ),
            "{:?}",
            report.lines()
        );
        assert!(
            has(
                &report,
                Verdict::Fail,
                "CLAUDE_CONFIG_DIR: .claude is a relative path"
            ),
            "{:?}",
            report.lines()
        );

        // Absolute or empty overrides add no line at all.
        let mut report = Report::quiet();
        check_overrides_from(env(base, ""), &mut report);
        assert!(report.lines().is_empty(), "{:?}", report.lines());
    }

    #[test]
    fn a_missing_config_fails_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let mut report = Report::quiet();
        check(&dir.path().join("publish.json"), &mut report);
        assert_eq!(report.failures(), 1);
        assert_eq!(report.lines().len(), 1, "nothing else is tried");
    }
}
