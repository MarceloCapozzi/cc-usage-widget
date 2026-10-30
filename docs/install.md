# Clean install

Every step, from nothing, for one Windows viewer and any number of Linux and
Windows publishers. The details behind each step are in the linked pages.

| Role | What it is |
|---|---|
| **Build PC** | Windows, with this repository, the build tools and access to the NATS account. Makes the installer and the bundles. It may be the viewer itself. |
| **Viewer** | Windows, runs the cc-usage app. The only machine that decrypts and shows the sessions. |
| **Publisher** | Linux or Windows, without the app. Sends its sessions, encrypted, to the viewer. |

A clean install gives the viewer a new key pair and every publisher a new
identity: each publisher needs a new bundle, and the viewer asks once to
accept each one.

## 0. Remove a previous install

Skip this on machines that never had cc-usage.

**Viewer.** Windows Settings → Apps → cc-usage → Uninstall, tick *Delete the
application data*. Then:

```powershell
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\cc-usage"
```

**Linux publisher.**

```sh
make -C ~/.local/share/cc-usage/bundle uninstall PURGE=1
rm -rf ~/.local/share/cc-usage
```

**Windows publisher.**

```bat
"%LOCALAPPDATA%\cc-usage\bundle\setup.cmd" uninstall -Purge
```

## 1. Build PC, once

1. Install the prerequisites in [README.md](../README.md#build): Rust
   1.89+, Visual Studio Build Tools (C++), the Tauri CLI, PowerShell 7, and
   Node 22+ for the tests.
2. Clone and prepare. Keep the build folder outside OneDrive:

   ```powershell
   git clone https://github.com/MarceloCapozzi/cc-usage-widget.git cc-usage; cd cc-usage
   Copy-Item .cargo\config.example.toml .cargo\config.toml   # set target-dir
   pwsh -File tools\stage-cc-hook.ps1
   ```

3. Linux publishers only, the cross toolchain:

   ```powershell
   pwsh -File tools\cross\setup.ps1
   ```

4. The NATS project file. Copy the template and set `account`:

   ```powershell
   Copy-Item infra\nats\cc-sessions.example.yaml infra\nats\cc-sessions.yaml
   ```

   Provisioning uses toolctl and the Synadia token, in `.env` (see
   `.env.example`) or in toolctl's credential store. Without toolctl, create
   the bucket and the users [by hand](remote.md#by-hand) and pass
   `-SkipProvision` to every bundle command below.

## 2. Build the installer

```powershell
cd app; cargo tauri build; cd ..
```

The installer is
`<target-dir>\release\bundle\nsis\cc-usage_<version>_x64-setup.exe`.

## 3. Viewer

1. Copy the installer to the viewer and run it. No administrator rights are
   needed. Claude Code must be installed and logged in there.
2. Start the app once. It creates the viewer key pair in
   `%LOCALAPPDATA%\cc-usage\keys`. Check Settings → *This machine's name*:
   it must differ from every publisher alias.
3. The viewer's NATS credentials. On the build PC:

   ```powershell
   toolctl apply -f infra\nats\cc-sessions.yaml --no-creds
   toolctl creds cc-sessions.viewer
   ```

   This writes `%LOCALAPPDATA%\cc-usage\nats\cc-sessions.viewer.creds`. If the
   build PC is not the viewer, move that file to the same path on the viewer
   over a private channel (your USB stick, a private share), then delete it
   from the build PC. Within 30 seconds the footer's Remote line shows
`● connected`.
4. If the build PC is not the viewer, copy the viewer's
   `%LOCALAPPDATA%\cc-usage\keys\viewer.pub` to `pub\viewer.pub` in the
   repository on the build PC. It is public, and git ignores it.

## 4. Each publisher

Pick an alias for each machine: lowercase letters, digits and dashes. On the
build PC, make the bundle. Leave out `-ViewerPub` when the build PC is the
viewer.

```powershell
pwsh -File tools\make-publisher-bundle.ps1 -Alias <alias> -ViewerPub pub\viewer.pub                     # Linux
pwsh -File tools\make-publisher-bundle.ps1 -Alias <alias> -Platform windows -ViewerPub pub\viewer.pub   # Windows
```

The bundle lands in `%LOCALAPPDATA%\cc-usage\bundles`. It holds the
machine's NATS credentials, so move it over a private channel, never by
e-mail or chat. Compare the SHA-256 the script printed.

**Linux**, as the user who runs Claude Code, inside `$HOME`, not with
`sudo`:

```sh
tar -xzf cc-usage-publisher-<alias>.tar.gz && cd cc-usage-publisher-<alias>
make install && make verify
```

**Windows:** right-click the `.zip` → Properties → **Unblock**, extract it
under your profile, then double-click `setup.cmd`.

Both print the machine's public key line and end with a PASS/FAIL table.
Delete the extracted folder and the archive once it passes.

## 5. Accept each publisher on the viewer

With the publisher's first value, the viewer's flyout shows "New machine
`<alias>` wants to publish · key `<kid>` `<fingerprint>`". Compare the key id
and the fingerprint with the line the install printed, then click
**Accept**. The machine's sessions show from its next write.

## 6. Back up the viewer

Settings → Data: tick **Include keys and credentials**, type a password
twice, **Back up…**. Keep the `.ccbak` file off the viewer and the password
apart from it. It is the only way to move the viewer later without
reinstalling every publisher ([Moving the viewer](remote.md#moving-the-viewer-to-another-pc)).

## Later

| Task | How |
|---|---|
| Update the viewer | run the new installer over the old one |
| Update a Linux publisher | `make-publisher-bundle.ps1 -Update -ViewerPub pub\viewer.pub`, then `sh cc-usage-publisher-update-<version>-<arch>/update.sh` on the machine ([details](publisher-linux.md#5-updating)) |
| Update a Windows publisher | a new bundle for the same alias, then `setup.cmd` (it keeps the identity, so nothing to accept) |
| Add a machine | steps 4 and 5 |
| A machine with the app as a publisher | [remote.md](remote.md#a-pc-with-the-app-as-a-publisher) |
| Other servers or bucket | Settings → Remote connection on the viewer, `-Servers` and `-Bucket` on the bundles ([Connection settings](remote.md#connection-settings)) |
