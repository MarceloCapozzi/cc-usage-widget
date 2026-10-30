# Linux publisher bundle

A Linux machine without the widget publishes its Claude Code sessions,
encrypted, to the `cc-sessions` bucket, and the widget on Windows shows them
as remote rows tagged with the machine's alias (see [remote.md](remote.md)).
Each session row carries its state, tool, model and context, plus its
title (at most 80 characters), its folder name (at most 64) and its
second-line text (at most 120); the full list, and what is never sent, is
in [What leaves the machine](data-and-privacy.md#what-leaves-the-machine).
Setting one up takes one command on the widget machine and two on Linux:

```powershell
pwsh -File tools\make-publisher-bundle.ps1 -Alias lab-01          # on the widget machine
```
```sh
tar -xzf cc-usage-publisher-lab-01.tar.gz && cd cc-usage-publisher-lab-01
make install && make verify                                        # on the Linux machine
```

The bundle is a folder (plus a `.tar.gz` of it) with everything the Linux
side needs, so the Linux machine needs no Rust, no Node, no internet access
beyond NATS, and no root:

| Path | Contents |
|---|---|
| `bin/cc-hook`, `bin/cc-publish` | static musl binaries, x86_64 or aarch64 |
| `secrets/cc-sessions.pub-<alias>.creds` | NATS credentials: may write only `$KV.cc-sessions.<alias>.>` |
| `config/viewer.pub` | the widget machine's public key (or the `-ViewerPub` file), which every value is sealed to (not secret) |
| `config/publish.json` | cc-publish's config: alias, servers and bucket (the viewer's, or `-Servers` and `-Bucket`; plaintext transports are refused), creds and key files relative to the install location, `log_file` off |
| `config/bundle.env` | alias, NATS user, architecture, version, build time |
| `Makefile`, `scripts/*.sh` | install, verify, status, logs, restart, uninstall (GNU make, POSIX sh) |
| `systemd/cc-publish.service.in` | the systemd user unit template |
| `README.md`, `SHA256SUMS` | short instructions; checksums of every other file |

There is no private key in it. The machine's key pair is its
identity: `make install` creates it on the machine the first time, prints
its public key line, and keeps it on every reinstall and update; the widget
asks to accept that key with the first value it receives (see
[Identity](#identity)).

## 1. One-time setup on Windows: the cross toolchain

```powershell
pwsh -File tools\cross\setup.ps1
```

Portable and without administrator rights. It downloads zig 0.15.2 from
ziglang.org into `%LOCALAPPDATA%\cc-cross` (SHA-256 pinned in
`tools\cross\common.ps1`; `$env:CC_CROSS_ROOT` moves it), installs
cargo-zigbuild 0.23.4 with `cargo install --locked`, and adds the
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` Rust targets.
Each step is skipped when already done. No WSL, Docker or Python needed.

The zip stays next to the unpacked toolchain: before every use the zip is
checked against the pinned hash and `zig.exe` against the zip's copy.

`tools\cross\build-linux.ps1 [-Target x86_64,aarch64]` then runs
`cargo zigbuild --release --locked --target <triple> -p cc-hook -p cc-publish`
with the pinned zig on PATH and checks every binary by its ELF header: 64-bit
little-endian, the right `e_machine` (0x3E x86-64, 0xB7 AArch64), no
`PT_INTERP` and no `DT_NEEDED`, meaning static. The bundle generator calls it.
Packages missing from the workspace are skipped with a warning (`-Strict`
makes that an error).

Why zig: the musl targets need a C cross compiler and linker for the crates
with C and assembly parts, chiefly `ring` (TLS for async-nats). cargo-zigbuild
points `CC_<triple>`, `AR_<triple>` and `CARGO_TARGET_<TRIPLE>_LINKER` at its
zig wrappers by itself, so no extra environment is needed. If it ever stops
setting those variables, the manual equivalent is a pair of `.cmd` wrappers
(`zig cc -target x86_64-linux-musl %*` and `zig ar %*`) named in
`CC_x86_64_unknown_linux_musl`, `AR_x86_64_unknown_linux_musl` and
`CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER`, then plain `cargo build`.

## 2. Generate a bundle

```powershell
pwsh -File tools\make-publisher-bundle.ps1 -Alias <alias> [-Target x86_64|aarch64] [-Out <folder>] [-ViewerPub <file>] [-Servers <urls>] [-Bucket <name>]
```

`-Platform linux` is the default; `-Platform windows` builds the
[Windows bundle](publisher-windows.md), and `-Platform app` the creds and
`viewer.pub` for a PC with the app in the Publisher role
([remote.md](remote.md#a-pc-with-the-app-as-a-publisher)).

The alias is 1 to 31 lowercase letters, digits or dashes, starting with a
letter or digit, no dots (see [Aliases](remote.md#aliases)). What the script
does, in order:

1. Validates the alias, the output folder and where the viewer's key comes
   from: a `-ViewerPub` file must hold valid key lines; without it, a PC in
   the app's Publisher role (`role` `"publisher"` in `settings.json` under
   `%APPDATA%\ar.cc.usage`, or `$env:CC_USAGE_CONFIG_DIR`) is refused
   before anything is built, provisioned or written. It also settles the
   NATS servers and bucket for `publish.json`: `-Servers` (URLs in the
   order to try, or one argument with commas) and `-Bucket`, by default the
   viewer's own settings on this PC, resolved like the app
   (`CC_USAGE_NATS_SERVERS` and `CC_USAGE_NATS_BUCKET`, else the `"nats"`
   object of that `settings.json`, only read), else the defaults; see
   [Connection settings](remote.md#connection-settings). An unusable viewer
   setting is warned about and replaced by the default; an unusable
   `-Servers` or `-Bucket` stops the script. Without `-SkipProvision`, the
   project file (`-ProjectFile`, by default `infra\nats\cc-sessions.yaml`)
   must exist, declare `project: cc-sessions` and the chosen bucket; when
   it is missing the script stops here and says to copy
   `infra\nats\cc-sessions.example.yaml`.
2. Checks the bundle sources: Makefile recipe lines start with a tab, no CR
   line endings, and `sh -n` (dash when available, which rejects bashisms) of
   every script. Text files are written with LF whatever git did to them.
3. Builds the Linux binaries (`-SkipBuild -BinDir <dir>` uses ready ones)
   and checks them as above.
4. Provisions the NATS user with toolctl (see
   [Provisioning](#provisioning)):
   `toolctl add-publisher <alias>` declares `cc-sessions.pub-<alias>` in
   the local project file (no change when it is already there; the file
   stays out of git), `toolctl apply --no-creds` creates the user or
   re-applies its permissions (it never deletes anything), and `toolctl
   creds` downloads its creds into `%LOCALAPPDATA%\cc-usage\nats`, which the
   script then makes private (current user and SYSTEM only; toolctl leaves
   the folder's inherited ACL). A rejected token stops the script with HTTP
   401/403 (renew the token). `-SkipProvision` uses creds already there
   (made private too), such as creds made [by hand](remote.md#by-hand).
5. The viewer's key, with a `cc-publish.exe` built for
   Windows here: `viewer-keygen --if-missing` makes sure this machine, the
   viewer, has its key pair (`%LOCALAPPDATA%\cc-usage\keys\viewer.key` and
   `viewer.pub`, or under `$env:CC_USAGE_DATA_DIR`; the app makes the same
   files on its first start), and `viewer.pub` is copied to
   `config/viewer.pub`. No publisher key is made or pinned: the machine
   makes its own when the bundle is installed. Only public keys are printed;
   the summary says that the machine prints its public key line on its
   first install and that the viewer asks to accept it.

   With `-ViewerPub <file>` (the viewer is another PC: a copy of its
   `keys\viewer.pub`, public, so any channel will do), that file becomes
   `config/viewer.pub` and its lines are printed to compare with the
   viewer's file, and nothing is generated on the building PC (see
   [Bundles once the viewer is another
   PC](remote.md#bundles-once-the-viewer-is-another-pc)).
6. Assembles `<Out>\cc-usage-publisher-<alias>\` with `SHA256SUMS`, and writes
   `<Out>\cc-usage-publisher-<alias>.tar.gz`. The archive stores every
   directory as 0700, every file as 0600 (0700 for binaries and scripts) and
   owner and group 0 without names, so extracting it on Linux never leaves
   the secrets readable by other users, whatever the umask. A normal tar
   gives the files to the user who extracts them; extracted with `sudo` they
   would belong to root (see [Troubleshooting](#troubleshooting)). It is
   ustar: a path over 100 bytes (`secrets/<user>.creds` of a long alias) is
   split at a `/` into the 155-byte prefix and 100-byte name fields, which
   GNU tar, bsdtar and busybox tar join again. The
   folder and the archive get a Windows ACL for the current user and SYSTEM
   only, with no inherited entries: the folder's own ACL is made private,
   then everything inside is reset to inherit only that, and each item is
   checked to be private and readable by its owner. The script stops if
   that ACL cannot be set.

The default `-Out` is `%LOCALAPPDATA%\cc-usage\bundles`. A folder inside the
repository is always refused, and one inside OneDrive unless
`-AllowSyncedFolder` is passed: a synced folder would upload the secrets.

Run it again at any time; it rebuilds the folder and the archive. A new
bundle for a machine already installed keeps its identity: `make install`
keeps the key it has. One alias per machine: a second machine with the same
alias has a key of its own, which the viewer holds back unless it is accepted.

## 3. Move it securely

The bundle holds the machine's NATS credentials, which let whoever holds
them write that alias's keys in the bucket (they decrypt nothing, and the
viewer only takes values sealed with the machine's own key). Move it over
scp or sftp only (not
e-mail, chat or a shared drive), and compare the archive's SHA-256, which the
script prints, on the other side:

```sh
scp "%LOCALAPPDATA%\cc-usage\bundles\cc-usage-publisher-lab-01.tar.gz" me@lab-01:
ssh me@lab-01 sha256sum cc-usage-publisher-lab-01.tar.gz
```

Delete the folder and the archive on both machines once `make verify`
passes. The Linux side keeps a copy of the tooling without binaries or
secrets in `~/.local/share/cc-usage/bundle`, so the bundle folder is not
needed afterwards.

If a bundle leaks, follow
[A leaked bundle](data-and-privacy.md#a-leaked-bundle): delete the NATS user
and generate a new bundle. The leaked bundle holds no key of the machine, so
its identity stays.

## 4. On the Linux machine

Extract as the user who runs Claude Code (not root, and not with `sudo`),
inside `$HOME` (a noexec `/tmp` cannot run the binaries), then:

| Command | What it does |
|---|---|
| `make` / `make help` | lists the targets |
| `make check-prereqs` | Linux; not run under `sudo` (FAIL) or as plain root (warning); every bundle file belongs to the user running make and none is world-writable (FAIL with a `chown` or re-extract hint otherwise); architecture matches the binaries; the binaries start; `sha256sum -c SHA256SUMS`; tools (`install`, `sha256sum`, `sed`, `awk`, `od`, ...); `systemd --user` reachable, or a warning that explains why not and offers `make autostart-profile`; `~/.claude` exists; a custom `CLAUDE_DIR` that this shell's `CLAUDE_CONFIG_DIR` does not match (warning); `claude` on PATH (warning) |
| `make install` | runs check-prereqs, then copies binaries to `~/.local/share/cc-usage/bin` (0755), creds to `.../nats/`, `viewer.pub` to `.../keys/viewer.pub` (0600, directories 0700, umask 077, a different existing `viewer.pub` or creds file is backed up), keeps the machine's key pair `.../keys/publisher.key` or makes it on the first install (`cc-publish keygen --if-missing`, printing its public key line; see [Identity](#identity)); writes `.../publish.json` with absolute paths and `.../install-manifest.sha256`, runs `cc-hook install` (hooks and status line chain in `~/.claude/settings.json`, with a backup), installs `cc-publish.service` into `$XDG_CONFIG_HOME/systemd/user` (default `~/.config/systemd/user`), `daemon-reload`, `enable`, `restart`, and suggests `loginctl enable-linger` (`LINGER=1` runs it). Idempotent: a second run reports *unchanged* and restarts the service. |
| `make verify` | the validation: PASS/FAIL table, exit 1 on any FAIL (see below) |
| `make setup` | check-prereqs, install and verify in one go |
| `make status` | `systemctl --user status`, `cc-publish status`, `cc-hook status` |
| `make logs` | `journalctl --user -u cc-publish -f` (or the log file with the profile fallback) |
| `make restart` | restarts the publisher |
| `make hooks` | runs `cc-hook install` again with the installed `cc-hook` (hooks and status line chain; idempotent, `settings.json` is rewritten, with a backup, only when something differs); `update.sh` runs it, and `make verify` points to it when an event is missing, stray or registered elsewhere |
| `make rotate-key` | replaces the machine's identity on purpose: asks for the alias (or `CONFIRM_ALIAS=<alias>`), `cc-publish keygen --rotate`, updates the manifest, restarts the publisher and prints the new public key line (see [Identity](#identity)) |
| `make uninstall` | stops and disables the service, removes the unit, `cc-hook uninstall` (restores the hooks and the previous status line), removes binaries and config; keeps the secrets and the key pair (so a reinstall keeps the identity), which `PURGE=1` removes too, with the spool and live files |
| `make autostart-profile` | for machines without `systemd --user` (WSL without systemd, containers): installs the same files plus a guarded block in `~/.profile` that starts `cc-publish run` in the background once, with a pid file, a lock and a log in the data dir; `make uninstall` removes the block |

Install, uninstall and update refuse to run under `sudo`, and refuse a
bundle folder with files owned by another user or writable by everyone.

Variables: `PREFIX` (default the cc-usage data dir:
`$CC_USAGE_DATA_DIR`, else `$XDG_DATA_HOME/cc-usage`, else
`~/.local/share/cc-usage`, the same rule as cc-hook, so the spool the hooks
write is the one cc-publish reads), `CLAUDE_DIR` (default
`$CLAUDE_CONFIG_DIR`, else `~/.claude`; its `settings.json` is the one
hooked, since the scripts run cc-hook without the shell's
`CLAUDE_CONFIG_DIR` and pass `--settings` for any folder but `~/.claude`;
one other than `~/.claude` is passed to the publisher as `CLAUDE_CONFIG_DIR`,
in the unit and in `publisher.env`, so `claude agents` and the session files
come from there; Claude Code itself must run with the same
`CLAUDE_CONFIG_DIR`), `UNIT_DIR` (default `$XDG_CONFIG_HOME/systemd/user`,
else `~/.config/systemd/user`, where the user manager looks),
`AUTOSTART=systemd|profile`, `LINGER=1`, `PURGE=1`, `FORCE=1` (install without
`~/.claude`), `VERIFY_WAIT` (seconds verify waits for a publish, default 45),
`CONFIRM_ALIAS=<alias>` (confirms a new identity without the prompt; see
[Identity](#identity)).
Paths may contain spaces but not quotes, backslashes, `$`, `%` or backquotes;
the scripts refuse those up front. These variables are read from the command
line only (`make install PREFIX=/opt/x`): a variable of the same name in the
environment is ignored, since Termux and build tools export `PREFIX`, an
exported `PURGE=1` must not purge the secrets, and an exported
`CONFIRM_ALIAS` must not replace the machine's identity (`CC_USAGE_DATA_DIR`,
`XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `CLAUDE_CONFIG_DIR` are read from the
environment on purpose, for the defaults). The tooling copy in
`$PREFIX/bundle` remembers `PREFIX`, the data dir, `CLAUDE_DIR`, the unit dir
and the profile it was installed with, so `make -C $PREFIX/bundle verify` (or
`uninstall`) acts on that install; from the original bundle folder, pass the
same variables as to `make install`. `check-prereqs` warns when `DATA_DIR`
differs from the folder cc-hook writes to in the current environment.

`make verify` checks, in order:

| Check | PASS when |
|---|---|
| bundle checksums | `sha256sum -c SHA256SUMS` (only while the bundle folder is there) |
| installed files | the installed binaries, secrets and config match the manifest install wrote |
| cc-hook / cc-publish `--version` | both start and report the bundle's version |
| file modes | `PREFIX`, `nats/` and `keys/` 0700; creds, `keys/publisher.key` and `publish.json` 0600; binaries 0755 |
| publish.json | alias matches, creds exist, both key files exist (each missing one is named, with how to get it), no members cc-publish rejects, `publisher.env` names cc-hook's data dir, and `CLAUDE_CONFIG_DIR` in `publisher.env` and the unit is `CLAUDE_DIR` when that is not `~/.claude` (and absent when it is) |
| hooks | `cc-hook status` lists every event as installed, registered to the installed `cc-hook` |
| status line chain | `cc-hook status` shows `statusLine` chained (a warning otherwise) |
| cc-publish check | `cc-publish check --config publish.json` exits 0: the key files (its output names the key ids it seals with and whether `viewer.pub` is there), NATS connect, bind, probe put and delete of a sealed value |
| service | `cc-publish.service` enabled and active (or the profile publisher running) |
| linger | warning only, when the service would stop at logout |
| recent publish | `cc-publish status --config publish.json` exits 0 within `VERIFY_WAIT` (a live publisher that wrote within 150 s); the detail is its `last write` line. The journal fallback (a line containing `published`) only applies to a cc-publish without `status` (exit 2) |

The NATS check opens a second connection for a few seconds while the service
holds the first. The connection limit is shared by the widget, every
publisher and, in a shared account, other projects (5 in all in the
[reference deployment](remote.md#reference-deployment)).

### Identity

The machine's key pair, `$PREFIX/keys/publisher.key`, is its identity: every
value is sealed with it, and the widget takes values for the alias only from
a key it has accepted. It is born on the machine and changes only on
purpose:

- **First install.** No key and no earlier install (neither
  `install-manifest.sha256` nor `publish.json` in `PREFIX`): `make install`
  runs `cc-publish keygen --if-missing` and prints `generated this machine's
  identity <kid>` and the public key line. On the widget machine, cc-usage
  asks to accept the new key with the first value it receives; accept it
  after comparing the key id and fingerprint it shows (the first 8
  characters of the key) with the ones printed here.
- **Reinstall and update.** A key that loads is kept: `make install` (from
  any bundle, also a new one) says `kept this machine's identity <kid>`,
  and so does `update.sh`. `make uninstall` without `PURGE=1` keeps the key, so uninstalling and
  installing again keeps the identity; `PURGE=1` deletes it, and the next
  install is a first install.
- **Lost key.** No key although a publisher was installed here before, or a
  key that does not load: `make install` and `update.sh` warn *This replaces
  &lt;alias&gt;'s identity. The viewer will hold back its values until you accept
  the new key there.* and ask for the alias in a terminal; without it they
  stop before changing anything. Restore the key from a backup and run them
  again, or confirm a new identity with `CONFIRM_ALIAS=<alias>` on the
  command line (`make install CONFIRM_ALIAS=lab-01`, `sh update.sh
  CONFIRM_ALIAS=lab-01`). A key that does not load is kept as
  `publisher.key.bak-<time>`.
- **The confirmation.** The alias in `CONFIRM_ALIAS` or typed at the
  prompt is compared with the installed one after trimming surrounding
  whitespace (spaces inside it still count), the same rule as
  `Install.ps1 -ConfirmAlias` on Windows. `make install`, `make rotate-key`
  and `update.sh` share it, in `scripts/identity.sh`, which the bundle, the
  installed copy and the update package all carry.
- **Rotation.** `make rotate-key` (from the bundle folder or
  `$PREFIX/bundle`) warns the same way and asks for the alias, or takes
  `CONFIRM_ALIAS=<alias>`; then it runs `cc-publish keygen --rotate --config
  $PREFIX/publish.json --confirm <alias>` (a new key in front of the old
  one; `--force`, after a backup, when the key does not load; a plain keygen
  when it is missing), recomputes the manifest line of the key, restarts the
  publisher and prints the new public key line. Until the new key is
  accepted on the widget machine, its values are held back there.

`cc-publish keygen --rotate` and `--force` themselves refuse (exit 2) to
replace an existing key without `--confirm <alias>` or the alias typed at
their own prompt (see [cc-publish's README](../crates/cc-publish/README.md#envelopes-and-keys)).

### No systemd

`check-prereqs` says why `systemctl --user` does not work: not installed, not
booted with systemd (on WSL: add `[boot]` and `systemd=true` to
`/etc/wsl.conf`, then `wsl --shutdown` from Windows), or the user manager is
not reachable from this shell (use a direct or ssh login, not `su`). Either
fix that and `make install`, or use `make autostart-profile`. The profile
block is skipped by bash login shells when `~/.bash_profile` or
`~/.bash_login` exists without sourcing `~/.profile`; install warns about it.

### The systemd unit

`ExecStart="<prefix>/bin/cc-publish" run --config "<prefix>/publish.json"`,
with `Type=simple`, `Restart=on-failure`, `RestartSec=10`,
`StartLimitIntervalSec=0`, `TimeoutStopSec=15`, output to the journal,
`CC_USAGE_DATA_DIR` set to the hooks' data dir, `CLAUDE_CONFIG_DIR` only for
a `CLAUDE_DIR` other than `~/.claude` (with it Claude Code also reads
`.claude.json` from that folder instead of `$HOME`, so it is not set for the
default), and a `PATH` that includes the directories of `claude` and `node`
as found at install time (user services get a minimal PATH, and cc-publish
runs `claude agents --json`).

Hardening is limited to what works without user namespaces: `UMask=0077`,
`NoNewPrivileges`, `LockPersonality`, `RestrictRealtime`,
`RestrictSUIDSGID`, `SystemCallArchitectures=native` and
`RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`. If `claude agents` fails
only under the service, adding `AF_NETLINK` to the last one is the first
thing to try. `ProtectSystem`, `ProtectHome` and `PrivateTmp` would need
mount namespaces, which user services get only through unprivileged user
namespaces (blocked by default on Ubuntu 24.04, where the unit would fail
with 226/NAMESPACE), and they would have to leave `~/.claude`,
`~/.claude.json` and the data dir writable for `claude agents` and the spool
anyway.

## 5. Updating

An update package holds the two binaries, `update.sh`, `viewer.pub` (the
widget machine's public key), `bundle/` (the tooling copy, with the
`~/.profile` launcher `scripts/cc-publish-autostart.sh`) and `SHA256SUMS`:
no secrets and no alias, so one package serves every Linux machine of that
architecture. No provisioning either. On the widget machine:

```powershell
pwsh -File tools\make-publisher-bundle.ps1 -Update       # -Target aarch64 for ARM machines
# -> %LOCALAPPDATA%\cc-usage\bundles\cc-usage-publisher-update-<version>-<arch>\ and .tar.gz
```

When the viewer is another PC, build it anywhere with a copy of the
viewer's `keys\viewer.pub`: `-Update -ViewerPub D:\viewer.pub`. Without
`-ViewerPub` the package carries this machine's `viewer.pub` (the viewer
key pair is created here when missing), and a PC in the Publisher role is
refused, as for a bundle.

Copy the `.tar.gz` over, then on the Linux machine, as the user who ran
`make install`:

```sh
tar -xzf cc-usage-publisher-update-<version>-x86_64.tar.gz
sh cc-usage-publisher-update-<version>-x86_64/update.sh     # PREFIX=<dir> if make install used another one
```

Like the Makefile, `update.sh` takes `PREFIX` and `CONFIRM_ALIAS` from its
command line only; the same variables in the environment are ignored.

`update.sh`:

1. refuses to run under `sudo`, and refuses a package folder with files that
   are not the user's or are world-writable;
2. checks that a publisher is installed in `PREFIX` and that the install
   belongs to the same user;
3. verifies the package against its `SHA256SUMS` and checks that
   `viewer.pub` is a public key file, and that `publish.json` names both key
   files (`publisher_key`, `viewer_pub`) by absolute path; otherwise it stops
   (run `make install` from a new bundle);
4. decides on the machine's identity before changing anything, with the
   package's `cc-publish` (which must run here first) and its
   `bundle/scripts/identity.sh`, read only once the checksums matched: a key that
   loads is kept; a missing one, or one that does not load, is replaced only
   when confirmed (`sh update.sh CONFIRM_ALIAS=<alias>`, or the alias typed
   at the prompt), otherwise it stops (see [Identity](#identity));
5. verifies every other installed file (creds, keys, `publish.json`,
   `publisher.env`) against `install-manifest.sha256`, and
   stops without changing anything if one differs (run `make install` from a
   bundle in that case; the key's line is not checked when a confirmed new
   key replaces it);
6. replaces `bin/cc-hook`, `bin/cc-publish` and the launcher
   `bin/cc-publish-autostart` (each copied next to its target, then renamed),
   and refreshes the tooling copy in `$PREFIX/bundle`;
7. installs `viewer.pub` where `publish.json` says, and keeps the
   machine's key (`kept this machine's identity <kid>`), or makes the
   confirmed new one (`cc-publish keygen --if-missing`, or `--force` after a
   backup for a key that does not load) and prints its public key line,
   which the widget asks to accept;
8. recomputes the manifest lines of the files it wrote (the binaries, the
   launcher and the two key files), keeping every other line;
9. registers the hooks again with the new `cc-hook` (`make hooks` from the
   tooling copy, which names the `CLAUDE_DIR` of `make install`), so an
   event list that changed between versions is applied;
10. restarts the publisher and runs `make verify`.

Running it again changes only the binaries, the launcher and the tooling
copy, plus `keys/viewer.pub` when the package carries a different one (a
package built for a new viewer moves the machine to it; the new viewer asks
to accept the machine's key with its first value); `settings.json` changes
only when the event list did. The machine's key, creds, the alias and the
service (its unit) stay as they are, and so do the servers and the bucket
in `publish.json`; to change the creds, the alias or the NATS connection,
generate a new bundle and run `make install` again (`publish.json` is in
the install manifest, so an edit by hand fails `make verify` and stops
`update.sh`).

## Provisioning

The bucket and users are declared in the local project file,
`infra/nats/cc-sessions.yaml`, a copy of
[`infra/nats/cc-sessions.example.yaml`](../infra/nats/cc-sessions.example.yaml)
that git ignores, and applied with `toolctl`, the maintainer's internal
provisioning tool (kept in a separate private repository, `tooling-ctl`).
The commands, the token handling and how to do the same by hand (then
build bundles with `-SkipProvision`) are in
[remote.md](remote.md#provisioning). The bundle generator finds toolctl as
`-Toolctl <path>`, `$env:TOOLCTL`, `toolctl` on PATH, or, with that
repository cloned next to this one, builds it with
`go -C ..\tooling-ctl build -o %LOCALAPPDATA%\toolctl\bin\toolctl.exe ./cmd/toolctl`.
`toolctl plan` must report no changes against the live account; when it
does not, make the file match reality (or apply what you added on purpose).

## What the bundle expects from cc-publish

The Makefile and scripts are written against this contract:

| Command | Behaviour |
|---|---|
| `cc-publish --version` | prints `cc-publish <version>`, exit 0 |
| `cc-publish keygen --if-missing --key FILE` | creates the private key FILE when missing (an existing one is kept; never prints it) and prints its public key line, alone, on stdout; exit 0, 1 when an existing FILE does not load (the scripts use that to tell a key that loads) |
| `cc-publish keygen --rotate\|--force --key FILE --config FILE --confirm ALIAS` | replaces an existing key (`--rotate`: a new key in front of the old ones) only when ALIAS is the config's alias, else exit 2 without changing anything; prints the new public key line alone on stdout |
| `cc-publish run --config FILE` | runs in the foreground until stopped; logs to stderr (connects, errors, stop, and which envelope it seals); reads the data dir from `CC_USAGE_DATA_DIR` |
| `cc-publish check --config FILE` | the key files, connect, bind the bucket, put and delete a probe value sealed as `run` would; exit 0 on success, non-zero with the reason |
| `cc-publish status --config FILE` | prints `config      FILE` for a config it loads (`update.sh` relies on it) and an `envelope` row (with a running publisher, the keys it uses, marked `(in use)`, plus `disk keys`/`disk creds` rows when the files differ); exit 0 when a live publisher wrote within the last 150 s, 1 otherwise (stopped, never wrote, stale, bad config), 2 on a usage error; last line `health  ok: publishing` or `health  NOT PUBLISHING: <reason>` |

`publish.json` is cc-publish's own config, which rejects unknown members, so
the NATS user lives in `config/bundle.env` and the data dir in the unit
(`Environment=CC_USAGE_DATA_DIR=...`) and in `$PREFIX/publisher.env`
(`DATA_DIR=...` and, for a custom Claude Code folder, `CLAUDE_CONFIG_DIR=...`,
read by the `~/.profile` launcher and by `make verify`). One
member per line (the scripts read it with sed and awk); relative paths
resolve against the config file's folder. As installed:

```json
{
  "alias": "lab-01",
  "servers": ["wss://connect.ngs.global", "tls://connect.ngs.global"],
  "bucket": "cc-sessions",
  "creds": "/home/me/.local/share/cc-usage/nats/cc-sessions.pub-lab-01.creds",
  "publisher_key": "/home/me/.local/share/cc-usage/keys/publisher.key",
  "viewer_pub": "/home/me/.local/share/cc-usage/keys/viewer.pub",
  "log_file": false
}
```

cc-publish seals every value with `publisher_key` to `viewer_pub`; both
must exist and load, or it does not publish. Any other member is an
error.

## Testing

- `tools/publisher-bundle/test/simulate.sh` runs the real Makefile and
  scripts end to end in a throwaway HOME with a space in it, against stubs
  of cc-hook, cc-publish, systemctl, journalctl, loginctl, uname and id:
  help, check-prereqs, bundle files owned by another user and `sudo`
  refused, install twice, verify and its failure modes, the journal
  fallback, status, restart, the tooling copy, creds backup, the identity (a
  first install makes the key, a reinstall keeps it; a lost or broken key
  refused without `CONFIRM_ALIAS`, also an
  exported one, and replaced with it, surrounding whitespace ignored; `make rotate-key` refused without or
  with a wrong alias, then rotating, restarting and keeping `make verify`
  green, also from the installed copy), `make hooks` (from the bundle and
  the installed copy, after `make verify` pointed to it), `update.sh`
  (binaries and the launcher on an install with its keys, keeping the key,
  registering the hooks again, ignoring an exported `PREFIX`; a lost key
  refused without `CONFIRM_ALIAS`; a new viewer key installed, the tooling
  copy refreshed, and running it again; a `publish.json` without both key
  files refused; a changed file stopping it),
  uninstall twice,
  ignored environment variables, a custom PREFIX, a custom `CLAUDE_DIR`
  reaching the publisher, the unit under `$XDG_CONFIG_HOME`, the profile
  fallback with PURGE=1, and checksum, architecture and `~/.claude`
  failures. It needs GNU make and a POSIX sh; `SIM_SH=dash` runs the scripts
  with dash. On Linux: `sh tools/publisher-bundle/test/simulate.sh`; on
  Windows it runs under Git Bash with a GNU make (`MAKE=<path>`; a make
  stand-in that reads the Makefile's targets and recipes works too).
- `tools/publisher-bundle-gen/test/Test-BundleGen.ps1` tests the
  generator's helpers in a temp folder, with Windows PowerShell 5.1 and
  PowerShell 7: the archive `Write-TarGz` writes, listed and extracted with
  Windows' bsdtar and Git's GNU tar (0700 folders and executables, 0600
  files, owner 0/0; paths over 100 bytes, such as the creds file of a
  31-character alias, through the ustar prefix field, with their full
  name), `Split-TarPath`'s limits, `Write-PrivateZip` (entries and a
  private ACL), `Test-InsideRepository`, `Test-UnderSyncedFolder`, the
  alias rule and the `-ViewerPub` checks (a `.pub` file of 32-byte keys,
  never a `viewer.key`).
- What needs a real Linux machine (`make install && make verify`,
  `update.sh`, the hardened unit) is in the
  [checks before a release](development.md#before-a-release).

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `cd: Permission denied` after extracting, *cannot read the bundle files*, or *bundle owner* FAIL | the archive was extracted with `sudo`, so its files belong to root (the archive stores owner 0). `sudo chown -R "$USER": cc-usage-publisher-<alias>`, or delete the folder and extract it again without `sudo` |
| *bundle owner* FAIL: world-writable | extract the bundle again (or `chmod -R go-w` it) |
| *user* FAIL: *running under sudo* | run `make install` as yourself, without `sudo`: it installs for the user who runs it |
| *architecture* FAIL | the machine is aarch64 (or x86_64): generate with `-Target` to match |
| *binary --version failed*, noexec hint | the bundle is on a noexec mount such as `/tmp`; extract under `$HOME` |
| *checksums* FAIL | the copy is incomplete or was changed; transfer the `.tar.gz` again |
| *systemd --user* WARN, install refuses | see [No systemd](#no-systemd) |
| *cc-publish check* FAIL | outbound connections to the `servers` in `publish.json` blocked (by default 443 for wss and 4222 for tls to `connect.ngs.global`), a proxy that breaks WebSocket, a `bucket` other than the viewer's or one the NATS user may not write, the creds re-issued since the bundle was made (new bundle), or the connection limit reached |
| *recent publish* FAIL | `make logs`; usually the same causes as the check |
| the widget holds back the machine's values (its flyout asks to accept a new key for the alias) | the machine seals with a key the widget has not accepted for its alias: a new identity (`make rotate-key`, a confirmed new key, or `PURGE=1` and a new install). Accept the new key on the widget machine, comparing it with the line `make install`, `update.sh` or `make rotate-key` printed |
| `make install` or `update.sh`: *keys/publisher.key is missing, but a publisher was installed* or *does not load* | the machine's identity is lost: restore the key from a backup and run it again, or make a new identity on purpose with `CONFIRM_ALIAS=<alias>` (see [Identity](#identity)) |
| `claude agents` fails only under the service | add `AF_NETLINK` to `RestrictAddressFamilies` in the installed unit, `systemctl --user daemon-reload`, `make restart`. `make install` rewrites the unit from the template, so report it and change `systemd/cc-publish.service.in` too |
| `update.sh`: *installed files changed since make install* | a secret or config file was edited or replaced; `make install` from a new bundle |
| remote rows never appear in the widget | the bundle (or update package) seals to a `viewer.pub` that is not the widget's: made on another machine without `-ViewerPub`, or with an old copy. Generate it on the widget's machine, or pass `-ViewerPub` with a current copy of the widget's `keys\viewer.pub` (`cc-publish check` names the viewer key id it seals to; the widget's is the first line of its `keys\viewer.pub`) |
| hooks FAIL after a Claude Code update rewrote `settings.json` | `make install` again |
| the publisher stops at logout | `loginctl enable-linger $USER` |
