# Windows publisher bundle

A Windows PC **without the cc-usage app** publishes its Claude Code sessions,
encrypted, to the `cc-sessions` bucket, and the widget on the main machine
shows them as remote rows tagged with the PC's alias (see
[remote.md](remote.md)). One command on the widget machine, one double-click
on the PC:

```powershell
pwsh -File tools\make-publisher-bundle.ps1 -Alias office-pc -Platform windows   # on the widget's machine
```

Then, on the other PC: unblock the `.zip`, extract it, double-click
`setup.cmd`. No administrator rights, no reboot, no Rust, Node or Git there.

A machine that has the app does not need this: the app shows its own
sessions, and publishes them itself in its Publisher role (`-Platform app`,
see [A PC with the app as a publisher](remote.md#a-pc-with-the-app-as-a-publisher)).
The installer refuses to run where the app is installed, because the app and
the publisher would drain the same spool in the same folder.

What a session sends (state, tool, model, context, title, folder name and
second-line text) is in
[What leaves the machine](data-and-privacy.md#what-leaves-the-machine).

| Path in the bundle | Contents |
|---|---|
| `bin\cc-hook.exe`, `bin\cc-publish.exe` | x86_64 Windows binaries (MSVC), the same ones the app ships and `cc-publish` |
| `secrets\cc-sessions.pub-<alias>.creds` | NATS credentials: may write only `$KV.cc-sessions.<alias>.>` |
| `config\viewer.pub` | the widget machine's public key (or the `-ViewerPub` file), which every value is sealed to (not secret) |
| `config\publish.json` | cc-publish's config: alias, servers and bucket (the viewer's, or `-Servers` and `-Bucket`; plaintext transports are refused), creds and key files relative, `log_file` on |
| `config\bundle.json` | alias, NATS user, architecture, target, version, build time |
| `setup.cmd` | double-click entry point: Install then Verify; `setup.cmd uninstall [-Purge]`; `setup.cmd rotate-key [-ConfirmAlias <alias>]` |
| `Install.ps1`, `Verify.ps1`, `Uninstall.ps1`, `scripts\common.ps1` | the logic, for Windows PowerShell 5.1 (and 7) |
| `README.md`, `SHA256SUMS` | short instructions; checksums of every other file |

There is no private key in it. The PC's key pair is its
identity: Install.ps1 creates it on the PC the first time, prints its public
key line, and keeps it on every reinstall; the widget asks to accept that key
with the first value it receives (see [Identity](#identity)).

## 1. Generate

```powershell
pwsh -File tools\make-publisher-bundle.ps1 -Alias <alias> -Platform windows [-Out <folder>] [-ViewerPub <file>] [-Servers <urls>] [-Bucket <name>]
```

Same alias rules, viewer key checks, NATS servers and bucket (`-Servers`,
`-Bucket`, by default the viewer's settings on the building PC),
provisioning, keys and output folder
rules as the [Linux bundle](publisher-linux.md#2-generate-a-bundle): a PC in
the app's Publisher role is refused unless `-ViewerPub` names a copy of the
viewer's `keys\viewer.pub`, in which case nothing is generated on
the building PC (for example `-Alias office-pc -Platform windows -ViewerPub
D:\viewer.pub`); the NATS user
`cc-sessions.pub-<alias>` is declared in the local project file
`infra\nats\cc-sessions.yaml` (a copy of `cc-sessions.example.yaml`, ignored
by git) and applied with toolctl, the internal provisioning tool
(`toolctl add-publisher`, `toolctl apply --no-creds`, `toolctl creds`; see
[Provisioning](publisher-linux.md#provisioning);
needs the Synadia token in the environment, `.env` or toolctl's credential
store; `-SkipProvision` uses creds already in
`%LOCALAPPDATA%\cc-usage\nats`, such as creds made
[by hand](remote.md#by-hand)), the widget machine's key pair is made sure
of (`keys\viewer.key`, `keys\viewer.pub`; the bundle's own `cc-publish.exe`
runs `viewer-keygen --if-missing`), no key pair is made or pinned for the PC
(it makes its own when the bundle is installed), and the default `-Out` is
`%LOCALAPPDATA%\cc-usage\bundles`; a folder inside the repository is always
refused, a OneDrive folder unless `-AllowSyncedFolder`. What differs:

1. The sources (`tools\publisher-bundle-windows`) are checked with
   PowerShell's own parser and must be ASCII (Windows PowerShell 5.1 reads a
   BOM-less script in the ANSI code page). They are written with CRLF line
   endings, which `cmd.exe` needs for `setup.cmd`.
2. `cargo build --release --locked -p cc-hook -p cc-publish` for
   `x86_64-pc-windows-msvc`; both binaries are checked by their PE header
   (machine 0x8664). `-SkipBuild -BinDir <dir>` uses ready ones.
   `-Target aarch64` builds `aarch64-pc-windows-msvc` into
   `cc-usage-publisher-<alias>-windows-arm64` when that Rust target and the
   MSVC ARM64 tools are installed (without the target, `-Target aarch64`
   stops with that message, and an x86_64 build notes that no ARM64 bundle
   was made); Windows on ARM runs the x64 bundle under emulation.
3. The output is `<Out>\cc-usage-publisher-<alias>-windows\` and
   `<Out>\cc-usage-publisher-<alias>-windows.zip` (entries checked after
   writing). As for the Linux bundle, the folder and the zip get an ACL for
   the current user and SYSTEM only: no inherited entries and nobody else's
   explicit ones; what is inside the folder inherits only that, and each
   item is checked to be private and readable by its owner. The script
   stops if that ACL cannot be set.

## 2. Transfer

The bundle holds the NATS credentials, which let whoever holds them write
that alias's keys in the bucket (they decrypt nothing, and the viewer only
takes values sealed with the PC's own key). Move the
`.zip` over a private channel: your
own USB stick, a private network share, `scp` (OpenSSH is built into
Windows). Not e-mail, chat or a shared drive. Compare the SHA-256 the
generator prints:

```powershell
Get-FileHash .\cc-usage-publisher-office-pc-windows.zip
```

A zip has no permissions: the extracted files take the ACL of the folder you
extract into, so extract under your own profile (Downloads, Documents), not
into a shared folder, and delete the folder and the zip once Verify passes.
If a bundle leaks, follow
[A leaked bundle](data-and-privacy.md#a-leaked-bundle): delete the NATS user
(`toolctl users delete`, or the tool that made it) and generate a new bundle. The leaked bundle holds
no key of the PC, so its identity stays.

## 3. Install

1. Right-click the `.zip`, **Properties**, tick **Unblock**, OK. Extract it.
2. Double-click `setup.cmd` in the extracted folder.

`setup.cmd` first checks every file against `SHA256SUMS` (a PowerShell
command, which the execution policy does not apply to); on a mismatch it
stops without unblocking or running anything. Then it clears the Mark of the
Web on the extracted files (`Unblock-File`, also a command), runs
`powershell -NoProfile -ExecutionPolicy Bypass -File Install.ps1`, then
`Verify.ps1`, and waits for a keypress so the table stays on screen
(`CCU_NO_PAUSE=1` skips the pause). Its exit code is Install's when that
fails, else Verify's. Extra arguments go to Install.ps1 (`setup.cmd -Force`) and then
to Verify.ps1 as well, so `-Prefix`, `-ClaudeSettings`, `-TaskName` and
`-SkipAppCheck` verify the install they made; Verify accepts and ignores the
Install-only `-Force`, `-Autostart`, `-RotateKey` and `-ConfirmAlias`.
`setup.cmd rotate-key [args]` runs `Install.ps1 -RotateKey [args]`, then
Verify (see [Identity](#identity)).

**Install.ps1** checks first and changes nothing when any check fails:

| Check | Fails when |
|---|---|
| Windows | not Windows 10 1809+ (build 17763, for the headless console) or Windows 11 |
| architecture | the bundle is ARM64 and Windows is x64 (x64 on ARM64 is a warning) |
| cc-usage app | the app's uninstall entry (`HKCU\...\Uninstall\cc-usage`, or a DisplayName `cc-usage` under HKCU/HKLM) or `%LOCALAPPDATA%\cc-usage\cc-usage.exe` exists |
| Claude Code profile | the folder of `-ClaudeSettings` (by default `%CLAUDE_CONFIG_DIR%` when set, else `%USERPROFILE%\.claude`) is missing (`-Force` installs anyway) |
| Claude Code folder | warning only: `CLAUDE_CONFIG_DIR` in the installing shell differs from the saved user (or machine) environment, which is what the publisher gets at sign-in |
| claude on PATH | warning only: without it cc-publish sees only hook events |
| checksums | a file differs from `SHA256SUMS`, or `bin\` holds a file it does not list (a DLL dropped next to the binaries would load when they run). Checked before anything is unblocked or run |
| cc-hook.exe, cc-publish.exe | `--version` fails (AppLocker, WDAC or an antivirus) |
| identity | never a FAIL: PASS when the PC's key is kept or this is a first install; a WARN when `keys\publisher.key` is missing although a publisher was installed here, or does not load. Then, after the table, the install asks for the alias (or takes `-ConfirmAlias`) and stops, changing nothing, without it (see [Identity](#identity)) |
| earlier rollback | a `.cc-usage-rollback-<pid>` folder in the install folder holds `ROLLBACK-INCOMPLETE.txt`: an earlier rollback did not complete and its copies are the only ones of the previous files ([below](#when-a-rollback-does-not-complete)) |

Running elevated is a warning: nothing needs it, and it installs for the same
user anyway. Then, idempotently (a second run reports *unchanged* and
restarts the publisher):

1. Opens a transaction: a private copy (in
   `%LOCALAPPDATA%\cc-usage\.cc-usage-rollback-<pid>\`) of every file the
   install may replace (binaries, creds, `viewer.pub`, `publish.json`, the
   manifest, `statusline-chain.json`, `settings.json`, the tooling copy),
   the publisher's task as XML with its state, and its Run entry. Then
   stops an installed publisher: disables its task (so the repetition
   cannot restart the old binary meanwhile), then `cc-publish stop` (clean),
   then ends the task, then any process left from that executable.
2. Copies `cc-hook.exe` and `cc-publish.exe` to `%LOCALAPPDATA%\cc-usage\`,
   where the app keeps `cc-hook.exe`, so hook commands look the same on every
   machine. A different existing binary is renamed aside first (a running
   `.exe` can be renamed, not overwritten) and deleted when free.
3. Copies the creds to `%LOCALAPPDATA%\cc-usage\nats\` and `viewer.pub` to
   `keys\viewer.pub`, all with a private ACL (below; `keys\` too). A
   different existing file is kept as `<name>.bak-<timestamp>`, also
   private. Keeps the PC's key pair, `keys\publisher.key` (*kept this
   machine's identity &lt;kid&gt;*), or makes it on the PC (`cc-publish
   keygen --if-missing`, or `--force` after a backup for a confirmed key
   that does not load) and prints its public key line, again at the end:
   the widget asks to accept it with the first value it receives.
4. Writes `%LOCALAPPDATA%\cc-usage\publish.json` with absolute creds and key
   file paths and `log_file: true` (log in `%LOCALAPPDATA%\cc-usage\cc-publish.log`,
   rotated at 1 MiB) and the bundle's servers and bucket, and stops if
   `cc-publish status` does not load it: any `config` row other than the
   file's path (cannot read, too large, invalid JSON, alias, servers or
   bucket) is an error.
5. Copies the scripts, without binaries or secrets, to
   `%LOCALAPPDATA%\cc-usage\bundle\` with a marker of where it installed, so
   Verify and Uninstall work after the bundle folder is deleted.
6. `cc-hook.exe install`: the hooks and the status line chain in
   `%USERPROFILE%\.claude\settings.json` (`%CLAUDE_CONFIG_DIR%\settings.json`
   when that is set), appended after existing hooks, with
   a timestamped backup, verified after writing.
7. Registers autostart (below) and records everything in
   `%LOCALAPPDATA%\cc-usage\publisher-manifest.json` (alias, version, paths,
   autostart kind, SHA-256 of every installed file), which Verify checks and
   Uninstall follows.
8. Starts the publisher and waits until it runs and has written its state;
   then the transaction is complete and the copies are deleted. The final
   hint names the installed copy's `Verify.ps1`, which still works once the
   bundle folder is deleted.

If any step from 1 on fails (cc-publish rejects the new `publish.json`,
`cc-hook install` or keygen fails, an ACL cannot be set, a file is locked,
the new publisher does not start within 20 s, or Ctrl+C), the install rolls
back before it exits 1: it stops the new publisher, registers the previous
task again from its XML (or removes a task the install created), restores
the Run entry, puts every copied file back (removing files that were not
there before), enables the task it disabled, and starts the previous
publisher the way it was installed, waiting until it runs. The PC's key
pair is never rolled back (a new key may already be accepted on the
widget); the restored manifest records it. A task the user had disabled
stays disabled. A failed first install leaves only its key pair. A
rollback folder without a marker (an install cut short before any rollback
ran) is deleted by the next install.

#### When a rollback does not complete

Before it restores anything, the rollback writes `ROLLBACK-INCOMPLETE.txt`
into its `%LOCALAPPDATA%\cc-usage\.cc-usage-rollback-<pid>\` folder: each
copy as `<NNN-name>  ->  <original path>`, the files that were not there
before the install, and, if a step failed, what it could not do. When every
step works, the marker and then the folder are deleted. When one fails, the
others still run, and the folder and its marker stay: those copies may be
the only ones of the previous files, secrets included. The error names the
folder and the task to enable.

While such a folder exists, `setup.cmd` refuses to install (a FAIL row,
*earlier rollback*, in the pre-checks; nothing is installed) and Verify
shows it as a WARN. Put back what you need from it (the marker says where
each copy goes), move the folder out of the install folder, delete it once
done (it holds copies of the creds), and run `setup.cmd` again.
`setup.cmd uninstall` deletes such folders too, and prints `removed <dir>
(copies kept for a rollback)` for each.

Sessions already open report fine-grained states after a restart (Claude Code
loads hooks when a session starts); until then they show busy or idle.

### Autostart: a per-user scheduled task

The task is called **cc-usage publisher**, in the root folder of Task
Scheduler, created by the user for the user:

| Setting | Value | Why |
|---|---|---|
| Triggers | at logon of this user, and a time trigger from the install on, both repeated every 5 minutes, indefinitely | the repetition restarts a publisher that died; a logon trigger's repetition only begins at the next logon, so the time trigger covers the session the install runs in |
| Multiple instances | ignore new | while one runs, the repetition does nothing |
| Principal | the user's SID, interactive token, least privilege | no password stored, no administrator rights, runs only while logged on |
| Action | `conhost.exe --headless "<prefix>\cc-publish.exe" --data-dir "<prefix>" run --config "<prefix>\publish.json"` | no window (below) |
| Time limit | none (`PT0S`) | it runs all day |
| Power | starts and keeps running on battery | laptops |
| Restart on failure | every minute, 10 times | when the task itself fails to start |

Why a task and not a service: a service needs administrator rights and runs
outside the user's session, while cc-publish must run as the user (it reads
`~/.claude`, runs `claude agents`, and finds the user's PATH). Why the 5
minute repetition: `conhost.exe` always exits 0, so Task Scheduler never sees
cc-publish fail, and restart-on-failure alone would not bring it back.

If the task cannot be registered (a policy against task creation),
Install.ps1 tries `schtasks.exe /Create /XML` and then falls back to an HKCU
**Run** entry with the same command line. It starts the publisher at logon
but does not restart it if it stops; Verify says which one is in use, and
fails if the Run entry was disabled in Task Manager's Startup tab.
`-Autostart Task` or `-Autostart RunKey` forces one.

### No window: conhost --headless

`cc-publish.exe` is a console program. Started by a task or at logon without
a console, Windows gives it a new console window (on Windows 11, possibly a
Windows Terminal tab). `conhost.exe --headless` (Windows 10 1809 and later)
creates the console without any window, and cc-publish runs inside it.
`powershell -WindowStyle Hidden` would flash a window, and VBScript wrappers
are being removed from Windows.

Verify checks that cc-publish's parent is `conhost.exe --headless` and that
neither process has a main window handle. The full check (no visible
window, no Windows Terminal), for the task and for the Run entry, is one of
the [checks before a release](development.md#before-a-release).

Ending the task in Task Scheduler kills `conhost.exe` but not cc-publish,
so the scripts stop it with `cc-publish stop`, which asks the run
loop to stop cleanly (last flush, state file marked stopped), and then kill
what is left of that executable.

### Secrets: private ACL

`nats\` and `keys\` (folders), the creds file and the key files get an ACL with only the current
user and SYSTEM, full control, and inheritance off: `icacls <path>
/inheritance:r /grant:r *<user SID>:F *S-1-5-18:F` (`(OI)(CI)F` on the
folder), then any other explicit entry is removed with `icacls /remove` (new
files under `%TEMP%`, for instance, carry explicit Administrators entries).
Each file is copied into a staging folder next to its destination
(`.cc-usage-staging-<pid>`) that is created empty and made private before
anything is written into it, so the copy inherits the private ACL from the
start; it is then made private explicitly and renamed into place (a rename
keeps the ACL), and the staging folder is removed (a leftover one is removed
by the next install or `-Purge`). A temp file next to the key would have
inherited the install folder's ACL, administrators included, for a moment.
The result is read
back with `Get-Acl` and install fails if anybody else can open it; Verify
checks it again. (An administrator can still take ownership of any file;
nothing on Windows prevents that.)

## 4. Verify

`setup.cmd` runs it after installing. Later:

```bat
powershell -NoProfile -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\cc-usage\bundle\Verify.ps1"
```

| Check | PASS when |
|---|---|
| cc-usage app | (FAIL only) the app is installed here |
| bundle checksums | `SHA256SUMS` matches (only from the full bundle) |
| installed files | binaries, creds, key files and `publish.json` match the manifest |
| cc-hook.exe / cc-publish.exe `--version` | both start and report the bundle's version |
| secrets ACL | creds, `keys\publisher.key`, `nats\` and `keys\` open to the user and SYSTEM only, inheritance off |
| publish.json | valid JSON, alias matches, creds exist, both key files exist (each missing one is named, with how to get it), no members cc-publish rejects |
| hooks | `cc-hook status` lists every event installed, registered to the installed `cc-hook.exe` |
| status line chain | `statusLine` chained to the installed cc-hook (a warning otherwise) |
| cc-publish check | the key files (the key ids it seals with, `viewer.pub` present), NATS connect, bind, sealed probe put and delete (a second connection for a few seconds) |
| autostart | the task exists, runs `conhost --headless`, has the logon trigger and is not disabled; or the Run entry is present and enabled |
| earlier rollback | (WARN only) no rollback folder with `ROLLBACK-INCOMPLETE.txt` is left in the install folder ([above](#when-a-rollback-does-not-complete)) |
| publisher process | running from the installed path, under `conhost --headless`, without a window |
| recent publish | `cc-publish status` exits 0 (alive and wrote within 150 s), waiting up to `-WaitSeconds` (75) |

Exit code 1 on any FAIL.

## 5. Uninstall

```bat
"%LOCALAPPDATA%\cc-usage\bundle\setup.cmd" uninstall           & rem keeps creds and keys (a reinstall keeps the identity)
"%LOCALAPPDATA%\cc-usage\bundle\setup.cmd" uninstall -Purge    & rem removes them too
```

When the cc-usage app is installed (it owns the same folder, cc-hook.exe
and hooks), Uninstall.ps1 changes nothing, unless a publisher was installed
there before the app (a publisher manifest exists): then it removes only the
publisher's task or Run entry, `cc-publish.exe`, `publish.json`, the
manifest, its state and log and the scripts copy, and leaves `cc-hook.exe`,
the hooks, the status line, the creds, the key and the spool to the app
(`-Purge` is ignored). Otherwise it first removes the task or the Run entry
(only if it runs this `cc-publish.exe`), so the task's repetition cannot
start a new publisher meanwhile, then stops the publisher, runs
`cc-hook.exe uninstall` (hooks out, the previous
status line back, byte for byte for files in Claude Code's own format), and
removes the binaries, `publish.json`, the manifest, the state file, the log
and the scripts copy (when it runs from that copy, a hidden helper deletes it
once the window closes), and any rollback folder, also one an incomplete
rollback left (`removed <dir> (copies kept for a rollback)`). `-Purge` also removes the creds,
`keys\publisher.key`, `keys\viewer.pub`, their backups, the spool, the live
files, the status line record and the folders if they end up empty.

## 6. Updating

There is no separate update package for Windows (`-Update` is Linux only).
Generate a new bundle for the same alias on the widget machine, or with
`-ViewerPub` elsewhere (provisioning is idempotent: same user, same creds),
and run its `setup.cmd` on the PC, as for the first install.
Install.ps1 stops the running publisher, replaces the binaries (a running
`.exe` is renamed aside first), reports unchanged files as *unchanged*,
rewrites the manifest, restarts the publisher and Verify runs. Hooks and
the PC's key pair stay (nothing to do on the widget machine); a different
creds file or `viewer.pub` is kept as a private `.bak-<timestamp>` copy.
An update that fails rolls back to the previous version, running and with
its task enabled (see [3. Install](#3-install)).

## Identity

The PC's key pair, `%LOCALAPPDATA%\cc-usage\keys\publisher.key`, is its
identity: every value is sealed with it, and the widget takes values for the
alias only from a key it has accepted. It is born on the PC and changes only
on purpose:

- **First install.** No key and no earlier install (neither
  `publisher-manifest.json` nor `publish.json` in the folder): Install.ps1
  runs `cc-publish keygen --if-missing` and prints `generated this machine's
  identity <kid>` and the public key line, once more at the end. On the
  widget machine, cc-usage asks to accept the new key with the first value
  it receives; accept it after comparing the key id and fingerprint it shows
  (the first 8 characters of the key) with the ones printed here.
- **Reinstall.** A key that loads is kept: Install.ps1 (from any bundle,
  also a new one) says `kept this machine's identity <kid>`.
  `setup.cmd uninstall` without `-Purge` keeps the key, so
  uninstalling and installing again keeps the identity; `-Purge` deletes it,
  and the next install is a first install.
- **Lost key.** No key although a publisher was installed here before, or a
  key that does not load: the pre-checks show it (*identity*, a WARN), then
  Install.ps1 warns *This replaces &lt;alias&gt;'s identity. The viewer will
  hold back its values until you accept the new key there.* and asks for the
  alias; without it nothing is installed. Restore the key from a backup and
  run `setup.cmd` again, or confirm a new identity with
  `setup.cmd -ConfirmAlias <alias>`. A key that does not load is kept as
  `publisher.key.bak-<time>`.
- **Rotation.** `setup.cmd rotate-key` (`Install.ps1 -RotateKey`), from the
  bundle folder or `%LOCALAPPDATA%\cc-usage\bundle`, warns the same way and
  asks for the alias, or takes `-ConfirmAlias <alias>`. It needs an install
  (the publisher manifest; on a PC with the app, the app's own key is never
  touched). It runs `cc-publish keygen --rotate --config <publish.json>
  --confirm <alias>` (a new key in front of the old one; `--force`, after a
  backup, when the key does not load; a plain keygen when it is missing),
  records the new key's hash in the manifest, restarts the publisher the
  way it was installed (task or Run entry), prints the new public key line
  and runs Verify. Until the new key is accepted on the widget machine, its
  values are held back there.

## Parameters (tests and unusual setups)

Install.ps1, Verify.ps1 and Uninstall.ps1 take `-Prefix` (install and data
folder, default `%LOCALAPPDATA%\cc-usage`), `-ClaudeSettings` (default
`%CLAUDE_CONFIG_DIR%\settings.json` when that is set, else
`%USERPROFILE%\.claude\settings.json`, the rule cc-hook follows), `-TaskName` (default `cc-usage
publisher`; also the Run entry's name) and `-SkipAppCheck`. Verify and
Uninstall default to what the installed copy and the manifest recorded.
Install.ps1 also takes `-RotateKey` and `-ConfirmAlias <alias>` (see
[Identity](#identity)). `-SkipAppCheck` exists for one reason: the app check refuses on the machine
that builds bundles, because that machine has the app. Use it only with the
other three pointing away from the real install. With a `-Prefix` other than
the default, Claude Code's hooks keep writing to the default folder unless
Claude Code runs with `CC_USAGE_DATA_DIR` set to the prefix.

## What the bundle expects from cc-publish

The Linux contract ([publisher-linux.md](publisher-linux.md#what-the-bundle-expects-from-cc-publish))
plus:

| Command | Behaviour |
|---|---|
| `cc-publish --data-dir DIR <command>` | DIR is the data directory, overriding `CC_USAGE_DATA_DIR` (a task cannot set environment variables) |
| `cc-publish stop [--wait SECS]` | writes `<data dir>/cc-publish.stop` naming the live publisher's pid; the run loop checks it every second and stops like on Ctrl-C; exit 0 when it stopped or none was running, 1 when it did not stop within the wait (default 15 s) |
| `run` on Windows | also stops on console close, logoff and shutdown events; a state file whose pid has exited does not block a new run (no 30 s wait after a forced kill) |

## Testing

`tools\publisher-bundle-windows\test\Test-BundleScripts.ps1` tests, without
installing, registering a task or writing the registry (a temp folder only):
`Install-PrivateFile` writes the secret only into
a folder that is already private (checked at the moment of the copy through a
`Copy-Item` proxy), ends private, leaves no staging folder, backs up a
different file; `Get-ConfigProblem` catches every config error the real
`cc-publish.exe status` prints (too large, unreadable, invalid JSON, unknown
member, bad alias); `setup.cmd`, run against stub scripts, passes its
arguments to Install.ps1 and Verify.ps1, only to Uninstall.ps1 for
`uninstall`, and runs `Install.ps1 -RotateKey` then Verify for
`rotate-key`; Verify.ps1 accepts every Install.ps1 parameter; the identity
helpers (the warning, the confirmation with `-ConfirmAlias`, a wrong alias
and no console, the plan for a first install, a lost key, a kept key and one
that does not load) and, with a `cc-publish.exe` that knows `keygen
--confirm` (release, else debug, or `-PublishExe`), `Install.ps1 -RotateKey`
on a fake install whose manifest records no autostart: refused without or
with a wrong alias (nothing changes), then a rotation, a key that does not
load and a missing key, each confirmed; and the install transaction, with
every scheduled task cmdlet, the Run entry helpers, `Start-Process` and
`Wait-PublisherStart` replaced by stubs (no real task or registry value is
read or written): a rollback after the files and the autostart changed
(previous binaries, config, private creds, `settings.json` and tooling
back, new files removed, the key pair kept and recorded in the manifest,
the task registered again from its XML, enabled and started), a rollback
before the autostart step (the task only enabled again), a task the user
disabled (left disabled, not started), a Run entry install (entry back, the
new task removed), a failing rollback step (reported, the rest done, the
copies kept and marked in `ROLLBACK-INCOMPLETE.txt` with where each one
goes, the files that were new and what failed; the next install refusing
to start over them; a leftover folder without the marker deleted, and the
marked one deleted only by `Remove-StaleRollback -All`, as Uninstall.ps1
calls it), a failed first install (only the key pair left) and a
transaction that cannot be prepared (no copies left). The checks that need
a built `cc-publish.exe` are skipped without one, except with
`CC_TEST_REQUIRE_ALL=1`, where each skip is a failure. Run it with Windows
PowerShell 5.1 and PowerShell 7:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\publisher-bundle-windows\test\Test-BundleScripts.ps1
pwsh -NoProfile -File tools\publisher-bundle-windows\test\Test-BundleScripts.ps1
```

What needs a real install (a test install beside the app, the real
defaults on a PC without it, a second PC) is in the
[checks before a release](development.md#before-a-release).

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| "Windows protected your PC" on `setup.cmd` | SmartScreen: the file came from the internet. Unblock the zip before extracting, or *More info*, *Run anyway*. |
| "running scripts is disabled" / "not digitally signed" | A Group Policy sets the execution policy (MachinePolicy or UserPolicy), which beats `-ExecutionPolicy Bypass`. `Get-ExecutionPolicy -List` shows it. RemoteSigned works once the files are unblocked; AllSigned needs IT. |
| "runs in ConstrainedLanguage mode" | AppLocker or WDAC in enforce mode; the scripts need full language mode. Ask IT to allow the bundle's scripts. |
| *--version* FAIL, "does not start" | AppLocker/WDAC blocks executables in `%LOCALAPPDATA%`, or the antivirus quarantined them. Ask IT to allow `%LOCALAPPDATA%\cc-usage\*.exe`. |
| Install refuses: *cc-usage app* | That machine already shows its own sessions in the app; nothing to install. |
| *autostart* is a Run entry | Task creation is blocked by policy. It works, but a publisher that stops stays stopped until the next logon or `Install.ps1`. |
| *autostart* FAIL, Run entry disabled | Task Manager, Startup apps: enable *cc-usage publisher*. |
| *cc-publish check* FAIL | Outbound connections to the `servers` in `publish.json` blocked or broken by a proxy or TLS inspection (by default `wss://connect.ngs.global:443`, and `tls://...:4222` too). cc-publish connects directly and does not use the system proxy, so the network must allow it. Or the `bucket` is not the viewer's, the creds were re-issued since the bundle was made (new bundle), or the connection limit is reached. |
| Install refuses: *earlier rollback* | An earlier install failed and its rollback did not complete; its copies are in the folder named. Put back what you need, move the folder out of the install folder, then run `setup.cmd` again (see [When a rollback does not complete](#when-a-rollback-does-not-complete)). |
| *recent publish* FAIL | `%LOCALAPPDATA%\cc-usage\cc-publish.log`; usually the same causes as the check. |
| Remote rows never appear in the widget | the bundle seals to a `viewer.pub` that is not the widget's: made on another machine without `-ViewerPub`, or with an old copy. Generate it on the widget's machine, or pass `-ViewerPub` with a current copy of the widget's `keys\viewer.pub`. |
| The widget holds back the PC's values (its flyout asks to accept a new key for the alias) | the PC seals with a key the widget has not accepted for its alias: a new identity (`setup.cmd rotate-key`, a confirmed new key, or `-Purge` and a new install). Accept the new key on the widget machine, comparing it with the line Install.ps1 printed. |
| *identity* WARN: `keys\publisher.key` missing although installed, or does not load | the PC's identity is lost: restore the key from a backup and run `setup.cmd` again, or make a new identity on purpose (`setup.cmd -ConfirmAlias <alias>`; see [Identity](#identity)). |
| A console window appears at logon | Windows older than 1809 (no `--headless`), or the Run entry edited by hand. Install.ps1 again on Windows 10 1809+ or 11. |
| Hooks FAIL after a Claude Code update rewrote `settings.json` | Run Install.ps1 (or `setup.cmd`) again. |
