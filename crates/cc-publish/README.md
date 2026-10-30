# cc-publish

Publishes the Claude Code sessions of a machine without the cc-usage widget,
encrypted, to a NATS KV bucket (by default `cc-sessions` on Synadia Cloud;
see [Config](#config)). The widget on
another machine shows them as remote rows. Windows and Linux. A machine with
the widget never runs it: the widget's Publisher role runs the same pipeline
(the `pipeline` module of this crate) from its own session pass.

## Setting up a machine

```sh
cc-publish init --alias devbox        # writes <data_dir>/publish.json
# copy <data_dir>/nats/cc-sessions.pub-devbox.creds (see docs/remote.md#provisioning)
# copy keys/viewer.pub from the viewer machine to <data_dir>/keys/viewer.pub
cc-publish keygen                     # this machine's key pair (its identity); prints its public key line
cc-publish check                      # PASS/WARN/FAIL lines, exit 1 on any FAIL
cc-publish run                        # until Ctrl-C / SIGTERM
```

The bundles do all of this (see docs/publisher-linux.md and
docs/publisher-windows.md). `<data_dir>` is `%LOCALAPPDATA%\cc-usage` on
Windows, and `$XDG_DATA_HOME/cc-usage` or `~/.local/share/cc-usage` on Linux.
`CC_USAGE_DATA_DIR`, set to an absolute path, overrides it. On Unix, `check`
fails if the creds file is world-readable or a private key file is not mode
600.

## Envelopes and keys

Every value is sealed in an envelope (HPKE Auth mode, X25519) from this
machine's private key, `keys/publisher.key`, to the viewer's public key,
`keys/viewer.pub`: only the viewer can open it, and it proves which key sealed
it (the viewer pins each machine's key to its alias, asking to accept a new
one; see
[docs/data-and-privacy.md](../../docs/data-and-privacy.md#security-model)).
The key pair is the machine's identity: it is made on the machine, kept by
every reinstall and update of the bundles, and replaced only on purpose.
Both files are required: when either is missing or does not load, `run`
refuses to start, and `check` and `status` say which file it is and how to
get it. `run` logs the key ids it seals with when it starts.

A running `run` needs no restart for new files: every 30 s it compares the
length and modification time of both key files and of the creds file with
what it last saw. Key files that changed and load are used from the next
write (and every value is put again soon, sealed with them). New creds for
`cc-sessions.pub-<alias>` (with a seed block and an `exp` still ahead) are
tried on a second connection first: it replaces the one in use only once it
is up with the bucket bound. A trial that fails (stale, expired or revoked
creds) is logged, recorded in the state file and tried again later (after
30 s, doubling up to 10 min), while the connection in use goes on with the
credentials it holds. A changed file that does not load (missing, broken,
creds for another user, expired, without a seed) is logged as a warning and
recorded in the state file, and the keys and connection in use stay: nothing
weaker ever stands in. `status` shows what the running process uses next to
what the files hold when they differ.

| Command | Machine | Writes |
|---|---|---|
| `keygen` | publisher | `keys/publisher.key` (private, 0600, atomic); prints the public key line alone on stdout |
| `keygen --if-missing` | publisher | nothing when the key exists and loads; prints its public key line either way (what the bundles' installers run) |
| `keygen --rotate --confirm <alias>` | publisher | adds a newer key and keeps the old ones; a running `run` seals with it within 30 s, and the viewer holds back its values until the new key is accepted there |
| `keygen --force --confirm <alias>` | publisher | replaces the key file with one new key (the same acceptance step) |
| `viewer-keygen [--if-missing \| --rotate \| --force]` | viewer | `keys/viewer.key` (private) and `keys/viewer.pub` (kept in step with it); prints the public key line. The cc-usage app makes them on its own on first start |

Without `--force`, `--if-missing` or `--rotate`, both `keygen` and
`viewer-keygen` refuse to overwrite an existing key. `keygen --rotate` and
`--force` on an existing key replace this machine's identity, so they need
the alias of the config (`--config P`, default `<data_dir>/publish.json`):
`--confirm <alias>`, or, when stdin is a terminal, the alias typed at the
prompt after the warning *This replaces &lt;alias&gt;'s identity. The viewer
will hold back its values until you accept the new key there.* Otherwise, or
with another alias, `keygen` changes nothing and exits 2. A first key and
`--if-missing` need no confirmation. The bundles wrap this as `make
rotate-key` and `setup.cmd rotate-key`. `--key P` writes another file
instead of `keys/publisher.key`. Key ids are
`p<YYYYMMDD>-<4 hex>` and `v<YYYYMMDD>-<4 hex>`. Only key ids, paths and
public keys are ever printed.

## Commands

| Command | Does | Exit |
|---|---|---|
| `init --alias A [--config P] [--force]` | writes the config with every default spelled out | 0, 1 exists, 2 bad alias |
| `keygen [--if-missing \| --rotate \| --force] [--key P] [--config P] [--confirm A]` | this machine's key pair (see above) | 0, 1 exists or does not load, 2 replacing it not confirmed |
| `viewer-keygen [--if-missing \| --rotate \| --force]` | the viewer's key pair, on the viewer | 0, 1 exists |
| `check [--config P]` | first a FAIL line for `CC_USAGE_DATA_DIR` or `CLAUDE_CONFIG_DIR` set to a relative path (ignored, so another folder is used; the line names it); config, alias, creds (exists, mode, user in the JWT), the key files (`publisher key`, `viewer.pub`: FAIL when missing or unreadable, naming how to get them) and the `envelope` it will seal with, claude CLI and hooks (WARN only), connect (each transport in order until one works) with the inbox prefix, bind, put and delete of a sealed probe `<alias>.check`, with latencies; last line `RESULT ...` | 0 all passed, 1 any FAIL |
| `run [--config P] [--force] [--for SECS]` | the publisher | 0 clean stop, 1 cannot start |
| `status [--config P]` | config and `bucket`; the `envelope` and `creds user` the running publisher uses, as it records them (`(in use)`), plus `disk keys` / `disk creds` rows when the files on disk differ or were not applied (not picked up yet, or not usable, with why: another user, expired, no seed, a trial that did not connect); with no publisher running, the envelope the files would give; the key files, publisher state, last write, counters, last error, service hints, health | 0 publisher alive and wrote within 150 s, 1 otherwise |
| `clear [--config P] [--force]` | deletes this machine's keys from the bucket now | 0, 1 |
| `stop [--wait SECS]` | asks the live publisher to stop cleanly (writes `<data_dir>/cc-publish.stop` with its pid; the run loop checks every second) and waits, 15 s by default | 0 stopped or none running, 1 still running |
| `--version` | `cc-publish <version>` | 0 |

Every command takes `--data-dir DIR` anywhere on the line: the data
directory, overriding `CC_USAGE_DATA_DIR` (a scheduled task cannot set
environment variables). Usage errors exit 2. `CC_PUBLISH_LOG` sets the log
level (`info` by default).

## Config

```json
{
  "alias": "devbox",
  "servers": ["wss://connect.ngs.global", "tls://connect.ngs.global"],
  "bucket": "cc-sessions",
  "creds": "<data_dir>/nats/cc-sessions.pub-devbox.creds",
  "publisher_key": "<data_dir>/keys/publisher.key",
  "viewer_pub": "<data_dir>/keys/viewer.pub",
  "log_file": false
}
```

Only `alias` is required; unknown fields are rejected. `servers`, `bucket`,
`publisher_key` and `viewer_pub` default to the values shown, and `init`
writes them all. `servers` accepts `tls://` and `wss://` URLs only
(`nats://` and `ws://` are refused), each with a host, an optional port
(443 for `wss://`, 4222 for `tls://`) and no user or password, and is
tried in order. `bucket` is 1 to 64 letters, digits, `-` or `_`, and must
be the viewer's: the publisher writes `$KV.<bucket>.<alias>.>`, which its
NATS user `cc-sessions.pub-<alias>` must be allowed (see
[docs/remote.md](../../docs/remote.md#connection-settings)). Both are
checked before any connection. Paths may start with
`~/` or `<data_dir>/`; relative ones are relative to the config file.
`log_file` adds `<data_dir>/cc-publish.log` (rotated at 1 MiB to `.log.1`);
it defaults to on for Windows and off elsewhere, where stderr goes to the
journal.

## What `run` does

Every 2 s it gathers local sessions with the widget's own code
(`cc_local::monitor`): `claude agents --json` at start, when
`~/.claude/sessions` changes and every 30 s (every 120 s while the machine has
no session at all); registry files; the cc-hook spool; transcripts;
`<data_dir>/live/<sid>.json` for context %. A new session shows from its
registry file at once, even if the CLI has not caught up; a session whose
registry file went away and whose process has exited leaves at once, even
while the CLI fails. Then it:

- puts `<alias>.s.<sid>` when anything but its heartbeat changed (state,
  tool, model, context, title, folder name, second-line text, subagent
  counts),
- re-puts every session and `<alias>.host` every 60 s,
- deletes `<alias>.s.<sid>` when the session leaves,
- scans the transcripts 5 minutes after the last scan ended (the last 30
  days, on a thread of its own, so a long scan never delays the session
  pass; the result goes out on the first pass after it ends) and puts one
  `<alias>.p.<pid>` per project (folder name, sessions, tokens per model,
  last activity) and `<alias>.activity` (tokens per local day and the
  busiest hour, at most 35 days); every project is put again every 2
  minutes, inside the bucket's 3-minute TTL, and deleted when it leaves the
  window.

`<sid>` and `<pid>` are hashed ids (8 hex characters of a SHA-256). A
session value holds its title, folder name (never the path) and one short
second-line text (question, tool summary, error or last message, at most
120 characters); no value holds prompts, the conversation, paths or costs;
see
[docs/data-and-privacy.md](../../docs/data-and-privacy.md#what-leaves-the-machine).

Values are sealed with cc-envelope at send time (`hb` = send time; see
[Envelopes and keys](#envelopes-and-keys)). One NATS
connection as `cc-sessions.pub-<alias>`; decisions go through a newest-wins
outbox so the network never blocks gathering. Connect failures retry with
backoff (1 s to 30 s); writes failing for 3 minutes rebuild the connection.
It stops on Ctrl-C, SIGTERM, `cc-publish stop`, and on Windows also on
console close, logoff and shutdown. On stop it flushes once (3 s at most) and
closes; keys are left to the TTL. A panic in the gathering thread ends the
process with exit code 70 (an abort in a release build) after logging why,
so systemd (`Restart=on-failure`) or the scheduled task starts a new one.

`<data_dir>/publish-state.json` is rewritten every 10 s (pid and its start
time, running, connected, server, counts, last publish, last error, published
keys, and `in_use`: the publisher and viewer key ids and creds user in
use, with why changed files were not applied) for `status`, `clear`
and `stop`; never key material or secrets.

One publisher per data directory: `run` takes an exclusive lock on
`<data_dir>/cc-publish.lock` (an empty file, left in place; the bundles'
uninstallers remove it once the publisher is stopped) before it reads
the state file and holds it until it stops; the cc-usage app in the
Publisher role takes the same lock. A second `run`, or the app, that finds it
taken does not start and names the pid the state file records, even with
`--force`. The OS releases the lock when the holder exits, however it exits,
so a killed or crashed publisher never blocks the next one. The state file
also blocks a `run` while the process it records still runs: same pid and
same start time (Windows and Linux; elsewhere, or for a file without a start
time, while the file is less than 30 s old); `--force` skips only this check.
The next run takes over the published keys: after its first registry
snapshot it deletes those of sessions that ended while no publisher ran.

Do not run `cc-publish` on a machine that also runs the widget: both drain the
same spool.

## Running as a service

Linux: the systemd user unit the Linux bundle installs
(`~/.config/systemd/user/cc-publish.service`, from
`tools/publisher-bundle/systemd/cc-publish.service.in`; see
[docs/publisher-linux.md](../../docs/publisher-linux.md#the-systemd-unit)).
Its core is:

```ini
[Service]
Type=simple
ExecStart="<prefix>/bin/cc-publish" run --config "<prefix>/publish.json"
Environment="CC_USAGE_DATA_DIR=<data dir>"
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

```sh
make -C ~/.local/share/cc-usage/bundle status    # or: logs, restart
loginctl enable-linger "$USER"                   # keep it running without a login session
journalctl --user -u cc-publish -f
```

Windows: the per-user scheduled task `cc-usage publisher` that the Windows
bundle registers ([docs/publisher-windows.md](../../docs/publisher-windows.md)): at logon, repeated every 5
minutes, running `conhost.exe --headless "<dir>\cc-publish.exe" --data-dir
"<dir>" run --config "<dir>\publish.json"` so no window opens. Ending the
task kills only conhost, so stop it with `cc-publish stop`. Logs in
`%LOCALAPPDATA%\cc-usage\cc-publish.log`.

## Tests

```sh
cargo test -p cc-publish
cargo test -p cc-publish --test live -- --ignored --nocapture   # the live deployment, alias "test"
```

`tests/project_file.rs` checks the bucket limits of
`infra/nats/cc-sessions.example.yaml` against the code (the project name,
the default bucket, `max_value_size` equal to the largest envelope,
`history` 1, `max_age` at least 3 heartbeats and at least the projects'
re-put plus one heartbeat). `tests/local_nats.rs` needs a local server
(`CC_TEST_NATS_URL`, see
[docs/development.md](../../docs/development.md#tests)).

`tests/stop.rs` runs the real binary offline (a closed local port, so `run`
keeps retrying): `stop` ends it cleanly, a stale request does not stop a new
run, `--data-dir` wins over `CC_USAGE_DATA_DIR`, a second `run` (even with
`--force`) is refused while one holds the lock, a killed publisher does not
block the next `run` (Windows and Linux), and a panic in the gathering thread
ends the process with a failure (`CC_PUBLISH_FAULT=gather-panic`, a test-only
fault injection).

The live test copies the `pub-test` and `viewer` creds (from the data
folder's `nats/`, so `CC_USAGE_DATA_DIR` can point it at a folder of test
creds) into a temporary data directory with a fresh key, runs `check` and a 65 s `run` with one synthetic
session, opens the values from the viewer side, and clears every key it wrote.
