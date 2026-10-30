# Roadmap

What is still open. Done work is in [CHANGELOG.md](../CHANGELOG.md).

## Security

- **Code signing** for the installer and the binaries (SmartScreen,
  AppLocker and WDAC rules by publisher).
- **Signed bundles and update packages.** `SHA256SUMS` catches a damaged or
  partial copy, not a swapped one (the attacker can rewrite the sums too).
  Sign the sums, or publish their hash over a separate channel.
- **Revoke the temporary Synadia token** of the reference deployment once
  the Windows client test is done; the token is only needed to add machines.
- **Plain root on Linux.** The bundle scripts refuse `sudo` but only warn
  when run as root without `SUDO_USER`.

## Testing and CI

- **A real CI run on GitHub** ([development.md](development.md#continuous-integration)).
  The workflow has not run yet. In particular the Linux job is the first
  run of the Unix and Linux test paths (the `/proc` branch of cc-local's
  `process.rs`, the `0600`/`0700` mode checks of cc-hook, cc-publish and
  cc-core, the process group of `process_tree`), which so far have only
  been compiled for `x86_64-unknown-linux-musl` on Windows, and of the
  local NATS test against a real `nats-server`. The Secrets job is the
  first gitleaks scan of the checkout and of the history.
- **DOM and UI smoke tests.** `node --test "app/ui-tests/*.test.js"` covers
  the pure modules; the modules that draw the DOM have no tests. A jsdom
  run of the flyout's scripts, or Playwright against the `--demo` build,
  would catch a broken view; until then the
  [visual checks](development.md#before-a-release) are by hand.
- **The Windows client bundle on a second PC**: a real logon starting the
  task, a standard account, corporate policies, SmartScreen and Mark of
  the Web on a real download, AppLocker or WDAC, a proxy, and a real
  upgrade that fails and rolls back (the rollback, and the refusal after
  one that did not complete, are tested only with stubs).
- **A real token expiry.** The renewal through `claude -p` with no prompt
  is tested piece by piece; it has not yet been seen bringing the usage
  back after a real expiry.
- **The hardened systemd unit on the Linux machine**:
  `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6` may need `AF_NETLINK`
  if `claude agents` uses it under the service.
- Manual done criteria from the build plan: sessions opening and closing
  within 3 s without hooks, needs you within 1 s with hooks, focus on VS Code
  and Windows Terminal, a remote session within 2 s.

## Code health

- **Tauri 2.12.** The global shortcut, dialog and fs plugins are held below
  the releases that need tauri 2.12, which brings tauri-build 2.7 and its
  static VC runtime (its empty `msvcrt.lib` breaks linking the workspace
  doctests; see `app/Cargo.toml`). Upgrade them together with tauri and the
  Tauri CLI (`~2.11` in [development.md](development.md#prerequisites)).
- **Duplicate crypto crates.** `sha2` 0.10 and 0.11 and `getrandom` 0.2,
  0.3 and 0.4 are both compiled: hpke uses the newer ones, ring, nkeys,
  ed25519-dalek and tauri's codegen the older. Moving cc-core and
  cc-envelope to `sha2` 0.11 saves nothing until those move too.
- **Splitting large files.** `app/src/publisher.rs` (the Publisher role:
  its setup, readiness and imports, the hub and the pipeline glue) is the largest
  module outside the tests, and `app/src/remote/tests.rs` the largest test
  file; split them by concern.

## Product

- **`idle_prompt` as needs you.** Claude Code's `idle_prompt` notification
  counts as needs you, so a finished turn left alone toasts about 60 s
  later. Decide after some real use whether to
  keep it.
- **Sounds** per transition (needs you, done, failed). Not started.
- **Usage view screenshots.** There are none: a capture shows the real
  account email, so they need the Account line faked (demo mode still reads
  the real profile), e.g. a demo profile in `--demo`.

## Remote

- **Remote map growth with "Never".** With "Remove offline sessions after:
  Never", offline rows stay in memory until the wastebasket (bounded at 200
  sessions per machine). Consider a hard cap or an age limit.
- **Activity days across time zones.** Each machine counts tokens per its
  own local day; machines in different time zones add up days that do not
  line up.
