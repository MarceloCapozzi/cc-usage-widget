//! The connected path against a local NATS server with JetStream, opt in:
//!
//! ```text
//! nats-server -js
//! CC_TEST_NATS_URL=nats://127.0.0.1:4222 cargo test -p cc-publish --test local_nats -- --nocapture
//! ```
//!
//! Without `CC_TEST_NATS_URL` the two server tests print `skipped: …` and
//! pass, or fail when `CC_TEST_REQUIRE_ALL=1` (the Linux CI job sets both). The
//! URL must name a loopback address: the tests create and delete buckets
//! (`cc-sessions` among them), so they refuse any other server. No secrets
//! are needed: the server runs without authentication, and the creds files
//! hold a fresh user seed and a JWT that only carries the expected name.
//!
//! cc-remote connects only over `tls://` or `wss://`, so the tests put a
//! loopback front ([`Front`]) before the server that does what a NATS server
//! with TLS does: it relays the server's `INFO` line in clear text with
//! `tls_required` set, completes the TLS handshake with a certificate for
//! `127.0.0.1` and relays the decrypted stream to the server. The
//! certificate, its key and the CA that signed it are made when the tests
//! run ([`pki`]), and `SSL_CERT_FILE` makes that CA the only trusted root.
//! Cutting every connection through the front ([`Front::cut`]) forces the
//! clients to reconnect.
//!
//! - `the_front_certificate_verifies_against_the_test_ca`: the generated
//!   certificates and the front's TLS settings, without a server (it never
//!   skips).
//! - `kv_put_watch_and_delete_on_a_throwaway_bucket`: cc-remote's
//!   [`cc_remote::Publisher`] and [`cc_remote::Viewer`] on a bucket of their
//!   own, with the watch carrying on across a reconnect.
//! - `a_publisher_serves_across_a_reconnect`: the cc-publish binary's
//!   `run` (the connect loop, the first puts, the re-put after a reconnect
//!   and the stop) on the `cc-sessions` bucket, read back as a viewer.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_nats::jetstream::{self, kv};
use base64::Engine as _;
use cc_core::events::SpoolEvent;
use cc_envelope::{OpenKeys, SecretKeys, ViewerKeys};
use cc_remote::KvChange;
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const URL_ENV: &str = "CC_TEST_NATS_URL";
/// `1` (CI sets it) makes a missing [`URL_ENV`] fail the tests.
const REQUIRE_ALL_ENV: &str = "CC_TEST_REQUIRE_ALL";
const BIN: &str = env!("CARGO_BIN_EXE_cc-publish");
const ALIAS: &str = "localtest";
const SESSION_ID: &str = "cc-publish-local-nats-0001";

/// How long a test waits for something the server or the publisher does.
const WAIT: Duration = Duration::from_secs(30);

/// The TLS front's certificates, made once per test process, so no key
/// material is in the repository or outlives the run.
struct TestPki {
    /// The root that signed [`TestPki::cert`], PEM: what [`setup`] writes to
    /// the file `SSL_CERT_FILE` names.
    ca_pem: String,
    /// The front's certificate, for `127.0.0.1` and `localhost`.
    cert: CertificateDer<'static>,
    /// The front's private key.
    key: PrivatePkcs8KeyDer<'static>,
}

/// The process's [`TestPki`]: ECDSA P-256 keys from rcgen (with ring), a CA
/// and a server certificate it signed, valid long past any test run.
fn pki() -> &'static TestPki {
    use rcgen::{
        BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
        IsCa, Issuer, KeyPair, KeyUsagePurpose,
    };
    static PKI: OnceLock<TestPki> = OnceLock::new();
    PKI.get_or_init(|| {
        let dn = |cn: &str| {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, cn);
            dn
        };
        let mut ca = CertificateParams::default();
        ca.distinguished_name = dn("cc-usage local NATS test CA");
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_key = KeyPair::generate().unwrap();
        let ca_pem = ca.self_signed(&ca_key).unwrap().pem();
        let issuer = Issuer::new(ca, ca_key);

        let mut server =
            CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()]).unwrap();
        server.distinguished_name = dn("127.0.0.1");
        server.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        server.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        server.use_authority_key_identifier_extension = true;
        let key = KeyPair::generate().unwrap();
        let cert = server.signed_by(&key, &issuer).unwrap();
        TestPki {
            ca_pem,
            cert: cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(key.serialize_der()),
        }
    })
}

/// The server's `host:port` from [`URL_ENV`] and the file holding
/// [`pki`]'s CA, or `None` (printing why the test is skipped) when the
/// variable is unset. Panics on a URL that is not a plain `nats://` URL of a
/// loopback address.
///
/// The first call also makes that CA the only root this process and its
/// children trust: rustls-native-certs (under async-nats) reads
/// `SSL_CERT_FILE` on every TLS connect. Every test calls this first, and
/// the others wait for the first, so no thread reads the environment while
/// it is written.
fn setup() -> Option<(String, PathBuf)> {
    static SERVER: OnceLock<Option<String>> = OnceLock::new();
    let ca = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cc-publish-local-nats-ca.pem");
    let server = SERVER.get_or_init(|| {
        let server = local_server()?;
        std::fs::write(&ca, &pki().ca_pem).unwrap();
        std::env::set_var("SSL_CERT_FILE", &ca);
        Some(server)
    });
    match server {
        Some(server) => Some((server.clone(), ca)),
        None => {
            let hint =
                format!("set {URL_ENV} (e.g. nats://127.0.0.1:4222) to a local `nats-server -js`");
            if std::env::var_os(REQUIRE_ALL_ENV).is_some_and(|v| v == "1") {
                panic!("{hint}: {REQUIRE_ALL_ENV}=1 requires this test to run");
            }
            eprintln!("skipped: {hint}");
            None
        }
    }
}

fn local_server() -> Option<String> {
    let url = std::env::var(URL_ENV)
        .ok()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())?;
    let addr = url
        .strip_prefix("nats://")
        .unwrap_or_else(|| panic!("{URL_ENV}={url}: use nats://<loopback>:<port>"));
    let host = addr.rsplit_once(':').map_or(addr, |(host, _)| host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    assert!(loopback, "{URL_ENV}={url}: the tests create and delete buckets, so only a loopback server is accepted");
    Some(addr.to_string())
}

/// A creds file for `user` as cc-publish checks it (a JWT whose `name` is
/// `user`, a user seed) and async-nats reads it (the seed signs the
/// server's nonce). The local server has no authentication and ignores both.
fn write_creds(path: &Path, user: &str) {
    let b64 = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
    };
    let jwt = format!(
        "{}.{}.bG9jYWw",
        b64(serde_json::json!({"typ": "JWT", "alg": "ed25519-nkey"})),
        b64(serde_json::json!({"name": user, "sub": "ULOCALTEST"}))
    );
    let seed = nkeys::KeyPair::new_user().seed().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
             -----BEGIN USER NKEY SEED-----\n{seed}\n------END USER NKEY SEED------\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// The TLS front before the local server.
struct Front {
    addr: SocketAddr,
    cut: broadcast::Sender<()>,
}

impl Front {
    /// Listens on a free loopback port and relays to `upstream`. Must run
    /// inside a Tokio runtime.
    async fn start(upstream: String) -> Front {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(server_tls());
        let (cut, _) = broadcast::channel(4);
        let cuts = cut.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let upstream = upstream.clone();
                let acceptor = acceptor.clone();
                let mut cut = cuts.subscribe();
                tokio::spawn(async move {
                    tokio::select! {
                        relayed = relay(client, &upstream, acceptor) => match relayed {
                            // A client that closes without a TLS close_notify.
                            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {}
                            Err(err) => eprintln!("front: {err}"),
                            Ok(()) => {}
                        },
                        _ = cut.recv() => {}
                    }
                });
            }
        });
        Front { addr, cut }
    }

    /// The URL clients connect to.
    fn url(&self) -> String {
        format!("tls://{}", self.addr)
    }

    /// Drops every connection open through the front; new ones are accepted.
    fn cut(&self) {
        let _ = self.cut.send(());
    }
}

/// The front's TLS settings: [`pki`]'s certificate and key, with ring.
fn server_tls() -> Arc<rustls::ServerConfig> {
    let pki = pki();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![pki.cert.clone()],
        PrivateKeyDer::Pkcs8(pki.key.clone_key()),
    )
    .unwrap();
    Arc::new(config)
}

/// [`pki`] and [`server_tls`] without a server, so they run wherever the
/// other tests skip: a client that trusts only the CA, read back from its
/// PEM as rustls-native-certs reads `SSL_CERT_FILE`, completes a handshake
/// with the front for both names the certificate carries, and refuses one
/// it does not carry.
#[test]
fn the_front_certificate_verifies_against_the_test_ca() {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::ServerName;
    let roots: Vec<_> = CertificateDer::pem_slice_iter(pki().ca_pem.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(roots.len(), 1, "one CA certificate in the PEM");
    let mut store = rustls::RootCertStore::empty();
    store.add_parsable_certificates(roots);
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(store)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let acceptor = tokio_rustls::TlsAcceptor::from(server_tls());

    runtime().block_on(async {
        for (name, trusted) in [
            ("127.0.0.1", true),
            ("localhost", true),
            ("example.com", false),
        ] {
            let (client_end, server_end) = tokio::io::duplex(16 * 1024);
            let acceptor = acceptor.clone();
            let server = tokio::spawn(async move { acceptor.accept(server_end).await.map(drop) });
            let server_name = ServerName::try_from(name).unwrap().to_owned();
            let client = connector.connect(server_name, client_end).await;
            assert_eq!(
                client.is_ok(),
                trusted,
                "{name}: {:?}",
                client.as_ref().err()
            );
            if trusted {
                server
                    .await
                    .unwrap()
                    .expect("the front completes the handshake");
            }
        }
    });
}

/// One client connection: the server's `INFO` in clear text with
/// `tls_required` set, the TLS handshake, then both directions.
async fn relay(
    client: TcpStream,
    upstream: &str,
    acceptor: tokio_rustls::TlsAcceptor,
) -> io::Result<()> {
    let mut server = BufReader::new(TcpStream::connect(upstream).await?);
    let mut line = String::new();
    server.read_line(&mut line).await?;
    let info = line
        .strip_prefix("INFO ")
        .ok_or_else(|| io::Error::other(format!("expected INFO from the server, got {line:?}")))?;
    let mut info: serde_json::Value = serde_json::from_str(info.trim())?;
    info["tls_required"] = true.into();
    let mut client = client;
    client
        .write_all(format!("INFO {info}\r\n").as_bytes())
        .await?;
    let mut tls = acceptor.accept(client).await?;
    tokio::io::copy_bidirectional(&mut tls, &mut server).await?;
    Ok(())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// A direct connection to the local server (no front, no TLS) for setting
/// buckets up and reading revisions.
async fn admin(server: &str) -> jetstream::Context {
    let client = async_nats::connect(format!("nats://{server}"))
        .await
        .expect("connect to the local server");
    jetstream::new(client)
}

/// A bucket shaped like `cc-sessions` (infra/nats/cc-sessions.yaml), in memory.
async fn create_bucket(js: &jetstream::Context, name: &str) -> kv::Store {
    let _ = js.delete_key_value(name).await;
    js.create_key_value(kv::Config {
        bucket: name.to_string(),
        history: 1,
        max_age: Duration::from_secs(180),
        max_bytes: 16 * 1024 * 1024,
        max_value_size: 4096,
        storage: jetstream::stream::StorageType::Memory,
        ..Default::default()
    })
    .await
    .expect("create the bucket")
}

fn remote_config(url: &str, creds: &Path, user: &str) -> cc_remote::Config {
    cc_remote::Config {
        servers: vec![url.to_string()],
        bucket: cc_core::BUCKET.into(),
        creds_path: creds.to_path_buf(),
        user: user.to_string(),
        name: format!("cc-publish-local-nats-{user}"),
    }
}

/// The next change on `watch`, within [`WAIT`].
async fn next_change(watch: &mut cc_remote::KvWatch) -> KvChange {
    tokio::time::timeout(WAIT, watch.next())
        .await
        .expect("a change within the wait")
        .unwrap_or_else(|| panic!("the watch ended: {:?}", watch.last_error()))
}

#[test]
fn kv_put_watch_and_delete_on_a_throwaway_bucket() {
    let Some((server, _)) = setup() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let pub_creds = tmp.path().join("pub.creds");
    let viewer_creds = tmp.path().join("viewer.creds");
    write_creds(&pub_creds, &cc_core::nats_user("pub-localtest"));
    write_creds(&viewer_creds, &cc_core::nats_user("viewer"));
    let bucket = format!(
        "cc-test-{}-{}",
        std::process::id(),
        cc_publish::util::now_secs() as u64
    );

    runtime().block_on(async {
        let js = admin(&server).await;
        create_bucket(&js, &bucket).await;
        let front = Front::start(server.clone()).await;

        let publisher_client = cc_remote::connect(remote_config(
            &front.url(),
            &pub_creds,
            &cc_core::nats_user("pub-localtest"),
        ))
        .await
        .expect("the publisher connects through TLS");
        assert!(!publisher_client.is_websocket());
        let publisher = cc_remote::Publisher::bind(&publisher_client, &bucket)
            .await
            .unwrap();
        let viewer_client = cc_remote::connect(remote_config(
            &front.url(),
            &viewer_creds,
            &cc_core::nats_user("viewer"),
        ))
        .await
        .expect("the viewer connects through TLS");
        let viewer = cc_remote::Viewer::bind(&viewer_client, &bucket)
            .await
            .unwrap();
        assert!(
            cc_remote::Viewer::bind(&viewer_client, "cc-test-no-such-bucket")
                .await
                .is_err()
        );

        // An empty bucket: the watch is current at once.
        let mut watch = viewer.watch_all().await.unwrap();
        assert!(watch.is_current());

        let first = publisher
            .put("localtest.s.0001", b"one".to_vec())
            .await
            .unwrap();
        match next_change(&mut watch).await {
            KvChange::Put {
                key,
                bytes,
                revision,
            } => {
                assert_eq!(
                    (key.as_str(), bytes.as_slice(), revision),
                    ("localtest.s.0001", &b"one"[..], first)
                );
            }
            other => panic!("expected the put, got {other:?}"),
        }
        assert_eq!(
            viewer.get("localtest.s.0001").await.unwrap().as_deref(),
            Some(&b"one"[..])
        );

        // Every connection drops; both clients reconnect and the watch
        // resumes after the last revision it delivered.
        front.cut();
        let deadline = Instant::now() + WAIT;
        let second = loop {
            match publisher.put("localtest.s.0002", b"two".to_vec()).await {
                Ok(revision) => break revision,
                Err(err) => {
                    assert!(
                        Instant::now() < deadline,
                        "no put after the reconnect: {err}"
                    );
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        };
        assert!(second > first);
        match next_change(&mut watch).await {
            KvChange::Put { key, revision, .. } => {
                assert_eq!((key.as_str(), revision), ("localtest.s.0002", second))
            }
            other => panic!("expected the put after the reconnect, got {other:?}"),
        }

        publisher.delete("localtest.s.0001").await.unwrap();
        assert_eq!(
            next_change(&mut watch).await,
            KvChange::Delete {
                key: "localtest.s.0001".into()
            }
        );
        assert_eq!(viewer.get("localtest.s.0001").await.unwrap(), None);
        let snapshot = viewer.snapshot().await.unwrap();
        assert_eq!(
            snapshot,
            [("localtest.s.0002".to_string(), b"two".to_vec())]
        );

        publisher_client.close().await;
        viewer_client.close().await;
        js.delete_key_value(&bucket)
            .await
            .expect("delete the throwaway bucket");
    });
}

/// The `cc-sessions` bucket on the local server, deleted when dropped.
struct SessionsBucket {
    server: String,
}

impl Drop for SessionsBucket {
    fn drop(&mut self) {
        let server = self.server.clone();
        // A runtime of its own: the test's may be gone or panicking.
        let _ = std::thread::spawn(move || {
            runtime().block_on(async {
                let _ = admin(&server).await.delete_key_value(cc_core::BUCKET).await;
            })
        })
        .join();
    }
}

/// A publisher process, killed when dropped unless it already exited.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Publisher {
    data: PathBuf,
    claude: PathBuf,
    home: PathBuf,
    empty_path: PathBuf,
    ca: PathBuf,
}

impl Publisher {
    /// `cc-publish --data-dir <data> <args>`, with nothing of this machine
    /// in reach: no `claude` on `PATH` (so no `claude agents`), the home
    /// folder and `CLAUDE_CONFIG_DIR` in the temp folder.
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.arg("--data-dir")
            .arg(&self.data)
            .args(args)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", &self.empty_path)
            .env("SSL_CERT_FILE", &self.ca)
            .env("CC_PUBLISH_LOG", "debug");
        c
    }

    fn cmd(&self, args: &[&str]) -> Output {
        let out = self.command(args).output().expect("run cc-publish");
        assert!(out.status.success(), "cc-publish {args:?}: {out:?}");
        out
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.data.join("cc-publish.log")).unwrap_or_default()
    }
}

/// Waits until `f` gives a value, failing after [`WAIT`] with `what`.
async fn wait_for<T>(what: &str, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = f().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[test]
fn a_publisher_serves_across_a_reconnect() {
    let Some((server, ca)) = setup() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let p = Publisher {
        data: tmp.path().join("data"),
        claude: tmp.path().join("claude"),
        home: tmp.path().join("home"),
        empty_path: tmp.path().join("bin"),
        ca,
    };
    for dir in [&p.data, &p.claude, &p.home, &p.empty_path] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let user = cc_core::nats_user(&format!("pub-{ALIAS}"));
    write_creds(&p.data.join("nats").join(format!("{user}.creds")), &user);
    let viewer_creds = tmp.path().join("viewer.creds");
    write_creds(&viewer_creds, &cc_core::nats_user("viewer"));

    // This machine's key pair and a viewer key pair, both in the data dir.
    p.cmd(&["keygen"]);
    p.cmd(&["viewer-keygen"]);
    let keys = ViewerKeys::new(
        SecretKeys::load(p.data.join("keys").join("viewer.key")).unwrap(),
        None,
    );
    let publisher_key = SecretKeys::load(p.data.join("keys").join("publisher.key")).unwrap();
    keys.pin(ALIAS, &publisher_key.current().public_key())
        .unwrap();

    // One hooked session in the spool.
    let spool = p.data.join("spool");
    std::fs::create_dir_all(&spool).unwrap();
    let now = cc_publish::util::now_secs();
    let mut start = SpoolEvent::new("SessionStart", SESSION_ID, now);
    start.model = Some("claude-opus-5-5".into());
    std::fs::write(
        spool.join(start.file_name("local0")),
        serde_json::to_vec(&start).unwrap(),
    )
    .unwrap();

    let rt = runtime();
    let _bucket = SessionsBucket {
        server: server.clone(),
    };
    let (front, store) = rt.block_on(async {
        let store = create_bucket(&admin(&server).await, cc_core::BUCKET).await;
        (Front::start(server.clone()).await, store)
    });
    let config = p.data.join("publish.json");
    std::fs::write(
        &config,
        serde_json::to_string_pretty(&serde_json::json!({
            "alias": ALIAS,
            "servers": [front.url()],
            "creds": format!("nats/{user}.creds"),
            "log_file": true,
        }))
        .unwrap(),
    )
    .unwrap();

    let mut run = Running(
        p.command(&["run", "--config", config.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cc-publish run"),
    );
    let host_key = cc_envelope::host_key(ALIAS);
    let session_key = cc_envelope::session_key(ALIAS, &cc_envelope::sid_for(SESSION_ID));

    rt.block_on(async {
        // The first puts reach the bucket, sealed for the viewer.
        let host_revision = wait_for("the host record", async || {
            store
                .entry(&host_key)
                .await
                .ok()
                .flatten()
                .map(|e| e.revision)
        })
        .await;
        let session = wait_for("the session", async || {
            store.get(&session_key).await.ok().flatten()
        })
        .await;
        let payload: cc_envelope::RemotePayload = keys
            .open_any(&session, &session_key)
            .expect("the session opens")
            .value;
        assert_eq!(payload.machine, ALIAS);
        assert_eq!(payload.model.as_deref(), Some("claude-opus-5-5"));

        // The viewer side through cc-remote sees the same.
        let viewer_client = cc_remote::connect(remote_config(
            &front.url(),
            &viewer_creds,
            &cc_core::nats_user("viewer"),
        ))
        .await
        .unwrap();
        let viewer = cc_remote::Viewer::bind(&viewer_client, cc_core::BUCKET)
            .await
            .unwrap();
        let keys_seen: Vec<String> = viewer
            .snapshot()
            .await
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(
            keys_seen.contains(&host_key) && keys_seen.contains(&session_key),
            "{keys_seen:?}"
        );
        let host = viewer
            .get(&host_key)
            .await
            .unwrap()
            .expect("the host record");
        let host: cc_envelope::HostPayload = keys
            .open_any(&host, &host_key)
            .expect("the host record opens")
            .value;
        assert_eq!(host.machine, ALIAS);
        viewer_client.close().await;

        // Every connection drops. The publisher reconnects and puts
        // everything again, well before its next heartbeat.
        front.cut();
        let cut_at = Instant::now();
        wait_for("the re-put of the host record", async || {
            store
                .entry(&host_key)
                .await
                .ok()
                .flatten()
                .filter(|e| e.revision > host_revision)
                .map(|_| ())
        })
        .await;
        assert!(
            cut_at.elapsed() < Duration::from_secs(45),
            "re-put after the reconnect, not by a heartbeat"
        );
        wait_for("the reconnect in the log", async || {
            p.log().contains("reconnected").then_some(())
        })
        .await;
    });

    let out = p.cmd(&["stop", "--wait", "20"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("stopped the publisher"),
        "{out:?}"
    );
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = run.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "cc-publish run did not exit");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "{status:?}\n{}", p.log());
    let state =
        cc_publish::state::PublishState::load(&p.data.join(cc_publish::state::STATE_FILE)).unwrap();
    assert!(!state.running, "marked stopped");
    assert!(
        state.connects >= 1 && state.host_puts >= 2 && state.puts >= 2,
        "{state:?}"
    );
    drop(front);
}
