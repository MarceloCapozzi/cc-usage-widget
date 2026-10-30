# Architecture

One Cargo workspace. Logic shared by the widget and the publisher lives in
crates with no UI dependency, so `cargo test` covers it without Tauri or
WebView2.

## Crates

| Crate | Responsibility |
|---|---|
| `crates/cc-core` | Contracts and pure logic, no I/O beyond what they need: the spool event cc-hook writes, the session vocabulary and registry entry, transcript marks, the remote payloads (session, project, activity; a session's title, folder name and second-line text are cut by `session::remote_text` to `MAX_REMOTE_TITLE`/`DIR`/`TEXT`, and a whole session payload is held to `MAX_REMOTE_PAYLOAD_BYTES`), data paths for Windows and Linux (with the one home folder and Claude Code folder rule every crate uses), `fsutil` (the one atomic file write, temp file then a rename retried while Windows reports the target briefly held open, used by cc-publish, the app, cc-hook and the key files), `process_tree` (a child and everything it starts in a kill-on-close job object on Windows, a process group on Unix, so a timeout kills the whole tree; used by cc-hook's status line chain and cc-local's `claude` runs), context window sizes per model, and `state::SessionTracker`, the state machine that turns events and snapshots into rows. |
| `crates/cc-local` | Local sources: `claude agents --json`, `~/.claude/sessions/*.json`, transcript titles and colours (read from the end), draining the spool, the status line files, process liveness, token usage per project and per day from transcripts. `monitor` is the one session pass shared by the widget and cc-publish. |
| `crates/cc-hook` | The binary Claude Code runs on every hook event (reduces the payload to a spool event, never writes to stdout or stderr, always exits 0), its `install`/`uninstall`/`status` for `settings.json`, and `statusline`, the status line chain. Windows and Linux. |
| `crates/cc-focus` | Finds the window that hosts a session's process (console attach, then the parent chain) and brings it to the front. Windows only; compiles to a no-op elsewhere. |
| `crates/cc-envelope` | Sealing with the KV key as associated data: the envelope (HPKE Auth mode, a key pair per machine, the viewer's pins, which take a new key only once its user accepts it); the key files and key ids for rotation, the bucket's key layout, alias rules, hashed session and project ids. |
| `crates/cc-remote` | NATS transport on async-nats: the one default connection (`DEFAULT_SERVERS`, `DEFAULT_BUCKET`) and the checks of a configured one (`validate_servers`, `validate_bucket`, run by `connect` before any network I/O), connect (TLS or WebSocket only, inbox prefix `_INBOX.<user>`, reconnect with backoff), `Publisher` (put and delete under its prefix), `Viewer` (watch everything), both bound to the configured bucket. Moves sealed bytes only. |
| `crates/cc-publish` | The publisher for machines without the app: `run`, `check`, `status`, `keygen`, `viewer-keygen`, `init`, `clear`, `stop`. Windows and Linux (static musl). Its `pipeline` module (gathering pass, newest-wins outbox, network loop with reconnects, backoff and resync, the shared state) and `reload` (key and creds files picked up without a restart, see [Roles](#data-flow)) are also the app's Publisher role. |
| `app` (crate `cc-usage`) | The Tauri 2 tray app. Only the binary knows Tauri: `main.rs` (builder and setup) and its modules `state.rs` (managed state), `commands.rs` (the frontend's commands), `poller.rs` (usage loop, session pass, Publisher start, rows sent to the flyout), `tray.rs` (menu, the icon and its "needs you" badge, two PNGs from `tools/make-icon.ps1` embedded at build time, tooltip), `window.rs` (geometry, movement, fitted height, reset, full-screen hiding, shortcut), `devflags.rs` (debug-only flags) and `links.rs` (opening `https://` links). The logic is in the library (`lib.rs`, `refresh` and `Snapshot`): the usage API, its shapes and the read-only OAuth token (`api.rs`, `model.rs`, `credentials.rs`), `history.rs`, `notifications.rs`, `statusline.rs` (the footer's context block), `sessions.rs` (the widget's `cc_local::monitor` setup), `projects.rs` (pricing included), `activity.rs`, `remote/` (remote sessions, Viewer role: `map.rs` the bucket's values and their rules, `view.rs` the rows, `hub.rs` the shared hub and footer status, `store.rs` the projects file, `review.rs` the publisher keys waiting for review, `task.rs` the watch and ticker loops), `publisher.rs` (Publisher role), `backup.rs` (Settings → Data backup and restore), `settings.rs` (with the `"nats"` connection of Settings → Remote connection, checked with cc-remote's rules, and its environment overrides), `window_state.rs`, `fullscreen.rs`, `autostart.rs`, `paths.rs` (data and config folders) and `demo.rs` (the `--demo` fixtures). `ui/` is plain HTML, CSS and JS ES modules, no framework or bundler (see [The flyout's scripts](#the-flyouts-scripts)). |

## The flyout's scripts

`app/ui/index.html` loads `scripts/theme-init.js`, a classic script in
`<head>` that applies the saved theme before the first paint, and
`scripts/main.js` as a module. The modules are served by Tauri's asset
protocol (`text/javascript`, same origin, allowed by the CSP's
`default-src 'self'`); `withGlobalTauri` provides `window.__TAURI__`, which
`tauri.js` wraps.

| Module | Draws or does |
|---|---|
| `main.js` | startup: wires every module in order, the usage events, the dev flags (`--view`, `--backdrop`, `--click`), the first paint |
| `state.js` | the state several modules share, one `state` object (view, chart, history range, machine filter, last report and calendar, saved settings) |
| `tauri.js`, `dom.js`, `theme.js` | `invoke`/`listen`/the window; element helpers; the theme setting and token colours |
| `window.js` | the height that follows the content, moving, resizing |
| `views.js` | the header buttons, the three views, the chart switch, chart redraws on a theme change |
| `footer.js`, `usage.js` | the limit lines and the Remote line; the usage view's account and context |
| `history-chart.js`, `activity.js`, `projects.js` | the usage view's chart, calendar and project list |
| `sessions.js` | the session rows and the header summary |
| `keys.js` | the notice above the rows: publisher keys waiting for Accept or Reject |
| `settings.js`, `role.js`, `backup.js` | the ⚙ view |
| `connection.js` | the ⚙ view's Remote connection section and its diagnostics (settings that could not be used, ignored folder overrides) |
| `format.js`, `session-text.js`, `history-math.js`, `report-data.js`, `settings-text.js`, `key-text.js`, `connection-text.js` | pure functions, no DOM or backend, unit-tested under Node (`app/ui-tests/`) |

Module code only defines things and looks up elements; whatever wires
events or calls the backend is an `init…` function that `main.js` calls.
State used by one module stays in that module.

## Tools and infrastructure

| Path | What |
|---|---|
| `tools/make-publisher-bundle.ps1` | the entry point for publisher bundles (`-Platform linux`, `windows`, `app`) and Linux update packages (`-Update`): parameters, the order of the steps and the summary |
| `tools/publisher-bundle-gen/` | the steps, dot-sourced by that script so they run in its scope: `common.ps1` (synced folder and repository checks, key file check, tar.gz and zip writers, private ACL), `provision.ps1` (toolctl), `keys.ps1` (the viewer's public key), `linux.ps1`, `windows.ps1`, `app.ps1`, `update.ps1` |
| `tools/publisher-bundle/`, `tools/publisher-bundle-windows/` | what the bundles carry: the Makefile, POSIX sh scripts and systemd unit template for Linux; `setup.cmd` and the Windows PowerShell 5.1 scripts for Windows; each with its tests |
| `tools/cross/` | the pinned zig toolchain and the static musl builds of cc-hook and cc-publish |
| `tools/stage-cc-hook.ps1` | the release cc-hook copied to `app/staging/`, the installer's resource (`beforeBuildCommand`) |
| `infra/nats/cc-sessions.example.yaml` | the template of the NATS project file: the KV bucket's limits and the users. A deployment copies it to the gitignored `cc-sessions.yaml` and applies it with toolctl, an internal maintainer tool, or by hand ([remote.md](remote.md#provisioning)); tests check its limits against the code |

The scripts are listed with their options in
[development.md](development.md#tools).

## Data flow

**Local sessions** (every 2 s, `cc_local::monitor`):

1. Claude Code runs `cc-hook` on each hook event (SessionStart,
   UserPromptSubmit, PreToolUse, Notification, SubagentStart, SubagentStop,
   Stop, StopFailure, PostToolUseFailure, SessionEnd). It writes one JSON
   file into `spool/` (temp name, then rename).
2. The pass reads the registry files every time and runs `claude agents
   --json` at start, when the set of files changes, and every 30 s (the
   publisher backs off to 120 s on an idle machine). A new file whose process
   is alive is listed at once and confirmed by a CLI run 10 s later.
3. It drains the spool, reads transcript marks (title, custom title, colour)
   incrementally and each session's `live/<session_id>.json`.
4. `SessionTracker` merges them. Hooks drive the fine-grained states (needs
   you, failed, done, tool, subagents); the registry only moves a hooked
   session between working and idle, and never clears needs you unless it
   reports idle for 60 s; for a session without hooks the registry is the
   state.
5. The widget turns the rows into the flyout and toasts; cc-publish turns
   them into KV puts.

**Subagents.** The tracker keeps each session's subagents by the hook
payload's `agent_id`: `SubagentStart` adds one, `SubagentStop` finishes it,
and the subagent's own `PreToolUse` events set its current tool and
summary (its permission prompt makes the whole session need you).
`SubagentStart` carries no task, so each main-agent `Agent` (or
`Task`) call queues its `description` with its `subagent_type` (the spool
event's `subagentType`) and `tool_use_id` (at most 16, for 2 minutes; a new
prompt empties the queue). The session's next `SubagentStart` takes the
oldest pending call whose `subagent_type` is the start's `agent_type`, else
the oldest call without a type, else the oldest, so parallel calls that
start out of order keep their own descriptions. A main-agent
`PostToolUseFailure` with the `tool_use_id` of a pending call (a failed or
denied call, which starts no subagent) removes it. A session whose turn ended while subagents still run stays
working; a subagent silent for 10 minutes (a missed `SubagentStop`) stops
counting until its next event, and finished ones drop off after 2
minutes. A remote payload carries the counts and a line for the first six
(running ones first; type, task, tool and summary cut to 24, 60, 32 and 60
characters). A whole session payload is held to `MAX_REMOTE_PAYLOAD_BYTES`
(2800 bytes of JSON, which seal to at most 3954 bytes, under the bucket's
4096), and `RemotePayload::fit_agents` gives the lines whatever
the other fields leave (`RemotePayload::json_len` measures it exactly):
six full lines fit next to texts at their caps in ASCII or CJK, and fewer
travel only when every text is in four-byte characters.

**Status line chain.** Claude Code only passes the exact context use to the
status line command. `cc-hook install` puts `"<cc-hook>" statusline` in
`statusLine.command` and records the old `statusLine` in
`statusline-chain.json`. On each render cc-hook writes `live/<sid>.json`,
then runs the original command through the shell Claude Code uses (Git Bash,
else PowerShell on Windows; `sh` elsewhere) with the same stdin, so its output
and exit code are unchanged. It has 5 s before it is killed. The settings
file is `settings.json` in Claude Code's folder (`CLAUDE_CONFIG_DIR` when
set, else `~/.claude`) unless `--settings` names another; the record is
keyed by that file, and `cc-hook statusline` finds it by the same rule,
since Claude Code passes its environment on to the command.

**Remote sessions.** cc-publish seals each value (the envelope: HPKE from its
own key pair to the viewer's public key; without those files it does not
publish) and puts it under `<alias>.s.<sid>`, `<alias>.host`, `<alias>.p.<pid>` or
`<alias>.activity`. A session value carries what the row shows: state, tool
name, model, context, the title, the folder name (never the path) and the
second line's text, each cut by `remote_text`; no prompts or transcript
content. The widget watches the bucket as `cc-sessions.viewer`
(snapshot, then live changes), opens values with its `ViewerKeys` (its own
key pair and each alias's pinned publisher key) into `RemoteMap`. A value whose sender key is not pinned for its alias, a new
machine's included, is held back and its key noted in `PendingKeys`
(`keys/publishers/<alias>.pending`), which `RemoteStatus.pending` and the
`remote-status` event bring to the flyout's notice; Accept and Reject
(`accept_publisher_key`, `reject_publisher_key`) write `<alias>.pub` or
`<alias>.rejected` and refresh the running `ViewerKeys`. From the map it
builds remote rows (`remote_view`, which cuts the texts again so whatever
a publisher sends still fits the row; merged with local ones in panel
order), remote projects
(saved in `remote-projects.json`) and activity (merged into `activity.json`).
A 5 s ticker applies the offline (150 s) and drop rules. Values under the
viewer's own name are ignored. See [remote.md](remote.md).

**Roles.** The app is either the viewer (above) or, in the Publisher role,
a publisher, fixed for a run (switching saves `role` in `settings.json` and
restarts). A Publisher starts no viewer task; `publisher.rs` builds a
`cc_publish::pipeline` from the same pieces `cc-publish run` uses:

```
app session pass (every 2 s, the one SessionMonitor)
  └─ monitor.payloads(alias) ─> Gatherer (Planner + ProjectsFeed) ─> Outbox ─┐
                                                                            │ Link (state, resync)
"publish" thread, current-thread tokio runtime: pipeline::serve ─ flush <───┘
  └─ one NATS connection as cc-sessions.pub-<alias> ─> KV cc-sessions (sealed values)
```

`cc-publish run` is the same with its own gathering thread and monitor in
place of the app's pass. Both hold an exclusive lock on `cc-publish.lock`
in the data folder while they publish (`cc_publish::instance`), which is
how each refuses to publish next to the other; the state written to
`publish-state.json` names the holder. The transcript scan for projects
and activity runs on a thread of its own, so a pass never waits for it. `serve` seals
with a shared `cc_publish::reload::Keys` and looks at the key and creds files
again every 30 s (the app also right after an import): new keys apply from
the next write, new creds are tried on a second connection that replaces the
one in use only once it is up (a failed trial keeps the connection and is
retried with backoff), files that do not load leave the keys in use, and
what is in use is recorded in the state file.

**Plan usage.** Every "Refresh usage every" minutes (default 5), the app
reads the OAuth token from `~/.claude/.credentials.json`, calls
`api.anthropic.com/api/oauth/usage` (and `/profile` once per login: the
email is cached with a hash of the token, so another account's login, or a
renewed token, fetches it again), normalizes the
limits, stores a history sample, decides notifications, updates the tray
badge and tooltip and emits the snapshot to the flyout. The loop in `main.rs`
reads the interval every turn, and `poller::poll_wake()` (`POLL_WAKE`) cuts
the wait short when the setting changes ("Refresh now" in the tray polls
once on its own). While the
token is expired the API is not called; `wait_for_renewal` re-reads the
credentials file every 20 s and refreshes as soon as Claude Code has renewed
it. Meanwhile `ask_claude_to_renew` runs cc-local's `agents::ask_to_renew`
(at once, then at most every 5 minutes): `claude -p` with no prompt, an
empty stdin and no console window, output discarded, 20 s at most. Claude
Code loads, and renews, its credentials when it starts, then stops on the
missing input, so nothing is sent. The process carries `CC_USAGE_NO_SPOOL`
(`cc_core::paths::NO_SPOOL_ENV`), which its hooks inherit: cc-hook writes
nothing for it, so its SessionStart and SessionEnd never make a row. How
the last request went is the expired message's tooltip.

**Where things live.** Data (history, activity, remote projects, creds and
keys, the spool, `live/`) is in `%LOCALAPPDATA%\cc-usage`, which is also
where the installer puts the app and cc-hook; the widget's `settings.json` and `window.json`
are in Tauri's config folder for the identifier `ar.cc.usage`
(`%APPDATA%\ar.cc.usage`). `CC_USAGE_DATA_DIR` (an absolute path; the app
resolves it through `cc_core::paths`, like every other crate) and
`CC_USAGE_CONFIG_DIR` move them for development and tests. One app runs per data folder: `main`
takes the named mutex `Local\cc-usage-<hash of the data folder>` before
anything else (`cc_usage::instance`); a second copy signals the event
`…-show`, on which the first shows its widget, and exits. The app's own
restart lets go of the mutex first. Both roles log through cc-publish's
logger to `cc-usage.log` in the data folder.

**Usage view.** On open, the app scans this machine's transcripts for the
last 30 days (tokens per project, model and local day), adds the remote
projects and activity, prices tokens with the built-in table plus
`pricing.json`, and sends one report. One scan runs at a time: opening the
view while one runs waits for it, and a report less than 30 s old is sent
again without scanning.

## Design decisions

- **One session pass for widget and publisher.** `cc_local::monitor` runs in
  both, so a remote row means exactly what a local one does. They drain the
  same spool, so they must not run on the same machine; a machine with the
  app publishes through the app (Publisher role), which feeds cc-publish's
  pipeline from its own pass.
- **Roles, not a dual mode.** A machine either views or publishes, and the
  switch restarts the app: one NATS connection per machine (the account's
  connection budget is small), no viewer and publisher fighting over state.
- **The CLI is expensive.** One `claude agents --json` costs about 0.6 s of
  CPU; every 2 s it would take a quarter of a core all day. Registry files are
  cheap and read every pass; the CLI runs only when the set changes and
  every 30 s.
- **The hook binary is silent and fast.** Claude Code acts on a hook's stdout
  and shows its stderr, so cc-hook never writes to either and always exits 0.
  About 11 ms per call, mostly process creation on Windows.
- **Read-only OAuth.** The widget never refreshes Claude Code's token: if the
  refresh token is single-use, refreshing it here would sign Claude Code out.
  Expired data is shown as stale instead, and the renewal is left to Claude
  Code itself (`claude -p`, see [Plan usage](#data-flow)).
- **End-to-end encryption over a shared NATS account.** The NATS server
  stores only ciphertext; the KV key is the associated data, so values
  cannot be moved between keys. Publishers are write-only to their own
  prefix, the viewer is read-only, each user has its own inbox prefix.
- **WebSocket first.** The default servers put `wss://` on 443 before
  `tls://`: it gets through corporate networks.
- **Configured, not compiled in.** One default connection lives in
  cc-remote; the app's `settings.json` (or environment variables) and
  cc-publish's `publish.json` name other servers and another bucket. A
  value that cannot be used falls back to the default and is named in the
  tooltip, so a client never connects elsewhere without saying so. The NATS
  user names stay fixed (`cc_core::PROJECT`).
- **Projects and activity outlive the TTL.** The bucket's 3-minute TTL suits
  presence; usage history does not, so the widget keeps the last project
  values and accumulates activity days locally.
- **Per-user everything.** NSIS `currentUser` install into
  `%LOCALAPPDATA%\cc-usage`, a user systemd unit on Linux, a per-user
  scheduled task on Windows publishers: no administrator rights anywhere.
- **Build output outside the repository** when it lives in a synced folder:
  a per-machine `.cargo/config.toml` (or `CARGO_TARGET_DIR`), see
  [development.md](development.md#fresh-clone).
- **The UI is the mockup.** `app/ui` uses the markup, class names and CSS of
  the design document's panel mockup (`docs/reference/flyout-mockup.html`);
  it only draws what the backend sends, and what is not known is not drawn
  (no made-up 0%).
