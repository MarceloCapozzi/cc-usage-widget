# Development

How to build, test and work on cc-usage. The code layout and design are in
[architecture.md](architecture.md).

## Prerequisites

| Tool | For | Notes |
|---|---|---|
| Rust 1.89+ (rustup, `x86_64-pc-windows-msvc`) | everything | the workspace's `rust-version` (the locked `notify-rust` needs 1.89); CI checks it with `cargo +1.89 check` and runs everything else on a pinned toolchain, see [Continuous integration](#continuous-integration) |
| Visual Studio Build Tools, C++ workload | the MSVC linker | |
| Tauri CLI 2.11 | the installer | `cargo install tauri-cli --version ~2.11 --locked` (2.11.5 builds the current installer); move it together with the tauri 2.12 upgrade |
| Node 22+ | the JS tests | no packages, no `npm install` |
| NSIS | the installer | downloaded by the Tauri CLI on the first `cargo tauri build` |
| WebView2 | running the app | part of Windows 11 |
| PowerShell 7 (`pwsh`) | the scripts in `tools\` | most also run on Windows PowerShell 5.1 |
| zig + cargo-zigbuild | Linux publisher binaries | `pwsh -File tools\cross\setup.ps1` installs them into `%LOCALAPPDATA%\cc-cross`, see [publisher-linux.md](publisher-linux.md#1-one-time-setup-on-windows-the-cross-toolchain) |
| Go | only to build `toolctl`, the maintainer's internal provisioning tool (a separate private repository, `..\tooling-ctl`) | the bundle generator builds it when it is not on PATH; not needed with `-SkipProvision` or [provisioning by hand](remote.md#by-hand) |
| Git Bash with GNU make | `simulate.sh` on Windows | |

`cargo` may not be on the system PATH; `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`
fixes that for a session.

## Fresh clone

```powershell
Copy-Item .cargo\config.example.toml .cargo\config.toml   # set target-dir; or set CARGO_TARGET_DIR instead
pwsh -File tools\stage-cc-hook.ps1                        # before any other cargo command
cargo build --workspace --locked
```

- **Target directory.** Build output grows to tens of GB. When the repository
  is in a synced folder (OneDrive, Dropbox), keep it outside:
  `.cargo/config.toml` (gitignored, a per-machine copy of
  `.cargo/config.example.toml` because cargo does not expand environment
  variables there; e.g. `%LOCALAPPDATA%\cargo-target\cc-usage`) or
  `CARGO_TARGET_DIR`. The scripts
  find it with `cargo metadata`, never by assuming `target\`. Below,
  `<target-dir>` is that folder.
- **Staging cc-hook.** The installer bundles `app\staging\cc-hook.exe`
  (`bundle > resources` in `app/tauri.conf.json`), and `tauri-build` checks
  that bundled resources exist even for `cargo build` and `cargo test`.
  `tools\stage-cc-hook.ps1` builds cc-hook in release and copies it there.
  `cargo tauri build` runs it again by itself (`beforeBuildCommand`).
- **`--locked`.** Builds and scripts use exactly what `Cargo.lock` pins. If
  it is out of date, update it on purpose (`cargo update -w`) and commit it.

## Build

```powershell
cargo build -p cc-usage --locked            # debug app: <target-dir>\debug\cc-usage.exe (with a console)
cd app; cargo tauri build                   # release installer: <target-dir>\release\bundle\nsis\cc-usage_<version>_x64-setup.exe
```

Toasts only show from the installed app, which has the Start menu
shortcut's AppUserModelID.

One app runs per data folder: a debug build started while the installed
app runs only shows the installed one's widget and exits. Quit the
installed app first, or give the debug build its own `CC_USAGE_DATA_DIR`
(an absolute path).

## Versioning and releases

- **One version** for the whole workspace and the app, in one place:
  `[workspace.package] version` in the root `Cargo.toml`. Every crate
  inherits it (`version.workspace = true`), and with it `cc-hook
  --version`, `cc-publish --version`, the publisher bundles, the host
  record and the app. `app/tauri.conf.json` has no `version` on purpose:
  Tauri then takes the `cc-usage` crate's, so the installer is
  `cc-usage_<version>_x64-setup.exe` and the exe's file version matches.
- **Semantic versioning.** A patch release for fixes; a minor release for
  new features or anything a machine must act on (a changed bucket layout,
  payload or file format that needs every publisher updated, a removed
  setting or command); a major release once there is a stable 1.0 to break.
  Docs never name a version: they describe the current one.
- **A CHANGELOG section per version** in [CHANGELOG.md](../CHANGELOG.md):
  `## X.Y.Z — unreleased` while work goes on, dated when it ships.
- **A git tag `vX.Y.Z` per released installer**, on the commit it was built
  from, starting with `v0.1.0`.

To bump it: the version in `Cargo.toml`, then `cargo update -w` (only the
workspace's own entries in `Cargo.lock` change), `pwsh -File
tools\stage-cc-hook.ps1`, and a new CHANGELOG section. To release: run the
[manual checks](#before-a-release), build the installer (`cargo tauri
build`), date the CHANGELOG section, commit, and tag `vX.Y.Z`.

## Tests

```powershell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
node --test "app/ui-tests/*.test.js"        # the flyout's pure JS modules; Node 22+, no dependencies
```

The JS tests import only the modules that touch neither the DOM nor the
backend (`format.js`, `session-text.js`, `history-math.js`,
`report-data.js`, `settings-text.js`, `key-text.js`); keep such logic there
so it stays testable. `node --check app/ui/scripts/<module>.js` checks one
module's syntax.

Ignored tests touch real data or the network and run only on request
(they are the first part of the [checks before a release](#before-a-release)):

| Command | What it does |
|---|---|
| `cargo test -p cc-local -- --ignored` | runs the real `claude` CLI and reads the real `~/.claude` sessions, transcripts and (a copy of) the spool |
| `cargo test -p cc-hook -- --ignored` | reads the real `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR`'s) and round-trips install and uninstall on a temp copy of it |
| `cargo test -p cc-publish --test live -- --ignored --nocapture` | against the live NATS deployment: copies the `pub-test` and viewer creds into a temp data dir with a fresh key, runs `check` and a 65 s `run`, opens the values as the viewer, clears every key it wrote |
| `cargo run -p cc-remote --example remote_check` | the live NATS permission checks, see [remote.md](remote.md#live-checks) |

The two live ones need provisioned creds for `cc-sessions.pub-test` and
`cc-sessions.viewer` ([remote.md](remote.md#provisioning)), as
`nats\<user>.creds` in the data folder. On a PC where a live publisher or
the app runs, keep the test creds apart: put them in a folder of their own
and point `CC_USAGE_DATA_DIR` at it (an absolute path) for the test's
shell; the live test copies them from there into its own temp folder and
never touches the real data folder. Both write only under the `test` alias
(`test.s.`); the viewer that runs at the same time sees a new key for
`test` in its notice: leave it waiting or reject it.

Three kinds of test skip themselves and say so (`skipped: …` on stderr, the
test still passes). With `CC_TEST_REQUIRE_ALL=1` (CI sets it) every one of
them fails instead, so a run never reports a skipped test as passed:

- **Shell tests.** cc-hook's status line chain tests need the shell Claude
  Code uses: Git Bash on Windows, `sh` elsewhere. Without it they print
  `skipped: no Git Bash`. `CC_TEST_REQUIRE_SHELL=1` fails only these.
- **Directory links.** cc-local's project scan test with linked folders (a
  loop and a link out of the tree) skips where the account may not create
  directory links (`skipped: cannot create directory links here`).
- **Local NATS.** `crates/cc-publish/tests/local_nats.rs` runs the connected
  path against a local server: cc-remote's put, watch and delete on a
  throwaway bucket, and the cc-publish binary's `run` (first puts, a
  reconnect, the re-put that follows, stop) on `cc-sessions`, read back as
  the viewer. It needs no creds or keys of yours (the server has no
  authentication; the test writes throwaway ones), and it puts a TLS front
  before the server, because cc-remote only connects over `tls://` or
  `wss://`. The front's CA, certificate and key are made by rcgen when the
  test runs (no key material is committed), and `SSL_CERT_FILE` makes that
  CA the only trusted root; a test in the same file checks them with a TLS
  handshake and no server, so it always runs. The test refuses a server
  that is not on a loopback address, since it creates and deletes buckets.
  Without `CC_TEST_NATS_URL` the server tests skip:

  ```sh
  nats-server -js                                   # or: docker run -p 4222:4222 nats:2.11 -js
  CC_TEST_NATS_URL=nats://127.0.0.1:4222 cargo test -p cc-publish --test local_nats -- --nocapture
  ```

Test folders are temporary folders removed when the test ends, also when it
fails (`tempfile`; cc-hook's tests use `TestDir` in `crates/cc-hook/src/testutil.rs`).

The publisher bundles and the bundle generator have their own tests:

```powershell
sh tools/publisher-bundle/test/simulate.sh                    # Git Bash + GNU make, or Linux; SIM_SH=dash for dash
powershell -NoProfile -ExecutionPolicy Bypass -File tools\publisher-bundle-windows\test\Test-BundleScripts.ps1
pwsh -NoProfile -File tools\publisher-bundle-windows\test\Test-BundleScripts.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File tools\publisher-bundle-gen\test\Test-BundleGen.ps1
pwsh -NoProfile -File tools\publisher-bundle-gen\test\Test-BundleGen.ps1
```

`Test-BundleScripts.ps1` needs a built `cc-publish.exe` (the newer of
release and debug, or `-PublishExe`) for its config, `keygen --confirm` and
rotation checks;
without one it skips them, and with `CC_TEST_REQUIRE_ALL=1` each such skip
is a failure.

### Continuous integration

`.github/workflows/ci.yml` runs on a push to `main`, on every pull
request, by hand (`workflow_dispatch`), and weekly (Mondays 06:17 UTC, the
Advisories job only, so a new advisory shows without a push). Clippy,
rustfmt and the tests use the toolchain pinned in its `RUST_TOOLCHAIN`
(a floating stable would break `clippy -D warnings` whenever new lints
ship; move it on purpose). The Windows and Linux jobs set
`CC_TEST_REQUIRE_ALL=1`. Every job has a timeout: Windows 60 minutes,
Linux 45, Advisories 15, Secrets 10.

| Job | Runs |
|---|---|
| Windows | `tools\stage-cc-hook.ps1` first (tauri-build needs `app\staging\cc-hook.exe`), `cargo fmt --all -- --check`, `cargo build`, `cargo test --workspace --exclude cc-publish --locked`, then `cargo test -p cc-publish --locked` with `CC_TEST_REQUIRE_ALL=0` and `CC_TEST_REQUIRE_SHELL=1` (only the Linux job has a NATS server, so `local_nats` skips here), `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo +1.89 check --workspace --all-targets --locked` (the minimum Rust version), `node --test "app/ui-tests/*.test.js"`, and `Test-BundleScripts.ps1` and `Test-BundleGen.ps1` under PowerShell 7 and Windows PowerShell 5.1 |
| Linux | a `nats-server -js` container and `CC_TEST_NATS_URL`, then `cargo test --workspace --exclude cc-usage --locked` (the Tauri crate needs webkit2gtk and the staged cc-hook.exe), clippy on the same crates, a `x86_64-unknown-linux-musl` build of cc-hook and cc-publish (with `musl-tools`), and `simulate.sh` under `sh` and with `SIM_SH=dash` and `SIM_SH=bash` |
| Advisories | `cargo audit` against the RustSec database, with `.cargo/audit.toml` |
| Secrets | the gitleaks release binary (Linux x64, checked against its pinned sha256), then `gitleaks dir . --config .gitleaks.toml --redact --exit-code 1` on the checkout and `gitleaks git .` with the same flags on the whole history (`fetch-depth: 0`), see [Advisories and secret scanning](#advisories-and-secret-scanning) |

Every action is pinned by commit SHA, with its release in a comment. To
move one, resolve the new tag's commit with `git ls-remote
https://github.com/<owner>/<repo> refs/tags/<tag>` and update both.
gitleaks is pinned by version and sha256 in the Secrets job's env
(`GITLEAKS_VERSION`, `GITLEAKS_SHA256`, the latter from the release's
`gitleaks_<version>_checksums.txt`); gitleaks-action is not used, as it
needs a license for an organisation's repository.

### Advisories and secret scanning

`cargo audit` (CI's Advisories job, or locally) reads `.cargo/audit.toml`.
It denies unmaintained, unsound and yanked crates, so any such warning not
on its reviewed list fails the audit. The list holds 10 warnings, one
comment each, all of them Tauri's dependencies with no fix this workspace
can pick: proc-macro-error and glib through gtk-rs 0.18 (Linux only), five
unic-* crates through urlpattern and tauri-utils, and zvariant, zbus_names
and zvariant_derive through tauri-plugin-notification (Linux only).
Vulnerabilities are never ignored: they are fixed by updating `Cargo.lock`
or the dependency. Drop an entry once `cargo tree -i <crate> --target all`
no longer finds the crate.

**Rule: no key material in the repository**, not even for tests: no private
key, nkey seed, creds file, JWT, token or certificate. A test that needs
one makes it at run time (a fresh nkey seed for its creds files, rcgen for
the local NATS test's TLS front, fresh X25519 keys checked against
x25519-dalek instead of a published test vector), and a fixture that only has to look like
a secret is a short placeholder such as `SUAFAKE` that no rule matches as a
real one.

`.gitleaks.toml` configures [gitleaks](https://github.com/gitleaks/gitleaks)
8.30 or later (older releases ignore its rule-scoped allowlists): the
default rules, a `nats-nkey-seed` rule, and allowlists for the fake tokens
and placeholders of the tests, each scoped to its rules, its files and the
exact string. None covers key material. CI's Secrets job runs both scans
on every push to `main` and every pull request; scan before a push too,
since a pushed secret is out even if CI then fails:

```powershell
gitleaks git . --config .gitleaks.toml --redact        # the commits
gitleaks dir . --config .gitleaks.toml --redact        # the files; run it on a clean checkout
```

`gitleaks dir` also reads gitignored files such as `.env`, creds and the
project file, so on a working copy it reports real secrets that never
reach the repository: scan a fresh clone instead.

### Before a release

What CI cannot run, by hand, on a PC with Claude Code and the live
deployment's test creds:

1. The ignored tests and live checks above: `cargo test -p cc-local --
   --ignored`, `cargo test -p cc-hook -- --ignored`, `cargo test -p
   cc-publish --test live -- --ignored --nocapture` and `cargo run -p
   cc-remote --example remote_check`.
2. The local NATS test with `CC_TEST_NATS_URL` and `CC_TEST_REQUIRE_ALL=1`,
   and `Test-BundleScripts.ps1` with a release `cc-publish.exe` built and
   `CC_TEST_REQUIRE_ALL=1`, under both PowerShells.
3. `gitleaks git` on the commits to be pushed, and `cargo audit`.

Then look at the app (a debug build, [Development flags](#development-flags)):

- `--demo` with **Theme** Light and then Dark: the session rows (states,
  context meters, agents tags, the offline row, subagent lines), the
  footer, `--view usage` and `--view settings`, and the panel at a narrow
  width.
- A publisher key waiting for review: a viewer with test creds in its own
  `CC_USAGE_DATA_DIR`, and `cc-publish run` as `test` with a fresh key. The
  notice shows above the rows with the key id and fingerprint; **Reject**
  and **Accept** work, and after Accept the rows appear.
- Remote subagent lines: on that publisher, a session that starts two or
  more subagents of different types at once. The viewer's row shows a line
  per subagent with its own task (not swapped) and the `2/5 agents` tag.

Then the publisher bundles on real machines
([publisher-windows.md](publisher-windows.md),
[publisher-linux.md](publisher-linux.md)):

- **Windows, a test install beside the app**, on a Windows 11 PC that has
  the app installed, without touching the real install: everything through
  `-Prefix`, `-ClaudeSettings`, `-TaskName "cc-usage publisher TEST <n>"`
  and `-SkipAppCheck`, in a folder with spaces under `%TEMP%`, the scripts
  run with Windows PowerShell 5.1. A bundle generated for alias `test`; the
  zip extracted with a simulated Mark of the Web; `setup.cmd` with the real
  defaults refusing because of the app (nothing changed) and clearing the
  Mark of the Web; Uninstall with the real defaults refusing too; an install
  with the task (files and ACLs, hooks and the status line chain in a
  settings file shaped like the real one, the task's settings), Verify all
  PASS, the viewer opening the host record and the sessions; a second
  install reporting unchanged and restarting cleanly; `cc-publish stop`
  ending the task; Verify from the installed copy; `setup.cmd uninstall`
  from the installed copy (settings restored byte for byte, the copy deleted
  after the window closed, secrets kept); then the Run entry mode, Verify,
  `cc-publish clear` and `Uninstall.ps1 -Purge` (folder gone). The real
  install, settings, chain record and keys stay unchanged throughout.
- **Windows, no window**, in that test install, for the task and for the
  Run entry: cc-publish's parent is `conhost.exe --headless`, neither
  process has a main window handle, an `EnumWindows` scan finds no visible
  top-level window owned by either, and no Windows Terminal process starts.
  As a control, the same scan on `conhost.exe` without `--headless`
  (minimized) finds its window. Ending the task in Task Scheduler leaves
  cc-publish running (only `conhost.exe` ends), which is why the scripts use
  `cc-publish stop`.
- **Windows, the autostart**, with a bundle carrying dummy creds (no NATS
  connection) and the scripts started from PowerShell 7: the task without
  the time trigger has no next run time after install (no restart before
  the next logon), and with it the task restarts a killed publisher within
  5 minutes; the `schtasks /XML` fallback registers the same task; a
  reinstall and Verify (all PASS but the two NATS checks); Uninstall with the
  app present removes only the publisher's files and leaves the hooks,
  `cc-hook.exe` and the secrets; Uninstall with the real defaults refuses and
  changes nothing.
- **Windows, the real defaults**, on a PC without the app (the app check
  passing, the default paths): install, Verify, uninstall.
- **Windows, a second PC**, from a real download: a real logon starting the
  task, task creation under a standard (non-admin) account and under
  corporate policies, SmartScreen and Mark of the Web, AppLocker or WDAC,
  NATS through that network's proxy, and an upgrade that fails and rolls
  back.
- **Linux, a real machine**: `make install && make verify`, `update.sh` on
  that install, and the hardened unit (`SystemCallArchitectures`,
  `RestrictAddressFamilies`) with `claude agents` running under the service.

**Rule: tests never touch the real machine state.** No test writes the real
registry, scheduled tasks, the user's `~/.claude` (`settings.json` above
all), the widget's settings or the data folder. Registry logic (`app/src/autostart.rs`) sits behind
traits tested with fakes; file logic takes its paths as arguments or uses
`CC_USAGE_DATA_DIR` / `CC_USAGE_CONFIG_DIR` / `CLAUDE_CONFIG_DIR` and
cc-hook's `--settings <file>` on temp folders. Every cc-hook a test spawns
also gets `USERPROFILE`, `HOME` and `CLAUDE_CONFIG_DIR` in a temp folder, so
a call without `--settings` cannot reach the real `~/.claude`; the bundle tests take `-Prefix`, `-ClaudeSettings` and `-TaskName`
and run in `%TEMP%`. A test that writes the real Run key can wipe the
user's startup entries.

## Development flags

Debug builds only; a release build ignores them all (an installed app must
not load arbitrary pages or click its own buttons). Unknown arguments are
ignored in both.

| Flag | Effect |
|---|---|
| `--demo` | sessions, footer and Remote line come from the fixtures in `app/src/demo.rs` (the mockup's data); real session sources are not read, rows cannot be clicked, nothing toasts |
| `--demo-empty` | the same, with no sessions |
| `--view usage` / `--view settings` | opens on that view |
| `--backdrop <colour>` | paints the transparent cushion around the panel (`white`, `#f4f5f9`): a window capture shows transparency as black |
| `--click <id or CSS selector>` | clicks that element once it exists (up to 15 s), e.g. `--click btn-hide`, `--click button.host-filter` |
| `--reference <file.html>` | loads that page in the widget window instead of the app, e.g. `docs\reference\flyout-mockup.html` |
| `--low-thresholds` | limit alerts at 1% and 2%, to see a real toast |

Demo mode still fetches the real account profile, so the usage view's
Account line shows the real email.

## Screenshots

```powershell
$t = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
$env:CC_USAGE_CONFIG_DIR = "$env:TEMP\cc-usage-shots"     # empty folder: first-launch size and position
& "$t\debug\cc-usage.exe" --demo --backdrop white
& "$t\debug\cc-usage.exe" --demo --view settings --backdrop white
& "$t\debug\cc-usage.exe" --reference docs\reference\flyout-mockup.html --backdrop white
```

Capture the window and save it in `docs/screenshots/`, overwriting the
file of the same name (no version in it). Current images: `panel.png`
(sessions), `panel-dark.png`, `settings.png`, `settings-dark.png`. Never
commit a capture of the usage view's Account line with a real account (even
demo mode reads the real profile), nor one of real sessions or projects. The UI follows
`docs/reference/flyout-mockup.html`, the design document's panel mockup:
same markup, class names and CSS.

## Diagnostics

```powershell
cargo run -p cc-usage --example probe                       # the usage and sessions as text; -- --watch 30 repeats
Get-Content "$env:LOCALAPPDATA\cc-usage\cc-usage.log" -Tail 50 -Wait     # the app's log, both roles (rotated at 1 MiB to .log.1)
$env:CC_USAGE_DEBUG = "1"                                    # then start the app: it also logs its first session pass
cargo run -p cc-local --example spool_view -- <copy of a spool folder>   # replays hook events (deletes what it reads)
cargo run -p cc-focus --example focus_probe -- <pid>        # where a click would land; --activate to try it
$env:CC_HOOK_DEBUG = "1"                                     # cc-hook logs to <data dir>\cc-hook.log
& "$env:LOCALAPPDATA\cc-usage\cc-hook.exe" status            # hooks and status line chain in settings.json
```

## Tools

| Script | Does |
|---|---|
| `tools\stage-cc-hook.ps1` | builds cc-hook (release, `--locked`) and copies it to `app\staging\` |
| `tools\make-icon.ps1` | draws `app/icons/icon.png`, `icon.ico` and the two tray images, `tray-grid.png` and `tray-grid-alert.png` (embedded by `tray.rs`; design: `docs/icons/final.png`) |
| `tools\seed-history.ps1 [-Hours N] [-NoBackup] [-Force]` | writes a synthetic `history.json` (`-Hours`, default 6, of 5-minute samples) into `CC_USAGE_DATA_DIR`, to develop the chart; refuses the real data folder (`CC_USAGE_DATA_DIR` unset or pointing at `%LOCALAPPDATA%\cc-usage`) without `-Force`, and copies an existing file to `history.json.bak-<timestamp>` first unless `-NoBackup` |
| `tools\make-publisher-bundle.ps1` | publisher bundles and the Linux update package, see [publisher-linux.md](publisher-linux.md) and [publisher-windows.md](publisher-windows.md); `-ViewerPub` takes the viewer's public key (a copy in `pub\` is gitignored); `-Servers` and `-Bucket` the NATS connection written to `publish.json` (the viewer's settings by default); `-ProjectFile` the NATS project file (`infra\nats\cc-sessions.yaml`, a copy of the example) |
| `tools\publisher-bundle-gen\` | the functions of `make-publisher-bundle.ps1`, dot-sourced by it (they run in its scope): `common.ps1` (synced folder and repository checks, key file check, tar.gz and zip writers, private ACL), `provision.ps1` (toolctl, the internal provisioning tool), `keys.ps1` (the viewer's key, the summary's note on the publisher's own), `windows.ps1`, `app.ps1`, `linux.ps1` and `update.ps1` (each platform's steps); not run on their own |
| `tools\publisher-bundle\`, `tools\publisher-bundle-windows\` | what goes into the Linux and Windows bundles (scripts, systemd unit, `setup.cmd`), with their tests |
| `tools\cross\setup.ps1`, `tools\cross\build-linux.ps1` | the zig toolchain (`%LOCALAPPDATA%\cc-cross`) and static Linux builds; `common.ps1` is what both share |
| `tools\pricing.example.json` | the format of `pricing.json` |

## Environment variables

| Variable | Effect |
|---|---|
| `CC_USAGE_DATA_DIR` | moves the data folder, `%LOCALAPPDATA%\cc-usage` (app, cc-hook, cc-publish, `seed-history.ps1`); only an absolute path counts |
| `CC_USAGE_CONFIG_DIR` | moves the app config folder, `%APPDATA%\ar.cc.usage` after the identifier in `app/tauri.conf.json` (`settings.json`, `window.json`); an absolute path only: a relative value is ignored, and the footer tooltip and the settings view name it |
| `CLAUDE_CONFIG_DIR` | Claude Code's folder instead of `~/.claude`; only an absolute path counts |
| `LOCALAPPDATA`, `XDG_DATA_HOME`, `USERPROFILE`, `HOME` | where the data folder and `~/.claude` are found (`cc_core::paths`) |
| `CC_USAGE_NATS_SERVERS`, `CC_USAGE_NATS_BUCKET` | the app's NATS servers (comma separated) and bucket, over `"nats"` in `settings.json`, see [remote.md](remote.md#connection-settings); `remote_check` reads the bucket one |
| `CARGO_TARGET_DIR` | the build folder, instead of `.cargo/config.toml` |
| `CC_CROSS_ROOT` | the zig toolchain folder (default `%LOCALAPPDATA%\cc-cross`) |
| `CC_USAGE_DEBUG`, `CC_HOOK_DEBUG` | diagnostics, above |
| `CC_PUBLISH_LOG` | log level of cc-publish and of the app's `cc-usage.log` (`info` by default) |
| `CC_TEST_REQUIRE_ALL` | `1`: every test that would skip fails instead (the shell tests, cc-local's directory links, `local_nats.rs` without `CC_TEST_NATS_URL`, `Test-BundleScripts.ps1` without a built `cc-publish.exe`), above |
| `CC_TEST_REQUIRE_SHELL` | `1`: only a test that needs Git Bash or `sh` fails instead of skipping, above |
| `CC_TEST_NATS_URL` | `nats://<loopback>:<port>` of a local `nats-server -js`: runs `tests/local_nats.rs` of cc-publish, above |
| `TOOLCTL`, `SYNADIA_CLOUD_ACCESS_TOKEN` | the bundle generator's toolctl (the internal provisioning tool) and Synadia token, see [remote.md](remote.md#with-toolctl) |

## Repository conventions

- **English** for code, comments, docs and UI text.
- **Line endings** are fixed by `.gitattributes` whatever `core.autocrlf`
  says: LF for `*.sh`, `Makefile`, `*.service.in` and everything under
  `tools/publisher-bundle/` (they run on Linux); CRLF for `*.ps1`, `*.cmd`,
  `*.bat` and `*.nsh`. The Windows bundle's text files must stay ASCII
  (Windows PowerShell 5.1 reads BOM-less files in the ANSI code page); the
  bundle generator checks it.
- **`.gitignore`** keeps out secrets (`.env`, `*.creds`, `*.nk`, `key`,
  `secrets/`, bundles and their archives), Settings → Data backups
  (`*.ccbak`), `pub/` (a viewer's public key copied for `-ViewerPub`: public,
  but it belongs to one installation), `infra/nats/cc-sessions.yaml` (a
  deployment's project file, which lists its machine aliases; the
  repository keeps `cc-sessions.example.yaml`), `.cargo/config.toml`, build
  output, `app/gen/schemas/` (regenerated by tauri-build), `app/staging/`,
  logs and OS and editor files.
- **No real machine names.** Examples, tests and fixtures use made-up
  aliases (`devbox`, `lab-01`, `office-pc`; `test` for the tests), never a
  deployment's.
- **`.env.example`** names the one variable the bundle generator may read,
  `SYNADIA_CLOUD_ACCESS_TOKEN`, for toolctl, the internal provisioning tool.
  Prefer `toolctl auth login`.
- **License**: MIT, in `LICENSE`.
