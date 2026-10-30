# Shared helpers for the cc-usage publisher bundle for Windows: Install.ps1,
# Verify.ps1 and Uninstall.ps1 dot-source this file.
#
# Windows PowerShell 5.1 and PowerShell 7, StrictMode 3. ASCII only, because
# Windows PowerShell reads a script without a BOM in the ANSI code page.
# Never prints secrets: the creds and key files are only copied, hashed and
# have their ACL changed.

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

# Started from PowerShell 7 (a pwsh terminal, or anything it launched),
# Windows PowerShell inherits a PSModulePath that lists PowerShell 7's module
# folders first, autoloads PowerShell 7's Microsoft.PowerShell.Security from
# there, fails, and Get-Acl stops working ("the module could not be loaded").
# Drop PowerShell 7's folders; Windows PowerShell's own ones stay.
if ($PSVersionTable.PSEdition -ne 'Core' -and $env:PSModulePath) {
    $env:PSModulePath = (@($env:PSModulePath -split ';' | Where-Object {
                $_ -and $_ -notmatch '\\PowerShell\\(7[^\\]*\\)?Modules\\?$' -and $_ -notmatch '\\Microsoft\.PowerShell_[^\\]*\\Modules\\?$'
            }) -join ';')
}

$DefaultTaskName = 'cc-usage publisher'
$SystemSid = 'S-1-5-18'
$RunKeyPath = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$StartupApprovedPath = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
$UninstallRoots = @(
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
)
$ManifestName = 'publisher-manifest.json'
$MarkerRel = 'config\installed-copy.json'
# The tooling Install.ps1 copies to <prefix>\bundle (no binaries, no secrets).
$ToolingFiles = @('setup.cmd', 'Install.ps1', 'Verify.ps1', 'Uninstall.ps1', 'README.md',
    'scripts\common.ps1', 'config\bundle.json', 'config\publish.json')
# Files the publisher writes into its data dir at run time.
$RuntimeFiles = @('publish-state.json', 'cc-publish.stop', 'cc-publish.log', 'cc-publish.log.1')
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

# ------------------------------------------------------------------ output

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Line([string]$Message) { Write-Host "  $Message" }
function Write-Note([string]$Message) { Write-Host "WARNING: $Message" -ForegroundColor Yellow }

$Results = New-Object System.Collections.Generic.List[object]

# Add-Result PASS|WARN|FAIL|SKIP name detail
function Add-Result([string]$Status, [string]$Name, [string]$Detail) {
    $Results.Add([pscustomobject]@{ Status = $Status; Name = $Name; Detail = $Detail })
}

# Prints the table and returns the number of FAILs.
function Show-Results([string]$Title) {
    Write-Host ''
    Write-Host $Title
    foreach ($r in $Results) {
        $color = switch ($r.Status) { 'PASS' { 'Green' } 'FAIL' { 'Red' } 'WARN' { 'Yellow' } default { 'Gray' } }
        Write-Host ('  {0,-4}  {1,-22} {2}' -f $r.Status, $r.Name, $r.Detail) -ForegroundColor $color
    }
    $fails = @($Results | Where-Object { $_.Status -eq 'FAIL' }).Count
    $warns = @($Results | Where-Object { $_.Status -eq 'WARN' }).Count
    $passes = @($Results | Where-Object { $_.Status -eq 'PASS' }).Count
    Write-Host ''
    Write-Host "  $passes passed, $warns warnings, $fails failed"
    return $fails
}

# ------------------------------------------------------------------ values

# A property of a JSON object, or $Default when it is missing (StrictMode
# throws on a missing property).
function Get-Prop($Object, [string]$Name, $Default = $null) {
    if ($null -eq $Object) { return $Default }
    $p = $Object.PSObject.Properties[$Name]
    if ($p) { return $p.Value }
    return $Default
}

function Read-Json([string]$Path) {
    $text = [System.IO.File]::ReadAllText($Path)
    return ($text.TrimStart([char]0xFEFF) | ConvertFrom-Json)
}

function Write-TextFile([string]$Path, [string]$Text) {
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    [System.IO.File]::WriteAllText($Path, $Text, $Utf8NoBom)
}

function Get-Sha256Hex([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        return (-join ($sha.ComputeHash($fs) | ForEach-Object { $_.ToString('x2') }))
    } finally {
        $fs.Dispose()
        $sha.Dispose()
    }
}

# ------------------------------------------------------------------ paths

function Get-LocalAppData {
    if ($env:LOCALAPPDATA) { return $env:LOCALAPPDATA }
    return (Join-Path $env:USERPROFILE 'AppData\Local')
}

# Where the cc-usage app lives and where cc-hook writes by default. The
# publisher installs into the same folder, so hook commands look the same.
function Get-DefaultPrefix { return (Join-Path (Get-LocalAppData) 'cc-usage') }

# Claude Code's folder, by the rule Claude Code and cc-hook follow:
# CLAUDE_CONFIG_DIR when set and not empty, else %USERPROFILE%\.claude.
function Get-ClaudeDir {
    if ($env:CLAUDE_CONFIG_DIR) { return $env:CLAUDE_CONFIG_DIR }
    return (Join-Path $env:USERPROFILE '.claude')
}

# cc-hook's default settings file: settings.json in Get-ClaudeDir. cc-hook
# runs with this environment, so it resolves the same file.
function Get-DefaultSettings { return (Join-Path (Get-ClaudeDir) 'settings.json') }

function Get-FullPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    if ($full.Length -gt 3) { $full = $full.TrimEnd('\') }
    return $full
}

function Test-SamePath([string]$A, [string]$B) {
    return ((Get-FullPath $A) -ieq (Get-FullPath $B))
}

# Paths end up in a task's command line, where Task Scheduler expands %VAR%
# and a double quote would end the argument.
function Assert-UsablePath([string]$Name, [string]$Path) {
    if ($Path -match '["%]') { throw "$Name contains a double quote or '%', which a scheduled task's command line cannot carry: $Path" }
}

# The task name also names the Run entry. Registry and task cmdlets treat
# * ? [ ] as wildcards, which could match somebody else's entry, so those
# (and what Task Scheduler forbids) are refused.
function Assert-TaskName([string]$Name) {
    if (-not $Name.Trim() -or $Name -match '[\\/:*?"<>|\[\]]') { throw "invalid task name '$Name'" }
}

# A hook command as cc-hook writes it: forward slashes, double quotes.
function Get-HookCommand([string]$Exe) { return ('"' + (Get-FullPath $Exe).Replace('\', '/') + '"') }

# ------------------------------------------------------------------ bundle

# Reads config\bundle.json of a bundle folder (or of the installed copy).
function Get-BundleInfo([string]$Dir) {
    $path = Join-Path $Dir 'config\bundle.json'
    if (-not (Test-Path -LiteralPath $path)) { throw "config\bundle.json not found in $Dir; run this from the bundle folder" }
    $info = Read-Json $path
    foreach ($k in 'alias', 'nats_user', 'version', 'arch') {
        if (-not (Get-Prop $info $k)) { throw "config\bundle.json has no '$k'" }
    }
    if ((Get-Prop $info 'alias') -cnotmatch '^[a-z0-9][a-z0-9-]{0,30}$') { throw "invalid alias in config\bundle.json: $(Get-Prop $info 'alias')" }
    return $info
}

# The installed copy's marker, or $null in a full bundle.
function Get-InstalledMarker([string]$Dir) {
    $path = Join-Path $Dir $MarkerRel
    if (Test-Path -LiteralPath $path) { return (Read-Json $path) }
    return $null
}

function Get-Manifest([string]$Prefix) {
    $path = Join-Path $Prefix $ManifestName
    if (Test-Path -LiteralPath $path) { return (Read-Json $path) }
    return $null
}

# Checks "<hex>  <relative/path>" lines against the files under $Root.
# Returns the problems (none when everything matches).
function Test-Sums([string]$Root, [string]$SumsFile) {
    $problems = @()
    $n = 0
    foreach ($line in [System.IO.File]::ReadAllLines($SumsFile)) {
        if ($line.Trim() -eq '') { continue }
        if ($line -notmatch '^([0-9a-f]{64})  (.+)$') { $problems += "unreadable line '$line'"; continue }
        $want = $Matches[1]
        $rel = $Matches[2]
        $n++
        $path = Join-Path $Root $rel.Replace('/', '\')
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { $problems += "$rel missing"; continue }
        if ((Get-Sha256Hex $path) -ne $want) { $problems += "$rel differs" }
    }
    if ($n -eq 0) { $problems += 'no entries' }
    return $problems
}

# Files under $Root\$Dir that $SumsFile does not list, as paths relative to
# $Root (e.g. a DLL dropped next to the binaries, which Windows would load
# when they run).
function Get-UnlistedFiles([string]$Root, [string]$SumsFile, [string]$Dir) {
    $listed = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($line in [System.IO.File]::ReadAllLines($SumsFile)) {
        if ($line -match '^[0-9a-f]{64}  (.+)$') { [void]$listed.Add($Matches[1].Replace('/', '\')) }
    }
    $rootFull = Get-FullPath $Root
    $unlisted = @()
    foreach ($f in @(Get-ChildItem -LiteralPath (Join-Path $rootFull $Dir) -Recurse -File -Force -ErrorAction SilentlyContinue)) {
        $rel = $f.FullName.Substring($rootFull.Length).TrimStart('\')
        if (-not $listed.Contains($rel)) { $unlisted += $rel }
    }
    return $unlisted
}

# ------------------------------------------------------------------ the app

# A description of the cc-usage app's installation on this machine, or $null.
# The app and the publisher drain the same spool in the same folder, so they
# must never both run; a machine with the app does not need the publisher.
function Get-AppInstall {
    $found = @()
    foreach ($root in $UninstallRoots) {
        if (-not (Test-Path $root)) { continue }
        foreach ($key in @(Get-ChildItem -LiteralPath $root -ErrorAction SilentlyContinue)) {
            $props = Get-ItemProperty -LiteralPath $key.PSPath -ErrorAction SilentlyContinue
            if ($key.PSChildName -eq 'cc-usage' -or (Get-Prop $props 'DisplayName') -eq 'cc-usage') {
                $found += "uninstall entry $($key.Name)"
            }
        }
    }
    $exe = Join-Path (Get-DefaultPrefix) 'cc-usage.exe'
    if (Test-Path -LiteralPath $exe) { $found += $exe }
    if ($found.Count -eq 0) { return $null }
    return ($found -join '; ')
}

# ------------------------------------------------------------------ processes

# Runs a program and returns Code, Lines and Text, with stderr folded in and
# without letting stderr output become a terminating error (Windows
# PowerShell 5.1 does that under ErrorActionPreference Stop). $Environment
# sets process environment variables for the call only.
function Invoke-Exe([string]$Exe, [string[]]$Arguments = @(), [hashtable]$Environment = @{}) {
    $saved = @{}
    foreach ($k in $Environment.Keys) {
        $saved[$k] = [Environment]::GetEnvironmentVariable($k, 'Process')
        [Environment]::SetEnvironmentVariable($k, [string]$Environment[$k], 'Process')
    }
    $oldPref = $ErrorActionPreference
    $oldEncoding = $null
    try { $oldEncoding = [Console]::OutputEncoding; [Console]::OutputEncoding = $Utf8NoBom } catch { $oldEncoding = $null }
    try {
        $ErrorActionPreference = 'Continue'
        $lines = @(& $Exe @Arguments 2>&1 | ForEach-Object { [string]$_ })
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $oldPref
        if ($null -ne $oldEncoding) { try { [Console]::OutputEncoding = $oldEncoding } catch { } }
        foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k], 'Process') }
    }
    return [pscustomobject]@{ Code = $code; Lines = $lines; Text = ($lines -join "`n") }
}

# The last non-empty line of a command's output.
function Get-LastLine($Result) {
    $nonEmpty = @($Result.Lines | Where-Object { $_ -and $_.Trim() })
    if ($nonEmpty.Count -eq 0) { return '' }
    return $nonEmpty[-1].Trim()
}

# cc-publish.exe processes started from $Exe (other copies are left alone).
function Get-PublisherProcess([string]$Exe) {
    $full = Get-FullPath $Exe
    $procs = @()
    foreach ($p in @(Get-Process -Name 'cc-publish' -ErrorAction SilentlyContinue)) {
        $path = $null
        try { $path = $p.Path } catch { $path = $null }
        if ($path -and ((Get-FullPath $path) -ieq $full)) { $procs += $p }
    }
    return $procs
}

# How a publisher process is shown: its parent (conhost --headless when the
# task or the Run entry started it) and whether it or the parent owns a
# window. Returns Hidden ($true when neither has a window), Headless and a
# Detail line.
function Get-WindowReport($Process) {
    $parentName = '?'
    $parentCmd = ''
    $parentHandle = [IntPtr]::Zero
    try {
        $me = Get-CimInstance Win32_Process -Filter "ProcessId = $($Process.Id)" -ErrorAction Stop
        $parent = Get-CimInstance Win32_Process -Filter "ProcessId = $($me.ParentProcessId)" -ErrorAction Stop
        if ($parent) {
            $parentName = [string]$parent.Name
            $parentCmd = [string]$parent.CommandLine
            $pp = Get-Process -Id $parent.ProcessId -ErrorAction SilentlyContinue
            if ($pp) { $parentHandle = $pp.MainWindowHandle }
        }
    } catch { $parentName = '?' }
    $own = (Get-Process -Id $Process.Id).MainWindowHandle
    $headless = ($parentName -ieq 'conhost.exe') -and ($parentCmd -match '--headless')
    $hidden = ($own -eq [IntPtr]::Zero) -and ($parentHandle -eq [IntPtr]::Zero)
    $how = if ($headless) { 'conhost --headless' } else { "parent $parentName" }
    return [pscustomobject]@{
        Hidden   = $hidden
        Headless = $headless
        Detail   = "pid $($Process.Id) under $how, window handles $own/$parentHandle"
    }
}

# Stops the publisher installed in $Prefix: `cc-publish stop` (clean: last
# flush, state file marked stopped), then the task, then whatever is left of
# that executable. Returns what it did.
function Stop-Publisher([string]$Prefix, [string]$TaskName) {
    $exe = Join-Path $Prefix 'cc-publish.exe'
    $did = @()
    if (@(Get-PublisherProcess $exe).Count -gt 0) {
        $r = Invoke-Exe $exe @('--data-dir', $Prefix, 'stop', '--wait', '15')
        $did += "cc-publish stop: $(Get-LastLine $r)"
    }
    $task = Get-PublisherTask $TaskName
    # Only our task: one with that name that runs something else is left alone.
    if ($task -and [string]$task.State -eq 'Running' -and (Test-OurTask $task $Prefix)) {
        try {
            Stop-ScheduledTask -TaskName $TaskName -TaskPath '\' -ErrorAction Stop
            $did += "ended the task '$TaskName'"
        } catch { }
    }
    foreach ($p in @(Get-PublisherProcess $exe)) {
        try { Stop-Process -Id $p.Id -Force -ErrorAction Stop; $did += "killed pid $($p.Id)" } catch { }
    }
    $deadline = (Get-Date).AddSeconds(10)
    while (@(Get-PublisherProcess $exe).Count -gt 0) {
        if ((Get-Date) -gt $deadline) { throw "cc-publish ($exe) is still running; end it in Task Manager and try again" }
        Start-Sleep -Milliseconds 200
    }
    return $did
}

# Waits until a publisher from $Prefix runs and has written its state file.
# Returns the process, or $null.
function Wait-PublisherStart([string]$Prefix, [int]$Seconds) {
    $exe = Join-Path $Prefix 'cc-publish.exe'
    $statePath = Join-Path $Prefix 'publish-state.json'
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        $procs = @(Get-PublisherProcess $exe)
        if ($procs.Count -gt 0 -and (Test-Path -LiteralPath $statePath)) {
            try {
                $state = Read-Json $statePath
                $p = $procs | Where-Object { $_.Id -eq [int](Get-Prop $state 'pid' 0) } | Select-Object -First 1
                if ($p -and (Get-Prop $state 'running' $false)) { return $p }
            } catch { }
        }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    return $null
}

# ------------------------------------------------------------------ autostart

function Get-PublisherTask([string]$Name) {
    try { return (Get-ScheduledTask -TaskName $Name -TaskPath '\' -ErrorAction Stop) } catch { return $null }
}

# The command line the task and the Run entry run: cc-publish under a
# headless console host, so no window ever opens. cc-publish is a console
# program: started from a task or at logon without a console, Windows would
# give it a visible console window (or a Windows Terminal tab).
function Get-LaunchCommand([string]$Prefix) {
    $conhost = Join-Path $env:SystemRoot 'System32\conhost.exe'
    $exe = Join-Path $Prefix 'cc-publish.exe'
    $config = Join-Path $Prefix 'publish.json'
    $arguments = "--headless `"$exe`" --data-dir `"$Prefix`" run --config `"$config`""
    return [pscustomobject]@{ Program = $conhost; Arguments = $arguments; Line = "`"$conhost`" $arguments" }
}

# Task Scheduler XML for a per-user task: logon trigger for this user, plus
# a time trigger from now on, both repeated every 5 minutes (a second
# instance is ignored while one runs, so this restarts a publisher that died:
# conhost always exits 0, so the restart-on-failure setting alone would not).
# The time trigger is what restarts it in the session the install runs in: a
# logon trigger's repetition only begins at the next logon. Runs only while
# the user is logged on, with the user's normal rights, no time limit, on
# battery too.
function New-PublisherTaskXml([string]$Prefix, [string]$Description) {
    $id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $cmd = Get-LaunchCommand $Prefix
    $e = { param([string]$s) [System.Security.SecurityElement]::Escape($s) }
    # Local time without an offset: the trigger follows the clock across time zones.
    $start = (Get-Date).ToString('yyyy-MM-ddTHH:mm:ss', [System.Globalization.CultureInfo]::InvariantCulture)
    return @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>$(& $e $id.Name)</Author>
    <Description>$(& $e $Description)</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>$(& $e $id.Name)</UserId>
      <Repetition>
        <Interval>PT5M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
    </LogonTrigger>
    <TimeTrigger>
      <Enabled>true</Enabled>
      <StartBoundary>$start</StartBoundary>
      <Repetition>
        <Interval>PT5M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>$(& $e $id.User.Value)</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>10</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>$(& $e $cmd.Program)</Command>
      <Arguments>$(& $e $cmd.Arguments)</Arguments>
      <WorkingDirectory>$(& $e $Prefix)</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"@
}

# Whether a task is the publisher's: its action runs cc-publish.exe from $Prefix.
function Test-OurTask($Task, [string]$Prefix) {
    $exe = Join-Path $Prefix 'cc-publish.exe'
    foreach ($a in @($Task.Actions)) {
        $arguments = [string](Get-Prop $a 'Arguments' '')
        $execute = [string](Get-Prop $a 'Execute' '')
        if ($arguments.IndexOf($exe, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or $execute -ieq $exe) { return $true }
    }
    return $false
}

function Get-RunEntry([string]$Name) {
    $props = Get-ItemProperty -LiteralPath $RunKeyPath -ErrorAction SilentlyContinue
    return (Get-Prop $props $Name)
}

function Set-RunEntry([string]$Name, [string]$Value) {
    # Never New-Item -Force on an existing key: it recreates the key and
    # drops every other program's startup entry.
    if (-not (Test-Path -LiteralPath $RunKeyPath)) { New-Item -Path $RunKeyPath | Out-Null }
    Set-ItemProperty -LiteralPath $RunKeyPath -Name $Name -Value $Value
}

function Remove-RunEntry([string]$Name) {
    Remove-ItemProperty -LiteralPath $RunKeyPath -Name $Name -ErrorAction SilentlyContinue
}

# False when the user turned the Run entry off in Task Manager's Startup tab.
function Test-RunEntryEnabled([string]$Name) {
    $props = Get-ItemProperty -LiteralPath $StartupApprovedPath -ErrorAction SilentlyContinue
    $value = Get-Prop $props $Name
    if ($value -is [byte[]] -and $value.Length -gt 0) { return (($value[0] -band 1) -eq 0) }
    return $true
}

# ------------------------------------------------------------------ ACLs

function Get-UserSid { return [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value }

function Get-SidName([string]$Sid) {
    try {
        return (New-Object System.Security.Principal.SecurityIdentifier($Sid)).Translate([System.Security.Principal.NTAccount]).Value
    } catch { return $Sid }
}

# $null when only the current user and SYSTEM can open $Path (no inherited
# entries), else what is wrong.
function Test-PrivateAcl([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return 'missing' }
    $sid = Get-UserSid
    $acl = Get-Acl -LiteralPath $Path
    if (-not $acl.AreAccessRulesProtected) { return 'inherits entries from its folder' }
    $others = @()
    $userCanRead = $false
    $read = [System.Security.AccessControl.FileSystemRights]::Read
    foreach ($rule in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) {
        if ($rule.AccessControlType -ne [System.Security.AccessControl.AccessControlType]::Allow) { continue }
        $s = $rule.IdentityReference.Value
        if ($s -eq $sid) {
            if (($rule.FileSystemRights -band $read) -eq $read) { $userCanRead = $true }
        } elseif ($s -ne $SystemSid) {
            $others += (Get-SidName $s)
        }
    }
    if ($others.Count -gt 0) { return "also open to $(($others | Select-Object -Unique) -join ', ')" }
    if (-not $userCanRead) { return 'the current user cannot read it' }
    return $null
}

# Gives $Path an ACL with only the current user and SYSTEM (full control),
# inheritance off, then checks it.
function Set-PrivateAcl([string]$Path) {
    $sid = Get-UserSid
    $icacls = Join-Path $env:SystemRoot 'System32\icacls.exe'
    $perm = if (Test-Path -LiteralPath $Path -PathType Container) { '(OI)(CI)F' } else { 'F' }
    $r = Invoke-Exe $icacls @($Path, '/inheritance:r', '/grant:r', "*${sid}:$perm", "*${SystemSid}:$perm", '/Q')
    if ($r.Code -ne 0) { throw "icacls could not restrict ${Path}: $($r.Text)" }
    # Explicit entries of anybody else (a hand-made file, a copy) go too.
    $acl = Get-Acl -LiteralPath $Path
    foreach ($rule in $acl.GetAccessRules($true, $false, [System.Security.Principal.SecurityIdentifier])) {
        $s = $rule.IdentityReference.Value
        if ($s -ne $sid -and $s -ne $SystemSid) {
            $r = Invoke-Exe $icacls @($Path, '/remove', "*$s", '/Q')
            if ($r.Code -ne 0) { throw "icacls could not remove $s from ${Path}: $($r.Text)" }
        }
    }
    $problem = Test-PrivateAcl $Path
    if ($problem) { throw "the ACL of $Path is not private after icacls: $problem" }
}

# ------------------------------------------------------------------ files

# Prefix of the staging folders Install-PrivateFile makes next to a secret.
$StagingPrefix = '.cc-usage-staging-'

# Copies a secret into place without it ever being readable by anybody else,
# not even for a moment: the copy is written into a staging folder next to
# $Dest that already has the private ACL (made empty, restricted, then
# filled; a new file inherits its folder's ACL, and the folder of $Dest, such
# as the install folder, may be readable by administrators), gets the private
# ACL explicitly, and is renamed over $Dest (a rename on the same volume
# keeps the ACL). A different existing $Dest is kept as
# $Dest.bak-<timestamp>, also private.
function Install-PrivateFile([string]$Source, [string]$Dest) {
    $note = ''
    if (Test-Path -LiteralPath $Dest) {
        if ((Get-Sha256Hex $Source) -eq (Get-Sha256Hex $Dest)) {
            Set-PrivateAcl $Dest
            return "unchanged  $Dest (private)"
        }
        $backup = "$Dest.bak-$(Get-Date -Format yyyyMMddHHmmss)"
        Move-Item -LiteralPath $Dest -Destination $backup
        Set-PrivateAcl $backup
        $note = ", the previous one kept as $(Split-Path -Leaf $backup)"
    }
    $staging = Join-Path (Split-Path -Parent $Dest) "$StagingPrefix$PID"
    if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
    New-Item -ItemType Directory -Path $staging | Out-Null
    try {
        Set-PrivateAcl $staging
        $tmp = Join-Path $staging (Split-Path -Leaf $Dest)
        Copy-Item -LiteralPath $Source -Destination $tmp -Force
        Unblock-File -LiteralPath $tmp -ErrorAction SilentlyContinue
        Set-PrivateAcl $tmp
        Move-Item -LiteralPath $tmp -Destination $Dest
    } finally {
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }
    Set-PrivateAcl $Dest
    return "installed  $Dest (private$note)"
}

# Deletes staging folders an interrupted Install-PrivateFile left in $Dir.
function Remove-StaleStaging([string]$Dir) {
    foreach ($d in @(Get-ChildItem -LiteralPath $Dir -Directory -Force -Filter "$StagingPrefix*" -ErrorAction SilentlyContinue)) {
        Remove-Item -LiteralPath $d.FullName -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# What is wrong with the config, from the output of `cc-publish status
# --config $ConfigPath`, or $null when cc-publish loaded it. status prints
# "config      <path>" for a config it loaded and "config      <error>"
# (cannot read, too large, invalid JSON, alias or servers) otherwise, so any
# config row that is not the path is a problem.
function Get-ConfigProblem($StatusResult, [string]$ConfigPath) {
    $rows = @($StatusResult.Lines | Where-Object { $_ -match '^config\s' })
    if ($rows.Count -eq 0) { return "cc-publish status printed no config line (exit $($StatusResult.Code)): $(Get-LastLine $StatusResult)" }
    $value = ($rows[0] -replace '^config\s+', '').Trim()
    if ($value -ieq $ConfigPath) { return $null }
    return $value
}

# Copies an executable into place. A running .exe cannot be overwritten or
# deleted, but it can be renamed: the old one becomes <name>.old-<timestamp>
# and is deleted when possible (else on the next install or uninstall).
function Install-Binary([string]$Source, [string]$Dest) {
    if ((Test-Path -LiteralPath $Dest) -and ((Get-Sha256Hex $Source) -eq (Get-Sha256Hex $Dest))) {
        return "unchanged  $Dest"
    }
    $tmp = "$Dest.new"
    Copy-Item -LiteralPath $Source -Destination $tmp -Force
    Unblock-File -LiteralPath $tmp -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $Dest) {
        $old = "$Dest.old-$(Get-Date -Format yyyyMMddHHmmss)"
        Move-Item -LiteralPath $Dest -Destination $old
        Move-Item -LiteralPath $tmp -Destination $Dest
        Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
    } else {
        Move-Item -LiteralPath $tmp -Destination $Dest
    }
    return "installed  $Dest"
}

# Deletes leftovers of earlier replacements (<exe>.old-*, *.new) in $Prefix.
function Remove-StaleBinaries([string]$Prefix) {
    foreach ($pattern in 'cc-hook.exe.old-*', 'cc-publish.exe.old-*', 'cc-hook.exe.new', 'cc-publish.exe.new') {
        foreach ($f in @(Get-ChildItem -LiteralPath $Prefix -Filter $pattern -File -ErrorAction SilentlyContinue)) {
            Remove-Item -LiteralPath $f.FullName -Force -ErrorAction SilentlyContinue
        }
    }
}

# Arguments for cc-hook install/uninstall/status: --settings only for a
# settings file other than the default one (so the status line command of the
# real file carries no --settings, like the app's).
function Get-SettingsArgs([string]$Settings) {
    if (Test-SamePath $Settings (Get-DefaultSettings)) { return @() }
    return @('--settings', $Settings)
}

# ------------------------------------------------------------------ identity
#
# This machine's identity is its key pair, keys\publisher.key:
# made here on the first install, kept by every reinstall, and changed only on
# purpose (Install.ps1 -RotateKey, or a confirmed install over a lost key),
# because the viewer holds back a new key's values until it is accepted there.

# The warning shown before $Alias's identity is replaced.
function Get-IdentityWarning([string]$Alias) {
    return "This replaces $Alias's identity. The viewer will hold back its values until you accept the new key there."
}

# How the viewer takes a new public key $Line. Its notice names the key id
# and the first 8 characters of the key.
function Get-AcceptNote([string]$Alias, [string]$Line) {
    $key = ($Line -split ':', 2)[1]
    return "on the viewer, accept this key when cc-usage asks about '$Alias': key $(Get-Kid $Line), fingerprint $($key.Substring(0, [Math]::Min(8, $key.Length)))"
}

# The key id of a public key line (<kid>:<base64>).
function Get-Kid([string]$Line) { return ($Line -split ':', 2)[0] }

# The public key line cc-publish keygen printed, or $null.
function Get-PublicLine($Result) {
    return (@($Result.Lines | Where-Object { $_ -match '^[A-Za-z0-9_-]{1,32}:[A-Za-z0-9+/]{43}=$' }) | Select-Object -First 1)
}

# Whether the alias can be asked for: an interactive session whose input is
# a console, and not powershell -NonInteractive.
function Test-CanPrompt {
    try {
        if ([Console]::IsInputRedirected -or -not [Environment]::UserInteractive) { return $false }
        return (@([Environment]::GetCommandLineArgs() | Where-Object { $_ -match '^-noni(nteractive)?$' }).Count -eq 0)
    } catch {
        return $false
    }
}

# Confirms replacing $Alias's identity: -ConfirmAlias ($ConfirmAlias), else
# the alias typed at a prompt when $CanPrompt. Prints the warning first;
# throws when not confirmed, the message ending with $Hint.
function Confirm-IdentityChange([string]$Alias, [string]$ConfirmAlias, [bool]$CanPrompt, [string]$Hint) {
    Write-Note (Get-IdentityWarning $Alias)
    if ($ConfirmAlias) {
        $given = $ConfirmAlias
    } elseif ($CanPrompt) {
        $given = [string](Read-Host "Type the alias ($Alias) to confirm")
    } else {
        throw "not confirmed: pass -ConfirmAlias $Alias (or run it in a console and type the alias). $Hint"
    }
    if ($given.Trim() -cne $Alias) { throw "not confirmed: '$($given.Trim())' is not this machine's alias '$Alias'. $Hint" }
}

# What an install does with this machine's identity, keys\publisher.key
# under $Prefix, decided before anything is changed; $PublishExe only reads
# an existing key (keygen --if-missing). Action is keep (it loads; Line is its
# public line), first (no key, nothing installed before: a new one is made),
# new (no key, but a publisher was installed here: its manifest or
# publish.json is there) or replace (it does not load; Detail says why). The
# last two need the confirmation.
function Get-IdentityPlan([string]$Prefix, [string]$PublishExe) {
    $key = Join-Path $Prefix 'keys\publisher.key'
    if (Test-Path -LiteralPath $key -PathType Leaf) {
        $r = Invoke-Exe $PublishExe @('keygen', '--if-missing', '--key', $key)
        $line = Get-PublicLine $r
        if ($r.Code -eq 0 -and $line) { return [pscustomobject]@{ Action = 'keep'; Key = $key; Line = $line; Detail = '' } }
        return [pscustomobject]@{ Action = 'replace'; Key = $key; Line = $null; Detail = (Get-LastLine $r) }
    }
    $before = (Test-Path -LiteralPath (Join-Path $Prefix $ManifestName)) -or (Test-Path -LiteralPath (Join-Path $Prefix 'publish.json'))
    $action = if ($before) { 'new' } else { 'first' }
    return [pscustomobject]@{ Action = $action; Key = $key; Line = $null; Detail = '' }
}

# Keeps a private copy of a key file that is about to be replaced, as
# <file>.bak-<timestamp>. Returns its path.
function Backup-KeyFile([string]$Path) {
    $backup = "$Path.bak-$(Get-Date -Format yyyyMMddHHmmss)"
    Copy-Item -LiteralPath $Path -Destination $backup
    Set-PrivateAcl $backup
    return $backup
}

# Records the new hash of $Rel (a path relative to $Prefix) in the install
# manifest, keeping everything else.
function Update-ManifestFile([string]$Prefix, [string]$Rel) {
    $path = Join-Path $Prefix $ManifestName
    $manifest = Read-Json $path
    $files = Get-Prop $manifest 'files'
    if ($null -eq $files) { return }
    $hash = Get-Sha256Hex (Join-Path $Prefix $Rel)
    $prop = $files.PSObject.Properties[$Rel]
    if ($prop) { $prop.Value = $hash } else { $files | Add-Member -NotePropertyName $Rel -NotePropertyValue $hash }
    Write-TextFile $path (($manifest | ConvertTo-Json -Depth 4) + "`n")
}

# ------------------------------------------------------------------ install transaction
#
# Install.ps1 changes an install as one transaction. Before anything is
# changed, New-InstallTransaction keeps a private copy of every file the
# install may write, the publisher's scheduled tasks (as XML, with their
# state) and its Run entries. Once the new publisher runs,
# Complete-InstallTransaction drops the copies. On any failure before that,
# Undo-InstallTransaction stops the new publisher, puts the files, the tasks
# and the Run entries back, enables the tasks Suspend-PublisherTasks disabled
# and starts the previous publisher again: an install that fails never
# leaves the publisher disabled. Files that did not exist before are
# removed. Paths left out of the copies (this machine's key pair, which is
# never rolled back) only get their manifest line refreshed. When part of
# the rollback fails, the copies stay, marked, and the next install refuses
# to run until they are moved away (Assert-NoIncompleteRollback).

# Prefix of the folder in the install folder that holds the copies.
$RollbackPrefix = '.cc-usage-rollback-'

# The file Undo-InstallTransaction writes into the rollback folder before it
# puts anything back, listing where each copy goes; the folder goes only
# when every step worked. A folder that has it may hold the only copy of a
# previous file, so it is never deleted by an install.
$RollbackMarkerName = 'ROLLBACK-INCOMPLETE.txt'

# The rollback folders in $Prefix whose rollback did not complete (they
# have the marker), as full paths.
function Get-IncompleteRollback([string]$Prefix) {
    return @(Get-ChildItem -LiteralPath $Prefix -Directory -Force -Filter "$RollbackPrefix*" -ErrorAction SilentlyContinue |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName $RollbackMarkerName) -PathType Leaf } |
        ForEach-Object { $_.FullName })
}

# What to do about the incomplete rollback in $Dir before installing again.
function Get-IncompleteRollbackHelp([string]$Dir) {
    return "the rollback of an earlier install did not complete: its copies of the previous files are in $Dir ($RollbackMarkerName there says where each one goes). Put back what you need from it, then move the folder out of $(Split-Path -Parent $Dir) (it holds copies of the secrets: delete it once done) and run setup.cmd again"
}

# Throws when an incomplete rollback is left in $Prefix (see above).
function Assert-NoIncompleteRollback([string]$Prefix) {
    $left = @(Get-IncompleteRollback $Prefix)
    if ($left.Count -gt 0) { throw (Get-IncompleteRollbackHelp $left[0]) }
}

# Deletes the rollback folders an interrupted install left in $Prefix (they
# hold copies of the secrets), except those of an incomplete rollback,
# unless $All (Uninstall.ps1). Returns the folders it deleted.
function Remove-StaleRollback([string]$Prefix, [switch]$All) {
    $removed = New-Object System.Collections.Generic.List[string]
    foreach ($d in @(Get-ChildItem -LiteralPath $Prefix -Directory -Force -Filter "$RollbackPrefix*" -ErrorAction SilentlyContinue)) {
        if (-not $All -and (Test-Path -LiteralPath (Join-Path $d.FullName $RollbackMarkerName) -PathType Leaf)) { continue }
        Remove-Item -LiteralPath $d.FullName -Recurse -Force -ErrorAction SilentlyContinue
        if (-not (Test-Path -LiteralPath $d.FullName)) { $removed.Add($d.FullName) }
    }
    return $removed
}

# Starts the transaction of an install into $Prefix: copies of the files in
# $Paths that exist (and a note of the ones that do not), the tasks named in
# $TaskNames that are the publisher's (Test-OurTask), the Run entries of
# those names, and the previous manifest (how to restart that publisher).
# $KeepPaths are files the install may change that are never rolled back.
# Refuses (Assert-NoIncompleteRollback) while an incomplete rollback is left.
function New-InstallTransaction([string]$Prefix, [string[]]$Paths, [string[]]$TaskNames, [string[]]$KeepPaths = @()) {
    if (-not (Test-Path -LiteralPath $Prefix)) { New-Item -ItemType Directory -Force -Path $Prefix | Out-Null }
    Assert-NoIncompleteRollback $Prefix
    Remove-StaleRollback $Prefix | Out-Null
    $dir = Join-Path $Prefix "$RollbackPrefix$PID"
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    try {
        Set-PrivateAcl $dir
        return (New-TransactionRecord $Prefix $dir $Paths $TaskNames $KeepPaths)
    } catch {
        # Nothing was changed yet; no copies of the secrets are left behind.
        Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
        throw "could not prepare the rollback of this install (nothing was changed): $($_.Exception.Message)"
    }
}

# The record New-InstallTransaction returns, with the copies made in $Dir.
function New-TransactionRecord([string]$Prefix, [string]$Dir, [string[]]$Paths, [string[]]$TaskNames, [string[]]$KeepPaths) {
    $files = New-Object System.Collections.Generic.List[object]
    $seen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($p in $Paths) {
        if (-not $p) { continue }
        $full = Get-FullPath $p
        if (-not $seen.Add($full)) { continue }
        $rec = [pscustomobject]@{ Path = $full; Backup = $null; Private = $false }
        if (Test-Path -LiteralPath $full -PathType Leaf) {
            # The copy is created in the private folder, so it is private
            # whatever the original's ACL.
            $rec.Backup = Join-Path $Dir ('{0:d3}-{1}' -f $files.Count, (Split-Path -Leaf $full))
            [System.IO.File]::Copy($full, $rec.Backup)
            $rec.Private = ($null -eq (Test-PrivateAcl $full))
        }
        $files.Add($rec)
    }
    $tasks = New-Object System.Collections.Generic.List[object]
    $runEntries = [ordered]@{}
    foreach ($name in @($TaskNames | Where-Object { $_ } | Select-Object -Unique)) {
        $xml = $null
        $enabled = $false
        $t = Get-PublisherTask $name
        if ($t -and (Test-OurTask $t $Prefix)) {
            $xml = [string](Export-ScheduledTask -TaskName $name -TaskPath '\')
            $enabled = ([string]$t.State -ne 'Disabled')
        }
        $tasks.Add([pscustomobject]@{ Name = $name; Xml = $xml; Enabled = $enabled; Disabled = $false })
        $runEntries[$name] = Get-RunEntry $name
    }
    return [pscustomobject]@{
        Prefix           = (Get-FullPath $Prefix)
        Dir              = $Dir
        Files            = $files
        Tasks            = $tasks
        RunEntries       = $runEntries
        KeepPaths        = @($KeepPaths | Where-Object { $_ } | ForEach-Object { Get-FullPath $_ })
        Previous         = (Get-Manifest $Prefix)
        AutostartTouched = $false
        Completed        = $false
    }
}

# Disables the transaction's tasks that are enabled, so their 5 minute
# repetition cannot start the old binary while it is replaced; the install
# enables the task again by registering it, and Undo-InstallTransaction by
# restoring it. A task the user disabled stays disabled either way.
function Suspend-PublisherTasks($Tx) {
    foreach ($t in $Tx.Tasks) {
        if (-not $t.Xml -or -not $t.Enabled) { continue }
        # Best effort: a task that cannot be disabled is stopped all the same
        # (Stop-Publisher), and the binaries are replaced by renaming.
        try {
            Disable-ScheduledTask -TaskName $t.Name -TaskPath '\' -ErrorAction Stop | Out-Null
            $t.Disabled = $true
        } catch { }
    }
}

# The install succeeded: drops the copies.
function Complete-InstallTransaction($Tx) {
    $Tx.Completed = $true
    Remove-Item -LiteralPath $Tx.Dir -Recurse -Force -ErrorAction SilentlyContinue
}

# Moves $Path out of the way (a running .exe can be renamed, not deleted)
# and deletes it when possible; Remove-StaleBinaries clears what is left.
function Remove-OrRename([string]$Path) {
    $old = "$Path.old-$(Get-Date -Format yyyyMMddHHmmss)"
    Move-Item -LiteralPath $Path -Destination $old -Force
    Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
}

# Puts one file of the transaction back as it was. Returns what it did, or
# $null when nothing was needed.
function Restore-SavedFile($Record) {
    $path = $Record.Path
    if ($null -eq $Record.Backup) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
        Remove-OrRename $path
        return "removed    $path (not there before)"
    }
    if ((Test-Path -LiteralPath $path -PathType Leaf) -and ((Get-Sha256Hex $path) -eq (Get-Sha256Hex $Record.Backup))) { return $null }
    $dir = Split-Path -Parent $path
    if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    # Written next to the target first, so it gets that folder's ACL like
    # the original (a private folder's, for the secrets), then renamed over it.
    $tmp = "$path.rollback-$PID"
    [System.IO.File]::Copy($Record.Backup, $tmp, $true)
    if (Test-Path -LiteralPath $path) { Remove-OrRename $path }
    Move-Item -LiteralPath $tmp -Destination $path
    if ($Record.Private) { Set-PrivateAcl $path }
    return "restored   $path"
}

# The text of the rollback folder's marker: where each copy goes, and what
# failed ($Failed, empty while the rollback runs).
function Get-RollbackMarkerText($Tx, [string[]]$Failed = @()) {
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.AppendLine("The rollback of a cc-usage publisher install into $($Tx.Prefix) did not complete.")
    [void]$sb.AppendLine("setup.cmd refuses to install there until this folder is moved out of it.")
    [void]$sb.AppendLine('')
    [void]$sb.AppendLine('Copies of the previous files, and where each one goes:')
    foreach ($f in @($Tx.Files | Where-Object { $_.Backup })) {
        [void]$sb.AppendLine("  $(Split-Path -Leaf $f.Backup)  ->  $($f.Path)")
    }
    $new = @($Tx.Files | Where-Object { -not $_.Backup })
    if ($new.Count -gt 0) {
        [void]$sb.AppendLine('')
        [void]$sb.AppendLine('Files that were not there before the install:')
        foreach ($f in $new) { [void]$sb.AppendLine("  $($f.Path)") }
    }
    if ($Failed.Count -gt 0) {
        [void]$sb.AppendLine('')
        [void]$sb.AppendLine('What the rollback could not do:')
        foreach ($l in $Failed) { [void]$sb.AppendLine("  $l") }
    }
    return $sb.ToString()
}

# Rolls the transaction back (see above). Never throws: every step runs, and
# what failed is reported. Returns Done and Failed (lines) and Dir, the
# folder with the copies, which is kept when something failed. The folder
# gets its marker ($RollbackMarkerName) first, so a rollback that fails or
# is cut short leaves a folder the next install will not delete.
# $WaitSeconds: how long to wait for the previous publisher to start.
function Undo-InstallTransaction($Tx, [int]$WaitSeconds = 20) {
    $done = New-Object System.Collections.Generic.List[string]
    $failed = New-Object System.Collections.Generic.List[string]
    $prefix = $Tx.Prefix
    $marker = Join-Path $Tx.Dir $RollbackMarkerName
    try { Write-TextFile $marker (Get-RollbackMarkerText $Tx) }
    catch { $failed.Add("marking ${marker}: $($_.Exception.Message)") }

    # 1. The new publisher, if it runs.
    foreach ($t in $Tx.Tasks) {
        try { foreach ($l in @(Stop-Publisher $prefix $t.Name)) { $done.Add($l) } }
        catch { $failed.Add("stopping the publisher: $($_.Exception.Message)") }
    }

    # 2. The autostart, when the install got to change it.
    if ($Tx.AutostartTouched) {
        foreach ($t in $Tx.Tasks) {
            try {
                if ($t.Xml) {
                    Register-ScheduledTask -TaskName $t.Name -TaskPath '\' -Xml $t.Xml -Force -ErrorAction Stop | Out-Null
                    $done.Add("restored   the scheduled task '$($t.Name)'")
                } else {
                    $now = Get-PublisherTask $t.Name
                    if ($now -and (Test-OurTask $now $prefix)) {
                        Unregister-ScheduledTask -TaskName $t.Name -TaskPath '\' -Confirm:$false -ErrorAction Stop
                        $done.Add("removed    the scheduled task '$($t.Name)' (not there before)")
                    }
                }
            } catch { $failed.Add("the scheduled task '$($t.Name)': $($_.Exception.Message)") }
        }
        foreach ($name in @($Tx.RunEntries.Keys)) {
            try {
                $was = $Tx.RunEntries[$name]
                $now = Get-RunEntry $name
                if ($null -eq $was -and $null -ne $now) {
                    Remove-RunEntry $name
                    $done.Add("removed    the Run entry '$name' (not there before)")
                } elseif ($null -ne $was -and [string]$now -ne [string]$was) {
                    Set-RunEntry $name ([string]$was)
                    $done.Add("restored   the Run entry '$name'")
                }
            } catch { $failed.Add("the Run entry '$name': $($_.Exception.Message)") }
        }
    }

    # 3. The files, last written first.
    for ($i = $Tx.Files.Count - 1; $i -ge 0; $i--) {
        $f = $Tx.Files[$i]
        try {
            $r = Restore-SavedFile $f
            if ($r) { $done.Add($r) }
        } catch { $failed.Add("$($f.Path): $($_.Exception.Message)") }
    }
    # The files that are kept as they are now (the key pair): the restored
    # manifest gets their current hash, so Verify.ps1 agrees with them.
    $prefixRoot = $prefix.TrimEnd('\') + '\'
    if (Test-Path -LiteralPath (Join-Path $prefix $ManifestName)) {
        foreach ($k in $Tx.KeepPaths) {
            if (-not $k.StartsWith($prefixRoot, [System.StringComparison]::OrdinalIgnoreCase) -or -not (Test-Path -LiteralPath $k -PathType Leaf)) { continue }
            try { Update-ManifestFile $prefix $k.Substring($prefixRoot.Length) }
            catch { $failed.Add("the manifest line of ${k}: $($_.Exception.Message)") }
        }
    }

    # 4. The tasks this install disabled, enabled again (a restored task
    # already is; enabling it again changes nothing).
    foreach ($t in $Tx.Tasks) {
        if (-not $t.Disabled) { continue }
        try {
            if (Get-PublisherTask $t.Name) {
                Enable-ScheduledTask -TaskName $t.Name -TaskPath '\' -ErrorAction Stop | Out-Null
                $done.Add("enabled    the scheduled task '$($t.Name)' again")
            } else {
                $failed.Add("the scheduled task '$($t.Name)' is gone")
            }
        } catch { $failed.Add("enabling the scheduled task '$($t.Name)': $($_.Exception.Message)") }
    }

    # 5. The previous publisher, started the way it was installed.
    $exe = Join-Path $prefix 'cc-publish.exe'
    $config = Join-Path $prefix 'publish.json'
    if ($Tx.Previous -and (Test-Path -LiteralPath $exe) -and (Test-Path -LiteralPath $config)) {
        $kind = [string](Get-Prop $Tx.Previous 'autostart' 'none')
        $task = [string](Get-Prop $Tx.Previous 'task_name' $DefaultTaskName)
        try {
            $started = $false
            if ($kind -eq 'task') {
                $t = Get-PublisherTask $task
                if ($t -and [string]$t.State -ne 'Disabled') {
                    Start-ScheduledTask -TaskName $task -TaskPath '\' -ErrorAction Stop
                    $started = $true
                }
            } elseif ($kind -eq 'runkey' -and $null -ne (Get-RunEntry $task)) {
                $launch = Get-LaunchCommand $prefix
                Start-Process -FilePath $launch.Program -ArgumentList $launch.Arguments -WorkingDirectory $prefix -WindowStyle Hidden
                $started = $true
            }
            if ($started) {
                $proc = Wait-PublisherStart $prefix $WaitSeconds
                if ($proc) { $done.Add("running    the previous publisher, pid $($proc.Id)") }
                else { $failed.Add("the previous publisher did not start within $WaitSeconds s") }
            }
        } catch { $failed.Add("starting the previous publisher: $($_.Exception.Message)") }
    }

    if ($failed.Count -eq 0) {
        # The marker first: a folder that cannot be deleted now is then an
        # ordinary leftover, which the next install deletes.
        Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $Tx.Dir -Recurse -Force -ErrorAction SilentlyContinue
    } else {
        try { Write-TextFile $marker (Get-RollbackMarkerText $Tx @($failed)) } catch { }
    }
    $Tx.Completed = $true
    return [pscustomobject]@{ Done = $done; Failed = $failed; Dir = $Tx.Dir }
}
