# Data and privacy

What cc-usage keeps on disk, what leaves the machine, and how the remote feed
is protected.

## Local files on the widget machine

### `%LOCALAPPDATA%\cc-usage`

The data folder. `CC_USAGE_DATA_DIR`, set to an absolute path, moves it
(tests, a second instance); an empty or relative value is ignored.
One app runs per data folder: starting it again shows the running one's
widget instead.
The installer puts its binaries here too; uninstalling removes only those.

| File | Contents |
|---|---|
| `cc-usage.exe`, `cc-hook.exe`, `uninstall.exe` | the installed app, the hook binary Claude Code runs, the uninstaller |
| `history.json` | one plan-usage sample per refresh, at least 5 minutes apart (a window reset always records), 30 days |
| `activity.json` | tokens per local day and busiest hour, per machine (this one and every publisher), 200 days; a day's count only grows |
| `remote-projects.json` | the last project value of each remote project key, so a machine that is off still shows what it used; dropped 30 days after its last activity |
| `notification-state.json` | which limit alerts were already shown, per reset window |
| `cc-usage.log` | the app's diagnostics, both roles (failed saves, remote and publisher errors, role switches), rotated at 1 MiB to `.log.1`; public key lines, never tokens, private keys or payloads |
| `statusline-chain.json` | the `statusLine` that `cc-hook install` replaced, so it keeps running and `uninstall` can restore it |
| `pricing.json` | optional price overrides for the per-project cost, written by you (format in [usage.md](usage.md#usage-view)) |
| `spool\` | one JSON file per hook call, written by cc-hook and deleted once read; unreadable ones go to `spool\failed\` |
| `live\` | one file per session from `cc-hook statusline`: context use, model, limits; files of ended sessions and day-old ones are deleted |
| `keys\viewer.key` | the viewer's private keys, created on the app's first start in the Viewer role; only this machine can open what publishers send (secret) |
| `keys\viewer.pub` | the matching public keys, written again whenever they drift from `viewer.key`; every publisher bundle carries a copy (not secret) |
| `keys\publishers\<alias>.pub` | each publisher's pinned public keys, written when you click Accept in the flyout's key notice (not secret, but it decides who may write as `<alias>`) |
| `keys\publishers\<alias>.rejected` | publisher keys you rejected for `<alias>`; their values are dropped unopened (not secret) |
| `keys\publishers\<alias>.pending` | publisher keys waiting for your Accept or Reject: key line, first and last seen, how many values were held back (not secret; never in a backup) |
| `nats\*.creds` | NATS credentials: `cc-sessions.viewer.creds` for the widget, plus any publisher creds the bundle generator downloaded (secret) |
| `bundles\` | publisher bundles made by `tools\make-publisher-bundle.ps1`; they hold the NATS creds (secret; delete once installed) and the viewer's public key, never a private key |
| `cc-hook.log` | cc-hook's timings, only while `CC_HOOK_DEBUG` is set |

`history.json`, `activity.json`, `remote-projects.json`,
and `notification-state.json` are written atomically (temp
file, then rename), so a crash never leaves a half-written file. A rename
that Windows refuses because another program holds the file open for a
moment (an antivirus scan) is retried for up to about three seconds. An
`activity.json` that does not parse is renamed to
`activity.json.corrupt-<timestamp>` and a new one starts; one that cannot be
read at all is left untouched and never overwritten.

Spool events older than 1 hour are deleted, and `spool\failed\` entries after
7 days. On Unix the spool and `live/` folders are private to the user.

### `%APPDATA%\ar.cc.usage`

The app config folder (`CC_USAGE_CONFIG_DIR` moves it). WebView2 keeps its
own cache in `%LOCALAPPDATA%\ar.cc.usage\EBWebView`.

| File | Contents |
|---|---|
| `settings.json` | shortcut, machine name, theme, refresh interval, idle and offline delays, full-screen rule, role (Viewer or Publisher); the NATS servers and bucket of Settings → Remote connection (a `"nats"` object, also editable by hand; [remote.md](remote.md#connection-settings)) |
| `window.json` | position and size |

Both are written atomically. Each setting is read on its own: a missing or
bad value resets only that setting to its default.

### Backups

Settings → Data → **Back up…** writes a `.ccbak` file wherever you choose
(Documents by default). It is JSON:

| Part | Contents |
|---|---|
| header | format and version, creation time (UTC), app version, this machine's name |
| `data` | `history.json`, `activity.json`, `remote-projects.json` and `pricing.json` as they are on disk; a missing or unreadable file is `null` |
| `secrets` | optional, only with "Include keys and credentials": the files `nats\*.creds` and anything under `keys\`, sealed |

The secrets are encrypted with XChaCha20-Poly1305 under a 32-byte key that
Argon2id derives from your password (64 MiB, 3 passes, 1 lane, a random
16-byte salt; the parameters are recorded in the file) with a random
24-byte nonce. Without the password they cannot be read, and a changed byte
makes them fail to open. The password is never stored. Nothing else in the
file is encrypted: it holds your plan usage over time, tokens per day per
machine, project folder names and machine aliases. Keep backups private,
and the password apart from a backup that carries keys.

**Restore…** adds a backup to this machine and never replaces anything (see
[usage.md](usage.md#data)). It does not restore `settings.json`,
`window.json` or `notification-state.json`, which belong
to the machine; the shortcut, machine name and theme are set again by hand.
It only writes secret files under those names, never outside the
data folder, and only where they are missing; an existing
`keys\viewer.key` gets the backup's other keys after its own (a viewer
moved to a new PC, where the app already made a key pair of its own: that
one stays the newest, the old one is added, so what publishers still seal to
the old key opens), and so does an existing `keys\publishers\<alias>.pub`
or `<alias>.rejected`; `keys\viewer.pub` is written again from
`viewer.key`. The keys waiting for review (`<alias>.pending`) are never
backed up, and a backup naming one is refused. Any other existing
file that differs, or a key id that holds two different keys, is left as it
is and reported. A backup that names any file outside the keys and
credentials is refused before anything is written. A backup is at most 64 MB; larger files are
refused. The backend only restores the file its own Open dialog picked in
the same run (the `restore_backup` command refuses any other path), so the
panel cannot point it at another file.

### Claude Code's files

| File | What cc-usage does with it |
|---|---|
| `~/.claude/.credentials.json` (or under `CLAUDE_CONFIG_DIR`) | reads the OAuth token to call `api.anthropic.com/api/oauth/usage` and `/profile`. Never writes it and never refreshes the token: Claude Code owns the file, and a refresh here could sign it out. An expired token shows the last data as stale until Claude Code renews it; meanwhile the widget runs `claude -p` with no prompt and an empty stdin (output discarded; Claude Code renews its token at start and exits before sending anything) at most every 5 minutes so that Claude Code itself can renew it, and reads the file again every 20 seconds (no API call), so the usage returns soon after the renewal. |
| `~/.claude/settings.json` | `cc-hook install` adds its hook entries after any others and puts itself in front of `statusLine.command`; `uninstall` removes exactly those entries and restores the previous `statusLine`. Each edit keeps a backup `settings.json.bak-cc-hook-<yyyyMMdd-HHmmss>` (at most 5), preserves the file mode, writes through a symlink to its target, and aborts if the file changed while it worked. |
| `~/.claude/sessions/*.json`, `claude agents --json` | the live session list. The `.key` files there are never opened. |
| `~/.claude/projects/**/*.jsonl` | transcripts: titles and colours (read from the end, never the conversation), token counts per project and per day. Each API response counts once, by its `message.id` and `requestId`, although Claude Code writes one line per content block and a resumed session can copy earlier lines into a new transcript; transcripts are read oldest first (by modification time, then path), so a copied response belongs to the transcript that first wrote it. The project scan skips files older than its window, reads line by line, goes at most 8 folders deep and does not follow symlinks, junctions or other reparse points. Session ids are validated before they are used to build a transcript path. |

`CLAUDE_CONFIG_DIR`, when set to an absolute path, replaces `~/.claude` for
all of these; an empty or relative value is ignored.

## Local files on a publisher machine

The data folder is `%LOCALAPPDATA%\cc-usage` on Windows and
`$XDG_DATA_HOME/cc-usage` or `~/.local/share/cc-usage` on Linux (the
`PREFIX` of `make install`).

| File | Contents |
|---|---|
| `cc-hook`, `cc-publish` | the binaries (`bin/` on Linux, the folder itself on Windows) |
| `nats/cc-sessions.pub-<alias>.creds` | the publisher's NATS creds (secret; 0600, or user and SYSTEM only on Windows) |
| `keys/publisher.key` | the machine's own private key, its identity: made on the machine by its first install (`make install`, `Install.ps1`), kept on every reinstall and update, never in a bundle (secret; 0600 in a 0700 folder, or user and SYSTEM only) |
| `keys/viewer.pub` | the viewer's public key, which every value is sealed to (not secret) |
| `publish.json` | cc-publish's config: alias, servers, bucket, creds and key file paths, `log_file` |
| `publish-state.json` | the running publisher's pid, connection, counters and last error, for `status`, `clear` and `stop` |
| `cc-publish.lock` | empty; the running publisher holds an exclusive lock on it, so a second one does not start |
| `cc-publish.stop` | a stop request from `cc-publish stop`, removed when handled |
| `cc-publish.log` | the log when `log_file` is on (the Windows default), rotated at 1 MiB to `.log.1`; on Linux stderr goes to the journal |
| `install-manifest.sha256` (Linux), `publisher-manifest.json` (Windows) | SHA-256 of every installed file, checked by verify and by `update.sh` |
| `publisher.env` (Linux) | the data dir and, for a custom Claude folder, `CLAUDE_CONFIG_DIR` |
| `cc-publish.pid` (Linux, profile autostart only) | the background publisher's pid |
| `bundle/` | a copy of the bundle's scripts, without binaries or secrets, for later verify and uninstall |
| `spool/`, `live/`, `statusline-chain.json` | the same as on the widget machine |

A PC with the app in the **Publisher role** keeps the app's files above
plus `nats\cc-sessions.pub-<alias>.creds` (imported by Settings → Role,
written privately), `keys\publisher.key` (made there by Generate key, never
copied anywhere; secret), `keys\viewer.pub` (imported; not secret) and
`publish-state.json` and `cc-publish.lock` (the same files `cc-publish`
keeps, written and locked by the app, so neither publishes while the other
runs). No `publish.json`, no
`cc-publish.log` (the app logs to `cc-usage.log`). The creds and the two
key files are covered by "Include keys and credentials" in a backup.

## What leaves the machine

The widget calls `api.anthropic.com` for the plan usage and profile, with
Claude Code's token. Nothing else leaves a widget machine in the Viewer
role. In the Publisher role the app also sends exactly what `cc-publish`
sends, below (same pipeline, same payloads, sealed the same way),
under this machine's name as the alias; the host record carries the
workspace version, the same for both.

A publisher writes four kinds of keys to the KV bucket (`cc-sessions`
unless the [connection settings](remote.md#connection-settings) name
another), each value sealed before it is sent (to the viewer's key, see
below):

| Key | Contents |
|---|---|
| `<alias>.s.<sid>` | one session, as its row shows it: state, time of the last state change, the current tool's name, model, context %, heartbeat; the title (custom, else AI, else registry name; at most 80 characters), the working directory's folder name (not the path, at most 64), and the one text the row's second line shows in that state, at most 120 characters: the question (needs you), the tool summary (working: a shell command's description or program name, a file name, a search pattern or query, a web host), the error (failed) or the last message (done, idle). Texts are flattened to one line and cut on the publisher, and cut again on the viewer. Also how many subagents run of how many (at most 64), and for the first six (running ones first, in what the payload's budget of 2800 bytes of JSON leaves after the other fields) a short line each, flattened and cut the same way: the subagent type (at most 24 characters), its task description (what the main agent wrote as the `Agent` call's description, at most 60), the name of the tool it runs (at most 32) and that tool's summary, made as above (at most 60). Never a prompt, neither the user's nor the one the main agent gives the subagent. A session whose turn ended while background agents run is sent as working. A session alias field exists in the schema but is always empty today. |
| `<alias>.host` | the machine: cc-publish version, OS (`windows`, `linux`), heartbeat |
| `<alias>.p.<pid>` | one project: its folder name (not the path, at most 64 characters), session count, tokens per model, last activity, the window in days (30), heartbeat |
| `<alias>.activity` | tokens per local day and the busiest hour of each, for at most the last 35 days with any |

`<sid>` and `<pid>` are the first 8 hex characters of a SHA-256 of the
session id and of the project's transcript folder name, so neither appears
in clear.

**Never published:** prompts, the conversation beyond the short texts above,
full tool input, file paths, costs, the account email, the plan usage. The
session texts are the ones the row shows, cut to 120 characters, and they
are free text: a question, an error or the last message can quote anything
Claude wrote, and a tool summary can contain part of a command (a shell
command's description, a search pattern). The only filter is cc-hook's
masking of obvious secrets in what it spools (see
[Local hardening](#local-hardening)); titles and texts that come from the
transcripts or Claude Code's session registry are sent as they are, cut.

## Security model

Exactly one machine is the **viewer**: the one with the cc-usage app in the
Viewer role, which reads the bucket. Every other machine is a **publisher**
(`cc-publish`, or the app in the Publisher role). Each has its own
key pair; only the viewer can decrypt, and each publisher proves which key
sealed a value.

- **The envelope: sealed to the viewer, authenticated by the sender.** Every
  value is [HPKE](https://www.rfc-editor.org/rfc/rfc9180) in Auth mode,
  DHKEM(X25519, HKDF-SHA256) with HKDF-SHA256 and ChaCha20-Poly1305, with
  a fresh ephemeral key per value, info `cc-sessions envelope` and the
  KV key as associated data (so a value cannot be moved to another key).
  It is sealed from the publisher's private key (`keys/publisher.key`) to
  the viewer's public key (`keys/viewer.pub`), and opens only with the
  viewer's private key (`keys/viewer.key`, which never leaves the viewer
  except inside a password-sealed backup) and that publisher's public key.
  The envelope is JSON of at most 4096 bytes:
  `{"v":1,"rk":"<viewer key id>","sk":"<sender key id>","spk":"<sender public key>","e":"<encapsulated key>","c":"<ciphertext>"}`;
  `v` is the format version, 1, and a value with any other is
  not opened.
- **A leaked publisher key or bundle reveals nothing.** A publisher holds
  only its own private key and the viewer's public key: it cannot read its
  own values once sent, nor any other machine's. A bundle carries no
  publisher key: whoever steals one gets the alias's NATS creds, and what
  it writes is sealed by a key of its own, which the viewer holds back as
  a new key (Reject it). Whoever also holds the machine's
  `keys/publisher.key` can write as that alias until the user and the
  pinned key are revoked (below), and nothing more.
- **Every key pinned with consent, per alias.** The viewer pins each
  publisher's public key to its alias in `keys/publishers/<alias>.pub`
  (`<kid>:<base64>` lines, newest first). The alias is the first token of
  the KV key, which NATS lets only `cc-sessions.pub-<alias>` write; the
  alias inside the payload is never trusted. There is no trust on first
  use: no bundle carries a publisher key and the bundle tool pins none, so
  every key a machine makes (on its first install, by a rotation, the app's
  **Generate key** or **Replace key…** in the Publisher role) waits for the
  viewer's user. Its values must still open with the key they claim (so the
  sender holds it), are never shown, and the key is offered in the flyout's
  notice with its key id and a short fingerprint (the first 8 base64
  characters) to compare with what the publisher printed. **Accept** pins
  it; **Reject** writes it to `<alias>.rejected`, whose values are dropped
  unopened from then on. At most 3 keys per machine and 16 machines wait at
  once, in `keys/publishers/<alias>.pending`.
- **What a new key means.** The notice says "`<alias>` presents a new key".
  Either the machine got a new key (`make rotate-key`, `setup.cmd
  rotate-key`, `cc-publish keygen --force` or `--rotate`, **Replace key…**,
  a lost key replaced on purpose, a reinstall after an uninstall that
  purged its data), or something else holds that alias's NATS creds and is
  publishing with its own key. Accept only a key whose line you saw on the
  machine (printed by `make install`, `Install.ps1`, `update.sh`, the
  rotate-key commands and `keygen`, and shown by **Generate key**);
  otherwise Reject it and
  revoke the NATS user. A pin file that holds no key at all refuses the
  alias without asking, and its tooltip says "`<alias>`: unknown sender key
  — values rejected".
- **What the NATS server's operator can see** (Synadia in the
  [reference deployment](remote.md#reference-deployment)): key names (aliases, the hashed ids, the kind of
  record), value sizes, when they are written, and in every value the key
  ids and the sender's public key (not secret). Never the contents.
- **Least privilege on NATS.** Each publisher has its own user,
  `cc-sessions.pub-<alias>`, that can only write under `<alias>.>` and cannot
  read anything. The widget's `cc-sessions.viewer` can read the whole bucket
  and cannot write. Each user receives replies only on its own inbox prefix,
  `_INBOX.<user>`.
- **TLS only.** The clients accept `tls://` and `wss://` servers and refuse
  `nats://` and `ws://`, and a server URL with a user or password in it;
  the credentials come only from the `.creds` file.
- **No other envelope.** A publisher without both key files does not
  publish, and the viewer rejects a value whose `v` is not 1 like any other
  value it cannot open (counted in the tooltip).
- **Distrust of remote values.** The envelope proves which key wrote a
  value, not that its contents make sense, so the widget:
  - ignores a value whose heartbeat is older than the one it holds for that
    key (a replay). If a publisher's clock steps back, its writes are ignored
    until its clock passes the last heartbeat held;
  - believes timestamps at most 1 day ahead of its own clock;
  - holds at most 200 sessions and 200 projects per machine, and at most
    64 machines: the values of further ones are ignored unopened (no key of
    theirs offered for review) and counted in the Remote line's tooltip; a place
    opens when a machine held has nothing left;
  - ignores activity days after tomorrow and days above 10^12 tokens;
  - cuts what it shows again whatever the publisher sent: a session's
    title, folder and text (80, 64 and 120 characters), tool name and
    model (64 each); a project's name to 64 characters and its models to
    12 plus one "(other models)" sum, whether heard live, loaded from
    `remote-projects.json` or restored from a backup;
  - goes by the KV key, not the payload, for a session's id, and rejects a
    session, host, project or activity value whose payload names another
    machine than its key (counted like a value it cannot open);
  - does not load a `remote-projects.json` over 8 MB: it is kept aside as
    `remote-projects.json.corrupt-<date>`;
  - never shows a value it cannot open (counted in the Remote line's
    tooltip).

### Key rotation

Every key file holds `<kid>:<base64>` lines, newest first; an envelope names
the keys it used (`rk`, `sk`), so old values stay readable while keys
rotate.

- **A publisher's key.** On the publisher, `cc-publish keygen --rotate`
  adds a new key and prints its line; a running publisher seals with it
  within 30 s, and the viewer holds its values back and offers the new key
  in the flyout's notice: accept it there after comparing it with that line
  (or add the line at the top of `keys\publishers\<alias>.pub` by hand).
  To revoke the old key afterwards, move its line from that file to
  `<alias>.rejected` (deleting it only puts it up for review again). `keygen
  --force`, or **Replace key…** in the app's Publisher role, replaces the key
  instead of adding one; the same accept step applies.
- **The viewer's key.** On the viewer, `cc-publish viewer-keygen --rotate`
  adds a new key to `viewer.key` and writes `viewer.pub`; the old key stays,
  so publishers that still seal to it keep working. Give every publisher the
  new `viewer.pub` (a new bundle, the Linux update package, or **Import
  viewer key…** on a PC in the app's Publisher role). No restart: a running
  `cc-publish` seals to it within 30 s, the app at once after the import
  (Settings says so), and every value is put again soon after. A
  `viewer.pub` that does not load is reported and the key in use stays.
  Once every machine seals to the new key (`cc-publish status` names the
  viewer key in use), the old line can be removed from `viewer.key`; values
  still sealed to it then do not open.

### Moving the viewer to another PC

`viewer.key`, `viewer.pub` and the pins and rejected keys under
`keys\publishers\` travel in a backup made with "Include keys and
credentials" (password-sealed); keys still waiting for review do not, and
are offered again by their next value. Restore it
on the new PC: its `viewer.key` keeps the key the app made there on first
start and gains the old one, which is what the publishers seal to, and the
pins come along, so nothing needs to change on the publishers. Its
`viewer.pub` then holds both keys, the new one first; bundles made anywhere
else from then on need a copy of it (`-ViewerPub`), and the viewer asks to
accept their keys. The old PC becomes a publisher with its own NATS
user and its own publisher key (Role: Publisher, see
[remote.md](remote.md#moving-the-viewer-to-another-pc)), and its old
`keys\viewer.key` and `nats\cc-sessions.viewer.creds` are deleted there:
only one PC may hold a viewer connection.

### Revoking a machine

1. On the viewer, delete the machine's line from
   `keys\publishers\<alias>.pub`, but keep the file, even empty or with
   only a `#` comment: an alias whose pin file holds no key is refused
   altogether, while an alias without a pin file would offer the next key
   that writes as it for review. Its values are then rejected (within 30
   seconds) and named in the tooltip. To let the machine back later, add
   its new key's line to the file by hand.
2. Revoke the NATS user: `toolctl users delete cc-sessions.pub-<alias> --yes`
   in the reference deployment, else with the tool that made it (`nsc
   revocations add-user`, `nsc delete user`, the Synadia UI). Generating the
   bundle again is not enough on its own, because `apply` keeps the user's
   nkey; after the delete, the next bundle for that alias creates the user
   anew with a new nkey.
3. Delete every copy of the old bundle (`%LOCALAPPDATA%\cc-usage\bundles`
   and wherever it was copied).

### A leaked bundle

A bundle holds a publisher's NATS creds and the viewer's public key,
nothing that decrypts and no publisher key. Revoke its NATS user (step 2
above) and make the machine a new bundle: its key never travelled, so its
pin can stay, and the values a thief seals with a key of its own are held
back for review (Reject it).

## Local hardening

- Links open through `ShellExecute`, and only `https://` links.
- Development flags (`--demo`, `--reference`, `--click`, ...) only exist in
  debug builds; a release build ignores them.
- The usage view asks for 1 to 365 days of project data, and a slow response
  that arrives after a newer one is dropped.
- Hook summaries of shell tools (Bash, PowerShell) are the tool's
  description or the program name, never the command line; obvious secrets
  in spooled text (tokens, `PASSWORD=`-style assignments) are masked.
- Hook commands are single-quoted on Unix; on Windows, install refuses paths
  containing `"`, `%`, `$` or a backquote.
- The chained status line gets 5 seconds, then is killed with its process
  tree. On Windows, background processes it leaves behind are killed even
  when it finishes in time.
- stdin is capped at 16 MiB for the hook receiver and 4 MiB for the status
  line.
