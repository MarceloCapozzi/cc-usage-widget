# Remote sessions

Machines without the app run `cc-publish`, which publishes their Claude Code
sessions, projects and activity, encrypted, to a NATS KV bucket; a machine
with the app publishes from the app itself, in the Publisher role
([below](#a-pc-with-the-app-as-a-publisher)). The widget on the viewer
watches the bucket and shows those sessions as rows tagged with the
machine's alias, their projects in the usage view and their days in the
activity calendar.

```
publisher machine                          NATS server                 widget machine
Claude Code --hooks--> cc-hook --> spool/
claude agents, transcripts, live/  --> cc-publish --put--> KV cc-sessions --watch--> cc-usage
                                   (seals to viewer.pub    (ciphertext,           (opens with viewer.key
                                    with publisher.key)     TTL 3 min)             and the pinned key of
                                                                                   the alias: rows,
                                                                                   projects, activity)
```

The widget machine is the **viewer**, the only one that can decrypt. Every
publisher seals with its own key pair (HPKE): see
[Keys](#keys) below.

What travels and how it is protected is in
[data-and-privacy.md](data-and-privacy.md#what-leaves-the-machine).

## Requirements

What any deployment needs. Nothing in the code is tied to one NATS
provider: the servers and the bucket are settings
([Connection settings](#connection-settings)).

- **A NATS server with JetStream**, reachable from the viewer and every
  publisher over `tls://` or `wss://`. The clients refuse `nats://` and
  `ws://`.
- **One KV bucket**, `cc-sessions` unless the settings name another, with
  these limits. Several are tied to constants in the code, and tests read
  them from the template ([Provisioning](#provisioning)) and fail when they
  drift apart (`crates/cc-publish/tests/project_file.rs`,
  `app/src/remote/tests.rs`):

  | Limit | Value | Why |
  |---|---|---|
  | `history` | 1 | only the latest value of a key counts |
  | `max_age` (TTL) | 3 minutes | at least 3 publisher heartbeats (60 s each) and at least the projects' re-put (every 2 minutes) plus one heartbeat, so a live key never expires; longer than the viewer's offline delay (150 s), so a row turns offline before its key goes |
  | `max_value_size` | 4096 bytes | the largest sealed value (`cc_envelope::MAX_ENVELOPE_BYTES`) |
  | `max_bytes` | 16 MiB | the template's size; far more than a few machines' sessions and projects need |
  | storage, replicas | file, 1 | the template's; more replicas work too |

- **NATS users with credentials files** (`.creds`: a user JWT and its nkey
  seed), whose names are fixed by the code: one viewer, and one publisher
  per machine. Each user may subscribe only to its own inbox prefix, and
  the clients connect with that prefix, `_INBOX.<user>`, so a publisher
  never sees the viewer's deliveries. Below, `KV_<bucket>` is the stream
  behind the bucket:

  | User | May publish | May subscribe |
  |---|---|---|
  | `cc-sessions.viewer` | `$JS.API.INFO`, `$JS.API.STREAM.INFO.KV_<bucket>`, `$JS.API.STREAM.MSG.GET.KV_<bucket>`, `$JS.API.DIRECT.GET.KV_<bucket>` and `.>`, `$JS.API.CONSUMER.CREATE.KV_<bucket>` and `.>`, `$JS.API.CONSUMER.INFO.KV_<bucket>.>`, `$JS.API.CONSUMER.DELETE.KV_<bucket>.>`, `$JS.FC.KV_<bucket>.>`; denied `$KV.<bucket>.>` (it reads everything, writes nothing) | `_INBOX.cc-sessions.viewer.>` |
  | `cc-sessions.pub-<alias>` | `$KV.<bucket>.<alias>.>`, `$JS.API.INFO`, `$JS.API.STREAM.INFO.KV_<bucket>` (it writes only its own keys and reads nothing) | `_INBOX.cc-sessions.pub-<alias>.>` |
  | `cc-sessions.pub-test` | the same for alias `test` | the same |

  `cc-sessions.pub-test` is only for the live checks and tests, never for a
  real machine. Every connection counts against the server's or account's
  connection limit: the viewer holds one, each running publisher one, and
  a check opens more for a few seconds (`cc-publish check` one, and so
  `make verify` and `Verify.ps1`; `remote_check` and `toolctl check` two).
- **Where the creds go**: `%LOCALAPPDATA%\cc-usage\nats\<user>.creds` on
  the viewer (`cc-sessions.viewer.creds`); publishers get theirs in a
  bundle ([Adding a machine](#adding-a-machine)).

## Reference deployment

The author's deployment, which the defaults point at:

| Resource | Value |
|---|---|
| NATS | Synadia Cloud, in an account shared with other personal tooling projects |
| Account limits (free plan) | 5 connections, 50 subscriptions, 64 KB payload, JetStream R1 1 GiB, 5 streams |
| Endpoints | `wss://connect.ngs.global` (443, tried first: it gets through corporate networks) and `tls://connect.ngs.global` (4222) |
| Bucket and users | as in [Requirements](#requirements), provisioned with toolctl ([below](#with-toolctl)) |

**The connection budget.** The account allows 5 connections in total,
shared with every other project in it, so with the viewer and a check
running, fewer publishers fit. A second viewer, such as an old viewer PC
still running with its creds, takes one too.

**R1 only.** The bucket lives on one server. If it goes down, remote rows go
offline until it returns; local rows are unaffected.

## Provisioning

The bucket and the users are declared in a project file. The repository
holds the template,
[`infra/nats/cc-sessions.example.yaml`](../infra/nats/cc-sessions.example.yaml),
with the bucket's limits, the viewer, `pub-test` and one example publisher
(`devbox`). A deployment's own copy, `infra/nats/cc-sessions.yaml`, is
ignored by git, because it lists that deployment's machine aliases. Copy
the template first and set its `account`:

```powershell
Copy-Item infra\nats\cc-sessions.example.yaml infra\nats\cc-sessions.yaml
```

The copy must keep `project: cc-sessions` (the user names come from it) and
declare the bucket the clients use. The bundle generator reads it (its
`-ProjectFile`, by default that copy) and stops before anything is built
or written when it is missing, unless `-SkipProvision` or `-Update`.

### With toolctl

`toolctl` is an internal maintainer tool: a Go CLI, kept in a separate
private repository (`tooling-ctl`), that applies project files like this one
to a Synadia Cloud account. Nothing in cc-usage needs it to build, run or
publish; without it, provision [by hand](#by-hand). The bundle generator
runs it by itself unless `-SkipProvision`; by hand:

```powershell
toolctl plan  -f infra\nats\cc-sessions.yaml                    # read-only diff; "No changes." when the account matches
toolctl add-publisher devbox -f infra\nats\cc-sessions.yaml     # edits the local file only (it stays out of git)
toolctl apply -f infra\nats\cc-sessions.yaml --no-creds         # creates or re-applies users
toolctl creds cc-sessions.pub-devbox                            # -> %LOCALAPPDATA%\cc-usage\nats
toolctl creds cc-sessions.viewer
toolctl check -f infra\nats\cc-sessions.yaml                    # end-to-end permission checks
toolctl users list
```

Never `apply --prune`: it deletes the project's users the file does not
list. The Synadia token comes from `SYNADIA_CLOUD_ACCESS_TOKEN` in the
environment, else from the repository's `.env` (see `.env.example`; the
bundle generator passes it to toolctl's process only), else from toolctl's
OS credential store (`toolctl auth login`). Neither the app nor the publisher
needs the token; it is only for adding machines.

`toolctl check` writes a probe under `<alias>.s.toolctl-probe`. The viewer
ignores it without opening it (`toolctl-probe` is not a session id), so it
never shows as a row or as a value that failed to open.

### By hand

Any NATS tooling does the same: the NATS CLI and `nsc` on a server of your
own, or the Synadia Cloud web UI.

1. Create the bucket with the limits in [Requirements](#requirements), for
   example `nats kv add cc-sessions --history 1 --ttl 3m --max-value-size
   4096 --max-bucket-size 16MiB --storage file --replicas 1`.
2. Create `cc-sessions.viewer`, `cc-sessions.pub-test` and one
   `cc-sessions.pub-<alias>` per machine, with exactly the publish and
   subscribe permissions in the users table, for example `nsc add user
   cc-sessions.pub-devbox --allow-pub
   '$KV.cc-sessions.devbox.>,$JS.API.INFO,$JS.API.STREAM.INFO.KV_cc-sessions'
   --allow-sub '_INBOX.cc-sessions.pub-devbox.>'`.
3. Save each user's credentials as `<user>.creds` in
   `%LOCALAPPDATA%\cc-usage\nats` on the viewer (the bundle generator's
   `-CredsDir`), and build bundles with `-SkipProvision`, which takes the
   publisher's creds from there.

Then run the live checks below. Keep the project file in step with what you
created, so the next maintainer sees the same thing.

### Live checks

The repository's own permission check runs 14 checks per transport with the
viewer and `pub-test` creds:

```powershell
cargo run -p cc-remote --example remote_check                              # the default servers, wss and tls
cargo run -p cc-remote --example remote_check -- wss://connect.ngs.global  # the servers given
```

The servers are its arguments, else the defaults; the bucket is
`CC_USAGE_NATS_BUCKET`, else `cc-sessions`. Every line must print PASS
(exit 1 otherwise). It writes only under `test.s.`.

## Connection settings

By default every client connects to `wss://connect.ngs.global`, then
`tls://connect.ngs.global` (WebSocket first), and uses the bucket
`cc-sessions`. Another deployment needs no rebuild:

| Client | Where its servers and bucket come from |
|---|---|
| the app, both roles | `CC_USAGE_NATS_SERVERS` (comma separated) and `CC_USAGE_NATS_BUCKET`, else Settings → Remote connection (the `"nats"` object of `settings.json` in `%APPDATA%\ar.cc.usage`, or `CC_USAGE_CONFIG_DIR`), else the defaults; each setting on its own, and a blank variable counts as unset |
| `cc-publish` | `servers` and `bucket` in its `publish.json` ([cc-publish's README](../crates/cc-publish/README.md#config)) |
| a bundle | `make-publisher-bundle.ps1 -Servers <urls> -Bucket <name>`, written to the bundle's `publish.json`; by default the viewer's own settings on the PC that builds it, resolved as the app does (`settings.json` is only read), else the defaults |

In the app, **Settings → Remote connection** edits them: the servers one
URL per line, the bucket, each blank for its default (shown greyed in the
field), and **Reset to defaults**. Save checks them with the rules below; a
value that breaks them is not saved, and the reason shows under the group.
A setting a `CC_USAGE_NATS_*` variable sets shows the value in force,
read-only, with a line naming the variable: the variable wins, so unset it
to change that setting there. Settings writes them to the `"nats"` object
of `settings.json`, which can also be edited by hand; both members are
optional, and a value equal to the default is not written:

```json
"nats": { "servers": ["tls://nats.example:4222", "wss://nats.example"], "bucket": "cc-sessions" }
```

- **Servers** are tried in order. Each needs `tls://` or `wss://`, a host
  and an optional port (443 for `wss://`, 4222 for `tls://` when left out),
  and no user or password in the URL; an empty list is invalid.
- **Bucket**: 1 to 64 letters, digits, `-` or `_`. Every machine must use
  the viewer's bucket: a publisher writes `$KV.<bucket>.<alias>.>`, which
  its NATS user must allow. The user names stay `cc-sessions.viewer` and
  `cc-sessions.pub-<alias>` whatever the bucket.
- **A value that cannot be used** (a hand edit, or a variable) is never
  half used: that setting falls back to its default, and a line in the
  footer's Remote tooltip, under Settings → Remote connection and as a
  warning in `cc-usage.log` names the value ignored and why (a password in
  a URL shows as `***`). An unknown member, such as `nats.server`, is
  reported as a typo, and a save in Settings drops it. So the app never connects somewhere else without
  saying so. `make-publisher-bundle.ps1` warns about an unusable viewer
  setting the same way and uses the default; an unusable `-Servers` or
  `-Bucket` stops it.
- **When it applies**: the viewer reads the settings before every
  connection, every 30 seconds and at once after a save in Settings, and
  reconnects when the servers or the bucket change. The Publisher role
  reads them when the role starts: after a change Settings shows
  **Restart cc-usage**, and a hand edit or a new variable needs a restart
  too. Connected, the tooltip says `Watching
  <bucket> over <server>` (viewer) or `Publishing <bucket> as <user> over
  <server>` (Publisher).

Settings are not part of a backup: when the viewer moves to another PC,
set Remote connection there again (or copy the `"nats"` object).

## Setting up the viewer

On the widget machine, once:

1. Viewer creds: `%LOCALAPPDATA%\cc-usage\nats\cc-sessions.viewer.creds`,
   from [provisioning](#provisioning) (`toolctl creds cc-sessions.viewer`
   writes it there).
2. Nothing else for the keys: the app creates its key pair,
   `keys\viewer.key` and `keys\viewer.pub`, on its first start (and logs the
   public key line to `cc-usage.log`). `cc-publish viewer-keygen --if-missing` does
   the same without the app; the bundle generator runs it before every
   bundle made without `-ViewerPub`. Generate bundles on the viewer, so
   they carry its `viewer.pub`, or on another PC with a copy of it
   ([below](#bundles-once-the-viewer-is-another-pc)).
3. For servers or a bucket other than the defaults, the `"nats"` object
   ([Connection settings](#connection-settings)).
4. The widget looks for the creds every 30 seconds and connects without a
   restart. Until they exist the footer reads `Remote  not configured`, and
   its tooltip says which file is missing. It also reloads the key files
   when they change and rereads the pin files every 30 seconds.

## Keys

The envelope (see [data-and-privacy.md](data-and-privacy.md#security-model)):
every value is sealed by a publisher's private key to the viewer's public
key, so only the viewer decrypts and each value proves its sender key. All
key files hold `<kid>:<base64>` lines, newest first.

| File (under the data folder) | Machine | What |
|---|---|---|
| `keys\viewer.key` | viewer | its private keys; never leaves it except in a password-sealed backup |
| `keys\viewer.pub` | viewer and every publisher | the viewer's public keys; publishers seal to the first line |
| `keys\publishers\<alias>.pub` | viewer | each publisher's pinned public keys |
| `keys\publishers\<alias>.rejected` | viewer | keys its user rejected for that alias (same format) |
| `keys\publishers\<alias>.pending` | viewer | keys waiting for review (JSON; never in a backup) |
| `keys/publisher.key` | publisher | its own private key; never leaves it |

**Pinning needs consent.** Every new publisher key is accepted by the
viewer's user, the first key of a new machine included: there is no trust
on first use. No publisher key travels in a bundle, and the bundle tool
pins none: each machine makes its own key pair on its first install
(`make install`, `Install.ps1`, `cc-publish keygen` by hand, the app's
**Generate key**), keeps it on every
reinstall and update, and prints its public key line. A key the viewer
has not pinned for that alias (a new machine's, or a new one from `make
rotate-key`, `setup.cmd rotate-key`, `keygen --rotate`, **Replace key…**
or a lost key replaced on purpose) is held back when its first value
arrives: the value must open with the key it claims, is not shown, and the
flyout shows a
notice above the sessions, "New machine `<alias>` wants to publish · key
`<kid>` `<fingerprint>`" or "`<alias>` presents a new key (`<kid>`
`<fingerprint>`)", with **Accept** and **Reject**; the tray tooltip says
"1 key to review". The fingerprint is the first 8 characters of the key's
base64: compare it, and the key id, with the line the publisher printed
(`make install`, `Install.ps1`, `update.sh`, the rotate-key commands,
`keygen`, or Settings → Role on an app). **Accept** puts the line at the top of `<alias>.pub`
(creating it) and the machine's values show from its next write;
**Reject** adds it to `<alias>.rejected`, and its values are dropped from
then on, counted in the Remote tooltip. Nothing is pinned or rejected
without the click. Adding a line at the top of a `.pub` file by hand also
accepts a key (read within 30 seconds). Removing a line does **not** revoke the key: its next value
puts it up for review again. To revoke a key, move its line from
`<alias>.pub` to `<alias>.rejected` (rejecting a pinned key does the same);
a `.pub` file left with no key refuses the alias without asking.

**Both key files are required.** A publisher without `keys/publisher.key`
and `keys/viewer.pub` does not publish (`cc-publish check` and `status`
name the missing file).

### Adding a machine

Make its bundle on the viewer: `pwsh -File tools\make-publisher-bundle.ps1
-Alias <alias>` (`-Platform windows` for a Windows PC). It provisions the NATS
user, makes sure the viewer key pair exists and puts the creds
(`secrets/`), `config/viewer.pub` and a `config/publish.json` with the
viewer's servers and bucket into the bundle; no publisher key, and
nothing is pinned. On a PC that is not the viewer, add `-ViewerPub`
([below](#bundles-once-the-viewer-is-another-pc)). Then install it as
[publisher-linux.md](publisher-linux.md) or
[publisher-windows.md](publisher-windows.md) say: the first install makes
the machine's key pair and prints its public key line, and the viewer's
flyout asks to accept that key with the machine's first value
([above](#keys)).

## Moving the viewer to another PC

The viewer's history, Activity days and other machines' projects exist only
on the viewer, and so do its creds and keys (`keys\viewer.key`, which the
publishers seal to, and the pinned publisher keys). To move it:

1. On the old viewer, Settings → Data: check **Include keys and
   credentials**, type a password twice, **Back up…**. Copy the `.ccbak`
   file to the new PC (a USB stick, a private share); tell the password
   separately.
2. On the new PC, install the app. If it ran `cc-publish` before, stop and
   uninstall the publisher first: a machine runs one or the other. If the
   app there is in the Publisher role, switch it to Viewer first. Copy the
   old viewer's `"nats"` object into its `settings.json`, if it had one.
3. Settings → Data → **Restore…**, pick the file, type the password,
   **Restore**. The keys and the viewer creds are written to
   `%LOCALAPPDATA%\cc-usage`, and the widget connects within 30 seconds.
   The app there already made a key pair of its own on first start: the
   restore keeps it as the newest and adds the old viewer's key after it,
   so everything the publishers seal (to the old key) opens, and the pins
   come along. Publishers need no change. The new PC's `keys\viewer.pub`
   now holds both public keys, the new one first: every later bundle,
   update package or app import seals to it, so wherever the bundle tool
   runs it needs a copy of that file
   ([`-ViewerPub`](#bundles-once-the-viewer-is-another-pc)).
   The old viewer's own days show under its name in the calendar; days it
   heard from the new PC as a publisher, under the new PC's "This machine's
   name", join the new PC's own, and its projects are skipped (the
   transcripts have them).
4. Turn the old PC into a publisher with the app's **Role: Publisher**, as
   in [A PC with the app as a publisher](#a-pc-with-the-app-as-a-publisher),
   under the name it had as "This machine's name", so its days continue
   under the same name on the new viewer (the new PC's name must differ:
   a viewer ignores values under its own name). Do it right after the
   restore: until then both PCs hold a viewer connection, and each counts
   against the connection limit; if it has to wait, quit the app on the old PC.
   In short:
   1. Copy the new viewer's `keys\viewer.pub` to the machine with this
      repository and the provisioning tools, and run `pwsh -File
      tools\make-publisher-bundle.ps1 -Alias <old name> -Platform app
      -ViewerPub <copy>`. It provisions `cc-sessions.pub-<old name>` and
      writes its creds and that `viewer.pub` to a private folder.
   2. On the old PC: Settings → Role: Publisher → **Switch and restart**
      (from then on it opens no viewer connection); then **Import creds…**,
      **Import viewer key…**, **Generate key**. The footer reads
      `publishing as <old name> · ● connected`.
   3. On the new viewer, accept the old PC's key in the flyout's notice
      (compare it with the line **Generate key** shows on the old PC).
   4. On the old PC, delete `keys\viewer.key` and
      `nats\cc-sessions.viewer.creds` (the backup has them): kept, the creds
      connect a second viewer as soon as the PC is switched back to Viewer.

A backup without keys moves only the data: the new PC then needs its own
creds, and every publisher needs a new bundle (or update package) with the
new PC's `viewer.pub`, since nothing else can open what they seal.
Restoring more than once, or a second backup on top, is safe: a restore only
adds.

### Bundles once the viewer is another PC

The bundle tool needs this repository and the provisioning tools (toolctl
and the Synadia token in the reference deployment), so it usually stays on
the development PC even after the viewer has moved. Copy
`%LOCALAPPDATA%\cc-usage\keys\viewer.pub` from the viewer (it is public;
compare the lines the script prints with the viewer's file) and pass it to
every bundle, update package and `-Platform app` folder:

```powershell
pwsh -File tools\make-publisher-bundle.ps1 -Update -ViewerPub D:\viewer.pub
pwsh -File tools\make-publisher-bundle.ps1 -Alias office-pc -Platform windows -ViewerPub D:\viewer.pub
```

The bundle's servers and bucket come from the settings of the PC that
builds it: when they differ from the viewer's, pass `-Servers` and
`-Bucket`.

Nothing is pinned on the PC that builds the bundle, with or without
`-ViewerPub`; the viewer asks about each new publisher key when its first
value arrives (the flyout's notice, [above](#keys)). A reinstall from a new
bundle keeps the machine's key, so the viewer asks nothing; a new machine
under an alias the viewer already pins shows there as "`<alias>` presents
a new key": accept it after comparing it with the public key line its
install printed (or add that line at the top of
`keys\publishers\<alias>.pub` on the viewer by hand). Without `-ViewerPub`
the viewer key is the local one, so the script refuses to run on a PC in the app's
Publisher role, before anything is built, provisioned or written (it would
otherwise make a viewer key pair there that nobody reads with).

## A PC with the app as a publisher

A PC that runs the app must never run `cc-publish` as well: both would drain
the same hook spool. When such a PC has to be seen from the viewer, the app
publishes itself: Settings → **Role: Publisher**
([usage.md](usage.md#role)). It runs the same pipeline as `cc-publish run`
(planner, 60 s heartbeat, deletes, host record, projects every 5 minutes,
activity, newest-wins outbox, reconnects with backoff and resync), fed by the
app's own session pass, so the spool is drained once. One connection, as
`cc-sessions.pub-<alias>`, to the servers and bucket of its own
[connection settings](#connection-settings), read when the role starts. The
alias is "This machine's name". It seals every value: it
does not publish without `keys\publisher.key` and `keys\viewer.pub`. In this role there is no viewer connection and
the PC shows only its own sessions, projects and activity.

It needs three files, which Settings → Role checks with ✓ or ✗:

| File | How it gets there |
|---|---|
| `nats\cc-sessions.pub-<alias>.creds` | **Import creds…** (the user inside must be `cc-sessions.pub-<alias>`) |
| `keys\viewer.pub` | **Import viewer key…** (the viewer's `keys\viewer.pub`) |
| `keys\publisher.key` | **Generate key** on the PC itself; replaced only by **Replace key…** with the machine's name typed back; never copied |

Preparing a PC, from the machine with this repository and the provisioning
tools:

```powershell
pwsh -File tools\make-publisher-bundle.ps1 -Alias <alias> -Platform app
```

It provisions the NATS user like the other platforms (`toolctl
add-publisher`, `apply --no-creds`, `creds`) and writes
`%LOCALAPPDATA%\cc-usage\bundles\cc-usage-publisher-<alias>-app\` (private
ACL) with `cc-sessions.pub-<alias>.creds` and the viewer's `viewer.pub`
(the building PC's own, so it must be the viewer; elsewhere add `-ViewerPub
<copy of the viewer's viewer.pub>`). No publisher private key: the app makes
its own on the PC, and the viewer's flyout asks to accept its public key
when its first value arrives (as "`<alias>` presents a new key" when the
viewer already pins keys for that alias; the script says so when it runs on
the viewer). Compare it with the line **Generate key** shows. With servers
or a bucket other than the defaults, the script also prints the `"nats"`
object that PC's `settings.json` must hold. The manual
equivalent is the publisher's creds file from [provisioning](#provisioning)
plus a copy of the viewer's `keys\viewer.pub`.

On the PC: check "This machine's name" is the alias, Role: Publisher →
**Switch and restart**, **Import creds…**, **Import viewer key…**,
**Generate key**. Once the three checks pass it starts publishing within 30
seconds; the footer reads `publishing as <alias> · ● connected`. Delete the
copied folder afterwards. Later imports (a rotated `viewer.pub`, new creds)
apply at once without a restart: the note under the buttons says what the
running publisher now uses ("Applied: …"; for new creds, "Accepted: trying
the new credentials…"), or, in red, why it kept what it had. New creds replace the connection only once a connection with them is
up; creds that do not connect leave it publishing and show in the footer's
tooltip. Files replaced by hand are picked up within 30 seconds.

The app holds `cc-publish.lock` and writes `publish-state.json` like
`cc-publish`, so a `cc-publish run` started on the same PC refuses to start,
the app does not publish while a `cc-publish` runs there, and after a restart the app deletes the keys of
sessions that ended while it was off.

Back to Viewer: Role: Viewer → **Switch and restart**. Its own session keys
expire from the bucket within 3 minutes, and as a viewer it ignores values
under its own name anyway. It connects only with viewer creds, and there is
one viewer: make sure the PC that was the viewer stops being one.

## Aliases

An alias names a machine: 1 to 31 lowercase letters, digits or dashes,
starting with a letter or digit, no dots. It is the first token of every key
the machine writes, the NATS user `cc-sessions.pub-<alias>`, and the tag on
its rows. Use one alias per machine: a second machine with the same alias
would overwrite the first one's keys. `test` is taken by the tests.

The widget's own rows carry "This machine's name" from the settings, by the
same rules.

## The publisher

`cc-publish run` gathers sessions every 2 seconds with the widget's own code
(`claude agents --json`, `~/.claude/sessions`, the cc-hook spool,
transcripts, the status line files) and writes:

| Key | When |
|---|---|
| `<alias>.s.<sid>` | when anything in its payload but the heartbeat changes (state, tool, model, context, title, folder name, second-line text); again every 60 s as a heartbeat; deleted when the session ends |
| `<alias>.host` | every 60 s |
| `<alias>.p.<pid>` | transcripts scanned every 5 minutes (the last 30 days); every project put again every 2 minutes, inside the 3-minute TTL; deleted when it leaves the window |
| `<alias>.activity` | with the projects |

One connection, user `cc-sessions.pub-<alias>`; writes go through a
newest-wins outbox, so the network never blocks gathering. Connect failures
retry with backoff up to 30 s, and 3 minutes of failed writes rebuild the
connection. A new run deletes the keys of sessions that ended while no
publisher ran.

Do not run `cc-publish` on a machine that also runs the widget: both drain
the same spool. (The Windows installer refuses to install there.) Such a
machine publishes with the app's Publisher role instead, which runs this
same pipeline (`cc_publish::pipeline`).

Installing a publisher: [publisher-linux.md](publisher-linux.md),
[publisher-windows.md](publisher-windows.md). Command reference:
[crates/cc-publish/README.md](../crates/cc-publish/README.md).

## How the widget keeps remote data

- **Sessions** live in memory only. A row last heard of more than 150 s ago
  shows offline (`no heartbeat since HH:MM`) and stays until the wastebasket
  or "Remove offline sessions after". A delete (the session ended) removes
  it at once. A value from the initial snapshot counts from the publisher's
  heartbeat, capped at the local time.
- **Projects** survive a delete or the TTL: a machine that is switched off
  still used what it used. The last value per key is saved in
  `remote-projects.json` (written at most every 5 seconds, and once more
  when the app quits) and dropped 30 days after its last activity; at
  most 200 projects per machine. Every project, heard live, loaded from
  that file or restored from a backup, is cut to what a publisher sends
  (name 64 characters, 12 models plus an "(other models)" sum); a file
  over 8 MB is not loaded but kept aside as
  `remote-projects.json.corrupt-<date>`, and the next save writes a new
  one.
- **Machines**: at most 64 at once. Values of a further machine are
  ignored without being opened, so no key of it is noted for review, and the
  Remote line's tooltip counts them ("more than 64 machines: N value(s) of
  the others ignored"); its place opens once a machine held has no row,
  host record, project or activity left.
- **Activity** is held per machine, also through a delete, and each day is
  merged into `activity.json` (kept 200 days; a day's count only grows),
  so the calendar keeps months of history although a publisher sends 35
  days at most.
- Values that fail to open are counted, logged once per key and never
  shown, and so are values that open but whose payload names another
  machine than their key. A key that is no cc-sessions key (such as
  `toolctl check`'s probe) is ignored unopened. A value from a sender key not pinned for its
  alias is held back while the key waits for review (the flyout's notice;
  at most 3 keys per machine and 16 machines, kept in
  `keys\publishers\<alias>.pending` so they survive a restart; the seen
  times and counts are saved every 10 minutes and on exit); one from a
  rejected key is dropped and counted per alias in the Remote tooltip; one
  of an alias whose pin file holds no usable key is named there as
  "`<alias>`: unknown sender key — values rejected". Replays, future
  timestamps, oversized texts and oversized
  machines are handled as in
  [data-and-privacy.md](data-and-privacy.md#security-model).
