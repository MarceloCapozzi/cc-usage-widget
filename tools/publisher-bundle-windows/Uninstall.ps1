<#
.SYNOPSIS
    Removes the cc-usage publisher from this Windows machine.

.DESCRIPTION
    Stops the publisher (cleanly, with `cc-publish stop`, then forcibly if
    needed), removes the scheduled task or the Run entry, takes the hooks and
    the status line chain out of the Claude Code settings (`cc-hook
    uninstall` restores the previous status line), and removes the binaries,
    the config, the state, the log and the copy of these scripts. The NATS
    credentials and the envelope keys are kept unless -Purge is given.

    When the cc-usage app is installed (it owns the same folder, the same
    cc-hook.exe and the same hooks), it changes nothing unless a publisher
    is installed there too (the app came later): then it removes only the
    publisher's autostart, cc-publish.exe, publish.json, manifest, state, log
    and scripts copy, and leaves cc-hook.exe, the hooks, the status line,
    the secrets and the spool to the app (-Purge is ignored).

    Safe to run when nothing, or only part, is installed. Works from the
    bundle folder and from the copy in %LOCALAPPDATA%\cc-usage\bundle.

.PARAMETER Prefix
    Install folder. Default: the one the installed copy records, else
    %LOCALAPPDATA%\cc-usage.

.PARAMETER ClaudeSettings
    Claude Code settings file. Default: the one recorded at install.

.PARAMETER TaskName
    Scheduled task (or Run entry) name. Default: the one recorded at install.

.PARAMETER Purge
    Also remove the creds, keys\publisher.key, keys\viewer.pub, their
    backups, and the runtime data (spool, live files, status line record,
    cc-hook log).

.PARAMETER SkipAppCheck
    For tests on a machine that has the cc-usage app, with -Prefix,
    -ClaudeSettings and -TaskName pointing away from the real ones.
#>
[CmdletBinding()]
param(
    [string]$Prefix,
    [string]$ClaudeSettings,
    [string]$TaskName,
    [switch]$Purge,
    [switch]$SkipAppCheck
)

if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') {
    Write-Host "ERROR: PowerShell runs in $($ExecutionContext.SessionState.LanguageMode) mode on this machine, which these scripts cannot run in. See Troubleshooting in README.md." -ForegroundColor Red
    exit 1
}
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0
. (Join-Path $PSScriptRoot 'scripts\common.ps1')

function Remove-IfPresent([string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Recurse -Force
        Write-Line "removed    $Path"
    }
}

# Removes the installed copy of the scripts. When this script runs from it,
# a helper removes it once this process (and the cmd.exe of setup.cmd, if
# that started it) has exited, since a running batch file must stay. The
# helper is a plain script file in %TEMP%, run with Windows PowerShell -File
# (no encoded command line, no console host tricks), that deletes itself at
# the end.
function Remove-InstalledCopy([string]$Copy, [string]$PrefixToPrune) {
    if (-not (Test-Path -LiteralPath $Copy)) { return }
    $here = Get-FullPath $PSScriptRoot
    $inside = $here.StartsWith((Get-FullPath $Copy), [StringComparison]::OrdinalIgnoreCase)
    if (-not $inside) {
        Remove-IfPresent $Copy
        return
    }
    $wait = @($PID)
    try {
        $me = Get-CimInstance Win32_Process -Filter "ProcessId = $PID" -ErrorAction Stop
        $parent = Get-CimInstance Win32_Process -Filter "ProcessId = $($me.ParentProcessId)" -ErrorAction Stop
        if ($parent -and $parent.Name -ieq 'cmd.exe') { $wait += [int]$parent.ProcessId }
    } catch { }
    $q = { param([string]$s) "'" + $s.Replace("'", "''") + "'" }
    $lines = @(
        '# Written by the cc-usage publisher Uninstall.ps1: removes its installed copy',
        '# once the uninstall has exited, then this file.',
        "foreach (`$i in @($($wait -join ','))) { try { Wait-Process -Id `$i -Timeout 86400 -ErrorAction Stop } catch { } }",
        'Start-Sleep -Milliseconds 500',
        "Remove-Item -LiteralPath $(& $q $Copy) -Recurse -Force -ErrorAction SilentlyContinue"
    )
    if ($PrefixToPrune) {
        $lines += "try { [System.IO.Directory]::Delete($(& $q $PrefixToPrune)) } catch { }"
    }
    $lines += 'Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue'
    $temp = [System.IO.Path]::GetTempPath()
    $helper = Join-Path $temp "cc-usage-uninstall-$PID-$([Guid]::NewGuid().ToString('N').Substring(0, 8)).ps1"
    # UTF-8 with a BOM: Windows PowerShell reads a script without one in the
    # ANSI code page, and the paths may hold non-ASCII characters.
    [System.IO.File]::WriteAllText($helper, (($lines -join "`r`n") + "`r`n"), (New-Object System.Text.UTF8Encoding($true)))
    $ps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    # The helper must not keep the copy as its working directory.
    Set-Location -LiteralPath $temp
    try {
        Start-Process -FilePath $ps -ArgumentList "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$helper`"" -WorkingDirectory $temp -WindowStyle Hidden
    } catch {
        Remove-Item -LiteralPath $helper -Force -ErrorAction SilentlyContinue
        Write-Note "could not start the cleanup of $Copy ($($_.Exception.Message)); delete it once this window closes"
        return
    }
    Write-Line "removing   $Copy (when this window closes)"
}

function Invoke-Uninstall {
    $bundle = $PSScriptRoot
    $info = Get-BundleInfo $bundle
    $marker = Get-InstalledMarker $bundle
    $Prefix = Get-FullPath $(if ($Prefix) { $Prefix } elseif ($marker) { [string]$marker.prefix } else { Get-DefaultPrefix })
    $manifest = Get-Manifest $Prefix
    if (-not $ClaudeSettings) {
        $ClaudeSettings = [string](Get-Prop $manifest 'claude_settings' (Get-Prop $marker 'claude_settings' (Get-DefaultSettings)))
    }
    $ClaudeSettings = Get-FullPath $ClaudeSettings
    if (-not $TaskName) {
        $TaskName = [string](Get-Prop $manifest 'task_name' (Get-Prop $marker 'task_name' $DefaultTaskName))
    }
    Assert-TaskName $TaskName
    $alias = $info.alias
    $credsName = "$($info.nats_user).creds"

    # The app owns the folder, cc-hook.exe and the hooks: leave them. Without
    # a publisher manifest there is nothing of the publisher's here: change
    # nothing. With one, the app was installed after the publisher, and the
    # publisher (which would compete with the app for the spool) must go:
    # remove only what is the publisher's own, i.e. its autostart,
    # cc-publish.exe, its config, manifest, state and log, and the scripts copy.
    $app = Get-AppInstall
    $appMode = $false
    if ($app -and -not $SkipAppCheck) {
        if (-not $manifest) {
            throw "the cc-usage app is installed here ($app) and no publisher is installed in $Prefix (no $ManifestName). The app owns $Prefix, cc-hook.exe and the hooks, so this uninstaller changes nothing. Uninstall the app from Settings > Apps if you want it gone."
        }
        $appMode = $true
        Write-Note "the cc-usage app is installed here ($app): removing only the publisher (its autostart, cc-publish.exe, publish.json, state and log); cc-hook.exe, the hooks, the status line, the creds, the keys and the spool stay for the app"
    }

    Write-Step "uninstalling the cc-usage publisher for '$alias' from $Prefix"

    # 1. Autostart first: the task's 5 minute repetition could otherwise start
    #    a new publisher between the stop and the removal of the task.
    $task = Get-PublisherTask $TaskName
    if ($task) {
        if (Test-OurTask $task $Prefix) {
            Unregister-ScheduledTask -TaskName $TaskName -TaskPath '\' -Confirm:$false
            Write-Line "removed    scheduled task '$TaskName'"
        } else {
            Write-Note "a task named '$TaskName' exists but does not run $Prefix\cc-publish.exe; left alone"
        }
    }
    $run = Get-RunEntry $TaskName
    if ($null -ne $run) {
        if (([string]$run).IndexOf((Join-Path $Prefix 'cc-publish.exe'), [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            Remove-ItemProperty -LiteralPath $RunKeyPath -Name $TaskName
            Remove-ItemProperty -LiteralPath $StartupApprovedPath -Name $TaskName -ErrorAction SilentlyContinue
            Write-Line "removed    HKCU Run entry '$TaskName'"
        } else {
            Write-Note "a Run entry named '$TaskName' exists but does not run $Prefix\cc-publish.exe; left alone"
        }
    }

    # 2. Stop the publisher.
    foreach ($line in @(Stop-Publisher $Prefix $TaskName)) { Write-Line $line }

    # 3. Hooks and status line chain (needs the chain record in the data dir,
    #    so before anything there is removed).
    $hook = Join-Path $Prefix 'cc-hook.exe'
    if (-not (Test-Path -LiteralPath $hook) -and -not $marker -and (Test-Path -LiteralPath (Join-Path $bundle 'bin\cc-hook.exe'))) {
        $hook = Join-Path $bundle 'bin\cc-hook.exe'
    }
    if ($appMode) {
        Write-Line "kept       the hooks and the status line chain in $ClaudeSettings (the app's too)"
    } elseif (-not (Test-Path -LiteralPath $hook)) {
        Write-Note "no cc-hook.exe found; hooks (if any) stay in $ClaudeSettings"
    } elseif (-not (Test-Path -LiteralPath $ClaudeSettings)) {
        Write-Line "skipped    hooks: $ClaudeSettings does not exist"
    } else {
        Write-Step 'removing hooks and the status line chain'
        $r = Invoke-Exe $hook (@('uninstall') + (Get-SettingsArgs $ClaudeSettings)) @{ CC_USAGE_DATA_DIR = $Prefix }
        foreach ($l in $r.Lines) { Write-Line $l }
        if ($r.Code -ne 0) { Write-Note "cc-hook uninstall failed (exit $($r.Code)); check $ClaudeSettings" }
    }

    # 4. Files.
    Write-Step 'removing files'
    $own = @('cc-publish.exe', 'publish.json', $ManifestName) + $RuntimeFiles
    if ($appMode) {
        foreach ($f in @(Get-ChildItem -LiteralPath $Prefix -Filter 'cc-publish.exe.*' -File -ErrorAction SilentlyContinue)) {
            if ($f.Name -like 'cc-publish.exe.old-*' -or $f.Name -eq 'cc-publish.exe.new') { Remove-Item -LiteralPath $f.FullName -Force -ErrorAction SilentlyContinue }
        }
    } else {
        Remove-StaleBinaries $Prefix
        # The publisher is stopped by now, so nothing holds its lock. With
        # the app, the lock stays: the app may be publishing from here, and
        # removing a held lock file would let a second publisher start.
        $own = @('cc-hook.exe', 'cc-publish.lock') + $own
    }
    # Copies an interrupted install kept for its rollback (they hold
    # secrets), also those of an incomplete rollback: with the publisher
    # gone, nothing is left to restore them into.
    foreach ($d in @(Remove-StaleRollback $Prefix -All)) { Write-Line "removed    $d (copies kept for a rollback)" }
    foreach ($f in $own) {
        $path = Join-Path $Prefix $f
        if (-not (Test-Path -LiteralPath $path)) { continue }
        try {
            Remove-Item -LiteralPath $path -Force
            Write-Line "removed    $path"
        } catch {
            # A Claude Code status line render can hold cc-hook.exe for a moment.
            Start-Sleep -Milliseconds 800
            try { Remove-Item -LiteralPath $path -Force; Write-Line "removed    $path" }
            catch { Write-Note "could not remove $path ($($_.Exception.Message)); delete it later" }
        }
    }

    $prune = $null
    if ($appMode) {
        if ($Purge) { Write-Note '-Purge ignored: the creds, the keys and the spool are left to the app' }
    } elseif ($Purge) {
        $nats = Join-Path $Prefix 'nats'
        $keys = Join-Path $Prefix 'keys'
        # Only the publisher's own key files; anything else in keys\ (a
        # viewer's keys, if this was ever the viewer) stays.
        foreach ($f in @(Get-ChildItem -LiteralPath $nats -Filter "$credsName*" -File -ErrorAction SilentlyContinue) +
            @(Get-ChildItem -LiteralPath $keys -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -match '^(publisher\.key|viewer\.pub)(\.bak-.*)?$' })) {
            Remove-Item -LiteralPath $f.FullName -Force
            Write-Line "removed    $($f.FullName)"
        }
        Remove-StaleStaging $Prefix
        Remove-StaleStaging $nats
        if (Test-Path -LiteralPath $keys) { Remove-StaleStaging $keys }
        foreach ($d in 'spool', 'live') { Remove-IfPresent (Join-Path $Prefix $d) }
        foreach ($f in 'statusline-chain.json', 'cc-hook.log') { Remove-IfPresent (Join-Path $Prefix $f) }
        try { [System.IO.Directory]::Delete($nats) } catch { }
        try { [System.IO.Directory]::Delete($keys) } catch { }
        $prune = $Prefix
        Write-Line 'purged     secrets and runtime data'
    } else {
        Write-Line "kept       $(Join-Path $Prefix "nats\$credsName") and $(Join-Path $Prefix 'keys') (this machine's identity, which a reinstall keeps; Uninstall.ps1 -Purge removes them)"
    }

    Remove-InstalledCopy (Join-Path $Prefix 'bundle') $prune
    if ($prune -and (Test-Path -LiteralPath $Prefix) -and -not (Get-ChildItem -LiteralPath $Prefix -Force | Select-Object -First 1)) {
        [System.IO.Directory]::Delete($Prefix)
        Write-Line "removed    $Prefix"
    }
    Write-Step 'uninstalled'
}

try {
    Invoke-Uninstall
    exit 0
} catch {
    Write-Host ''
    Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
