<#
.SYNOPSIS
    Builds the self-contained Linux (default) or Windows publisher bundle for
    one machine alias.

.DESCRIPTION
    -Platform windows builds the Windows bundle instead, for a Windows PC
    without the cc-usage app: <Out>\cc-usage-publisher-<alias>-windows\ and a
    .zip of it, with bin\cc-hook.exe, bin\cc-publish.exe, the same creds
    and viewer.pub, config\publish.json and config\bundle.json, setup.cmd,
    Install.ps1, Verify.ps1, Uninstall.ps1, scripts\common.ps1, README.md and
    SHA256SUMS (sources in tools\publisher-bundle-windows; see
    docs\publisher-windows.md). The rest of this description is about the
    Linux bundle; provisioning, the keys and the output folder rules are the
    same for both.

    -Platform app writes the files for a PC that HAS the cc-usage app and is
    to publish with it (Settings > Role: Publisher), instead of a bundle:
    <Out>\cc-usage-publisher-<alias>-app\ (private ACL, no archive) with the
    NATS credentials of cc-sessions.pub-<alias> (provisioned as below) and
    this viewer's viewer.pub. No binaries and no publisher private key: the
    app makes its own key on that PC (Generate key), and this viewer asks to
    accept its public key with the first value it receives. The steps to
    follow there are printed at the end (docs\remote.md).

    Produces <Out>\cc-usage-publisher-<alias>\ and a .tar.gz of it, holding
    everything a Linux machine needs to publish its Claude Code sessions to
    the cc-usage widget on this machine:

        bin/cc-hook, bin/cc-publish     static Linux binaries (musl)
        secrets/<user>.creds            NATS credentials of cc-sessions.pub-<alias>
        config/viewer.pub               this viewer's public key, sealed to
        config/publish.json             cc-publish's config: alias, servers and
                                        bucket (-Servers, -Bucket: the viewer's
                                        by default), creds and key files
                                        relative to PREFIX
        config/bundle.env               alias, NATS user, architecture, version
        Makefile, scripts/, systemd/    install, verify, uninstall on the Linux side
        README.md, SHA256SUMS

    Steps: validate the alias; check the bundle sources (Makefile tabs, LF line
    endings, `sh -n` of every script when a POSIX shell is available); build
    the Linux binaries with tools\cross\build-linux.ps1 (or use built ones);
    provision the NATS user with toolctl (see below); the keys (below);
    assemble the folder and the archive.

    Keys (docs\data-and-privacy.md): this machine is the viewer
    (or -ViewerPub names the viewer's public key). A Windows cc-publish.exe
    built here (the Windows bundle's own, or `cargo build --release -p
    cc-publish`) runs `viewer-keygen --if-missing` on this machine's data
    dir, so keys\viewer.key and keys\viewer.pub exist (the app creates the
    same files on its first start); viewer.pub goes into the bundle. No
    publisher key is made or pinned here: a publisher's key pair is its
    identity and is born on the machine. make install (Install.ps1 on
    Windows) creates it on the first install, prints its public key line and
    keeps it on every reinstall and update; the viewer asks to accept that
    key with the first value it receives. Only public keys are printed.

    Provisioning is declarative: infra\nats\cc-sessions.yaml (a local copy
    of cc-sessions.example.yaml, ignored by git) lists the project's bucket
    and users. The script runs
        toolctl add-publisher <alias> -f infra\nats\cc-sessions.yaml
        toolctl apply -f infra\nats\cc-sessions.yaml --no-creds
        toolctl creds cc-sessions.pub-<alias> -o <CredsDir>\cc-sessions.pub-<alias>.creds
    add-publisher appends the publisher to the local file unless it is
    already there; apply never prunes. toolctl is the maintainer's internal
    tool, kept in a separate private repository: -Toolctl, else
    $env:TOOLCTL, else toolctl on PATH, else built with go from a checkout
    at ..\tooling-ctl into %LOCALAPPDATA%\toolctl\bin. Without toolctl,
    create the user and its creds by hand (docs\remote.md, "By hand") and
    pass -SkipProvision. The Synadia token comes from
    SYNADIA_CLOUD_ACCESS_TOKEN in the environment, else from the
    repository's .env (passed to toolctl's process only), else from
    toolctl's OS credential store (toolctl auth login).

    The bundle contains a secret, the creds. It is written outside OneDrive by default,
    and the archive stores every file as 0600 and every directory as 0700, so
    extraction on Linux never makes the secrets readable by other users. It
    stores owner and group 0 without names: a normal (non-root) tar gives the
    files to whoever extracts them. Extracted with sudo they would belong to
    root; make install refuses such a folder and says how to fix it. No
    secret is ever printed.

.PARAMETER Update
    Linux only: build an update package instead of a bundle, for machines
    that already ran make install: <Out>\cc-usage-publisher-update-<version>-<arch>\
    and a .tar.gz of it, with bin/cc-hook, bin/cc-publish, update.sh,
    viewer.pub (this viewer's public key; the viewer key pair is created
    here when missing), bundle/ (the tooling copy for <prefix>/bundle) and
    SHA256SUMS. No secrets, no provisioning, no alias: the same package
    updates every Linux machine. On the machine: tar -xzf it, then
    sh <folder>/update.sh (replaces the binaries and the ~/.profile
    launcher, installs viewer.pub, keeps the machine's own key pair,
    registers the hooks again for the new event list, restarts the
    publisher and runs make verify; secrets, config and service stay). A machine without a
    key gets a new one only when confirmed (CONFIRM_ALIAS=<alias>); its
    public key line is printed and the viewer asks to accept it.

.PARAMETER ViewerPub
    The viewer's public key file, when the viewer is another PC: a copy of
    keys\viewer.pub from %LOCALAPPDATA%\cc-usage on the viewer (public, any
    channel will do; compare the printed lines with the viewer's file). The
    bundle or package seals to it; no key is generated on this machine.
    Without it the viewer key pair is this machine's, which must then be
    the viewer: a PC in the app's Publisher role is refused.
.PARAMETER Alias
    Machine alias (not used with -Update): 1 to 31 lowercase letters, digits or dashes, starting with
    a letter or digit, no dots. Becomes the NATS user cc-sessions.pub-<alias>
    and the tag of the machine's rows in the widget.

.PARAMETER Platform
    linux (default), windows, or app (a PC with the cc-usage app in the
    Publisher role: creds and viewer.pub only).

.PARAMETER Target
    x86_64 (default) or aarch64. For Windows, aarch64 needs the Rust target
    aarch64-pc-windows-msvc and the MSVC ARM64 build tools; without them use
    x86_64, which Windows on ARM runs under emulation.

.PARAMETER Out
    Output folder; a relative path is relative to the current location.
    Default: %LOCALAPPDATA%\cc-usage\bundles (private to the user, not
    synced). A folder inside this repository is always refused; one inside
    OneDrive is refused unless -AllowSyncedFolder is given. The bundle folder
    and its archive get an ACL for the current user and SYSTEM only (no
    inherited entries, nobody else's explicit ones); the script stops when
    that cannot be set.

.PARAMETER SkipBuild
    Do not build; use the binaries already in -BinDir.

.PARAMETER BinDir
    Where the binaries are. Default: <target dir>\<triple>\release (Linux),
    <target dir>\release (Windows x86_64 on an x86_64 host).

.PARAMETER SkipProvision
    Do not run toolctl; the creds file must already exist in -CredsDir.

.PARAMETER Toolctl
    Path of toolctl.exe. Default: $env:TOOLCTL, else toolctl on PATH, else
    built from a checkout of the internal tool at ..\tooling-ctl (needs go).

.PARAMETER ProjectFile
    The NATS project file. Default: infra\nats\cc-sessions.yaml of this
    repository, the deployment's own copy of
    infra\nats\cc-sessions.example.yaml (ignored by git, since it lists the
    real machine aliases); the script stops with that hint when it is
    missing. It must declare the project cc-sessions (the NATS users are
    cc-sessions.viewer and cc-sessions.pub-<alias>) and the bucket of
    -Bucket. Not needed with -SkipProvision or -Update.

.PARAMETER Servers
    The NATS servers the publisher connects to, written to publish.json:
    tls:// or wss:// URLs with a host and an optional port (tls://host:4222,
    wss://host is port 443), in the order to try them; one argument with
    commas works too. Default: the viewer's own, as the app on this machine
    resolves them (CC_USAGE_NATS_SERVERS, else "nats": {"servers"} in its
    settings.json, %APPDATA%\ar.cc.usage or CC_USAGE_CONFIG_DIR, read
    only), else wss://connect.ngs.global then tls://connect.ngs.global. A
    viewer setting the app would ignore is ignored here too, with a warning.
    Not used with -Update (an installed publisher keeps its publish.json).

.PARAMETER Bucket
    The KV bucket, written to publish.json: 1 to 64 letters, digits, '-' or
    '_'. Default: the viewer's (CC_USAGE_NATS_BUCKET, else "nats":
    {"bucket"} in its settings.json), else cc-sessions.

.PARAMETER CredsDir
    Where the publisher's creds file is written and read:
    %LOCALAPPDATA%\cc-usage\nats. The file is made private (current user
    and SYSTEM only), like the bundle's copy.

.PARAMETER NoArchive
    Do not write the .tar.gz (the .zip for Windows).

.EXAMPLE
    pwsh -File tools\make-publisher-bundle.ps1 -Alias lab-01
    pwsh -File tools\make-publisher-bundle.ps1 -Alias pi-4 -Target aarch64
    pwsh -File tools\make-publisher-bundle.ps1 -Alias office-pc -Platform windows
    pwsh -File tools\make-publisher-bundle.ps1 -Alias old-viewer -Platform app
    pwsh -File tools\make-publisher-bundle.ps1 -Update
    pwsh -File tools\make-publisher-bundle.ps1 -Update -ViewerPub D:\viewer.pub
    pwsh -File tools\make-publisher-bundle.ps1 -Alias office-pc -Platform windows -ViewerPub D:\viewer.pub
    pwsh -File tools\make-publisher-bundle.ps1 -Alias devbox -Servers tls://nats.example:4222,wss://nats.example -Bucket cc-sessions
#>
[CmdletBinding()]
param(
    [string]$Alias,
    [switch]$Update,
    [ValidateSet('linux', 'windows', 'app')][string]$Platform = 'linux',
    [ValidateSet('x86_64', 'aarch64')][string]$Target = 'x86_64',
    [string]$Out,
    [switch]$SkipBuild,
    [string]$BinDir,
    [switch]$SkipProvision,
    [string]$Toolctl,
    [string]$ProjectFile,
    [string[]]$Servers,
    [string]$Bucket,
    [string]$CredsDir,
    [switch]$AllowSyncedFolder,
    [string]$ViewerPub,
    [switch]$NoArchive
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\cross\common.ps1"

# The NATS project, cc_core::PROJECT: the users are <project>.viewer and
# <project>.pub-<alias>, fixed in the code (the servers and the bucket are
# not; see -Servers and -Bucket). The project file must declare it.
$Project = 'cc-sessions'
$SourceDir = Join-Path $PSScriptRoot 'publisher-bundle'
$LocalAppData = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { Join-Path $env:USERPROFILE 'AppData\Local' }
if (-not $Out) { $Out = Join-Path $LocalAppData 'cc-usage\bundles' }
if (-not $ProjectFile) { $ProjectFile = Join-Path $RepoRoot 'infra\nats\cc-sessions.yaml' }
if (-not $CredsDir) { $CredsDir = Join-Path $LocalAppData 'cc-usage\nats' }
$DataDir = if ($env:CC_USAGE_DATA_DIR) { $env:CC_USAGE_DATA_DIR } else { Join-Path $LocalAppData 'cc-usage' }
$Utf8 = New-Object System.Text.UTF8Encoding($false)

# The functions are in publisher-bundle-gen\, dot-sourced here so they run in
# this script's scope and see its parameters and the variables above and
# below. Inside them $PSScriptRoot is their own folder, hence $ToolsDir.
$ToolsDir = $PSScriptRoot
. "$PSScriptRoot\publisher-bundle-gen\common.ps1"
. "$PSScriptRoot\publisher-bundle-gen\provision.ps1"
. "$PSScriptRoot\publisher-bundle-gen\keys.ps1"
. "$PSScriptRoot\publisher-bundle-gen\windows.ps1"
. "$PSScriptRoot\publisher-bundle-gen\app.ps1"
. "$PSScriptRoot\publisher-bundle-gen\linux.ps1"
. "$PSScriptRoot\publisher-bundle-gen\update.ps1"

# ---------------------------------------------------------------- 1. inputs

if ($Update) {
    if ($Platform -ne 'linux') { throw '-Update builds the Linux package only; a Windows PC gets a new bundle' }
    $Alias = 'update'
} elseif (-not $Alias) {
    throw 'pass -Alias <machine alias> (or -Update for an update package)'
}
Assert-Alias $Alias
$User = "$Project.pub-$Alias"
$CredsName = "$User.creds"
# Where the publisher connects: -Servers and -Bucket, else the viewer's.
$nats = Resolve-NatsTarget $Servers $Bucket (Get-ViewerSettingsPath)
foreach ($w in $nats.Warnings) { Write-Warn $w }
$Servers = $nats.Servers
$Bucket = $nats.Bucket
if (-not $Update) { Write-Step "NATS: bucket $Bucket over $($Servers -join ', ') ($($nats.Source))" }
# The project file, checked before anything is built: it is needed to
# provision, and must declare this project and bucket.
if (-not $Update -and -not $SkipProvision) {
    $projectInfo = Read-ProjectFile $ProjectFile (Join-Path $RepoRoot 'infra\nats\cc-sessions.example.yaml')
    if ($projectInfo.Project -ne $Project) {
        throw "$ProjectFile declares the project '$($projectInfo.Project)', but the code's NATS users are $Project.viewer and $Project.pub-<alias>; use a project file for '$Project'"
    }
    if ($projectInfo.Buckets -notcontains $Bucket) {
        throw "$ProjectFile declares the bucket(s) '$($projectInfo.Buckets -join ', ')', not '$Bucket'; pass -Bucket, or change the viewer's settings or the project file"
    }
}
# A relative -Out is relative to the current location (PowerShell's, which
# .NET's GetFullPath alone would not see).
$Out = [System.IO.Path]::GetFullPath($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out))
if (Test-InsideRepository $Out) {
    throw "$Out is inside the repository ($(Split-Path -Parent $PSScriptRoot)). A bundle holds secrets and must never be written where git could pick it up. Use another -Out (default: $LocalAppData\cc-usage\bundles)."
}
if ((Test-UnderSyncedFolder $Out) -and -not $AllowSyncedFolder) {
    throw "$Out is inside OneDrive, which would upload the bundle's secrets. Use another -Out (default: $LocalAppData\cc-usage\bundles) or pass -AllowSyncedFolder."
}
# Where the viewer's key comes from, checked before anything is built,
# provisioned or written.
if ($ViewerPub) {
    [void](Test-KeyFile (Assert-ViewerPubFile $ViewerPub))
} else {
    Assert-ViewerHere
}
if ($Platform -eq 'windows') {
    New-WindowsBundle
    return
}
if ($Platform -eq 'app') {
    New-AppFiles
    return
}
# Linux, the default: the bundle or, with -Update, the update package.
New-LinuxBundle
