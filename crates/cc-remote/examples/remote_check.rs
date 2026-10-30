//! Live permission check for the cc-sessions bucket: the way to validate the
//! account setup. It runs 14 checks, once per transport, with the
//! `cc-sessions.viewer` and `cc-sessions.pub-test` credentials from
//! `%LOCALAPPDATA%\cc-usage\nats`.
//!
//! ```text
//! cargo run -p cc-remote --example remote_check                 # wss and tls
//! cargo run -p cc-remote --example remote_check -- wss://connect.ngs.global
//! ```
//!
//! The servers are the arguments, else cc-remote's defaults; the bucket is
//! `CC_USAGE_NATS_BUCKET`, else `cc-sessions`.
//!
//! Every line must print PASS; the exit code is 1 otherwise. It opens two
//! connections at a time and closes them before the next transport. Only
//! keys under `test.s.` are written. Credentials are never printed.

use std::future::Future;
use std::time::Duration;

use cc_remote::{connect, Client, Config, KvChange, KvWatch, Publisher, Viewer};
use futures_util::StreamExt;

const VIEWER: &str = "cc-sessions.viewer";
const PUBLISHER: &str = "cc-sessions.pub-test";
const SEE_TIMEOUT: Duration = Duration::from_secs(10);
const SHORT_TIMEOUT: Duration = Duration::from_secs(5);
const SUBSCRIBE_WAIT: Duration = Duration::from_secs(3);

/// Outcome of every check for one transport.
#[derive(Default)]
struct Report {
    results: Vec<(String, bool)>,
}

impl Report {
    /// Records a check: `succeeded` is whether the operation worked,
    /// `want_ok` whether it should have.
    fn check(&mut self, label: &str, succeeded: bool, want_ok: bool, detail: &str) {
        let pass = succeeded == want_ok;
        println!(
            "[{}] {label} -> {detail}",
            if pass { "PASS" } else { "FAIL" }
        );
        self.results.push((label.to_string(), pass));
    }

    fn check_result<T>(&mut self, label: &str, result: &cc_remote::Result<T>, want_ok: bool) {
        match result {
            Ok(_) => self.check(label, true, want_ok, "ok"),
            Err(err) => self.check(label, false, want_ok, &format!("err={err}")),
        }
    }
}

/// The bucket checked: `CC_USAGE_NATS_BUCKET` when set, else the default.
fn bucket() -> String {
    std::env::var("CC_USAGE_NATS_BUCKET")
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| cc_remote::DEFAULT_BUCKET.to_string())
}

fn config(server: &str, user: &str) -> Config {
    Config {
        servers: vec![server.to_string()],
        bucket: bucket(),
        creds_path: cc_core::paths::nats_dir().join(format!("{user}.creds")),
        user: user.to_string(),
        name: format!("cc-remote-check-{user}"),
    }
}

/// Runs `fut` with a deadline, mapping a timeout to an error.
async fn within<T>(
    after: Duration,
    op: &'static str,
    fut: impl Future<Output = cc_remote::Result<T>>,
) -> cc_remote::Result<T> {
    tokio::time::timeout(after, fut)
        .await
        .unwrap_or(Err(cc_remote::RemoteError::Timeout { op, after }))
}

/// Waits until the watch yields a change for `key` that matches `want`.
async fn wait_for(
    watch: &mut KvWatch,
    key: &str,
    want: impl Fn(&KvChange) -> bool,
) -> Option<KvChange> {
    let fut = async {
        while let Some(change) = watch.next().await {
            if change.key() == key && want(&change) {
                return Some(change);
            }
            println!("   (other change: {})", change.key());
        }
        None
    };
    tokio::time::timeout(SEE_TIMEOUT, fut).await.ok().flatten()
}

/// Subscribes the publisher to `subject` and reports whether the server
/// answered with a permissions violation within a few seconds.
async fn subscribe_denied(client: &Client, subject: &str) -> Result<(), String> {
    let mut events = client.events();
    while events.try_recv().is_ok() {}
    let sub = client
        .nats()
        .subscribe(subject.to_string())
        .await
        .map_err(|err| format!("client refused: {err}"))?;
    let _ = client.nats().flush().await;
    let wait = async {
        loop {
            match events.recv().await {
                Ok(ev) if ev.is_permission_violation() => return Some(format!("{ev:?}")),
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    };
    let denied = tokio::time::timeout(SUBSCRIBE_WAIT, wait)
        .await
        .ok()
        .flatten();
    let mut sub = sub;
    let _ = sub.unsubscribe().await;
    match denied {
        Some(ev) => Err(ev),
        None => Ok(()),
    }
}

async fn run(server: &str) -> Report {
    let mut report = Report::default();
    let tag = if server.starts_with("wss") {
        "wss"
    } else {
        "tls"
    };
    let key = format!("test.s.rsprobe-{tag}");
    println!("== server: {server}");

    let vc = match connect(config(server, VIEWER)).await {
        Ok(c) => c,
        Err(err) => {
            report.check("viewer connect", false, true, &format!("err={err}"));
            return report;
        }
    };
    let pc = match connect(config(server, PUBLISHER)).await {
        Ok(c) => c,
        Err(err) => {
            report.check("publisher connect", false, true, &format!("err={err}"));
            vc.close().await;
            return report;
        }
    };
    let info = vc.nats().server_info();
    println!(
        "connected; server {} websocket? {}",
        info.server_name,
        vc.is_websocket()
    );

    // 1-2. Both users bind the bucket.
    let viewer = Viewer::bind(&vc, &bucket()).await;
    report.check_result("viewer binds bucket", &viewer, true);
    let publisher = Publisher::bind(&pc, &bucket()).await;
    report.check_result("publisher binds bucket", &publisher, true);
    let (Ok(viewer), Ok(publisher)) = (viewer, publisher) else {
        pc.close().await;
        vc.close().await;
        return report;
    };

    // 3. The viewer watches everything and drains the initial snapshot.
    let mut watch = viewer.watch_all().await;
    report.check_result("viewer watch all", &watch, true);
    if let Ok(w) = watch.as_mut() {
        let drained = tokio::time::timeout(SEE_TIMEOUT, async {
            let mut n = 0;
            while !w.is_current() {
                if w.next().await.is_none() {
                    break;
                }
                n += 1;
            }
            n
        })
        .await;
        println!(
            "   initial snapshot: {drained:?} entries, current={}",
            w.is_current()
        );
    }

    // 4-5. Publisher writes under its own prefix; the viewer sees it.
    let value = br#"{"probe":"remote_check"}"#.to_vec();
    let put = publisher.put(&key, value.clone()).await;
    report.check_result("publisher put own prefix", &put, true);
    match watch.as_mut() {
        Ok(w) => match wait_for(w, &key, |c| matches!(c, KvChange::Put { .. })).await {
            Some(KvChange::Put {
                key,
                bytes,
                revision,
            }) => report.check(
                "viewer saw put",
                true,
                true,
                &format!("{key} rev={revision} len={}", bytes.len()),
            ),
            _ => report.check("viewer saw put", false, true, "nothing in 10s"),
        },
        Err(_) => report.check("viewer saw put", false, true, "no watch"),
    }

    // 6. Publisher writes under another prefix: denied.
    let other = within(
        SHORT_TIMEOUT * 2,
        "put",
        publisher.put("other.s.rsprobe01", b"x".to_vec()),
    )
    .await;
    report.check_result("publisher put OTHER prefix (must fail)", &other, false);

    // 7. Publisher reads: denied (bind is allowed, the get is not).
    let read = within(SHORT_TIMEOUT * 2, "get", async {
        Viewer::bind(&pc, &bucket()).await?.get(&key).await
    })
    .await;
    report.check_result("publisher read (must fail)", &read, false);

    // 8. Publisher watches: denied, or at least receives nothing.
    let pwatch = within(SHORT_TIMEOUT * 2, "watch", async {
        let mut w = Viewer::bind(&pc, &bucket()).await?.watch_all().await?;
        match tokio::time::timeout(SHORT_TIMEOUT, w.next()).await {
            Ok(Some(change)) => Ok(format!("got entry {}", change.key())),
            _ => Err(cc_remote::RemoteError::Timeout {
                op: "watch (consumer created, no data)",
                after: SHORT_TIMEOUT,
            }),
        }
    })
    .await;
    report.check_result("publisher watch (must fail)", &pwatch, false);

    // 9. Viewer writes: denied.
    let vput = within(SHORT_TIMEOUT * 2, "put", async {
        Publisher::bind(&vc, &bucket())
            .await?
            .put("test.s.fromviewer", b"x".to_vec())
            .await
    })
    .await;
    report.check_result("viewer put (must fail)", &vput, false);

    // 10-11. Publisher cannot listen on the viewer's inbox or on all inboxes.
    for subject in ["_INBOX.cc-sessions.viewer.>", "_INBOX.>"] {
        let label = format!("publisher subscribe {subject} (must fail)");
        match subscribe_denied(&pc, subject).await {
            Ok(()) => report.check(&label, true, false, "no permissions violation in 3s"),
            Err(why) => report.check(&label, false, false, &format!("err={why}")),
        }
    }

    // 12. Viewer reads the key.
    let got = viewer.get(&key).await;
    let detail = match &got {
        Ok(Some(v)) => format!("value: {}", String::from_utf8_lossy(v)),
        Ok(None) => "key not found".to_string(),
        Err(err) => format!("err={err}"),
    };
    let ok = matches!(&got, Ok(Some(v)) if *v == value);
    report.check("viewer get", ok, true, &detail);

    // Extra, not one of the 14: the snapshot API sees the key too.
    let snap = viewer.snapshot().await;
    let detail = match &snap {
        Ok(entries) => format!("{} entries", entries.len()),
        Err(err) => format!("err={err}"),
    };
    let ok = matches!(&snap, Ok(entries) if entries.iter().any(|(k, v)| *k == key && *v == value));
    println!(
        "   [extra {}] viewer snapshot has key -> {detail}",
        if ok { "PASS" } else { "FAIL" }
    );

    // 13-14. Publisher deletes its key; the viewer sees the delete.
    let del = publisher.delete(&key).await;
    report.check_result("publisher delete own key", &del, true);
    match watch.as_mut() {
        Ok(w) => match wait_for(w, &key, |c| matches!(c, KvChange::Delete { .. })).await {
            Some(change) => report.check("viewer saw delete", true, true, change.key()),
            None => report.check("viewer saw delete", false, true, "nothing in 10s"),
        },
        Err(_) => report.check("viewer saw delete", false, true, "no watch"),
    }

    drop(watch);
    pc.close().await;
    vc.close().await;
    report
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let servers: Vec<String> = if args.is_empty() {
        cc_remote::default_servers()
    } else {
        args
    };

    let mut all_pass = true;
    let mut summary = Vec::new();
    for server in &servers {
        let report = run(server).await;
        let passed = report.results.iter().filter(|(_, ok)| *ok).count();
        all_pass &= passed == report.results.len() && report.results.len() == 14;
        summary.push((server.clone(), passed, report.results.len()));
        println!();
    }
    for (server, passed, total) in &summary {
        println!("{server}: {passed}/{total} PASS");
    }
    if !all_pass {
        std::process::exit(1);
    }
}
