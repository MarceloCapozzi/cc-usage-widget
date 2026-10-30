# Changelog

## 0.1.0 — unreleased

### Usage

- The footer shows every plan limit (the 5-hour session, the weekly limit,
  per-model weekly sub-limits, extra usage credits), each with a meter of
  what is used, the share left and the time to the reset, kept current
  between refreshes. The refresh interval is a setting (2 to 30 minutes).
- When a refresh fails the last good values stay, dimmed, marked as stale.
  An expired token is left to Claude Code: the widget runs `claude -p` with
  no prompt (at most every 5 minutes) so Claude Code renews it, re-reads the
  credentials file every 20 s and refreshes once it is renewed. The widget
  never writes or refreshes the token itself.
- Toasts when a limit reaches 80% and 95%, once per reset window, also
  after a restart (`notification-state.json`).
- The usage view: an activity calendar of six months (tokens per day for
  every machine, the busiest hour of each day), the usage history chart
  (1 hour to 30 days, cut at window resets and gaps) and tokens and
  estimated cost per project over the last 30 days, across machines and
  filterable by machine.
- Tokens count each API response once, by `message.id` and `requestId`,
  across transcripts read oldest first, so a resumed session's copied lines
  count once.
- The calendar's days are saved in `activity.json` (200 days) and only
  grow, so it keeps months of history although Claude Code keeps about 30
  days of transcripts. Plan usage samples are kept in `history.json` (30
  days).
- Costs at built-in API list prices, overridable per model with
  `pricing.json`; a model without a price shows `n/a` and a notice names it.

### Sessions

- Every local Claude Code session, most urgent first: needs you, failed,
  working, done, idle, offline. Each row has its title, what it is doing
  (the question, the tool and its summary, the last message or the error),
  a context meter with the model, its subagents and the machine tag.
- Hook-based states from cc-hook, registered by the installer in Claude
  Code's `settings.json` (after any other hooks, with a timestamped backup);
  sessions without hooks report busy or idle.
- The exact context use comes from cc-hook's status line chain, which runs
  the user's own `statusLine` command unchanged; otherwise it is estimated
  from the transcript.
- Subagents as up to four lines each (type, task, tool and summary, running
  time), matched to the `Agent` call that started them.
- A click brings a local session's window to the front (the console window,
  or the application for VS Code and Windows Terminal).
- A toast when a session needs you, for local and remote sessions; none for
  sessions already waiting at start or after a clear.
- Shell tool summaries are the tool's description or the program name,
  never the command line, and obvious secrets in spooled text are masked.

### Widget

- A frameless, always-on-top panel with three views (sessions, usage,
  settings) and the plan usage footer; moved by dragging, resized from any
  edge, its height following the content until resized by hand, position
  and size remembered (back to the primary monitor when its monitor is
  gone).
- The tray icon (an activity grid, with a red badge while a session needs
  you), its tooltip and menu; a global show/hide shortcut (`Alt+Shift+U` by
  default).
- Settings: shortcut, this machine's name, start with Windows, theme
  (System, Light, Dark), refresh interval, hiding idle sessions, removing
  offline sessions, getting out of the way of full-screen applications,
  the role, the remote connection, and backup and restore.
- Backup and restore (Settings → Data): one `.ccbak` file with the usage
  history, the activity calendar, the other machines' projects and
  `pricing.json`, optionally with the keys and NATS credentials sealed with
  a password (Argon2id and XChaCha20-Poly1305). A restore only adds, never
  replaces.
- One app per data folder: starting it again shows the running copy's
  widget.
- Per-user NSIS installer into `%LOCALAPPDATA%\cc-usage`, with no
  administrator rights; the data stays on uninstall.
- Diagnostics in `cc-usage.log` in the data folder, rotated at 1 MiB.

### Remote sessions

- Sessions, projects and activity of other Windows and Linux machines
  appear through a NATS KV bucket (any NATS server with JetStream over
  `tls://` or `wss://`), as rows tagged with the machine's alias.
- End-to-end encryption: every value is sealed with HPKE in Auth mode from
  the publisher's key pair to the viewer's, with the KV key as associated
  data; the server only stores ciphertext.
- Every publisher key is pinned to its alias with the viewer user's
  consent: the flyout's notice shows its key id and fingerprint, with
  Accept and Reject. Rejected keys' values are dropped unopened.
- Remote values are distrusted: replays, timestamps far ahead, oversized
  texts, payloads that name another machine than their key and machines
  past the limits are refused or cut.
- The NATS servers and bucket are settings (Settings → Remote connection,
  the `"nats"` object in `settings.json`, or `CC_USAGE_NATS_SERVERS` and
  `CC_USAGE_NATS_BUCKET`); the viewer reconnects when they change.
- The footer's Remote line shows the machines and the connection state;
  its tooltip names values that could not be opened, keys waiting for
  review and settings that were ignored.
- Other machines' projects are kept in `remote-projects.json` after a
  machine goes offline, until their last activity is 30 days old.

### Publisher

- `cc-publish` publishes a machine's sessions, host record, projects and
  activity, sealed to the viewer's key; one publisher per data folder.
- A PC with the app publishes from the app itself (Role: Publisher), with
  Import creds, Generate key, Replace key and Import viewer key in Settings.
- A machine's key is made on the machine by its first install and kept on
  reinstall and update; rotation needs a confirmation by alias. A running
  publisher picks up new keys and credentials within 30 s.
- Linux bundle (`make install`, verify, uninstall, rotate-key, a systemd
  user unit or a `~/.profile` launcher) and `update.sh` packages.
- Windows bundle (`setup.cmd`: install, verify, uninstall, rotate-key), with
  a hidden per-user scheduled task or Run entry; an install or update is a
  transaction that rolls back on failure.
- `tools\make-publisher-bundle.ps1` provisions the NATS user and builds
  either bundle, or the files for a PC with the app.

### Repository

- A Rust workspace: the app (Tauri 2) and the crates cc-core, cc-envelope,
  cc-focus, cc-hook, cc-local, cc-publish and cc-remote; the UI in plain
  JavaScript modules with Node tests.
- CI on Windows and Linux: fmt, build, tests, clippy, the minimum Rust
  version, UI and bundle script tests, a local NATS server test, a musl
  build, `cargo audit` and gitleaks.
- No key material in the repository, not even for tests.
- Documentation in `docs/`: install, usage, data and privacy, remote
  sessions, the publisher bundles, development and architecture.
