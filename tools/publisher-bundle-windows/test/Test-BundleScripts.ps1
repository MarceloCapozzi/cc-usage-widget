<#
.SYNOPSIS
    Isolated tests of the Windows publisher bundle's scripts: what can be
    checked without installing for real.

.DESCRIPTION
    Everything happens in a new folder under %TEMP%; the test never writes the
    registry, never registers or changes a scheduled task, never touches
    %LOCALAPPDATA%\cc-usage, the Claude Code settings or a running publisher.

      - Install-PrivateFile: the secret is only ever written into a folder that
        already has the private ACL (checked at the moment of the copy), ends
        up private, leaves no staging folder, backs up a different file and
        leaves an identical one alone.
      - Get-ConfigProblem: every way cc-publish status reports a config it
        could not load is caught (cannot read, too large, invalid JSON,
        alias, servers), with fake output and, when a built cc-publish.exe is
        found (or -PublishExe is given), with the real one, which also
        refuses a member it does not know (such as "key").
      - setup.cmd passes its arguments to Verify.ps1 as well as Install.ps1,
        and `setup.cmd rotate-key` runs Install.ps1 -RotateKey (run against
        stub scripts), checks SHA256SUMS before it unblocks or runs anything
        (a changed file stops it), and Verify.ps1 accepts every parameter
        Install.ps1 has.
      - The machine's identity: the warning, Confirm-IdentityChange
        (-ConfirmAlias, a wrong alias, no console), Get-IdentityPlan (first
        install, a lost key, a kept key, one that does not load) and, with a
        cc-publish.exe that knows keygen --confirm, Install.ps1 -RotateKey on
        a fake install under %TEMP% whose manifest records no autostart:
        refused without or with a wrong -ConfirmAlias (nothing changes), a
        rotation (new key in front, manifest updated, private ACL), a key
        that does not load (replaced, backed up), a missing key, no install.
      - Get-DefaultSettings and Get-SettingsArgs follow CLAUDE_CONFIG_DIR
        like cc-hook (this process's environment only, restored).
      - The install transaction (New-InstallTransaction, Suspend-PublisherTasks,
        Undo-InstallTransaction, Complete-InstallTransaction) on fake installs
        under %TEMP%, with every scheduled task cmdlet, the Run entry helpers,
        Start-Process and Wait-PublisherStart replaced by stubs: a failure
        after the files and the autostart changed puts back the binaries,
        config, creds (private), settings.json and tooling, removes new
        files, keeps the key pair (and records it in the manifest),
        registers the previous task again and starts it; a failure before
        the autostart step only enables the task again; a task the user
        disabled stays disabled; a Run entry install gets its entry back; a
        failing rollback step is reported while the rest still runs, and
        its copies are kept with ROLLBACK-INCOMPLETE.txt (where each goes,
        what failed), which the next install refuses to run over and only
        Remove-StaleRollback -All (Uninstall.ps1) deletes; a failed first
        install leaves only its key pair.

    The checks that need a built cc-publish.exe are skipped without one,
    unless CC_TEST_REQUIRE_ALL=1 (CI), which makes each skip a failure.

    Run it with both Windows PowerShell 5.1 and PowerShell 7:

        powershell -NoProfile -ExecutionPolicy Bypass -File tools\publisher-bundle-windows\test\Test-BundleScripts.ps1
        pwsh -NoProfile -File tools\publisher-bundle-windows\test\Test-BundleScripts.ps1

    Exits 1 when any check fails.

.PARAMETER PublishExe
    A cc-publish.exe for the real config and identity checks. Default:
    the newer of release\cc-publish.exe and debug\cc-publish.exe in cargo's
    target dir (Get-CargoTargetDir of tools\cross\common.ps1, which honours
    CARGO_TARGET_DIR), when one exists.
#>
[CmdletBinding()]
param([string]$PublishExe)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0
$bundleSrc = Split-Path -Parent $PSScriptRoot
# Get-CargoTargetDir comes from the cross-build helpers; the bundle's own
# helpers are loaded after them, so theirs win where the names overlap.
. (Join-Path $bundleSrc '..\cross\common.ps1')
. (Join-Path $bundleSrc 'scripts\common.ps1')

$root = Join-Path ([System.IO.Path]::GetTempPath()) "cc-win-bundle-test $PID"
New-Item -ItemType Directory -Force -Path $root | Out-Null
$script:failures = 0
$script:passes = 0
function Assert([bool]$Ok, [string]$What) {
    if ($Ok) { $script:passes++; Write-Host "ok   $What" }
    else { $script:failures++; Write-Host "FAIL $What" -ForegroundColor Red }
}
# Checks that cannot run here are skipped, except with CC_TEST_REQUIRE_ALL=1
# (CI), where a missing prerequisite is a failure rather than a quiet pass.
function Skip-Check([string]$What) {
    if ($env:CC_TEST_REQUIRE_ALL -eq '1') { Assert $false "$What (required: CC_TEST_REQUIRE_ALL=1)" }
    else { Write-Host "skip $What" }
}

# Records, for every Copy-Item the scripts run, whether the destination
# folder was private at that moment. A function takes precedence over the
# cmdlet of the same name, so Install-PrivateFile calls this one.
$script:copies = New-Object System.Collections.Generic.List[object]
function Copy-Item {
    [CmdletBinding()]
    param([string]$LiteralPath, [string]$Destination, [switch]$Force)
    $dir = Split-Path -Parent $Destination
    $script:copies.Add([pscustomobject]@{ Dir = $dir; Problem = (Test-PrivateAcl $dir) })
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath $LiteralPath -Destination $Destination -Force:$Force
}

try {
    Write-Host "PowerShell $($PSVersionTable.PSVersion) ($($PSVersionTable.PSEdition)), test folder $root"

    # ------------------------------------------------------------ Install-PrivateFile
    # The prefix keeps the default inherited ACL of %TEMP%, like the install
    # folder does under %LOCALAPPDATA%: a file created in it is not private.
    $prefix = Join-Path $root 'prefix dir'
    New-Item -ItemType Directory -Force -Path $prefix | Out-Null
    $src = Join-Path $root 'publisher.key.src'
    [System.IO.File]::WriteAllText($src, "p1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=`n")
    $dest = Join-Path $prefix 'publisher.key'
    Assert ($null -ne (Test-PrivateAcl $prefix)) 'the test prefix is not private (as an install folder is not)'

    $line = Install-PrivateFile $src $dest
    Assert ($line -like 'installed  *') "first install: $line"
    Assert ($script:copies.Count -eq 1) 'one copy made'
    Assert ($null -eq $script:copies[0].Problem) "the copy was written into a private folder ($($script:copies[0].Problem))"
    Assert ((Split-Path -Leaf $script:copies[0].Dir) -like "$StagingPrefix*") 'the copy went to a staging folder, not next to the secret'
    Assert ($null -eq (Test-PrivateAcl $dest)) 'the installed secret is private'
    Assert ((Get-Sha256Hex $dest) -eq (Get-Sha256Hex $src)) 'the installed secret has the right content'
    Assert (@(Get-ChildItem -LiteralPath $prefix -Force -Filter "$StagingPrefix*").Count -eq 0) 'no staging folder left'

    $line = Install-PrivateFile $src $dest
    Assert ($line -like 'unchanged  *') "identical file: $line"
    Assert ($script:copies.Count -eq 1) 'an identical file is not copied again'

    [System.IO.File]::WriteAllText($src, "p2:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=`n")
    $line = Install-PrivateFile $src $dest
    $backups = @(Get-ChildItem -LiteralPath $prefix -Filter 'publisher.key.bak-*' -File)
    Assert ($line -like '*previous one kept as publisher.key.bak-*') "different file: $line"
    Assert ($backups.Count -eq 1 -and $null -eq (Test-PrivateAcl $backups[0].FullName)) 'the backup exists and is private'
    Assert ($null -eq $script:copies[-1].Problem) 'the replacement was also written into a private folder'
    Assert ((Get-Sha256Hex $dest) -eq (Get-Sha256Hex $src)) 'the replacement has the new content'

    # A staging folder left by an interrupted install is removed.
    $stale = Join-Path $prefix "${StagingPrefix}99999"
    New-Item -ItemType Directory -Force -Path $stale | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $stale 'publisher.key'), 'x')
    Remove-StaleStaging $prefix
    Assert (-not (Test-Path -LiteralPath $stale)) 'Remove-StaleStaging removes a leftover staging folder'

    # ------------------------------------------------------------ SHA256SUMS
    $sb = Join-Path $root 'sums bundle'
    New-Item -ItemType Directory -Force -Path (Join-Path $sb 'bin') | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $sb 'bin\a.exe'), 'a')
    [System.IO.File]::WriteAllText((Join-Path $sb 'Install.ps1'), 'b')
    $sumsPath = Join-Path $sb 'SHA256SUMS'
    [System.IO.File]::WriteAllText($sumsPath, "$(Get-Sha256Hex (Join-Path $sb 'bin\a.exe'))  bin/a.exe`n$(Get-Sha256Hex (Join-Path $sb 'Install.ps1'))  Install.ps1`n")
    Assert (@(Test-Sums $sb $sumsPath).Count -eq 0) 'Test-Sums: matching files pass'
    Assert (@(Get-UnlistedFiles $sb $sumsPath 'bin').Count -eq 0) 'Get-UnlistedFiles: nothing unlisted in bin'
    [System.IO.File]::WriteAllText((Join-Path $sb 'bin\version.dll'), 'x')
    $unlisted = @(Get-UnlistedFiles $sb $sumsPath 'bin')
    Assert ($unlisted.Count -eq 1 -and $unlisted[0] -eq 'bin\version.dll') "Get-UnlistedFiles finds a DLL dropped into bin ($($unlisted -join ', '))"
    [System.IO.File]::WriteAllText((Join-Path $sb 'Install.ps1'), 'c')
    $bad = @(Test-Sums $sb $sumsPath)
    Assert ($bad.Count -eq 1 -and $bad[0] -eq 'Install.ps1 differs') "Test-Sums: a changed file is caught ($($bad -join '; '))"

    # ------------------------------------------------------------ Get-ConfigProblem
    $cfg = Join-Path $prefix 'publish.json'
    function Fake([string[]]$Lines, [int]$Code = 0) {
        return [pscustomobject]@{ Code = $Code; Lines = $Lines; Text = ($Lines -join "`n") }
    }
    $okRun = Fake @('cc-publish 0.1.0', "config      $cfg", 'alias       test (user cc-sessions.pub-test)')
    Assert ($null -eq (Get-ConfigProblem $okRun $cfg)) 'a loaded config is no problem'
    Assert ($null -eq (Get-ConfigProblem $okRun $cfg.ToUpperInvariant())) 'the path compares case-insensitively'
    foreach ($bad in @("config $cfg is too large", "cannot read config ${cfg}: not found", 'invalid config: expected value at line 1',
            'invalid alias "A": bad', 'invalid servers: empty')) {
        $r = Fake @('cc-publish 0.1.0', "config      $bad", 'hint        cc-publish init --alias <alias>') 1
        Assert ((Get-ConfigProblem $r $cfg) -eq $bad) "caught: $bad"
    }
    Assert ($null -ne (Get-ConfigProblem (Fake @('something else') 2) $cfg)) 'output without a config row is a problem'

    if (-not $PublishExe) {
        # Cargo's target dir is outside the repository (see .cargo\config.toml);
        # ask the shared helper rather than assuming target\ next to it.
        $targetDir = $null
        try { $targetDir = Get-CargoTargetDir } catch { $targetDir = $null }
        if ($targetDir) {
            # The newer of release\ and debug\: a stale release build would
            # test an older config parser.
            $newest = @('release', 'debug') | ForEach-Object { Join-Path $targetDir "$_\cc-publish.exe" } |
                Where-Object { Test-Path -LiteralPath $_ } | Get-Item | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
            if ($newest) { $PublishExe = $newest.FullName }
        }
    }
    if ($PublishExe) {
        $data = Join-Path $root 'data dir'
        New-Item -ItemType Directory -Force -Path $data | Out-Null
        $cases = [ordered]@{
            'valid'     = @{ Text = "{`n  `"alias`": `"test`",`n  `"servers`": [`"wss://connect.ngs.global`"],`n  `"log_file`": true`n}`n"; Want = $null }
            # What Install.ps1 writes: the key files named too.
            'key files' = @{ Text = "{`n  `"alias`": `"test`",`n  `"creds`": `"nats/x.creds`",`n  `"publisher_key`": `"keys/publisher.key`",`n  `"viewer_pub`": `"keys/viewer.pub`",`n  `"log_file`": true`n}`n"; Want = $null }
            # A bundle made with -Bucket: Install.ps1 keeps the member.
            'bucket'    = @{ Text = "{`n  `"alias`": `"test`",`n  `"servers`": [`"wss://connect.ngs.global`"],`n  `"bucket`": `"other_bucket`",`n  `"log_file`": true`n}`n"; Want = $null }
            'bad bucket' = @{ Text = '{"alias": "test", "bucket": "a.b"}'; Want = 'invalid bucket' }
            # A "key" member is unknown, like any other.
            'key member' = @{ Text = "{`n  `"alias`": `"test`",`n  `"creds`": `"nats/x.creds`",`n  `"key`": `"key`",`n  `"publisher_key`": `"keys/publisher.key`",`n  `"viewer_pub`": `"keys/viewer.pub`",`n  `"log_file`": true`n}`n"; Want = 'unknown field' }
            'too large' = @{ Text = '{"alias": "test"' + (' ' * 70000) + '}'; Want = 'too large' }
            'not JSON'  = @{ Text = '{"alias": '; Want = 'invalid config' }
            'unknown'   = @{ Text = '{"alias": "test", "user": "x"}'; Want = 'invalid config' }
            'alias'     = @{ Text = '{"alias": "Not.Valid"}'; Want = 'invalid alias' }
            'missing'   = @{ Text = $null; Want = 'cannot read' }
        }
        foreach ($name in $cases.Keys) {
            $c = $cases[$name]
            $path = Join-Path $data "publish-$($name -replace ' ', '-').json"
            if ($null -ne $c.Text) { [System.IO.File]::WriteAllText($path, $c.Text) }
            $r = Invoke-Exe $PublishExe @('--data-dir', $data, 'status', '--config', $path)
            $p = Get-ConfigProblem $r $path
            if ($null -eq $c.Want) { Assert ($null -eq $p) "real cc-publish, $name config: accepted ($p)" }
            else { Assert ($null -ne $p -and $p -like "*$($c.Want)*") "real cc-publish, $name config: $p" }
        }
    } else {
        Skip-Check 'real cc-publish config checks (no release\ or debug\cc-publish.exe in the cargo target dir; pass -PublishExe)'
    }

    # ------------------------------------------------------------ setup.cmd
    $fake = Join-Path $root 'fake bundle'
    New-Item -ItemType Directory -Force -Path $fake | Out-Null
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath (Join-Path $bundleSrc 'setup.cmd') -Destination $fake
    $argsLog = Join-Path $root 'args.log'
    foreach ($s in 'Install', 'Verify', 'Uninstall') {
        # Each stub logs its name and arguments, one per line.
        [System.IO.File]::WriteAllText((Join-Path $fake "$s.ps1"), "Add-Content -LiteralPath '$argsLog' -Value ('$s ' + (`$args -join '|'))`nexit 0`n")
    }
    $env:CCU_NO_PAUSE = '1'
    try {
        $out = & cmd.exe /d /c "`"$(Join-Path $fake 'setup.cmd')`" -Prefix `"C:\some dir\cc-usage`" -SkipAppCheck -Force" 2>&1
        $rc = $LASTEXITCODE
        $log = @(Get-Content -LiteralPath $argsLog)
        Assert ($rc -eq 0) "setup.cmd exits 0 ($rc)"
        Assert ($log.Count -eq 2 -and $log[0] -eq 'Install -Prefix|C:\some dir\cc-usage|-SkipAppCheck|-Force') "Install.ps1 got the arguments: $($log[0])"
        Assert ($log.Count -eq 2 -and $log[1] -eq 'Verify -Prefix|C:\some dir\cc-usage|-SkipAppCheck|-Force') "Verify.ps1 got the same arguments: $(if ($log.Count -gt 1) { $log[1] })"
        Remove-Item -LiteralPath $argsLog
        $out = & cmd.exe /d /c "`"$(Join-Path $fake 'setup.cmd')`" uninstall -Purge" 2>&1
        $log = @(Get-Content -LiteralPath $argsLog)
        Assert ($log.Count -eq 1 -and $log[0] -eq 'Uninstall -Purge') "setup.cmd uninstall runs only Uninstall.ps1: $($log -join ' / ')"
        Remove-Item -LiteralPath $argsLog
        $out = & cmd.exe /d /c "`"$(Join-Path $fake 'setup.cmd')`" rotate-key -ConfirmAlias lab-01" 2>&1
        $log = @(Get-Content -LiteralPath $argsLog)
        Assert ($log.Count -eq 2 -and $log[0] -eq 'Install -RotateKey|-ConfirmAlias|lab-01') "setup.cmd rotate-key runs Install.ps1 -RotateKey: $($log -join ' / ')"
        Assert ($log.Count -eq 2 -and $log[1] -eq 'Verify -ConfirmAlias|lab-01') "then Verify.ps1 with the same arguments: $($log -join ' / ')"
        Remove-Item -LiteralPath $argsLog

        # With a SHA256SUMS, setup.cmd checks it before it unblocks or runs
        # anything: a matching bundle installs, a changed file stops it.
        $sumsText = New-Object System.Text.StringBuilder
        foreach ($f in @(Get-ChildItem -LiteralPath $fake -File | Sort-Object Name)) {
            [void]$sumsText.Append("$(Get-Sha256Hex $f.FullName)  $($f.Name)`n")
        }
        [System.IO.File]::WriteAllText((Join-Path $fake 'SHA256SUMS'), $sumsText.ToString())
        $out = & cmd.exe /d /c "`"$(Join-Path $fake 'setup.cmd')`" -Force" 2>&1
        $rc = $LASTEXITCODE
        $log = @(if (Test-Path -LiteralPath $argsLog) { Get-Content -LiteralPath $argsLog })
        Assert ($rc -eq 0 -and $log.Count -eq 2 -and $log[0] -eq 'Install -Force') "a bundle that matches SHA256SUMS installs ($rc): $($log -join ' / ')"
        if (Test-Path -LiteralPath $argsLog) { Remove-Item -LiteralPath $argsLog }
        Add-Content -LiteralPath (Join-Path $fake 'Install.ps1') -Value '# changed'
        $out = & cmd.exe /d /c "`"$(Join-Path $fake 'setup.cmd')`" -Force" 2>&1
        $rc = $LASTEXITCODE
        Assert ($rc -ne 0) "a changed file makes setup.cmd fail ($rc)"
        Assert (-not (Test-Path -LiteralPath $argsLog)) 'a changed file stops setup.cmd before it runs any script'
        Assert ((@($out) -join "`n") -match 'differs: Install\.ps1') 'setup.cmd names the file that differs'
        $out = & cmd.exe /d /c "`"$(Join-Path $fake 'setup.cmd')`" uninstall" 2>&1
        Assert (-not (Test-Path -LiteralPath $argsLog)) 'a changed file stops setup.cmd uninstall too'
        Remove-Variable out
    } finally {
        Remove-Item Env:\CCU_NO_PAUSE -ErrorAction SilentlyContinue
    }

    # Every Install.ps1 parameter is one Verify.ps1 accepts, so setup.cmd can
    # pass them on.
    function Get-ScriptParams([string]$Path) {
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($Path, [ref]$tokens, [ref]$errors)
        return @($ast.ParamBlock.Parameters | ForEach-Object { $_.Name.VariablePath.UserPath })
    }
    $installParams = Get-ScriptParams (Join-Path $bundleSrc 'Install.ps1')
    $verifyParams = Get-ScriptParams (Join-Path $bundleSrc 'Verify.ps1')
    $missing = @($installParams | Where-Object { $verifyParams -notcontains $_ })
    Assert ($missing.Count -eq 0) "Verify.ps1 accepts every Install.ps1 parameter (missing: $($missing -join ', '))"

    # ------------------------------------------------------------ identity
    # The key pair is the machine's identity: replacing it needs the alias.
    Assert ((Get-IdentityWarning 'lab-01') -eq "This replaces lab-01's identity. The viewer will hold back its values until you accept the new key there.") 'the identity warning'
    $note = Get-AcceptNote 'lab-01' ('p1:BBBBBBBB' + ('C' * 34) + 'A=')
    Assert ($note -eq "on the viewer, accept this key when cc-usage asks about 'lab-01': key p1, fingerprint BBBBBBBB") "the accept note names the key id and fingerprint the viewer shows: $note"
    $thrown = $null
    try { Confirm-IdentityChange 'lab-01' 'lab-01' $false 'Hint.' } catch { $thrown = $_.Exception.Message }
    Assert ($null -eq $thrown) "-ConfirmAlias with the alias confirms ($thrown)"
    $thrown = $null
    try { Confirm-IdentityChange 'lab-01' 'lab-02' $true 'Hint.' } catch { $thrown = $_.Exception.Message }
    Assert ($thrown -like "not confirmed: 'lab-02' is not this machine's alias 'lab-01'. Hint.") "the wrong alias is refused without a prompt: $thrown"
    $thrown = $null
    try { Confirm-IdentityChange 'lab-01' '' $false 'Hint.' } catch { $thrown = $_.Exception.Message }
    Assert ($thrown -like '*pass -ConfirmAlias lab-01*Hint.') "no -ConfirmAlias and no console: refused with the hint: $thrown"

    $idPrefix = Join-Path $root 'identity prefix'
    New-Item -ItemType Directory -Force -Path $idPrefix | Out-Null
    Assert ((Get-IdentityPlan $idPrefix 'not-run.exe').Action -eq 'first') 'no key and no install: a first install makes one'
    [System.IO.File]::WriteAllText((Join-Path $idPrefix 'publish.json'), '{}')
    Assert ((Get-IdentityPlan $idPrefix 'not-run.exe').Action -eq 'new') 'no key, but publish.json: a lost identity'
    Remove-Item -LiteralPath (Join-Path $idPrefix 'publish.json')
    [System.IO.File]::WriteAllText((Join-Path $idPrefix $ManifestName), '{}')
    Assert ((Get-IdentityPlan $idPrefix 'not-run.exe').Action -eq 'new') 'no key, but the manifest: a lost identity'

    # With a cc-publish.exe that knows keygen --confirm: the plan of an
    # existing key, and Install.ps1 -RotateKey on a fake install whose
    # manifest records no autostart (so no task or Run entry is touched).
    $confirmSupported = $false
    if ($PublishExe) { $confirmSupported = ((Invoke-Exe $PublishExe @('keygen', '--confirm', 'x', '--help')).Code -eq 0) }
    if ($confirmSupported) {
        $exe = Join-Path $idPrefix 'cc-publish.exe'
        Microsoft.PowerShell.Management\Copy-Item -LiteralPath $PublishExe -Destination $exe
        $keysDir = Join-Path $idPrefix 'keys'
        New-Item -ItemType Directory -Force -Path $keysDir | Out-Null
        $idKey = Join-Path $keysDir 'publisher.key'
        $r = Invoke-Exe $exe @('--data-dir', $idPrefix, 'keygen', '--key', $idKey)
        $firstLine = Get-PublicLine $r
        Assert ($r.Code -eq 0 -and $firstLine) "a first key ($(Get-LastLine $r))"
        $plan = Get-IdentityPlan $idPrefix $exe
        Assert ($plan.Action -eq 'keep' -and $plan.Line -eq $firstLine) "a key that loads is kept ($($plan.Action), $($plan.Line))"
        $idConfig = Join-Path $idPrefix 'publish.json'
        $cfgObj = [ordered]@{ alias = 'rot-test'; creds = (Join-Path $idPrefix 'nats\x.creds'); publisher_key = $idKey; viewer_pub = (Join-Path $keysDir 'viewer.pub'); log_file = $true }
        Write-TextFile $idConfig (($cfgObj | ConvertTo-Json) + "`n")
        $man = [ordered]@{
            format = 1; alias = 'rot-test'; autostart = 'none'; task_name = "cc-usage test $PID"
            files = [ordered]@{ 'keys\publisher.key' = (Get-Sha256Hex $idKey); 'publish.json' = (Get-Sha256Hex $idConfig) }
        }
        Write-TextFile (Join-Path $idPrefix $ManifestName) (($man | ConvertTo-Json -Depth 4) + "`n")
        $psExe = (Get-Process -Id $PID).Path
        $installPs1 = Join-Path $bundleSrc 'Install.ps1'
        function Invoke-Rotate([string[]]$Extra) {
            return (Invoke-Exe $psExe (@('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $installPs1, '-RotateKey', '-Prefix', $idPrefix) + $Extra))
        }
        function Get-KeyCount { return @([System.IO.File]::ReadAllLines($idKey) | Where-Object { $_.Trim() -and -not $_.StartsWith('#') }).Count }
        $before = Get-Sha256Hex $idKey

        $r = Invoke-Rotate @()
        Assert ($r.Code -eq 1 -and $r.Text.Contains((Get-IdentityWarning 'rot-test')) -and $r.Text.Contains('-ConfirmAlias rot-test')) "-RotateKey without -ConfirmAlias (no console) is refused with the warning ($($r.Code)): $(Get-LastLine $r)"
        Assert ((Get-Sha256Hex $idKey) -eq $before) 'the refused rotation changed nothing'
        $r = Invoke-Rotate @('-ConfirmAlias', 'other')
        Assert ($r.Code -eq 1 -and $r.Text.Contains("'other' is not this machine's alias 'rot-test'")) "-RotateKey with the wrong alias is refused: $(Get-LastLine $r)"
        Assert ((Get-Sha256Hex $idKey) -eq $before) 'the wrong alias changed nothing'

        $r = Invoke-Rotate @('-ConfirmAlias', 'rot-test')
        $now = Get-PublicLine (Invoke-Exe $exe @('keygen', '--if-missing', '--key', $idKey))
        Assert ($r.Code -eq 0) "-RotateKey -ConfirmAlias rot-test succeeds ($($r.Code)): $(Get-LastLine $r)"
        Assert ($r.Text.Contains("this machine's new public key: $now") -and $now -ne $firstLine) "it prints the new public key line ($now)"
        Assert ((Get-KeyCount) -eq 2) 'the new key goes in front of the old one (--rotate)'
        Assert ([string](Read-Json (Join-Path $idPrefix $ManifestName)).files.'keys\publisher.key' -eq (Get-Sha256Hex $idKey)) 'the manifest records the new key'
        Assert ($null -eq (Test-PrivateAcl $idKey)) 'the rotated key is private'

        [System.IO.File]::WriteAllText($idKey, "not a key`n")
        Assert ((Get-IdentityPlan $idPrefix $exe).Action -eq 'replace') 'a key that does not load is to be replaced'
        $r = Invoke-Rotate @('-ConfirmAlias', 'rot-test')
        Assert ($r.Code -eq 0 -and (Get-KeyCount) -eq 1) "a key that does not load is replaced by a single new key: $(Get-LastLine $r)"
        Assert (@(Get-ChildItem -LiteralPath $keysDir -Filter 'publisher.key.bak-*').Count -eq 1) 'and kept as a backup'

        Remove-Item -LiteralPath $idKey
        $r = Invoke-Rotate @('-ConfirmAlias', 'rot-test')
        Assert ($r.Code -eq 0 -and (Test-Path -LiteralPath $idKey)) "a missing key is made anew: $(Get-LastLine $r)"

        $empty = Join-Path $root 'no install'
        New-Item -ItemType Directory -Force -Path $empty | Out-Null
        $r = Invoke-Exe $psExe @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $installPs1, '-RotateKey', '-Prefix', $empty, '-ConfirmAlias', 'rot-test')
        Assert ($r.Code -eq 1 -and $r.Text.Contains('no cc-usage publisher installed')) "-RotateKey without an install is refused: $(Get-LastLine $r)"
    } else {
        Skip-Check 'the identity checks with a real cc-publish (none built that knows keygen --confirm; pass -PublishExe)'
    }

    # ------------------------------------------------------------ Claude Code's folder
    # Get-DefaultSettings follows CLAUDE_CONFIG_DIR like cc-hook; only this
    # process's environment changes, and it is restored.
    $savedClaude = $env:CLAUDE_CONFIG_DIR
    try {
        $alt = Join-Path $root 'claude alt'
        $env:CLAUDE_CONFIG_DIR = $alt
        Assert ((Get-DefaultSettings) -eq (Join-Path $alt 'settings.json')) "Get-DefaultSettings uses CLAUDE_CONFIG_DIR ($(Get-DefaultSettings))"
        Assert (@(Get-SettingsArgs (Join-Path $alt 'settings.json')).Count -eq 0) 'Get-SettingsArgs: no --settings for the CLAUDE_CONFIG_DIR file'
        $homeSettings = Join-Path $env:USERPROFILE '.claude\settings.json'
        Assert ((@(Get-SettingsArgs $homeSettings) -join '|') -eq "--settings|$homeSettings") 'Get-SettingsArgs: --settings for ~/.claude when CLAUDE_CONFIG_DIR is elsewhere'
        Remove-Item Env:\CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue
        Assert ((Get-DefaultSettings) -eq $homeSettings) "Get-DefaultSettings without CLAUDE_CONFIG_DIR is %USERPROFILE%\.claude\settings.json ($(Get-DefaultSettings))"
    } finally {
        if ($null -ne $savedClaude) { $env:CLAUDE_CONFIG_DIR = $savedClaude } else { Remove-Item Env:\CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue }
    }

    # ------------------------------------------------------------ install transaction
    # The rollback of a failed install, against stubs: every scheduled task
    # cmdlet, the Run entry helpers, Start-Process and Wait-PublisherStart
    # are replaced by functions below (a function takes precedence over a
    # cmdlet, and a later definition over common.ps1's), so no real task or
    # registry value is read or written. Files live under the test folder.
    $script:fakeTasks = @{}
    $script:fakeRun = @{}
    $script:calls = New-Object System.Collections.Generic.List[string]
    $script:failEnable = $false
    $script:failExport = $false
    function New-FakeTaskXml([string]$Arguments, [string]$State) { return "ARGS=$Arguments;STATE=$State" }
    function Get-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName, [string]$TaskPath)
        if (-not $script:fakeTasks.ContainsKey($TaskName)) { throw "stub: no task $TaskName" }
        $t = $script:fakeTasks[$TaskName]
        return [pscustomobject]@{ TaskName = $TaskName; State = $t.State; Actions = @([pscustomobject]@{ Execute = 'conhost.exe'; Arguments = $t.Arguments }) }
    }
    function Export-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName, [string]$TaskPath)
        if ($script:failExport) { throw 'stub: export refused' }
        return $script:fakeTasks[$TaskName].Xml
    }
    function Register-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName, [string]$TaskPath, [string]$Xml, [switch]$Force)
        $script:calls.Add("register $TaskName")
        $state = if ($Xml -match ';STATE=(.*)$') { $Matches[1] } else { 'Ready' }
        $arguments = if ($Xml -match '^ARGS=(.*);STATE=') { $Matches[1] } else { '' }
        $script:fakeTasks[$TaskName] = @{ Xml = $Xml; Arguments = $arguments; State = $state }
    }
    function Unregister-ScheduledTask {
        [CmdletBinding(SupportsShouldProcess = $true)]
        param([string]$TaskName, [string]$TaskPath)
        $script:calls.Add("unregister $TaskName")
        $script:fakeTasks.Remove($TaskName)
    }
    function Disable-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName, [string]$TaskPath)
        $script:calls.Add("disable $TaskName")
        $script:fakeTasks[$TaskName].State = 'Disabled'
    }
    function Enable-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName, [string]$TaskPath)
        if ($script:failEnable) { throw 'stub: enable refused' }
        $script:calls.Add("enable $TaskName")
        $script:fakeTasks[$TaskName].State = 'Ready'
    }
    function Start-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName, [string]$TaskPath)
        $script:calls.Add("start $TaskName")
    }
    function Stop-ScheduledTask {
        [CmdletBinding()]
        param([string]$TaskName, [string]$TaskPath)
        $script:calls.Add("stop $TaskName")
    }
    function Start-Process {
        [CmdletBinding()]
        param([string]$FilePath, [string]$ArgumentList, [string]$WorkingDirectory, [string]$WindowStyle)
        $script:calls.Add("process $ArgumentList")
    }
    function Get-RunEntry([string]$Name) { if ($script:fakeRun.ContainsKey($Name)) { return $script:fakeRun[$Name] }; return $null }
    function Set-RunEntry([string]$Name, [string]$Value) { $script:calls.Add("run set $Name"); $script:fakeRun[$Name] = $Value }
    function Remove-RunEntry([string]$Name) { $script:calls.Add("run remove $Name"); $script:fakeRun.Remove($Name) }
    function Wait-PublisherStart([string]$Prefix, [int]$Seconds) { $script:calls.Add('wait'); return [pscustomobject]@{ Id = 4242 } }
    # Refuse to go on unless every stub is the one that runs.
    $stubbed = @('Get-ScheduledTask', 'Export-ScheduledTask', 'Register-ScheduledTask', 'Unregister-ScheduledTask', 'Disable-ScheduledTask',
        'Enable-ScheduledTask', 'Start-ScheduledTask', 'Stop-ScheduledTask', 'Start-Process')
    $real = @($stubbed | Where-Object { (Get-Command $_).CommandType -ne 'Function' })
    if ($real.Count -gt 0) { throw "the task stubs are not in effect ($($real -join ', ')); stopping before anything real is touched" }
    Assert ($real.Count -eq 0) 'the scheduled task cmdlets are stubbed'

    $tn = "cc-usage test $PID"
    $txPrefix = Join-Path $root 'tx prefix'
    $txClaude = Join-Path $root 'tx claude'
    $txSettings = Join-Path $txClaude 'settings.json'
    $txKey = Join-Path $txPrefix 'keys\publisher.key'
    $txCreds = Join-Path $txPrefix 'nats\x.creds'
    $txCopy = Join-Path $txPrefix 'bundle'
    $txPaths = @((Join-Path $txPrefix 'cc-hook.exe'), (Join-Path $txPrefix 'cc-publish.exe'), $txCreds, (Join-Path $txPrefix 'keys\viewer.pub'),
        (Join-Path $txPrefix 'publish.json'), (Join-Path $txPrefix $ManifestName), (Join-Path $txPrefix 'statusline-chain.json'), $txSettings,
        (Join-Path $txCopy $MarkerRel), (Join-Path $txCopy 'Install.ps1'), (Join-Path $txCopy 'Uninstall.ps1'))
    $taskArgs = "--headless `"$(Join-Path $txPrefix 'cc-publish.exe')`" run"
    $taskXml = New-FakeTaskXml $taskArgs 'Ready'
    # A previous install: its files, its manifest (autostart $Kind) and,
    # for a task, its task.
    function New-PreviousInstall([string]$Kind) {
        if (Test-Path -LiteralPath $txPrefix) { Microsoft.PowerShell.Management\Remove-Item -LiteralPath $txPrefix -Recurse -Force }
        foreach ($d in $txPrefix, (Join-Path $txPrefix 'nats'), (Join-Path $txPrefix 'keys'), $txCopy, $txClaude) {
            New-Item -ItemType Directory -Force -Path $d | Out-Null
        }
        Set-PrivateAcl (Join-Path $txPrefix 'nats')
        [System.IO.File]::WriteAllText((Join-Path $txPrefix 'cc-hook.exe'), 'old hook')
        [System.IO.File]::WriteAllText((Join-Path $txPrefix 'cc-publish.exe'), 'old publish')
        [System.IO.File]::WriteAllText($txCreds, 'old creds')
        Set-PrivateAcl $txCreds
        [System.IO.File]::WriteAllText((Join-Path $txPrefix 'keys\viewer.pub'), 'old viewer')
        [System.IO.File]::WriteAllText($txKey, 'p1:old')
        [System.IO.File]::WriteAllText((Join-Path $txPrefix 'publish.json'), 'old config')
        [System.IO.File]::WriteAllText($txSettings, 'old settings')
        [System.IO.File]::WriteAllText((Join-Path $txCopy 'Install.ps1'), 'old tooling')
        $m = [ordered]@{ format = 1; autostart = $Kind; task_name = $tn; files = [ordered]@{ 'keys\publisher.key' = (Get-Sha256Hex $txKey) } }
        Write-TextFile (Join-Path $txPrefix $ManifestName) (($m | ConvertTo-Json -Depth 4) + "`n")
        $script:fakeTasks.Clear()
        $script:fakeRun.Clear()
        $script:calls.Clear()
        if ($Kind -eq 'task') { $script:fakeTasks[$tn] = @{ Xml = $taskXml; Arguments = $taskArgs; State = 'Ready' } }
        if ($Kind -eq 'runkey') { $script:fakeRun[$tn] = 'previous launch line' }
    }
    # What a failing install may have done by then.
    function Set-NewFiles {
        [System.IO.File]::WriteAllText((Join-Path $txPrefix 'cc-hook.exe'), 'new hook')
        [System.IO.File]::WriteAllText((Join-Path $txPrefix 'cc-publish.exe'), 'new publish')
        [System.IO.File]::WriteAllText($txCreds, 'new creds')
        [System.IO.File]::WriteAllText((Join-Path $txPrefix 'publish.json'), 'new config')
        [System.IO.File]::WriteAllText((Join-Path $txPrefix 'statusline-chain.json'), 'new chain')
        [System.IO.File]::WriteAllText($txSettings, 'new settings')
        [System.IO.File]::WriteAllText((Join-Path $txCopy 'Install.ps1'), 'new tooling')
        [System.IO.File]::WriteAllText((Join-Path $txCopy 'Uninstall.ps1'), 'new tooling')
        [System.IO.File]::WriteAllText($txKey, 'p2:new')
    }
    function Get-Text([string]$Path) { return [System.IO.File]::ReadAllText($Path) }

    # A failure after the files and the autostart changed: everything back.
    New-PreviousInstall 'task'
    $tx = New-InstallTransaction $txPrefix $txPaths @($tn, $tn) @($txKey)
    Assert ((Test-Path -LiteralPath $tx.Dir) -and $null -eq (Test-PrivateAcl $tx.Dir)) "the copies go into a private folder ($(Split-Path -Leaf $tx.Dir))"
    Assert ($tx.Tasks.Count -eq 1 -and $tx.Tasks[0].Xml -eq $taskXml) 'the task is saved as XML (once per name)'
    Suspend-PublisherTasks $tx
    Assert ($script:fakeTasks[$tn].State -eq 'Disabled' -and $tx.Tasks[0].Disabled) 'Suspend-PublisherTasks disables the enabled task'
    Set-NewFiles
    $tx.AutostartTouched = $true
    Register-ScheduledTask -TaskName $tn -TaskPath '\' -Xml (New-FakeTaskXml $taskArgs 'Ready-new') -Force
    Set-RunEntry $tn 'new launch line'
    $u = Undo-InstallTransaction $tx 1
    Assert ($u.Failed.Count -eq 0) "the rollback completes ($($u.Failed -join '; '))"
    Assert ((Get-Text (Join-Path $txPrefix 'cc-hook.exe')) -eq 'old hook' -and (Get-Text (Join-Path $txPrefix 'cc-publish.exe')) -eq 'old publish') 'the previous binaries are back'
    Assert ((Get-Text (Join-Path $txPrefix 'publish.json')) -eq 'old config' -and (Get-Text $txSettings) -eq 'old settings') 'the previous config and settings.json are back'
    Assert ((Get-Text $txCreds) -eq 'old creds' -and $null -eq (Test-PrivateAcl $txCreds)) 'the previous creds are back, private'
    Assert ((Get-Text (Join-Path $txCopy 'Install.ps1')) -eq 'old tooling') 'the previous tooling copy is back'
    Assert (-not (Test-Path -LiteralPath (Join-Path $txCopy 'Uninstall.ps1')) -and -not (Test-Path -LiteralPath (Join-Path $txPrefix 'statusline-chain.json'))) 'files that were not there before are removed'
    Assert ((Get-Text $txKey) -eq 'p2:new') 'the key pair is not rolled back'
    Assert ([string](Read-Json (Join-Path $txPrefix $ManifestName)).files.'keys\publisher.key' -eq (Get-Sha256Hex $txKey)) 'the restored manifest records the kept key'
    Assert ($script:fakeTasks[$tn].Xml -eq $taskXml -and $script:fakeTasks[$tn].State -eq 'Ready') 'the previous task is registered again, enabled'
    Assert (-not $script:fakeRun.ContainsKey($tn)) 'the Run entry the install added is removed'
    Assert ($script:calls.Contains("start $tn") -and $script:calls.Contains('wait')) "the previous publisher is started by its task ($($script:calls -join ', '))"
    Assert (-not (Test-Path -LiteralPath $tx.Dir) -and $tx.Completed) 'a complete rollback drops the copies'
    $left = @(Get-ChildItem -LiteralPath $txPrefix -Recurse -Force -File | Where-Object { $_.Name -match '\.(old-|rollback-)' })
    Assert ($left.Count -eq 0) "nothing is left behind ($(($left | ForEach-Object { $_.Name }) -join ', '))"

    # A failure before the autostart changed: the task is only enabled again.
    New-PreviousInstall 'task'
    $tx = New-InstallTransaction $txPrefix $txPaths @($tn) @($txKey)
    Suspend-PublisherTasks $tx
    [System.IO.File]::WriteAllText((Join-Path $txPrefix 'cc-publish.exe'), 'new publish')
    $u = Undo-InstallTransaction $tx 1
    Assert ($u.Failed.Count -eq 0 -and -not $script:calls.Contains("register $tn")) 'before the autostart step, the task is not registered again'
    Assert ($script:fakeTasks[$tn].State -eq 'Ready' -and $script:calls.Contains("enable $tn") -and $script:calls.Contains("start $tn")) "the disabled task is enabled again and started ($($script:calls -join ', '))"
    Assert ((Get-Text (Join-Path $txPrefix 'cc-publish.exe')) -eq 'old publish') 'the previous cc-publish.exe is back'

    # A task the user disabled stays disabled, and is not started.
    New-PreviousInstall 'task'
    $script:fakeTasks[$tn].State = 'Disabled'
    $tx = New-InstallTransaction $txPrefix $txPaths @($tn) @($txKey)
    Suspend-PublisherTasks $tx
    Assert (-not $tx.Tasks[0].Disabled -and -not $script:calls.Contains("disable $tn")) 'a disabled task is not disabled again'
    $u = Undo-InstallTransaction $tx 1
    Assert ($script:fakeTasks[$tn].State -eq 'Disabled' -and -not $script:calls.Contains("start $tn")) "it stays disabled and is not started ($($script:calls -join ', '))"

    # A Run entry install, moved to a task by the failing install.
    New-PreviousInstall 'runkey'
    $tx = New-InstallTransaction $txPrefix $txPaths @($tn) @($txKey)
    $tx.AutostartTouched = $true
    Register-ScheduledTask -TaskName $tn -TaskPath '\' -Xml $taskXml -Force
    Remove-RunEntry $tn
    $u = Undo-InstallTransaction $tx 1
    Assert ($u.Failed.Count -eq 0 -and -not $script:fakeTasks.ContainsKey($tn)) "the task the install registered is removed ($($u.Failed -join '; '))"
    Assert ($script:fakeRun[$tn] -eq 'previous launch line') 'the previous Run entry is back'
    Assert (@($script:calls | Where-Object { $_ -like 'process *' }).Count -eq 1) "the previous publisher is started like its Run entry does ($($script:calls -join ', '))"

    # A step that fails is reported; the others still run, the copies stay.
    New-PreviousInstall 'task'
    $tx = New-InstallTransaction $txPrefix $txPaths @($tn) @($txKey)
    Suspend-PublisherTasks $tx
    Set-NewFiles
    $script:failEnable = $true
    $u = $null
    $thrown = $null
    try { $u = Undo-InstallTransaction $tx 1 } catch { $thrown = $_.Exception.Message }
    $script:failEnable = $false
    Assert ($null -eq $thrown -and $null -ne $u) "Undo-InstallTransaction does not throw ($thrown)"
    Assert ($null -ne $u -and $u.Failed.Count -eq 1 -and $u.Failed[0] -like "*'$tn'*enable refused*") "the failed step is reported ($(if ($u) { $u.Failed -join '; ' }))"
    Assert ((Get-Text (Join-Path $txPrefix 'cc-publish.exe')) -eq 'old publish' -and (Get-Text $txCreds) -eq 'old creds') 'the files are restored all the same'
    Assert ((Test-Path -LiteralPath $tx.Dir) -and (Get-Text (Join-Path $tx.Dir '001-cc-publish.exe')) -eq 'old publish') 'the copies are kept when the rollback is incomplete'
    $markerPath = Join-Path $tx.Dir $RollbackMarkerName
    $markerText = if (Test-Path -LiteralPath $markerPath) { Get-Text $markerPath } else { '' }
    Assert ($markerText.Contains("001-cc-publish.exe  ->  $(Join-Path $txPrefix 'cc-publish.exe')") -and $markerText.Contains($txCreds)) "the kept copies are marked ($RollbackMarkerName), with where each one goes"
    Assert ($markerText.Contains((Join-Path $txCopy 'Uninstall.ps1')) -and $markerText.Contains('enable refused')) 'the marker names the files that were new and what failed'
    $incomplete = @(Get-IncompleteRollback $txPrefix)
    Assert ($incomplete.Count -eq 1 -and $incomplete[0] -eq $tx.Dir) "Get-IncompleteRollback finds it ($($incomplete -join ', '))"
    $removed = @(Remove-StaleRollback $txPrefix)
    Assert ($removed.Count -eq 0 -and (Test-Path -LiteralPath $markerPath)) 'Remove-StaleRollback keeps the copies of an incomplete rollback'
    $thrown = $null
    try { New-InstallTransaction $txPrefix $txPaths @($tn) @($txKey) | Out-Null } catch { $thrown = $_.Exception.Message }
    Assert ($null -ne $thrown -and $thrown.StartsWith('the rollback of an earlier install did not complete') -and $thrown.Contains($tx.Dir) -and $thrown.Contains("move the folder out of $txPrefix")) "the next install refuses while they are there: $thrown"
    Assert ((Get-Text (Join-Path $tx.Dir '001-cc-publish.exe')) -eq 'old publish' -and @(Get-ChildItem -LiteralPath $txPrefix -Directory -Force -Filter "$RollbackPrefix*").Count -eq 1) 'the refused install keeps them and makes no copies of its own'
    # A leftover without the marker (an install cut short before any
    # rollback) is deleted; -All (Uninstall.ps1) deletes the marked one too.
    $stale = Join-Path $txPrefix "${RollbackPrefix}99999"
    New-Item -ItemType Directory -Force -Path $stale | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $stale '000-x.creds'), 'x')
    $removed = @(Remove-StaleRollback $txPrefix)
    Assert ($removed.Count -eq 1 -and -not (Test-Path -LiteralPath $stale) -and (Test-Path -LiteralPath $tx.Dir)) 'Remove-StaleRollback removes an unmarked leftover rollback folder, and only that'
    $removed = @(Remove-StaleRollback $txPrefix -All)
    Assert ($removed.Count -eq 1 -and -not (Test-Path -LiteralPath $tx.Dir)) 'Remove-StaleRollback -All removes the copies of an incomplete rollback too'
    Assert ($null -ne (New-InstallTransaction $txPrefix $txPaths @($tn) @($txKey))) 'once they are gone, an install can start again'
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath (Join-Path $txPrefix "$RollbackPrefix$PID") -Recurse -Force

    # A first install that fails: nothing of it is left but the key pair.
    $firstPrefix = Join-Path $root 'tx first'
    $script:fakeTasks.Clear()
    $script:fakeRun.Clear()
    $script:calls.Clear()
    $firstPaths = @((Join-Path $firstPrefix 'cc-publish.exe'), (Join-Path $firstPrefix 'publish.json'), (Join-Path $firstPrefix $ManifestName))
    $firstKey = Join-Path $firstPrefix 'keys\publisher.key'
    $tx = New-InstallTransaction $firstPrefix $firstPaths @($tn) @($firstKey)
    Assert ($null -eq $tx.Previous -and $null -eq $tx.Tasks[0].Xml) 'a first install has no previous install or task'
    New-Item -ItemType Directory -Force -Path (Join-Path $firstPrefix 'keys') | Out-Null
    foreach ($f in $firstPaths + @($firstKey)) { [System.IO.File]::WriteAllText($f, 'new') }
    $tx.AutostartTouched = $true
    Register-ScheduledTask -TaskName $tn -TaskPath '\' -Xml (New-FakeTaskXml "--headless `"$(Join-Path $firstPrefix 'cc-publish.exe')`" run" 'Ready') -Force
    $u = Undo-InstallTransaction $tx 1
    Assert ($u.Failed.Count -eq 0 -and @($firstPaths | Where-Object { Test-Path -LiteralPath $_ }).Count -eq 0) "the files of a failed first install are removed ($($u.Failed -join '; '))"
    Assert (-not $script:fakeTasks.ContainsKey($tn) -and -not $script:calls.Contains("start $tn")) 'its task is removed, and nothing is started'
    Assert ((Test-Path -LiteralPath $firstKey)) 'its key pair stays'

    # Complete-InstallTransaction drops the copies; a transaction that cannot
    # be prepared leaves none.
    New-PreviousInstall 'task'
    $tx = New-InstallTransaction $txPrefix $txPaths @($tn) @($txKey)
    Complete-InstallTransaction $tx
    Assert ($tx.Completed -and -not (Test-Path -LiteralPath $tx.Dir)) 'Complete-InstallTransaction drops the copies'
    $script:failExport = $true
    $thrown = $null
    try { New-InstallTransaction $txPrefix $txPaths @($tn) @($txKey) | Out-Null } catch { $thrown = $_.Exception.Message }
    $script:failExport = $false
    Assert ($thrown -like 'could not prepare the rollback*nothing was changed*') "a transaction that cannot be prepared stops the install: $thrown"
    Assert (@(Get-ChildItem -LiteralPath $txPrefix -Directory -Force -Filter "$RollbackPrefix*").Count -eq 0) 'and leaves no copies behind'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host "$($script:passes) passed, $($script:failures) failed"
if ($script:failures -gt 0) { exit 1 }
exit 0
