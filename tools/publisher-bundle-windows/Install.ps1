<#
.SYNOPSIS
    Installs the cc-usage publisher on this Windows machine, without
    administrator rights.

.DESCRIPTION
    Run it from the extracted bundle folder (setup.cmd does, then runs
    Verify.ps1). It checks the machine first and stops on any FAIL, then:

      - copies cc-hook.exe and cc-publish.exe to %LOCALAPPDATA%\cc-usage
        (cc-hook.exe where the cc-usage app keeps it, so hook commands look
        the same on every machine);
      - copies the NATS credentials to %LOCALAPPDATA%\cc-usage\nats\ and the
        viewer's public key to keys\viewer.pub (only the viewer can
        decrypt), readable by the current user and SYSTEM only
        (inheritance off, checked afterwards);
      - keeps this machine's identity, its key pair in keys\publisher.key:
        made here on the first install (cc-publish keygen --if-missing; its
        public key line is printed, and the viewer asks to accept it) and
        kept by every later install. When the key is missing although a
        publisher was installed here before (its manifest or publish.json
        is there), or does not load, a new one is made only when confirmed
        (-ConfirmAlias, or the alias typed at the prompt); otherwise nothing
        is installed;
      - writes %LOCALAPPDATA%\cc-usage\publish.json;
      - runs `cc-hook.exe install`: the hooks and the status line chain in
        Claude Code's settings.json (in %CLAUDE_CONFIG_DIR% when set, else
        in %USERPROFILE%\.claude), with a timestamped backup;
      - registers the per-user scheduled task "cc-usage publisher" (logon
        trigger, repeated every 5 minutes, hidden: conhost --headless), or,
        when task registration is not allowed, an HKCU Run entry, and starts
        the publisher;
      - keeps a copy of these scripts, without binaries or secrets, in
        %LOCALAPPDATA%\cc-usage\bundle, and a manifest of what it installed.

    Idempotent: running it again upgrades in place and restarts the publisher.
    An upgrade is all or nothing: the files it replaces, the task and the Run
    entry are copied first, and if anything fails before the new publisher
    runs, they are put back, the task is enabled again and the previous
    publisher restarted (this machine's key pair is kept as it is). When
    part of that rollback fails, the copies stay in a .cc-usage-rollback-*
    folder of the install folder, with ROLLBACK-INCOMPLETE.txt saying where
    each one goes, and the next install refuses to run until that folder is
    moved away.

    -RotateKey (setup.cmd rotate-key) replaces this machine's identity on
    purpose instead, from the bundle folder or from the installed copy: it
    asks for the alias (or takes -ConfirmAlias), runs cc-publish keygen
    --rotate (--force after a backup when the key does not load, a plain
    keygen when it is missing), records the new key in the manifest,
    restarts the publisher and prints the new public key line, which the
    viewer must accept before it takes this machine's values again.

    The cc-usage app must not be installed here: the app and the publisher
    would drain the same spool, and a machine with the app does not need the
    publisher. Install refuses when it finds the app.

.PARAMETER Prefix
    Install folder, also the publisher's data folder. Default and normal
    value: %LOCALAPPDATA%\cc-usage. Another folder is for tests: Claude Code's
    hooks keep writing to the default folder unless Claude Code runs with
    CC_USAGE_DATA_DIR set to it.

.PARAMETER ClaudeSettings
    Claude Code settings file for the hooks. Default: settings.json in
    %CLAUDE_CONFIG_DIR% when that is set, else
    %USERPROFILE%\.claude\settings.json (the file Claude Code reads).

.PARAMETER TaskName
    Name of the scheduled task (and of the Run entry). Default: cc-usage publisher.

.PARAMETER Autostart
    Auto (default: the task, else the Run entry), Task or RunKey.

.PARAMETER SkipAppCheck
    For testing on a machine that has the cc-usage app, with -Prefix,
    -ClaudeSettings and -TaskName pointing away from the real ones. Never use
    it for a real install.

.PARAMETER Force
    Install even without Claude Code's folder (%CLAUDE_CONFIG_DIR%, else
    %USERPROFILE%\.claude; Claude Code not run yet).

.PARAMETER RotateKey
    Replace this machine's identity (its key pair) instead of installing;
    see the description. Asks for the alias unless -ConfirmAlias is given.

.PARAMETER ConfirmAlias
    This machine's alias, confirming a new identity without the prompt (for
    scripted use): with -RotateKey, or for an install over a lost key.
#>
[CmdletBinding()]
param(
    [string]$Prefix,
    [string]$ClaudeSettings,
    [string]$TaskName = 'cc-usage publisher',
    [ValidateSet('Auto', 'Task', 'RunKey')][string]$Autostart = 'Auto',
    [switch]$SkipAppCheck,
    [switch]$Force,
    [switch]$RotateKey,
    [string]$ConfirmAlias
)

if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') {
    Write-Host "ERROR: PowerShell runs in $($ExecutionContext.SessionState.LanguageMode) mode on this machine (AppLocker, WDAC or a device policy), which these scripts cannot run in. See Troubleshooting in README.md." -ForegroundColor Red
    exit 1
}
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0
. (Join-Path $PSScriptRoot 'scripts\common.ps1')

function Invoke-Install {
    $bundle = $PSScriptRoot
    $info = Get-BundleInfo $bundle
    if (Get-InstalledMarker $bundle) {
        throw "this is the installed copy in $bundle (no binaries or secrets); run Install.ps1 from the original bundle folder"
    }
    $alias = $info.alias
    $user = $info.nats_user
    $credsName = "$user.creds"
    $defaultPrefix = Get-DefaultPrefix
    $Prefix = Get-FullPath $(if ($Prefix) { $Prefix } else { $defaultPrefix })
    $ClaudeSettings = Get-FullPath $(if ($ClaudeSettings) { $ClaudeSettings } else { Get-DefaultSettings })
    Assert-UsablePath 'Prefix' $Prefix
    Assert-TaskName $TaskName

    Write-Step "installing the cc-usage publisher for '$alias' ($user, bundle $($info.version)) into $Prefix"

    # ---------------------------------------------------------- pre-checks
    # Integrity first: nothing of the bundle is unblocked or run before every
    # file matches SHA256SUMS and bin\ holds nothing it does not list (a DLL
    # dropped next to the binaries would be loaded when they run).
    $sums = Join-Path $bundle 'SHA256SUMS'
    $verified = $false
    if (Test-Path -LiteralPath $sums -PathType Leaf) {
        $bad = @(Test-Sums $bundle $sums)
        $bad += @(Get-UnlistedFiles $bundle $sums 'bin' | ForEach-Object { "$_ is not in SHA256SUMS" })
        if ($bad.Count -eq 0) {
            $verified = $true
            Add-Result PASS 'checksums' 'every file matches SHA256SUMS'
        } else {
            Add-Result FAIL 'checksums' "$(($bad | Select-Object -First 3) -join '; ') - copy the bundle again"
        }
    } else {
        Add-Result FAIL 'checksums' 'SHA256SUMS missing from the bundle'
    }
    if ($verified) {
        # Mark of the Web (a download, a zip extracted by Explorer): clear it,
        # now that the files are known to be the bundle's, so SmartScreen and
        # the execution policy leave them alone.
        Get-ChildItem -LiteralPath $bundle -Recurse -File | Unblock-File -ErrorAction SilentlyContinue
    }

    $os = [Environment]::OSVersion.Version
    if ($os.Major -eq 10 -and $os.Build -ge 17763) {
        $name = if ($os.Build -ge 22000) { 'Windows 11' } else { 'Windows 10' }
        Add-Result PASS 'Windows' "$name, build $($os.Build)"
    } else {
        Add-Result FAIL 'Windows' "version $os; needs Windows 10 1809 (build 17763) or later, or Windows 11 (headless console)"
    }
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    $barch = $info.arch
    if (($arch -eq 'AMD64' -and $barch -eq 'x86_64') -or ($arch -eq 'ARM64' -and $barch -eq 'aarch64')) {
        Add-Result PASS 'architecture' "$arch matches the bundle ($barch)"
    } elseif ($arch -eq 'ARM64' -and $barch -eq 'x86_64') {
        Add-Result WARN 'architecture' 'ARM64 Windows runs the x64 binaries under emulation'
    } else {
        Add-Result FAIL 'architecture' "this Windows is $arch, the bundle is $barch"
    }
    $principal = New-Object System.Security.Principal.WindowsPrincipal([System.Security.Principal.WindowsIdentity]::GetCurrent())
    if ($principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Add-Result WARN 'elevation' 'running as administrator is not needed; it installs for this user all the same'
    }
    $app = Get-AppInstall
    if ($app -and -not $SkipAppCheck) {
        Add-Result FAIL 'cc-usage app' "installed here ($app). The app already shows this machine's sessions and publishing would compete with it for the same spool; a machine with the app does not need the publisher."
    } elseif ($app) {
        Add-Result WARN 'cc-usage app' "installed here, check skipped (-SkipAppCheck: tests only)"
    } else {
        Add-Result PASS 'cc-usage app' 'not installed (good: the publisher replaces it on machines without it)'
    }
    $claudeDir = Split-Path -Parent $ClaudeSettings
    if (Test-Path -LiteralPath $claudeDir -PathType Container) {
        Add-Result PASS 'Claude Code profile' $claudeDir
    } elseif ($Force) {
        Add-Result WARN 'Claude Code profile' "$claudeDir missing (-Force); run claude once so the hooks have a settings file"
    } else {
        Add-Result FAIL 'Claude Code profile' "$claudeDir missing: install Claude Code and run it once (or pass -Force)"
    }
    # The publisher starts at sign-in with the user's saved environment, so a
    # CLAUDE_CONFIG_DIR set only in this shell would not reach it.
    if ($env:CLAUDE_CONFIG_DIR) {
        $saved = @('User', 'Machine' | ForEach-Object { [Environment]::GetEnvironmentVariable('CLAUDE_CONFIG_DIR', $_) } | Where-Object { $_ })
        if ($saved.Count -eq 0 -or $saved[0] -ne $env:CLAUDE_CONFIG_DIR) {
            $savedText = if ($saved.Count -eq 0) { 'none' } else { $saved[0] }
            Add-Result WARN 'Claude Code folder' "CLAUDE_CONFIG_DIR=$env:CLAUDE_CONFIG_DIR in this shell, but the saved environment has $savedText, which is what the publisher gets at sign-in; save it for the user (System Properties > Environment Variables) if Claude Code always runs with it"
        }
    }
    $claude = Get-Command claude -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($claude) {
        Add-Result PASS 'claude on PATH' $claude.Source
    } else {
        Add-Result WARN 'claude on PATH' "not found; cc-publish lists sessions with 'claude agents', so without it only hook events show"
    }
    foreach ($b in 'cc-hook.exe', 'cc-publish.exe') {
        $path = Join-Path $bundle "bin\$b"
        if (-not (Test-Path -LiteralPath $path)) { Add-Result FAIL $b "bin\$b missing from the bundle"; continue }
        if (-not $verified) { Add-Result SKIP $b 'not run: the bundle does not match SHA256SUMS'; continue }
        try {
            $r = Invoke-Exe $path @('--version')
            if ($r.Code -eq 0) { Add-Result PASS $b (Get-LastLine $r) }
            else { Add-Result FAIL $b "--version exited $($r.Code): $(Get-LastLine $r)" }
        } catch {
            Add-Result FAIL $b "does not start: $($_.Exception.Message) (blocked by AppLocker or antivirus?)"
        }
    }
    foreach ($s in "secrets\$credsName", 'config\publish.json', 'config\viewer.pub') {
        if (-not (Test-Path -LiteralPath (Join-Path $bundle $s))) { Add-Result FAIL 'bundle files' "$s missing" }
    }
    if (-not (Test-SamePath $Prefix $defaultPrefix)) {
        Add-Result WARN 'install folder' "$Prefix is not $defaultPrefix; Claude Code's hooks write there only when it runs with CC_USAGE_DATA_DIR=$Prefix"
    }
    $old = Join-Path $Prefix 'cc-publish.exe'
    if (Test-Path -LiteralPath $old) {
        $r = Invoke-Exe $old @('--version')
        Add-Result PASS 'existing install' "$(Get-LastLine $r) in $Prefix (upgraded in place)"
    }
    # The copies an incomplete rollback kept may be the only ones left of
    # the previous files; another install would bury them.
    foreach ($d in @(Get-IncompleteRollback $Prefix)) {
        Add-Result FAIL 'earlier rollback' (Get-IncompleteRollbackHelp $d)
    }
    # This machine's identity, decided before anything is changed (the
    # bundle's cc-publish.exe only reads an existing key, and only once the
    # bundle is known to be intact).
    $identity = $null
    if ($verified) {
        $identity = Get-IdentityPlan $Prefix (Join-Path $bundle 'bin\cc-publish.exe')
        switch ($identity.Action) {
            'keep' { Add-Result PASS 'identity' "this machine's key $(Get-Kid $identity.Line) is kept" }
            'first' { Add-Result PASS 'identity' 'first install: this machine makes its key pair' }
            'new' { Add-Result WARN 'identity' "$($identity.Key) is missing, but a publisher was installed here before: the viewer knows $alias by that key" }
            'replace' { Add-Result WARN 'identity' "$($identity.Key) does not load ($($identity.Detail))" }
        }
    }
    if ((Show-Results "Pre-checks ($alias, bundle $($info.version), $barch)") -gt 0) {
        throw 'pre-checks failed; nothing was installed'
    }
    if ($identity.Action -in 'new', 'replace') {
        $what = if ($identity.Action -eq 'new') { 'create a new identity on purpose' } else { 'replace it on purpose' }
        Confirm-IdentityChange $alias $ConfirmAlias (Test-CanPrompt) "Nothing was installed. Restore $($identity.Key) from a backup and run setup.cmd again, or $what`: setup.cmd -ConfirmAlias $alias"
    }
    Write-Host ''

    # ---------------------------------------------------------- transaction
    # From here on, a failure rolls everything back (Undo-InstallTransaction,
    # run by the top level): the previous files, task and Run entry come
    # back, the task is enabled again and the previous publisher restarted.
    # The copies are dropped once the new publisher runs.
    $existingManifest = Get-Manifest $Prefix
    $oldTask = [string](Get-Prop $existingManifest 'task_name' $TaskName)
    $nats = Join-Path $Prefix 'nats'
    $keys = Join-Path $Prefix 'keys'
    $copy = Join-Path $Prefix 'bundle'
    $credsPath = Join-Path $nats $credsName
    $publisherKeyPath = Join-Path $keys 'publisher.key'
    $viewerPubPath = Join-Path $keys 'viewer.pub'
    $configPath = Join-Path $Prefix 'publish.json'
    $saved = @((Join-Path $Prefix 'cc-hook.exe'), (Join-Path $Prefix 'cc-publish.exe'), $credsPath, $viewerPubPath, $configPath,
        (Join-Path $Prefix $ManifestName), (Join-Path $Prefix 'statusline-chain.json'), $ClaudeSettings, (Join-Path $copy $MarkerRel))
    $saved += @($ToolingFiles | ForEach-Object { Join-Path $copy $_ })
    # The key pair is never rolled back: a key made here may already be
    # accepted on the viewer, and the previous one, if any, is kept as it is.
    $script:InstallTx = New-InstallTransaction $Prefix $saved @($oldTask, $TaskName) @($publisherKeyPath)

    # ---------------------------------------------------------- stop the installed publisher
    # Disabled first, so its 5 minute repetition cannot start the old binary
    # again while it is being replaced; registering the task below enables it.
    Suspend-PublisherTasks $script:InstallTx
    foreach ($line in @(Stop-Publisher $Prefix $oldTask)) { Write-Line $line }
    if ($oldTask -ne $TaskName) {
        foreach ($line in @(Stop-Publisher $Prefix $TaskName)) { Write-Line $line }
    }

    # ---------------------------------------------------------- files
    Write-Step 'copying binaries, secrets and config'
    foreach ($d in $Prefix, $nats, $keys) {
        if (-not (Test-Path -LiteralPath $d)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }
    }
    Set-PrivateAcl $nats
    Set-PrivateAcl $keys
    Remove-StaleBinaries $Prefix
    Remove-StaleStaging $Prefix
    Remove-StaleStaging $nats
    Remove-StaleStaging $keys
    Write-Line (Install-Binary (Join-Path $bundle 'bin\cc-hook.exe') (Join-Path $Prefix 'cc-hook.exe'))
    Write-Line (Install-Binary (Join-Path $bundle 'bin\cc-publish.exe') (Join-Path $Prefix 'cc-publish.exe'))
    Write-Line (Install-PrivateFile (Join-Path $bundle "secrets\$credsName") $credsPath)
    # viewer.pub is not secret; private all the same, like the rest of keys\.
    Write-Line (Install-PrivateFile (Join-Path $bundle 'config\viewer.pub') $viewerPubPath)
    # This machine's identity (decided in the pre-checks): kept, or made
    # here; the private key never leaves this machine.
    $newLine = $null
    if ($identity.Action -eq 'keep') {
        Set-PrivateAcl $publisherKeyPath
        Write-Line "kept       this machine's identity $(Get-Kid $identity.Line) ($publisherKeyPath)"
    } else {
        $keygen = @('keygen', '--if-missing', '--key', $publisherKeyPath)
        if ($identity.Action -eq 'replace') {
            Write-Line "backup     $(Backup-KeyFile $publisherKeyPath) (the key that does not load)"
            $keygen = @('keygen', '--force', '--key', $publisherKeyPath, '--config', (Join-Path $bundle 'config\publish.json'), '--confirm', $alias)
        }
        $r = Invoke-Exe (Join-Path $Prefix 'cc-publish.exe') $keygen
        $newLine = Get-PublicLine $r
        if ($r.Code -ne 0 -or -not $newLine) { throw "cc-publish keygen failed (exit $($r.Code)): $(Get-LastLine $r)" }
        Set-PrivateAcl $publisherKeyPath
        Write-Line "generated  this machine's identity $(Get-Kid $newLine) in $publisherKeyPath (private)"
        Write-Line "public key $newLine"
    }

    # publish.json: the bundle's, with creds and key files made absolute.
    # cc-publish rejects unknown members, so only the ones it knows. The
    # bucket is kept when the bundle sets one (cc-publish defaults it).
    $src = Read-Json (Join-Path $bundle 'config\publish.json')
    $config = [ordered]@{
        alias         = [string]$src.alias
        servers       = @($src.servers)
        creds         = $credsPath
        publisher_key = $publisherKeyPath
        viewer_pub    = $viewerPubPath
        log_file      = $true
    }
    $bucket = [string](Get-Prop $src 'bucket' '')
    if ($bucket) { $config.Insert(2, 'bucket', $bucket) }
    if ($config.alias -ne $alias) { throw "config\publish.json is for alias '$($config.alias)', bundle.json for '$alias'; rebuild the bundle" }
    $json = ($config | ConvertTo-Json -Depth 4) + "`n"
    if ((Test-Path -LiteralPath $configPath) -and ([System.IO.File]::ReadAllText($configPath) -eq $json)) {
        Write-Line "unchanged  $configPath"
    } else {
        Write-TextFile "$configPath.new" $json
        Move-Item -LiteralPath "$configPath.new" -Destination $configPath -Force
        Write-Line "installed  $configPath"
    }
    $check = Invoke-Exe (Join-Path $Prefix 'cc-publish.exe') @('--data-dir', $Prefix, 'status', '--config', $configPath)
    $problem = Get-ConfigProblem $check $configPath
    if ($problem) { throw "cc-publish rejects $configPath`: $problem" }

    # A copy of the tooling, without binaries or secrets, so Verify.ps1 and
    # Uninstall.ps1 keep working after the bundle folder is deleted.
    foreach ($rel in $ToolingFiles) {
        $from = Join-Path $bundle $rel
        if (-not (Test-Path -LiteralPath $from)) { continue }
        $to = Join-Path $copy $rel
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $to) | Out-Null
        Copy-Item -LiteralPath $from -Destination $to -Force
    }
    $marker = [ordered]@{ prefix = $Prefix; claude_settings = $ClaudeSettings; task_name = $TaskName }
    Write-TextFile (Join-Path $copy $MarkerRel) (($marker | ConvertTo-Json) + "`n")
    Write-Line "installed  $copy (scripts only)"

    # ---------------------------------------------------------- hooks
    Write-Step 'registering hooks and the status line chain'
    $hook = Join-Path $Prefix 'cc-hook.exe'
    # The data dir decides where cc-hook keeps the status line chain record.
    $r = Invoke-Exe $hook (@('install') + (Get-SettingsArgs $ClaudeSettings)) @{ CC_USAGE_DATA_DIR = $Prefix }
    foreach ($l in $r.Lines) { Write-Line $l }
    if ($r.Code -ne 0) { throw "cc-hook install failed (exit $($r.Code))" }

    # ---------------------------------------------------------- autostart
    Write-Step 'registering autostart'
    $launch = Get-LaunchCommand $Prefix
    $description = "Publishes this machine's Claude Code sessions ($alias) to the cc-usage widget. Installed by the cc-usage publisher bundle; remove it with $copy\setup.cmd uninstall."
    $kind = $null
    $why = ''
    $script:InstallTx.AutostartTouched = $true
    if ($Autostart -ne 'RunKey') {
        $xml = New-PublisherTaskXml $Prefix $description
        try {
            Register-ScheduledTask -TaskName $TaskName -TaskPath '\' -Xml $xml -Force -ErrorAction Stop | Out-Null
            $kind = 'task'
        } catch {
            $why = $_.Exception.Message.Trim()
            # Same task through schtasks.exe, which some policies treat differently.
            $tmpXml = Join-Path ([System.IO.Path]::GetTempPath()) "cc-usage-task-$PID.xml"
            try {
                [System.IO.File]::WriteAllText($tmpXml, $xml, [System.Text.Encoding]::Unicode)
                $r = Invoke-Exe (Join-Path $env:SystemRoot 'System32\schtasks.exe') @('/Create', '/TN', $TaskName, '/XML', $tmpXml, '/F')
                if ($r.Code -eq 0) { $kind = 'task' } else { $why += " / schtasks: $(Get-LastLine $r)" }
            } finally {
                Remove-Item -LiteralPath $tmpXml -Force -ErrorAction SilentlyContinue
            }
        }
        if (-not $kind -and $Autostart -eq 'Task') { throw "cannot register the scheduled task '$TaskName': $why" }
    }
    if ($kind -eq 'task') {
        if ($null -ne (Get-RunEntry $TaskName)) {
            Remove-RunEntry $TaskName
            Write-Line "removed    the Run entry '$TaskName' (the task replaces it)"
        }
        Write-Line "installed  scheduled task '$TaskName' (at logon, every 5 min if not running, hidden)"
    } else {
        if ($why) { Write-Note "the scheduled task could not be registered ($why); using a Run entry instead" }
        Set-RunEntry $TaskName $launch.Line
        $kind = 'runkey'
        $t = Get-PublisherTask $TaskName
        if ($t -and (Test-OurTask $t $Prefix)) { Unregister-ScheduledTask -TaskName $TaskName -TaskPath '\' -Confirm:$false }
        Write-Line "installed  HKCU Run entry '$TaskName' (at logon, hidden; no restart if it dies)"
    }

    # ---------------------------------------------------------- manifest
    $files = [ordered]@{}
    $installed = @('cc-hook.exe', 'cc-publish.exe', "nats\$credsName", 'keys\publisher.key', 'keys\viewer.pub', 'publish.json')
    foreach ($rel in $installed) {
        $files[$rel] = Get-Sha256Hex (Join-Path $Prefix $rel)
    }
    $manifest = [ordered]@{
        format          = 1
        alias           = $alias
        nats_user       = $user
        version         = [string]$info.version
        arch            = [string]$info.arch
        prefix          = $Prefix
        claude_settings = $ClaudeSettings
        autostart       = $kind
        task_name       = $TaskName
        launch          = $launch.Line
        files           = $files
        installed_at    = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
    }
    Write-TextFile (Join-Path $Prefix $ManifestName) (($manifest | ConvertTo-Json -Depth 4) + "`n")
    Write-Line "installed  $(Join-Path $Prefix $ManifestName)"

    # ---------------------------------------------------------- start
    Write-Step 'starting the publisher'
    if ($kind -eq 'task') {
        Start-ScheduledTask -TaskName $TaskName -TaskPath '\'
    } else {
        Start-Process -FilePath $launch.Program -ArgumentList $launch.Arguments -WorkingDirectory $Prefix -WindowStyle Hidden
    }
    $proc = Wait-PublisherStart $Prefix 20
    if (-not $proc) {
        $log = Join-Path $Prefix 'cc-publish.log'
        $tail = if (Test-Path -LiteralPath $log) { (Get-Content -LiteralPath $log -Tail 5) -join ' | ' } else { '<no log yet>' }
        throw "the publisher did not start within 20 s. Last log lines: $tail"
    }
    # The new publisher runs: the install is done, the copies of the
    # previous one go.
    Complete-InstallTransaction $script:InstallTx
    $w = Get-WindowReport $proc
    Write-Line "running    $($w.Detail)"
    if (-not $w.Hidden) { Write-Note 'the publisher has a window; it should not (see Troubleshooting in README.md)' }
    Write-Host ''
    if ($newLine) {
        Write-Step "this machine's public key: $newLine"
        Write-Line (Get-AcceptNote $alias $newLine)
    }
    # For a Verify.ps1 run by hand (setup.cmd runs it itself): the installed
    # copy, which stays when the bundle folder is deleted, and finds the
    # install folder, settings file and task name in its marker and manifest.
    $verifyArgs = if ($SkipAppCheck) { ' -SkipAppCheck' } else { '' }
    Write-Step "installed. Validate with:  powershell -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $copy 'Verify.ps1')`"$verifyArgs"
    Write-Line 'The bundle folder holds secrets: delete it (and the .zip) once Verify passes.'
}

# -RotateKey: replaces the identity of the publisher installed in $Prefix
# (the one the installed copy records, else the default folder), after the
# alias is confirmed, then restarts it the way it was installed.
function Invoke-RotateKey {
    $marker = Get-InstalledMarker $PSScriptRoot
    $Prefix = Get-FullPath $(if ($Prefix) { $Prefix } elseif ($marker) { [string]$marker.prefix } else { Get-DefaultPrefix })
    $manifest = Get-Manifest $Prefix
    $exe = Join-Path $Prefix 'cc-publish.exe'
    $configPath = Join-Path $Prefix 'publish.json'
    # The manifest is the publisher's: on a PC with the cc-usage app, the
    # app's own key in the same folder is never touched here.
    if (-not $manifest -or -not (Test-Path -LiteralPath $exe) -or -not (Test-Path -LiteralPath $configPath)) {
        throw "no cc-usage publisher installed in $Prefix (no $ManifestName, cc-publish.exe or publish.json); run setup.cmd first"
    }
    $config = Read-Json $configPath
    $alias = [string](Get-Prop $config 'alias' '')
    if (-not $alias) { throw "$configPath has no alias" }
    $key = Get-FullPath ([string](Get-Prop $config 'publisher_key' (Join-Path $Prefix 'keys\publisher.key')))
    Write-Step "replacing the identity of '$alias' ($key)"
    $mode = @()
    if (Test-Path -LiteralPath $key -PathType Leaf) {
        $r = Invoke-Exe $exe @('--data-dir', $Prefix, 'keygen', '--if-missing', '--key', $key)
        $line = Get-PublicLine $r
        if ($r.Code -eq 0 -and $line) {
            $mode = @('--rotate')
            Write-Line "current identity $(Get-Kid $line)"
        } else {
            $mode = @('--force')
            Write-Line "$key does not load ($(Get-LastLine $r)); it is replaced (a copy is kept as $(Split-Path -Leaf $key).bak-<time>)"
        }
    } else {
        Write-Line "$key is missing"
    }
    Confirm-IdentityChange $alias $ConfirmAlias (Test-CanPrompt) 'Nothing changed.'

    if ($mode -contains '--force') { Write-Line "backup     $(Backup-KeyFile $key)" }
    $dir = Split-Path -Parent $key
    if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null; Set-PrivateAcl $dir }
    $r = Invoke-Exe $exe (@('--data-dir', $Prefix, 'keygen') + $mode + @('--key', $key, '--config', $configPath, '--confirm', $alias))
    $newLine = Get-PublicLine $r
    if ($r.Code -ne 0 -or -not $newLine) { throw "cc-publish keygen failed (exit $($r.Code)): $(Get-LastLine $r)" }
    Set-PrivateAcl $key
    Write-Line "generated  the new identity $(Get-Kid $newLine) in $key"
    $prefixRoot = $Prefix.TrimEnd('\') + '\'
    if ($key.StartsWith($prefixRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        Update-ManifestFile $Prefix $key.Substring($prefixRoot.Length)
        Write-Line "updated    $(Join-Path $Prefix $ManifestName)"
    }

    # Restart: the publisher reloads its key within 30 s anyway; a restart
    # makes the new key the one in use at once.
    $kind = [string](Get-Prop $manifest 'autostart' 'none')
    $task = [string](Get-Prop $manifest 'task_name' $DefaultTaskName)
    Write-Step 'restarting the publisher'
    foreach ($l in @(Stop-Publisher $Prefix $task)) { Write-Line $l }
    if ($kind -eq 'task') {
        Start-ScheduledTask -TaskName $task -TaskPath '\'
    } elseif ($kind -eq 'runkey') {
        $launch = Get-LaunchCommand $Prefix
        Start-Process -FilePath $launch.Program -ArgumentList $launch.Arguments -WorkingDirectory $Prefix -WindowStyle Hidden
    } else {
        Write-Note "no autostart recorded in the manifest; start the publisher with setup.cmd"
    }
    if ($kind -in 'task', 'runkey') {
        $proc = Wait-PublisherStart $Prefix 20
        if ($proc) { Write-Line "running    pid $($proc.Id)" } else { Write-Note 'the publisher did not start within 20 s; run Verify.ps1' }
    }
    Write-Host ''
    Write-Step "this machine's new public key: $newLine"
    Write-Line (Get-AcceptNote $alias $newLine)
    Write-Line "the viewer holds back the values of '$alias' until you accept the new key there"
}

# Prints what Undo-InstallTransaction did, and what to do when part of it
# failed.
function Write-RollbackReport($Tx, $Undo) {
    foreach ($l in $Undo.Done) { Write-Line $l }
    if ($Undo.Failed.Count -eq 0) {
        if ($Tx.Previous) { Write-Line 'rolled back: the previous install is back in place, with its autostart' }
        else { Write-Line "rolled back: nothing of this install is left but this machine's key pair (keys\publisher.key)" }
        return
    }
    foreach ($l in $Undo.Failed) { Write-Note "rollback: $l" }
    $names = (@($Tx.Tasks | ForEach-Object { "'$($_.Name)'" }) | Select-Object -Unique) -join ' / '
    Write-Note "the rollback was not complete. Copies of the previous files are kept in $($Undo.Dir) ($RollbackMarkerName there says where each one goes). Put back what you need from it, then move the folder out of $($Tx.Prefix) (it holds copies of the secrets: delete it once done); setup.cmd refuses to install until then. If the publisher does not run, enable the task $names in Task Scheduler."
}

# The install transaction, set by Invoke-Install once it starts changing
# things; rolled back when the install does not complete.
$script:InstallTx = $null
$exitCode = 1
try {
    if ($RotateKey) { Invoke-RotateKey } else { Invoke-Install }
    $exitCode = 0
} catch {
    Write-Host ''
    Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
} finally {
    # Also on Ctrl+C, which skips the catch.
    if ($script:InstallTx -and -not $script:InstallTx.Completed) {
        Write-Host ''
        Write-Step 'rolling back this install'
        try {
            Write-RollbackReport $script:InstallTx (Undo-InstallTransaction $script:InstallTx)
        } catch {
            Write-Note "rollback failed: $($_.Exception.Message). Copies of the previous files are in $($script:InstallTx.Dir); while it holds $RollbackMarkerName, setup.cmd refuses to install: put back what you need from it, then move it out of $($script:InstallTx.Prefix) and run setup.cmd again."
        }
        $exitCode = 1
    }
}
exit $exitCode
