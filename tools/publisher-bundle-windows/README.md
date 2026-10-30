# cc-usage publisher bundle for Windows

This folder makes a Windows PC without the cc-usage app publish its Claude
Code sessions, encrypted, to the cc-usage widget on another machine. It holds
everything needed: `bin\cc-hook.exe` and `bin\cc-publish.exe`, the NATS
credentials (`secrets\`), the widget machine's public key and the config
(`config\`), and the scripts. Values are sealed to the widget machine's key:
only it can read them. There is no private key in it: this PC makes its own
key pair, its identity, on the first install and keeps it (see
[Identity](#identity)).

**It contains a secret, the creds.** Copy it over a private channel (your own USB stick,
a private network share, scp) and delete the folder and the `.zip` once
Verify passes.

## Install

1. If you got a `.zip`: right-click it, Properties, tick **Unblock**, OK.
   Then extract it (into Downloads or Documents is fine, not a shared folder).
2. Double-click **setup.cmd** in the extracted folder.

It installs, then prints a PASS/FAIL table, and waits for a keypress. No
administrator rights and no reboot are needed. From a terminal:

```bat
setup.cmd
```

`setup.cmd` first checks every file against `SHA256SUMS` and stops, without
running anything, if one differs. Then it runs `Install.ps1`, then
`Verify.ps1`, with `powershell -NoProfile -ExecutionPolicy Bypass -File`.
Install:

- checks the machine (Windows 10 1809+ or 11, x64, Claude Code present,
  checksums, the binaries start) and **refuses if the cc-usage app is
  installed**: that machine already shows its own sessions, and the app and
  the publisher would compete for the same spool;
- copies the binaries to `%LOCALAPPDATA%\cc-usage`, the creds to
  `%LOCALAPPDATA%\cc-usage\nats\` and `viewer.pub` to
  `%LOCALAPPDATA%\cc-usage\keys\`, readable only by you (and SYSTEM);
- on the first install, makes this PC's key pair, `keys\publisher.key`
  (the private key never leaves the PC), and prints its public key line. On
  the widget machine, accept that key when cc-usage asks (compare the key
  id and fingerprint it prints). A later install keeps it (`kept this machine's identity <kid>`);
- registers the hooks and chains the status line in
  `%USERPROFILE%\.claude\settings.json`, or in `settings.json` in
  `%CLAUDE_CONFIG_DIR%` when you set that for Claude Code (`cc-hook install`,
  with a backup);
- registers the scheduled task **cc-usage publisher** (at logon, checked every
  5 minutes, no window) and starts it. If your company does not let you create
  tasks, it uses a Run entry instead (`HKCU\...\CurrentVersion\Run`).

Sessions you already have open start reporting fine-grained states after you
restart them (the hooks load when a session starts).

Then delete the bundle folder and the `.zip`. A copy of the scripts, without
binaries or secrets, stays in `%LOCALAPPDATA%\cc-usage\bundle`.

## Everyday commands

```bat
rem Remove the publisher (keeps creds and keys, so a reinstall keeps the identity):
"%LOCALAPPDATA%\cc-usage\bundle\setup.cmd" uninstall
rem Remove everything, secrets included:
"%LOCALAPPDATA%\cc-usage\bundle\setup.cmd" uninstall -Purge
rem Replace this PC's identity (its key pair), on purpose:
"%LOCALAPPDATA%\cc-usage\bundle\setup.cmd" rotate-key
rem Check the install:
powershell -NoProfile -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\cc-usage\bundle\Verify.ps1"
rem Publisher state:
"%LOCALAPPDATA%\cc-usage\cc-publish.exe" status
rem Stop it until the next logon (or the task's next 5-minute check):
"%LOCALAPPDATA%\cc-usage\cc-publish.exe" stop
```

The log is `%LOCALAPPDATA%\cc-usage\cc-publish.log`.

## Updating

Ask for a new bundle for the same machine and run its `setup.cmd` the same
way. It stops the publisher, replaces the binaries, keeps this PC's key pair
and the hooks, restarts the publisher and verifies again. Nothing changes on
the widget machine. If the update fails, it puts the previous version back,
with its scheduled task enabled, and starts it again; the error says so. If
part of that fails, the copies of the previous files stay in a
`%LOCALAPPDATA%\cc-usage\.cc-usage-rollback-<number>` folder, with
`ROLLBACK-INCOMPLETE.txt` saying where each one goes, and the next
`setup.cmd` refuses to run until you have put back what you need and moved
that folder out of `%LOCALAPPDATA%\cc-usage` (delete it once done: it holds
copies of the creds). `setup.cmd uninstall` deletes it too.

## Identity

This PC's key pair, `%LOCALAPPDATA%\cc-usage\keys\publisher.key`, is how the
widget machine recognises it: every value is sealed with it, and a value from
another key for the same alias is held back there until accepted. So it is made here once
and kept:

- `setup.cmd` makes it on the first install and keeps it on every later one,
  also from a new bundle. `setup.cmd uninstall`
  (without `-Purge`) keeps it too.
- If it is missing although a publisher was installed here before (its
  `publisher-manifest.json` or `publish.json` is there), or it does not
  load, the pre-checks say so and the install asks you to type the alias;
  without it, nothing is installed. Restore the key from a backup and run
  `setup.cmd` again, or confirm a new identity on purpose:
  `setup.cmd -ConfirmAlias <alias>`.
- `setup.cmd rotate-key` (`Install.ps1 -RotateKey`) replaces it on purpose,
  from the bundle folder or from `%LOCALAPPDATA%\cc-usage\bundle`. It warns
  *This replaces &lt;alias&gt;'s identity. The viewer will hold back its values
  until you accept the new key there.* and asks you to type the alias;
  `setup.cmd rotate-key -ConfirmAlias <alias>` confirms without the prompt.
  It adds a new key in front of the old one (`cc-publish keygen --rotate`; a
  key that does not load is replaced, with a backup), updates the manifest,
  restarts the publisher, prints the new public key line and runs Verify.

After a new identity, accept its key on the widget machine when cc-usage
asks; until then its values from this PC are held back.

## Troubleshooting

| Symptom | What to do |
|---|---|
| "Windows protected your PC" (SmartScreen) on setup.cmd | The file came from the internet. Unblock the `.zip` before extracting (step 1), or click *More info*, *Run anyway*. |
| "running scripts is disabled" or "is not digitally signed" | A Group Policy sets the PowerShell execution policy, which overrides `-ExecutionPolicy Bypass`. Ask IT to allow the scripts, or to run them for you. |
| "runs in ConstrainedLanguage mode" | AppLocker or WDAC restricts PowerShell. Ask IT to allow the bundle's scripts and `%LOCALAPPDATA%\cc-usage\*.exe`. |
| `cc-hook.exe` / `cc-publish.exe` *does not start* | AppLocker, WDAC or the antivirus blocks programs in `%LOCALAPPDATA%`. Ask IT to allow them. |
| Install refuses: *cc-usage app installed* | Nothing to do: the app already shows this machine's sessions. |
| *autostart* uses a Run entry | Task creation is blocked by policy. The Run entry starts the publisher at logon but does not restart it if it stops: log off and on, or run Install.ps1 again. |
| *cc-publish check* FAIL | The network blocks the NATS servers in `%LOCALAPPDATA%\cc-usage\publish.json` (by default WebSocket on port 443 to `connect.ngs.global`, and port 4222 too), for example a proxy or TLS inspection. Ask IT to allow them (by default `wss://connect.ngs.global`). |
| Install refuses: *earlier rollback* | An earlier update failed and could not put everything back. Follow the message: put back what you need from the folder it names, move that folder out of `%LOCALAPPDATA%\cc-usage`, and run `setup.cmd` again. |
| *recent publish* FAIL | Read `%LOCALAPPDATA%\cc-usage\cc-publish.log`; usually the same as the check. |
| "does not match its SHA256SUMS" | The copy is incomplete or was changed. Copy the bundle again. |
| *identity*: `publisher.key` is missing or does not load | Restore it from a backup and run `setup.cmd` again, or make a new identity on purpose (see [Identity](#identity)). |
| The widget holds back this PC's values | Its key changed: accept the new key there, comparing it with the line the install or `rotate-key` printed. |
