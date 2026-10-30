# cc-usage publisher bundle

This folder makes a Linux machine publish its Claude Code sessions,
encrypted, to the cc-usage widget on Windows. It holds everything needed:
static binaries (`bin/`), the NATS credentials (`secrets/`), the viewer's
public key and the config (`config/`), and this Makefile. Values are sealed to
the viewer's key: only the widget machine can read them. There is no private
key in it: this machine makes its own key pair, its identity, on the first
install and keeps it (see [Identity](#identity)).

**It contains a secret, the creds.** Copy it over a secure channel (scp,
sftp) and delete it once `make verify` passes.

## Install

```sh
tar -xzf cc-usage-publisher-<alias>.tar.gz
cd cc-usage-publisher-<alias>
make install && make verify
```

Extract and run it as the user who runs Claude Code, without `sudo`: the
archive's files would otherwise belong to root, and the install would be
for root.

`make install` checks the machine first (`make check-prereqs`), then:

- copies `cc-hook` and `cc-publish` to `~/.local/share/cc-usage/bin`, the
  creds to `~/.local/share/cc-usage/nats/` and `viewer.pub` to
  `~/.local/share/cc-usage/keys/` (0600, folders 0700), and writes
  `~/.local/share/cc-usage/publish.json`;
- on the first install, makes this machine's key pair,
  `keys/publisher.key` (`cc-publish keygen --if-missing`; the private key
  never leaves the machine), and prints its public key line. On the widget
  machine, accept that key when cc-usage asks (compare the key id and
  fingerprint it prints). A later `make install` keeps it (`kept this
  machine's identity <kid>`);
- registers the hooks and chains the status line in
  `~/.claude/settings.json` (`cc-hook install`, with a timestamped backup);
- installs and starts the systemd user service `cc-publish.service`.

`make verify` prints a PASS/FAIL table: checksums, binaries, file modes,
hooks, a NATS round trip (`cc-publish check`), the service, and a recent
successful publish. It exits non-zero on any FAIL.

To keep the publisher running after you log out, and start it at boot:

```sh
loginctl enable-linger "$USER"
```

Then delete the bundle: `cd .. && rm -rf cc-usage-publisher-<alias>*`. A
copy of the tooling, without binaries or secrets, stays in
`~/.local/share/cc-usage/bundle`, so later you can run
`make -C ~/.local/share/cc-usage/bundle verify` (or `status`, `uninstall`).

## No systemd (WSL without systemd, containers)

```sh
make autostart-profile && make verify
```

This adds a guarded block to `~/.profile` that starts `cc-publish run` in the
background once (pid file and log in `~/.local/share/cc-usage`). On WSL you
can also enable systemd instead: add `[boot]` / `systemd=true` to
`/etc/wsl.conf`, run `wsl --shutdown` from Windows, then `make install`.

## Everyday commands

| Command | What it does |
|---|---|
| `make status` | service, publisher and hook status |
| `make logs` | follow the publisher's log |
| `make restart` | restart the publisher |
| `make hooks` | register the hooks and the status line chain again (`cc-hook install`; changes `settings.json`, with a backup, only when something differs) |
| `make rotate-key` | replace this machine's identity, on purpose (see [Identity](#identity)) |
| `make uninstall` | stop and remove everything, restore `settings.json`; keeps the secrets |
| `make uninstall PURGE=1` | the same, and remove the creds, the keys and runtime data |

Running `make install` again is safe: it upgrades in place and restarts the
service. It keeps this machine's key pair: see [Identity](#identity).

## Identity

This machine's key pair, `~/.local/share/cc-usage/keys/publisher.key`, is
how the widget machine recognises it: every value is sealed with it, and a
value from another key for the same alias is held back there until accepted. So it is made
here once and kept:

- `make install` makes it on the first install, and keeps it on every later
  one, also from a new bundle. `update.sh` and
  `make uninstall` (without `PURGE=1`) keep it too.
- If it is missing although a publisher was installed here before (its
  `install-manifest.sha256` or `publish.json` is there), or it does not
  load, `make install` and `update.sh` warn as below and ask you to type the
  alias (spaces around it are ignored); without it they stop, changing
  nothing. Restore the key from a
  backup and run them again, or confirm a new identity on purpose (in a
  script: `make install CONFIRM_ALIAS=<alias>`,
  `sh update.sh CONFIRM_ALIAS=<alias>`).
- `make rotate-key` replaces it on purpose, from the bundle folder or from
  `~/.local/share/cc-usage/bundle`. It warns *This replaces &lt;alias&gt;'s
  identity. The viewer will hold back its values until you accept the new key
  there.* and asks you to type the alias; `make rotate-key
  CONFIRM_ALIAS=<alias>` confirms without the prompt (only on the command
  line: an exported `CONFIRM_ALIAS` is ignored). It adds a new key in front
  of the old one (`cc-publish keygen --rotate`; a key that does not load is
  replaced, with a backup), updates the install manifest, restarts the
  publisher and prints the new public key line.

After a new identity, accept its key on the widget machine when cc-usage
asks; until then its values from this machine are held back.

## Updating

New versions come as an update package,
`cc-usage-publisher-update-<version>-<arch>.tar.gz`, with the two binaries,
`update.sh`, the viewer's public key `viewer.pub`, a fresh copy of the
tooling and `SHA256SUMS` (no secrets). As the user who ran `make install`,
without `sudo`:

```sh
tar -xzf cc-usage-publisher-update-<version>-<arch>.tar.gz
sh cc-usage-publisher-update-<version>-<arch>/update.sh     # add PREFIX=<dir> if make install had one
```

It checks the package against its `SHA256SUMS` and the other installed files
against the install manifest (and stops, changing nothing, if one of them was
modified), replaces the two binaries and the `~/.profile` launcher
(`bin/cc-publish-autostart`), installs `keys/viewer.pub`, keeps this
machine's key pair (`kept this machine's identity <kid>`), registers the
hooks again for the new `cc-hook`'s event list (`make hooks`), then restarts
the publisher and runs `make verify`. `PREFIX` counts only on its command
line: one exported by the shell (Termux, build environments) is ignored. A
machine whose key pair is lost gets a new one only when confirmed: `sh
update.sh CONFIRM_ALIAS=<alias>`, or the alias typed at the prompt; its
public key line is printed, to accept on the widget machine (see
[Identity](#identity)). Running it again
changes only the binaries, the launcher, the tooling copy and, when the
package carries a new one, `viewer.pub`; `settings.json` changes only when
the event list did. Creds and the service stay. A new bundle and
`make install` also work, and are needed to change the creds or the alias.

## Troubleshooting

- `check-prereqs` fails on *architecture*: generate the bundle again with
  `-Target aarch64` (or `x86_64`).
- *binary --version failed* with a noexec hint: extract the bundle under
  `$HOME`, not `/tmp`.
- *systemd --user not reachable*: log in directly or over ssh (not `su`), or
  use `make autostart-profile`.
- *cc-publish check* fails: the network blocks the NATS servers in
  `~/.local/share/cc-usage/publish.json` (by default
  `wss://connect.ngs.global:443` and `tls://connect.ngs.global:4222`), or the
  creds were re-issued; ask for a new bundle.
- *recent publish* fails: `make logs`.
- *hooks* fails (an event `missing`, `stray` or registered elsewhere):
  `make -C ~/.local/share/cc-usage/bundle hooks`, then `make verify`.
- `cd: Permission denied`, *cannot read the bundle files* or *bundle owner*
  fails: the archive was extracted with `sudo`. Fix it with
  `sudo chown -R "$USER": cc-usage-publisher-<alias>`, or extract it again
  without `sudo`. A world-writable bundle folder is refused too: extract it
  again.
- `update.sh` says *installed files changed since make install*: a creds,
  key or config file was changed by hand; ask for a new bundle and run
  `make install`.
- `make install` or `update.sh` says `keys/publisher.key` *is missing* or
  *does not load*: restore it from a backup, or make a new identity on
  purpose (`CONFIRM_ALIAS=<alias>`, see [Identity](#identity)).
- The widget holds back this machine's values: its key changed; accept the new
  key there, comparing it with the line `make install`, `update.sh` or
  `make rotate-key` printed.
- Claude Code runs with `CLAUDE_CONFIG_DIR` (not `~/.claude`): install from a
  shell that has it set, or pass `make install CLAUDE_DIR=<that folder>`; the
  publisher gets the same `CLAUDE_CONFIG_DIR`. `CLAUDE_DIR` always decides
  which `settings.json` gets the hooks: the scripts run cc-hook without the
  shell's `CLAUDE_CONFIG_DIR`, so `make install CLAUDE_DIR=$HOME/.claude`
  hooks `~/.claude` even from a shell that has one.
