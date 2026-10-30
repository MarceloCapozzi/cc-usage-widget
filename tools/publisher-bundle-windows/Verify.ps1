<#
.SYNOPSIS
    Checks the installed cc-usage publisher end to end and prints a PASS/FAIL
    table. Exits 1 when any check fails.

.DESCRIPTION
    Works from the bundle folder and from the copy Install.ps1 leaves in
    %LOCALAPPDATA%\cc-usage\bundle (after the bundle folder, with its
    secrets, has been deleted). Changes nothing.

    Checks: bundle checksums (full bundle only), installed files against the
    install manifest, no copies left by an incomplete rollback (a warning:
    the next install refuses to run while they are there), both binaries
    start, the secrets' ACL (current user and
    SYSTEM only), publish.json, the hooks and the status line chain
    (cc-hook status), a NATS round trip (cc-publish check), the autostart
    entry, the publisher running without a window, and a recent publish
    (cc-publish status, waiting up to -WaitSeconds after a fresh install).

.PARAMETER Prefix
    Install folder. Default: the one the installed copy records, else
    %LOCALAPPDATA%\cc-usage.

.PARAMETER ClaudeSettings
    Claude Code settings file. Default: the one recorded at install.

.PARAMETER TaskName
    Scheduled task (or Run entry) name. Default: the one recorded at install.

.PARAMETER WaitSeconds
    How long to wait for a successful publish. Default 75: a publisher
    started a moment ago needs a few seconds to connect, and its heartbeat
    is every 60 s.

.PARAMETER SkipAppCheck
    For tests on a machine that has the cc-usage app.

.PARAMETER Autostart
    Install.ps1's option, accepted and ignored: setup.cmd passes Verify.ps1
    the arguments it gave Install.ps1, so that -Prefix, -ClaudeSettings and
    -TaskName reach both.

.PARAMETER Force
    Install.ps1's option, accepted and ignored (see -Autostart).

.PARAMETER RotateKey
    Install.ps1's option, accepted and ignored (see -Autostart).

.PARAMETER ConfirmAlias
    Install.ps1's option, accepted and ignored (see -Autostart; setup.cmd
    rotate-key passes it to both).
#>
[CmdletBinding()]
param(
    [string]$Prefix,
    [string]$ClaudeSettings,
    [string]$TaskName,
    [int]$WaitSeconds = 75,
    [switch]$SkipAppCheck,
    [string]$Autostart,
    [switch]$Force,
    [switch]$RotateKey,
    [string]$ConfirmAlias
)

if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') {
    Write-Host "ERROR: PowerShell runs in $($ExecutionContext.SessionState.LanguageMode) mode on this machine, which these scripts cannot run in. See Troubleshooting in README.md." -ForegroundColor Red
    exit 1
}
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0
. (Join-Path $PSScriptRoot 'scripts\common.ps1')

function Invoke-Verify {
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
    $hook = Join-Path $Prefix 'cc-hook.exe'
    $publish = Join-Path $Prefix 'cc-publish.exe'
    $configPath = Join-Path $Prefix 'publish.json'
    $kind = [string](Get-Prop $manifest 'autostart' 'none')
    Write-Step "verifying the cc-usage publisher for '$alias' in $Prefix"

    # 0. The app owns this folder on machines that have it.
    $app = Get-AppInstall
    if ($app -and -not $SkipAppCheck) {
        Add-Result FAIL 'cc-usage app' "installed here ($app); the publisher must not run next to it. Uninstall.ps1 removes the publisher and leaves the app's files, hooks and secrets alone."
    }

    # 1. Bundle integrity, while this is the full bundle.
    $sums = Join-Path $bundle 'SHA256SUMS'
    if (-not $marker -and (Test-Path -LiteralPath $sums)) {
        $bad = @(Test-Sums $bundle $sums)
        if ($bad.Count -eq 0) { Add-Result PASS 'bundle checksums' 'SHA256SUMS' }
        else { Add-Result FAIL 'bundle checksums' (($bad | Select-Object -First 3) -join '; ') }
    }

    # 2. Installed files are the ones Install.ps1 wrote.
    if ($manifest) {
        $changed = @()
        foreach ($p in (Get-Prop $manifest 'files').PSObject.Properties) {
            $path = Join-Path $Prefix $p.Name
            if (-not (Test-Path -LiteralPath $path)) { $changed += "$($p.Name) missing" }
            elseif ((Get-Sha256Hex $path) -ne [string]$p.Value) { $changed += "$($p.Name) changed" }
        }
        if ($changed.Count -eq 0) { Add-Result PASS 'installed files' "match $ManifestName" }
        else { Add-Result FAIL 'installed files' "$($changed -join ', ') since install; run Install.ps1 again" }
    } else {
        Add-Result FAIL 'installed files' "$(Join-Path $Prefix $ManifestName) missing; run Install.ps1"
    }
    # Copies an incomplete rollback kept; the next install refuses to run
    # while they are there.
    foreach ($d in @(Get-IncompleteRollback $Prefix)) {
        Add-Result WARN 'earlier rollback' (Get-IncompleteRollbackHelp $d)
    }

    # 3. Binaries start.
    foreach ($b in $hook, $publish) {
        $leaf = Split-Path -Leaf $b
        if (-not (Test-Path -LiteralPath $b)) { Add-Result FAIL "$leaf --version" "$b missing"; continue }
        try {
            $r = Invoke-Exe $b @('--version')
            $v = Get-LastLine $r
            if ($r.Code -ne 0) { Add-Result FAIL "$leaf --version" "exit $($r.Code): $v" }
            elseif ($v -match [regex]::Escape([string]$info.version)) { Add-Result PASS "$leaf --version" $v }
            else { Add-Result WARN "$leaf --version" "$v (bundle says $($info.version))" }
        } catch {
            Add-Result FAIL "$leaf --version" "does not start: $($_.Exception.Message)"
        }
    }

    # 4. Secrets readable by this user and SYSTEM only.
    $bad = @()
    $secretPaths = @((Join-Path $Prefix "nats\$credsName"), (Join-Path $Prefix 'nats'),
        (Join-Path $Prefix 'keys'), (Join-Path $Prefix 'keys\publisher.key'))
    foreach ($path in $secretPaths) {
        $problem = Test-PrivateAcl $path
        if ($problem) { $bad += "$(Split-Path -Leaf $path): $problem" }
    }
    if ($bad.Count -eq 0) { Add-Result PASS 'secrets ACL' "creds, keys and nats\ open to $(Get-SidName (Get-UserSid)) and SYSTEM only" }
    else { Add-Result FAIL 'secrets ACL' "$($bad -join '; ') - run Install.ps1 again" }

    # 5. publish.json.
    if (Test-Path -LiteralPath $configPath) {
        try {
            $c = Read-Json $configPath
            $problems = @()
            if ([string](Get-Prop $c 'alias') -ne $alias) { $problems += "alias $(Get-Prop $c 'alias')" }
            $creds = [string](Get-Prop $c 'creds' '')
            if (-not $creds -or -not (Test-Path -LiteralPath $creds)) { $problems += "creds ($creds) missing" }
            # Both key files are required.
            $pk = [string](Get-Prop $c 'publisher_key' (Join-Path $Prefix 'keys\publisher.key'))
            $vp = [string](Get-Prop $c 'viewer_pub' (Join-Path $Prefix 'keys\viewer.pub'))
            if (-not (Test-Path -LiteralPath $pk)) { $problems += "publisher_key ($pk) missing: this machine's identity; restore it from a backup, or make a new one on purpose: setup.cmd rotate-key" }
            if (-not (Test-Path -LiteralPath $vp)) { $problems += "viewer_pub ($vp) missing; install a new bundle, which carries viewer.pub" }
            $unknown = @($c.PSObject.Properties.Name | Where-Object { $_ -notin 'alias', 'servers', 'bucket', 'creds', 'publisher_key', 'viewer_pub', 'log_file' })
            if ($unknown.Count -gt 0) { $problems += "members cc-publish rejects: $($unknown -join ', ')" }
            if ($problems.Count -eq 0) { Add-Result PASS 'publish.json' "alias $alias, servers $((@($c.servers)) -join ', '), both key files present" }
            else { Add-Result FAIL 'publish.json' ($problems -join '; ') }
        } catch {
            Add-Result FAIL 'publish.json' "not valid JSON: $($_.Exception.Message)"
        }
    } else {
        Add-Result FAIL 'publish.json' "$configPath missing"
    }

    # 6. Hooks and the status line chain, registered to the installed cc-hook.
    if (Test-Path -LiteralPath $hook) {
        $r = Invoke-Exe $hook (@('status') + (Get-SettingsArgs $ClaudeSettings)) @{ CC_USAGE_DATA_DIR = $Prefix }
        $lines = @($r.Lines)
        $ours = Get-HookCommand $hook
        $installed = @($lines | Where-Object { $_ -match '^  [A-Za-z]+ +installed$' }).Count
        $broken = @($lines | Where-Object { $_ -match '^  [A-Za-z]+ +(missing|duplicated|stray)' } | ForEach-Object { ($_.Trim() -split '\s+')[0..1] -join '=' })
        $mine = @($lines | Where-Object { $_.IndexOf($ours, [StringComparison]::OrdinalIgnoreCase) -ge 0 }).Count
        if ($r.Code -ne 0) {
            Add-Result FAIL 'hooks' "cc-hook status failed: $(Get-LastLine $r)"
        } elseif ($r.Text -match 'file does not exist') {
            Add-Result FAIL 'hooks' "$ClaudeSettings does not exist; run Install.ps1"
        } elseif ($broken.Count -gt 0) {
            Add-Result FAIL 'hooks' "$($broken -join ' ') - run Install.ps1 again"
        } elseif ($installed -eq 0) {
            Add-Result FAIL 'hooks' 'cc-hook status shows no installed events'
        } elseif ($mine -lt $installed) {
            Add-Result FAIL 'hooks' "registered, but not all to $ours; run Install.ps1 again"
        } else {
            Add-Result PASS 'hooks' "$installed events registered to $ours"
        }
        $chained = @($lines | Where-Object { $_ -match '^  statusLine +chained: ' })
        if ($chained.Count -gt 0 -and $chained[0].IndexOf($ours, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            $then = @($lines | Where-Object { $_ -match '^\s+then runs ' } | ForEach-Object { $_.Trim() })
            Add-Result PASS 'status line chain' $(if ($then.Count) { $then[0] } else { 'chained' })
        } elseif ($chained.Count -gt 0) {
            Add-Result WARN 'status line chain' "chained to another cc-hook: $($chained[0].Trim())"
        } else {
            Add-Result WARN 'status line chain' 'not chained; context use will be estimated (Install.ps1 chains it)'
        }
    } else {
        Add-Result FAIL 'hooks' "$hook missing"
    }

    # 7. NATS: connect, bind the bucket, put and delete a sealed probe. This
    #    opens a second connection for a moment (the publisher holds one).
    if ((Test-Path -LiteralPath $publish) -and (Test-Path -LiteralPath $configPath)) {
        Write-Line '...        cc-publish check (NATS connect, bind, probe put/delete)'
        $r = Invoke-Exe $publish @('--data-dir', $Prefix, 'check', '--config', $configPath)
        if ($r.Code -eq 0) { Add-Result PASS 'cc-publish check' (Get-LastLine $r) }
        else {
            $fails = @($r.Lines | Where-Object { $_ -match '^FAIL' } | Select-Object -First 2)
            Add-Result FAIL 'cc-publish check' "$($fails -join ' | ') $(Get-LastLine $r)"
        }
    }

    # 8. Autostart.
    $launch = Get-LaunchCommand $Prefix
    $task = Get-PublisherTask $TaskName
    $run = Get-RunEntry $TaskName
    if ($task -and (Test-OurTask $task $Prefix)) {
        $action = @($task.Actions)[0]
        $headless = ([string](Get-Prop $action 'Execute' '') -ieq $launch.Program) -and ([string](Get-Prop $action 'Arguments' '') -match '^--headless ')
        $trigger = @($task.Triggers | Where-Object { $_.CimClass.CimClassName -eq 'MSFT_TaskLogonTrigger' }).Count -gt 0
        $state = [string]$task.State
        if (-not $headless) { Add-Result FAIL 'autostart' "task '$TaskName' does not run conhost --headless; run Install.ps1 again" }
        elseif (-not $trigger) { Add-Result FAIL 'autostart' "task '$TaskName' has no logon trigger; run Install.ps1 again" }
        elseif ($state -eq 'Disabled') { Add-Result FAIL 'autostart' "task '$TaskName' is disabled; enable it in Task Scheduler" }
        else { Add-Result PASS 'autostart' "scheduled task '$TaskName' (logon, every 5 min), state $state" }
    } elseif ($null -ne $run) {
        if ([string]$run -ne $launch.Line) { Add-Result FAIL 'autostart' "Run entry '$TaskName' does not start $($launch.Program) --headless ... ; run Install.ps1 again" }
        elseif (-not (Test-RunEntryEnabled $TaskName)) { Add-Result FAIL 'autostart' "Run entry '$TaskName' is disabled in Task Manager > Startup apps" }
        else { Add-Result PASS 'autostart' "HKCU Run entry '$TaskName' (logon only, no restart if it dies)" }
    } else {
        Add-Result FAIL 'autostart' "neither a task nor a Run entry named '$TaskName' (manifest says $kind); run Install.ps1"
    }

    # 9. The publisher runs, without a window.
    $procs = @(Get-PublisherProcess $publish)
    if ($procs.Count -eq 0) {
        Add-Result FAIL 'publisher process' "no cc-publish.exe from $Prefix is running; start it with Install.ps1 or log off and on"
    } else {
        if ($procs.Count -gt 1) { Add-Result WARN 'publisher process' "$($procs.Count) copies running (pids $(($procs | ForEach-Object Id) -join ', '))" }
        $w = Get-WindowReport $procs[0]
        if ($w.Hidden -and $w.Headless) { Add-Result PASS 'publisher process' "running hidden: $($w.Detail)" }
        elseif ($w.Hidden) { Add-Result WARN 'publisher process' "running without a window, but not under conhost --headless: $($w.Detail)" }
        else { Add-Result FAIL 'publisher process' "running WITH a window: $($w.Detail)" }
    }

    # 10. A recent successful publish.
    if ((Test-Path -LiteralPath $publish) -and (Test-Path -LiteralPath $configPath)) {
        Write-Line "...        waiting up to ${WaitSeconds}s for a successful publish"
        $deadline = (Get-Date).AddSeconds($WaitSeconds)
        $r = $null
        while ($true) {
            $r = Invoke-Exe $publish @('--data-dir', $Prefix, 'status', '--config', $configPath)
            if ($r.Code -eq 0 -or (Get-Date) -ge $deadline -or @(Get-PublisherProcess $publish).Count -eq 0) { break }
            Start-Sleep -Seconds 3
        }
        $lastWrite = @($r.Lines | Where-Object { $_ -match '^last write\s+' } | ForEach-Object { ($_ -replace '^last write\s+', '').Trim() })
        $health = @($r.Lines | Where-Object { $_ -match '^health\s+' } | ForEach-Object { ($_ -replace '^health\s+', '').Trim() })
        if ($r.Code -eq 0) {
            Add-Result PASS 'recent publish' $(if ($lastWrite.Count) { "last write $($lastWrite[0])" } else { 'ok' })
        } else {
            $log = Join-Path $Prefix 'cc-publish.log'
            $tail = if (Test-Path -LiteralPath $log) { (Get-Content -LiteralPath $log -Tail 2) -join ' | ' } else { '<no log>' }
            Add-Result FAIL 'recent publish' "$(if ($health.Count) { $health[0] } else { Get-LastLine $r }). Last log: $tail"
        }
    }

    $mode = if ($kind -eq 'runkey') { 'Run entry' } else { 'scheduled task' }
    return (Show-Results "Verification ($alias, $mode, $Prefix)")
}

try {
    $fails = Invoke-Verify
    if ($fails -gt 0) { exit 1 }
    exit 0
} catch {
    Write-Host ''
    Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
