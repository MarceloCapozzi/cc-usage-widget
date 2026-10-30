<#
.SYNOPSIS
    Isolated tests of the bundle generator's helpers
    (tools\publisher-bundle-gen\common.ps1 and keys.ps1).

.DESCRIPTION
    Everything happens in a new folder under %TEMP%: nothing is built,
    provisioned or written to %LOCALAPPDATA%\cc-usage, and toolctl, cargo and
    the app's settings are never touched.

      - Write-TarGz: the archive lists, with tar -tvzf (Windows' bsdtar and,
        when found, Git's GNU tar), every entry under the top folder with
        mode 0700 for folders and the executables given and 0600 for the
        other files, owner 0/0; paths over 100 bytes (the creds file of a
        31-character alias, a deep folder) go through the ustar prefix field
        and extract with their full name and content; Split-TarPath keeps
        short paths whole, splits long ones at a '/', and refuses a name
        part over 100 bytes, a prefix over 155 bytes and non-ASCII.
      - Write-PrivateZip: entries "<top>/<path>" with forward slashes, files
        only, the right content, and the zip private (current user and
        SYSTEM only, no inherited entries); New-PrivateFile and
        Set-PrivateAcl (a folder, recursively).
      - Test-InsideRepository: the repository and folders in it, in any
        case, but not a sibling that shares the name's start, nor %TEMP%.
      - Test-UnderSyncedFolder: under %OneDrive% (this process's
        environment only, restored) or a "\OneDrive - <org>\" folder, not
        elsewhere.
      - Assert-Alias: 1 to 31 lowercase letters, digits or dashes, starting
        with a letter or digit.
      - Assert-ViewerPubFile and Test-KeyFile: -ViewerPub must name an
        existing *.pub file (never a viewer.key) of <kid>:<base64 of 32
        bytes> lines.

    Run it with both Windows PowerShell 5.1 and PowerShell 7:

        powershell -NoProfile -ExecutionPolicy Bypass -File tools\publisher-bundle-gen\test\Test-BundleGen.ps1
        pwsh -NoProfile -File tools\publisher-bundle-gen\test\Test-BundleGen.ps1

    Exits 1 when any check fails.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0
$genDir = Split-Path -Parent $PSScriptRoot
# What tools\make-publisher-bundle.ps1 sets up before it dot-sources the
# generator's files.
$ToolsDir = Split-Path -Parent $genDir
. (Join-Path $ToolsDir 'cross\common.ps1')
$Utf8 = New-Object System.Text.UTF8Encoding($false)
. (Join-Path $genDir 'common.ps1')
. (Join-Path $genDir 'keys.ps1')

$root = Join-Path ([System.IO.Path]::GetTempPath()) "cc-bundle-gen-test $PID"
New-Item -ItemType Directory -Force -Path $root | Out-Null
$script:failures = 0
$script:passes = 0
function Assert([bool]$Ok, [string]$What) {
    if ($Ok) { $script:passes++; Write-Host "ok   $What" }
    else { $script:failures++; Write-Host "FAIL $What" -ForegroundColor Red }
}
function Get-Thrown([scriptblock]$Block) {
    try { & $Block | Out-Null; return $null } catch { return $_.Exception.Message }
}
function Write-Bytes([string]$Path, [string]$Text) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Path) | Out-Null
    [System.IO.File]::WriteAllBytes($Path, $Utf8.GetBytes($Text))
}
# $null when only the current user and SYSTEM have entries on $Path, the
# current user has a read entry and can actually read it (open the file,
# list the folder), and none is inherited (with -MayInherit, for what is in
# a private folder: inherited from it); else what is wrong.
function Get-AclProblem([string]$Path, [switch]$MayInherit) {
    $acl = Get-Acl -LiteralPath $Path
    if (-not $MayInherit -and -not $acl.AreAccessRulesProtected) { return 'inherits entries' }
    $me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $allowed = @($me, 'S-1-5-18')
    $rules = @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    $others = @($rules | Where-Object { $allowed -notcontains $_.IdentityReference.Value } | ForEach-Object { $_.IdentityReference.Value })
    if ($others.Count -gt 0) { return "also open to $($others -join ', ')" }
    $read = [System.Security.AccessControl.FileSystemRights]::Read
    $allow = [System.Security.AccessControl.AccessControlType]::Allow
    $mine = @($rules | Where-Object { $_.IdentityReference.Value -eq $me -and $_.AccessControlType -eq $allow -and (($_.FileSystemRights -band $read) -eq $read) })
    if ($mine.Count -eq 0) { return 'no read entry for the current user' }
    try {
        if (Test-Path -LiteralPath $Path -PathType Container) { [void](Get-ChildItem -LiteralPath $Path -Force -ErrorAction Stop) }
        else { [System.IO.File]::OpenRead($Path).Dispose() }
    } catch { return "not readable by the current user: $($_.Exception.Message)" }
    return $null
}
# The tar programs to read the archives with: Windows' bsdtar (gzip built
# in), and GNU tar from Git for Windows when it is there (it needs
# --force-local for a path with a drive letter, and gets the unpacked tar).
function Get-Tars {
    $tars = @()
    $bsd = Join-Path $env:SystemRoot 'System32\tar.exe'
    if (Test-Path -LiteralPath $bsd) { $tars += [pscustomobject]@{ Name = 'bsdtar'; Exe = $bsd; Extra = @(); Gzip = $true } }
    foreach ($gnu in @((Join-Path $env:ProgramFiles 'Git\usr\bin\tar.exe'))) {
        if (Test-Path -LiteralPath $gnu) { $tars += [pscustomobject]@{ Name = 'GNU tar'; Exe = $gnu; Extra = @('--force-local'); Gzip = $false } }
    }
    return $tars
}
function Invoke-Tar($Tar, [string[]]$Arguments) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $lines = @(& $Tar.Exe @($Tar.Extra + $Arguments) 2>&1 | ForEach-Object { [string]$_ })
        return [pscustomobject]@{ Code = $LASTEXITCODE; Lines = $lines }
    } finally { $ErrorActionPreference = $old }
}

try {
    Write-Host "PowerShell $($PSVersionTable.PSVersion) ($($PSVersionTable.PSEdition)), test folder $root"

    # ------------------------------------------------------------ Split-TarPath
    $p = Split-TarPath 'top/bin/cc-hook'
    Assert ($p[0] -eq '' -and $p[1] -eq 'top/bin/cc-hook') 'a short path stays in the name field'
    $hundred = 'a/' + ('b' * 98)
    $p = Split-TarPath $hundred
    Assert ($p[0] -eq '' -and $p[1] -eq $hundred) 'a path of exactly 100 bytes stays in the name field'
    $long = ('d' * 60) + '/' + ('e' * 30) + '/' + ('f' * 40)
    $p = Split-TarPath $long
    Assert ($p[0] -eq ('d' * 60) -and $p[1] -eq (('e' * 30) + '/' + ('f' * 40))) "a long path is split at the first '/' that fits ($($p[0].Length) + $($p[1].Length))"
    $p = Split-TarPath "$long/"
    Assert ($p[1].EndsWith('/') -and $p[0] -eq ('d' * 60)) "a folder keeps its trailing '/' in the name part"
    $p = Split-TarPath (('x' * 155) + '/' + ('y' * 100))
    Assert ($p[0].Length -eq 155 -and $p[1].Length -eq 100) 'the longest split: a 155-byte prefix and a 100-byte name'
    Assert ((Get-Thrown { Split-TarPath ('top/' + ('n' * 101)) }) -like 'path too long*') 'a name part over 100 bytes is refused'
    Assert ((Get-Thrown { Split-TarPath (('x' * 156) + '/' + ('y' * 10)) }) -like 'path too long*') 'a prefix over 155 bytes is refused'
    Assert ((Get-Thrown { Split-TarPath "top/caf$([char]0xE9)" }) -like '*not printable ASCII*') 'a non-ASCII path is refused'

    # ------------------------------------------------------------ Write-TarGz
    # The layout of a Linux bundle for the longest alias.
    $alias = 'a' + ('b' * 29) + 'z'
    Assert ($alias.Length -eq 31) 'the test alias has the maximum length'
    $top = "cc-usage-publisher-$alias"
    $src = Join-Path $root 'tar src'
    $credsRel = "secrets/cc-sessions.pub-$alias.creds"
    $deepRel = (@(1..6 | ForEach-Object { "folder-$_-" + ('q' * 12) }) -join '/') + '/deep.txt'
    $files = [ordered]@{
        'bin/cc-hook'         = "#!/bin/sh`necho hook`n"
        'bin/cc-publish'      = "#!/bin/sh`necho publish`n"
        'scripts/install.sh'  = "#!/bin/sh`nexit 0`n"
        'config/publish.json' = "{ `"alias`": `"$alias`" }`n"
        'empty.txt'           = ''
    }
    $files[$credsRel] = "-----BEGIN NATS USER JWT-----`nsim`n"
    $files[$deepRel] = ('content ' * 100)
    foreach ($rel in $files.Keys) { Write-Bytes (Join-Path $src $rel.Replace('/', '\')) $files[$rel] }
    Assert ("$top/$credsRel".Length -gt 100) "the creds path is over 100 bytes ($("$top/$credsRel".Length))"
    Assert ("$top/$($deepRel.Substring(0, $deepRel.LastIndexOf('/')))/".Length -gt 100) 'a folder path is over 100 bytes too'
    $exec = @('bin/cc-hook', 'bin/cc-publish', 'scripts/install.sh')
    $archive = Join-Path $root 'bundle.tar.gz'
    $thrown = Get-Thrown { Write-TarGz $src $top $archive $exec }
    Assert ($null -eq $thrown) "Write-TarGz writes the archive ($thrown)"
    $expected = @{ $top = 'd' }
    foreach ($item in Get-ChildItem -LiteralPath $src -Recurse) {
        $rel = $item.FullName.Substring($src.Length + 1).Replace('\', '/')
        $expected["$top/$rel"] = if ($item.PSIsContainer) { 'd' } elseif ($exec -contains $rel) { 'x' } else { 'f' }
    }
    # GNU tar needs a gzip program on PATH for z; it reads the archive
    # unpacked here instead (which also checks the gzip layer on its own).
    $plainTar = Join-Path $root 'bundle.tar'
    $inStream = [System.IO.File]::OpenRead($archive)
    try {
        $gunzip = New-Object System.IO.Compression.GZipStream($inStream, [System.IO.Compression.CompressionMode]::Decompress)
        $outStream = [System.IO.File]::Create($plainTar)
        try { $gunzip.CopyTo($outStream) } finally { $outStream.Dispose(); $gunzip.Dispose() }
    } finally { $inStream.Dispose() }
    Assert ((Get-Item -LiteralPath $plainTar).Length % 512 -eq 0) 'the archive unpacks to whole 512-byte tar blocks'
    $tars = @(Get-Tars)
    Assert ($tars.Count -gt 0) "a tar to check the archive with ($(($tars | ForEach-Object { $_.Name }) -join ', '))"
    foreach ($tar in $tars) {
        if ($tar.Gzip) { $file = $archive; $z = 'z' } else { $file = $plainTar; $z = '' }
        $r = Invoke-Tar $tar @("-tv${z}f", $file)
        Assert ($r.Code -eq 0) "$($tar.Name) lists the archive (exit $($r.Code))"
        $seen = @{}
        $bad = @()
        foreach ($line in $r.Lines) {
            $cols = @($line -split '\s+' | Where-Object { $_ })
            if ($cols.Count -lt 2) { continue }
            $mode = $cols[0]
            $name = $cols[-1].TrimEnd('/')
            $seen[$name] = $true
            if (-not $expected.ContainsKey($name)) { $bad += "unexpected $name"; continue }
            $want = switch ($expected[$name]) { 'd' { 'drwx------' } 'x' { '-rwx------' } default { '-rw-------' } }
            if ($mode -ne $want) { $bad += "$name is $mode, not $want" }
            if ($tar.Name -eq 'GNU tar' -and $cols[1] -ne '0/0') { $bad += "$name is owned by $($cols[1]), not 0/0" }
        }
        $missing = @($expected.Keys | Where-Object { -not $seen.ContainsKey($_) })
        Assert ($bad.Count -eq 0) "$($tar.Name): modes 0700 (folders, executables) and 0600 (files) ($($bad -join '; '))"
        Assert ($missing.Count -eq 0) "$($tar.Name): every entry is listed under $top, long paths whole ($($missing -join ', '))"
        $out = Join-Path $root "extract $($tar.Name)"
        New-Item -ItemType Directory -Force -Path $out | Out-Null
        $r = Invoke-Tar $tar @("-x${z}f", $file, '-C', $out)
        Assert ($r.Code -eq 0) "$($tar.Name) extracts the archive (exit $($r.Code)) $($r.Lines -join ' ')"
        $diff = @()
        foreach ($rel in $files.Keys) {
            $path = Join-Path $out ("$top/$rel").Replace('/', '\')
            if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { $diff += "$rel missing"; continue }
            if ([System.IO.File]::ReadAllText($path) -cne $files[$rel]) { $diff += "$rel differs" }
        }
        Assert ($diff.Count -eq 0) "$($tar.Name): every file extracts with its full name and content ($($diff -join '; '))"
    }
    $tooLong = Join-Path $root 'tar too long'
    Write-Bytes (Join-Path $tooLong ('n' * 101)) 'x'
    $thrown = Get-Thrown { Write-TarGz $tooLong 'top' (Join-Path $root 'too-long.tar.gz') @() }
    Assert ($thrown -like 'path too long*') "Write-TarGz refuses a file name over 100 bytes: $thrown"

    # ------------------------------------------------------------ Write-PrivateZip
    $zipSrc = Join-Path $root 'zip src'
    $zipFiles = [ordered]@{ 'bin\cc-hook.exe' = 'hook'; 'config\publish.json' = '{}'; 'setup.cmd' = '@echo off' }
    foreach ($rel in $zipFiles.Keys) { Write-Bytes (Join-Path $zipSrc $rel) $zipFiles[$rel] }
    New-Item -ItemType Directory -Force -Path (Join-Path $zipSrc 'empty dir') | Out-Null
    $zipPath = Join-Path $root 'bundle.zip'
    Write-PrivateZip $zipSrc 'cc-usage-publisher-x-windows' $zipPath
    Assert ($null -eq (Get-AclProblem $zipPath)) "the zip is private ($(Get-AclProblem $zipPath))"
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
    try {
        $names = @($zip.Entries | ForEach-Object { $_.FullName } | Sort-Object)
        $want = @($zipFiles.Keys | ForEach-Object { 'cc-usage-publisher-x-windows/' + $_.Replace('\', '/') } | Sort-Object)
        Assert (($names -join '|') -ceq ($want -join '|')) "the zip holds the files under the top folder, with '/' ($($names -join ', '))"
        $entry = $zip.GetEntry('cc-usage-publisher-x-windows/config/publish.json')
        $reader = New-Object System.IO.StreamReader($entry.Open())
        try { $text = $reader.ReadToEnd() } finally { $reader.Dispose() }
        Assert ($text -eq '{}') 'a zip entry has the file content'
    } finally { $zip.Dispose() }
    $privDir = Join-Path $root 'private dir'
    $secret = Join-Path $privDir 'sub\secret.txt'
    Write-Bytes $secret 's'
    Write-Bytes (Join-Path $privDir 'top.txt') 't'
    # A child with an explicit entry for Everyone of its own, which must go.
    $r = Invoke-Icacls @($secret, '/grant', '*S-1-1-0:R')
    Assert ($r[0] -eq 0) "the test gives secret.txt an Everyone entry (icacls exit $($r[0]))"
    Set-PrivateAcl $privDir
    Assert ($null -eq (Get-AclProblem $privDir)) "Set-PrivateAcl makes a folder private ($(Get-AclProblem $privDir))"
    $problems = @(foreach ($p in @((Join-Path $privDir 'sub'), $secret, (Join-Path $privDir 'top.txt'))) { $x = Get-AclProblem $p -MayInherit; if ($x) { "${p}: $x" } })
    Assert ($problems.Count -eq 0) "what was already in it inherits the private ACL only and stays readable ($($problems -join '; '))"
    Assert ([System.IO.File]::ReadAllText($secret) -eq 's') 'a file in the private folder reads back'
    Assert (-not (Get-Acl -LiteralPath $secret).AreAccessRulesProtected) 'a file in the private folder inherits its ACL'
    # Again on the same folder: nothing breaks, nothing opens up.
    Set-PrivateAcl $privDir
    Assert ($null -eq (Get-AclProblem $secret -MayInherit) -and $null -eq (Get-AclProblem $privDir)) 'Set-PrivateAcl twice leaves the folder private and readable'
    $emptyDir = Join-Path $root 'private empty dir'
    New-Item -ItemType Directory -Force -Path $emptyDir | Out-Null
    Set-PrivateAcl $emptyDir
    Assert ($null -eq (Get-AclProblem $emptyDir)) 'Set-PrivateAcl on an empty folder'
    $after = Join-Path $emptyDir 'later.txt'
    Write-Bytes $after 'l'
    Assert ($null -eq (Get-AclProblem $after -MayInherit)) 'a file written into a private folder later is private and readable'
    $newFile = Join-Path $root 'new private'
    Write-Bytes $newFile 'old'
    New-PrivateFile $newFile
    Assert ((Get-Item -LiteralPath $newFile).Length -eq 0 -and $null -eq (Get-AclProblem $newFile)) 'New-PrivateFile leaves an empty private file'

    # ------------------------------------------------------------ Test-InsideRepository
    $repo = Split-Path -Parent $ToolsDir
    Assert (Test-InsideRepository $repo) 'the repository itself is inside'
    Assert (Test-InsideRepository (Join-Path $repo 'some\bundles')) 'a folder in the repository is inside'
    Assert (Test-InsideRepository ($repo.ToUpperInvariant() + '\X')) 'the check ignores case'
    Assert (Test-InsideRepository ($repo + '\')) 'a trailing backslash changes nothing'
    Assert (-not (Test-InsideRepository ($repo + '-other\bundles'))) 'a sibling whose name starts the same is not inside'
    Assert (-not (Test-InsideRepository $root)) 'the temp folder is not inside'

    # ------------------------------------------------------------ Test-UnderSyncedFolder
    $savedEnv = @{}
    foreach ($n in 'OneDrive', 'OneDriveCommercial', 'OneDriveConsumer') { $savedEnv[$n] = [Environment]::GetEnvironmentVariable($n, 'Process') }
    try {
        foreach ($n in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($n, $null, 'Process') }
        $synced = Join-Path $root 'Sync Root'
        [Environment]::SetEnvironmentVariable('OneDrive', $synced, 'Process')
        Assert (Test-UnderSyncedFolder (Join-Path $synced 'bundles')) 'a folder under %OneDrive% is synced'
        Assert (Test-UnderSyncedFolder $synced) '%OneDrive% itself is synced'
        Assert (-not (Test-UnderSyncedFolder ($synced + ' 2\bundles'))) 'a sibling whose name starts the same is not'
        Assert (-not (Test-UnderSyncedFolder (Join-Path $root 'elsewhere'))) 'another folder is not'
        [Environment]::SetEnvironmentVariable('OneDrive', $null, 'Process')
        [Environment]::SetEnvironmentVariable('OneDriveCommercial', $synced, 'Process')
        Assert (Test-UnderSyncedFolder (Join-Path $synced 'x')) 'a folder under %OneDriveCommercial% is synced'
        [Environment]::SetEnvironmentVariable('OneDriveCommercial', $null, 'Process')
        Assert (Test-UnderSyncedFolder 'C:\Users\someone\OneDrive - Contoso\bundles') "a '\OneDrive - <org>\' folder is synced without the variables"
        Assert (Test-UnderSyncedFolder 'D:\OneDrive\bundles') "a '\OneDrive\' folder is synced"
        Assert (-not (Test-UnderSyncedFolder 'D:\OneDriveBackup\bundles')) 'a folder that only starts with OneDrive is not'
    } finally {
        foreach ($n in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($n, $savedEnv[$n], 'Process') }
    }

    # ------------------------------------------------------------ Assert-Alias
    foreach ($ok in @('a', '0', 'lab-01', 'pi-4', $alias)) {
        Assert ($null -eq (Get-Thrown { Assert-Alias $ok })) "alias '$ok' is valid"
    }
    foreach ($bad in @('', ($alias + 'x'), 'Lab', '-lab', 'lab.01', 'lab_01', 'lab 01', "lab$([char]0xE9)")) {
        Assert ((Get-Thrown { Assert-Alias $bad }) -like "invalid alias*") "alias '$bad' is refused"
    }

    # ------------------------------------------------------------ NATS servers and bucket
    $repo = Split-Path -Parent $ToolsDir
    $clientRs = [System.IO.File]::ReadAllText((Join-Path $repo 'crates\cc-remote\src\client.rs'))
    $m = [regex]::Match($clientRs, 'DEFAULT_SERVERS:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\];', 'Singleline')
    $rustServers = @([regex]::Matches($m.Groups[1].Value, '"([^"]+)"') | ForEach-Object { $_.Groups[1].Value })
    Assert ($m.Success -and ($rustServers -join ',') -ceq ($DefaultNatsServers -join ',')) "the default servers are cc-remote's ($($rustServers -join ', '))"
    $coreRs = [System.IO.File]::ReadAllText((Join-Path $repo 'crates\cc-core\src\lib.rs'))
    Assert ($coreRs.Contains("pub const PROJECT: &str = `"$DefaultNatsBucket`";") -and $coreRs.Contains('pub const BUCKET: &str = PROJECT;')) "the default bucket is cc_core::BUCKET ($DefaultNatsBucket)"
    $conf = [System.IO.File]::ReadAllText((Join-Path $repo 'app\tauri.conf.json')) | ConvertFrom-Json
    Assert ($conf.identifier -ceq $AppIdentifier) "the app config folder is named after the Tauri identifier ($($conf.identifier))"

    foreach ($ok in @('tls://nats.example:4222', 'wss://nats.example', 'wss://nats.example:8443/nats', 'tls://10.0.0.5:4443', 'tls://[::1]:4222', 'TLS://Upper.Example')) {
        Assert ($null -eq (Get-ServerProblem $ok)) "server '$ok' is accepted"
    }
    foreach ($bad in @('nats://x:4222', 'ws://x', 'https://x', 'tls://', 'tls://host:0', 'tls://host:70000', 'wss://me:secret@host', 'connect.ngs.global')) {
        $p = Get-ServerProblem $bad
        Assert ($null -ne $p -and -not $p.Contains('secret')) "server '$bad' is refused ($p)"
    }
    foreach ($ok in @('cc-sessions', 'team_1', ('b' * 64))) { Assert ($null -eq (Get-BucketProblem $ok)) "bucket '$ok' is accepted" }
    foreach ($bad in @('', 'cc.sessions', 'a b', '>', ('b' * 65))) { Assert ($null -ne (Get-BucketProblem $bad)) "bucket '$bad' is refused" }

    $savedNats = @{}
    foreach ($n in $NatsServersEnv, $NatsBucketEnv) { $savedNats[$n] = [Environment]::GetEnvironmentVariable($n, 'Process') }
    try {
        foreach ($n in $savedNats.Keys) { [Environment]::SetEnvironmentVariable($n, $null, 'Process') }
        $settings = Join-Path $root 'settings dir\settings.json'
        $t = Resolve-NatsTarget $null $null (Join-Path $root 'no such\settings.json')
        Assert (($t.Servers -join ',') -ceq ($DefaultNatsServers -join ',') -and $t.Bucket -ceq 'cc-sessions' -and $t.Warnings.Count -eq 0) 'without settings: the defaults, WebSocket first'
        Write-Bytes $settings '{"refreshMins":15,"nats":{"servers":["tls://nats.example:4222","wss://nats.example"],"bucket":"team"}}'
        $t = Resolve-NatsTarget $null $null $settings
        Assert (($t.Servers -join ',') -ceq 'tls://nats.example:4222,wss://nats.example' -and $t.Bucket -ceq 'team' -and $t.Warnings.Count -eq 0) "the viewer's settings.json is the default ($($t.Servers -join ', '); $($t.Bucket))"
        [Environment]::SetEnvironmentVariable($NatsServersEnv, 'wss://env.example, tls://env.example', 'Process')
        [Environment]::SetEnvironmentVariable($NatsBucketEnv, 'envbucket', 'Process')
        $t = Resolve-NatsTarget $null $null $settings
        Assert (($t.Servers -join ',') -ceq 'wss://env.example,tls://env.example' -and $t.Bucket -ceq 'envbucket') 'the variables win over settings.json, as in the app'
        $t = Resolve-NatsTarget @('tls://cli.example:4222,wss://cli.example') 'clibucket' $settings
        Assert (($t.Servers -join ',') -ceq 'tls://cli.example:4222,wss://cli.example' -and $t.Bucket -ceq 'clibucket' -and $t.Source -eq '-Servers') '-Servers (comma separated) and -Bucket win over everything'
        foreach ($n in $savedNats.Keys) { [Environment]::SetEnvironmentVariable($n, $null, 'Process') }
        Write-Bytes $settings '{"nats":{"servers":["tls://ok.example","nats://plain.example:4222"],"bucket":"cc.sessions"}}'
        $t = Resolve-NatsTarget $null $null $settings
        Assert (($t.Servers -join ',') -ceq ($DefaultNatsServers -join ',') -and $t.Bucket -ceq 'cc-sessions' -and $t.Warnings.Count -eq 2) "unusable viewer settings fall back to the defaults with a warning each ($($t.Warnings -join ' | '))"
        Write-Bytes $settings '{"nats":"wss://x"}'
        $t = Resolve-NatsTarget $null $null $settings
        Assert ($t.Warnings.Count -eq 1 -and $t.Warnings[0] -like '*not an object*') 'a nats member that is no object is reported'
        Write-Bytes $settings '{ not json'
        $t = Resolve-NatsTarget $null $null $settings
        Assert ($t.Warnings.Count -eq 1 -and $t.Bucket -ceq 'cc-sessions') 'an unreadable settings.json is reported and ignored'
        Assert ((Get-Thrown { Resolve-NatsTarget @('nats://x:4222') $null $null }) -like '-Servers:*plaintext*') 'a plaintext -Servers is refused'
        Assert ((Get-Thrown { Resolve-NatsTarget @(' ', '') $null $null }) -like '-Servers: no servers') 'an empty -Servers is refused'
        Assert ((Get-Thrown { Resolve-NatsTarget $null 'a.b' $null }) -like '-Bucket:*') 'a bad -Bucket is refused'
        $saved = $env:CC_USAGE_CONFIG_DIR
        try {
            $env:CC_USAGE_CONFIG_DIR = Join-Path $root 'cfg'
            Assert ((Get-ViewerSettingsPath) -eq (Join-Path $root 'cfg\settings.json')) 'CC_USAGE_CONFIG_DIR names the settings folder'
            $env:CC_USAGE_CONFIG_DIR = 'cfg\relative'
            if ($env:APPDATA) { Assert ((Get-ViewerSettingsPath) -eq (Join-Path $env:APPDATA "$AppIdentifier\settings.json")) 'a relative CC_USAGE_CONFIG_DIR is ignored, like the app does' }
            $env:CC_USAGE_CONFIG_DIR = $null
            if ($env:APPDATA) { Assert ((Get-ViewerSettingsPath) -eq (Join-Path $env:APPDATA "$AppIdentifier\settings.json")) 'else %APPDATA%\ar.cc.usage' }
        } finally { $env:CC_USAGE_CONFIG_DIR = $saved }
    } finally {
        foreach ($n in $savedNats.Keys) { [Environment]::SetEnvironmentVariable($n, $savedNats[$n], 'Process') }
    }

    # ------------------------------------------------------------ project file
    $example = Join-Path $repo 'infra\nats\cc-sessions.example.yaml'
    $info = Read-ProjectFile $example $example
    Assert ($info.Project -ceq 'cc-sessions' -and ($info.Buckets -join ',') -ceq 'cc-sessions') "the example project file declares cc-sessions and its bucket ($($info.Project); $($info.Buckets -join ', '))"
    $missing = Join-Path $root 'no-such.yaml'
    $thrown = Get-Thrown { Read-ProjectFile $missing $example }
    Assert ($thrown -like "NATS project file not found: $missing*copy $example to $missing*") "a missing project file says to copy the example ($thrown)"

    # ------------------------------------------------------------ -ViewerPub
    $keyLine = 'k1:' + [Convert]::ToBase64String((New-Object byte[] 32))
    $pub = Join-Path $root 'viewer copy.pub'
    Write-Bytes $pub "# viewer`n$keyLine`n"
    Assert ((Assert-ViewerPubFile $pub) -eq (Resolve-Path -LiteralPath $pub).Path) 'a .pub file is accepted, as a full path'
    Assert ((Test-KeyFile $pub) -eq 1) 'its key line is checked (one key)'
    $priv = Join-Path $root 'viewer.key'
    Write-Bytes $priv "$keyLine`n"
    Assert ((Get-Thrown { Assert-ViewerPubFile $priv }) -like '*is not a .pub file*never viewer.key*') 'a viewer.key (same line format) is refused by its name'
    Assert ((Get-Thrown { Assert-ViewerPubFile (Join-Path $root 'missing.pub') }) -like '*does not exist') 'a missing file is refused'
    $short = Join-Path $root 'short.pub'
    Write-Bytes $short ('k1:' + [Convert]::ToBase64String((New-Object byte[] 16)) + "`n")
    Assert ((Get-Thrown { Test-KeyFile $short }) -like '*16 bytes, expected 32') 'a key of the wrong length is refused'
    $none = Join-Path $root 'none.pub'
    Write-Bytes $none "# nothing`n"
    Assert ((Get-Thrown { Test-KeyFile $none }) -like '*holds no key') 'a file without keys is refused'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host "$($script:passes) passed, $($script:failures) failed"
if ($script:failures -gt 0) { exit 1 }
exit 0
