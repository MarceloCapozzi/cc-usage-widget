# Helpers of tools\make-publisher-bundle.ps1 that every platform uses: the
# alias check, the NATS servers and bucket, the project file, the synced
# folder and repository checks, LF text files, the key file check, the
# tar.gz and zip writers and the private ACL. Dot-sourced by that script
# (after tools\cross\common.ps1), so it runs in the script's scope and uses
# its variables ($Utf8, $ToolsDir); not meant to be run on its own.

# cc-remote's DEFAULT_SERVERS and DEFAULT_BUCKET (crates\cc-remote\src\
# client.rs; Test-BundleGen.ps1 checks they match): what the viewer uses
# when its settings name nothing else.
$DefaultNatsServers = @('wss://connect.ngs.global', 'tls://connect.ngs.global')
$DefaultNatsBucket = 'cc-sessions'
# The app's config folder under %APPDATA% (its Tauri identifier), which
# holds settings.json, and the variables that override its "nats" object.
$AppIdentifier = 'ar.cc.usage'
$NatsServersEnv = 'CC_USAGE_NATS_SERVERS'
$NatsBucketEnv = 'CC_USAGE_NATS_BUCKET'

# Why a server URL cannot be used, or $null: like cc-remote, only tls:// or
# wss://, a host, an optional port, no user or password in the URL.
function Get-ServerProblem([string]$Url) {
    if ($Url -match '^(?i)(nats|ws)://') { return "${Url}: plaintext transports are refused; use tls:// or wss://" }
    if ($Url -notmatch '^(?i)(tls|wss)://') { return "${Url}: use tls:// or wss://" }
    $authority = ($Url -replace '^(?i)(tls|wss)://', '') -replace '[/?#].*$', ''
    if ($authority.Contains('@')) { return 'a server URL holds a user or password; the creds file authenticates' }
    $uri = $null
    if (-not [System.Uri]::TryCreate($Url, [System.UriKind]::Absolute, [ref]$uri)) { return "${Url}: not a valid URL" }
    if (-not $uri.Host) { return "${Url}: no host" }
    if ($authority -match ':(\d*)$' -and $authority -notmatch '\]$') {
        $port = $Matches[1]
        if (-not $port -or [int64]$port -lt 1 -or [int64]$port -gt 65535) { return "${Url}: bad port" }
    }
    return $null
}

# Why a bucket name cannot be used, or $null (cc-remote's validate_bucket).
function Get-BucketProblem([string]$Name) {
    if ($Name -cnotmatch '^[A-Za-z0-9_-]{1,64}$') { return "bucket '$Name': use 1 to 64 letters, digits, '-' or '_'" }
    return $null
}

# The app config dir as the app finds it: CC_USAGE_CONFIG_DIR when it is an
# absolute path (the app ignores a relative one), else %APPDATA%\ar.cc.usage.
function Get-ViewerConfigDir {
    $override = $env:CC_USAGE_CONFIG_DIR
    if ($override -and $override -match '^([A-Za-z]:[\\/]|[\\/]{2})') { return $override }
    if ($env:APPDATA) { return (Join-Path $env:APPDATA $AppIdentifier) }
    return $null
}

# settings.json of the app on this machine, in Get-ViewerConfigDir.
function Get-ViewerSettingsPath {
    $dir = Get-ViewerConfigDir
    if (-not $dir) { return $null }
    return (Join-Path $dir 'settings.json')
}

# The servers and bucket a bundle publishes to: -Servers and -Bucket when
# given (an unusable one throws), else the viewer's effective settings as
# the app resolves them (CC_USAGE_NATS_SERVERS, comma separated, and
# CC_USAGE_NATS_BUCKET, else the "nats" object of $SettingsPath, read only),
# else the defaults. A viewer setting that cannot be used is reported in
# Warnings and replaced by the default, as the app does. Returns
# @{ Servers; Bucket; Source; Warnings }.
function Resolve-NatsTarget([string[]]$Servers, [string]$Bucket, [string]$SettingsPath) {
    $warnings = New-Object System.Collections.Generic.List[string]
    $fileServers = $null
    $fileBucket = $null
    if ($SettingsPath -and (Test-Path -LiteralPath $SettingsPath -PathType Leaf)) {
        $nats = $null
        try {
            $json = [System.IO.File]::ReadAllText($SettingsPath) | ConvertFrom-Json
            if ($null -ne $json -and $json.PSObject.Properties['nats']) { $nats = $json.nats }
        } catch { $warnings.Add("${SettingsPath}: not JSON; its nats settings are ignored") }
        if ($null -ne $nats) {
            if ($nats -isnot [System.Management.Automation.PSCustomObject]) {
                $warnings.Add('settings.json: nats is not an object; using the default servers and bucket')
            } else {
                if ($nats.PSObject.Properties['servers'] -and $null -ne $nats.servers) { $fileServers = @($nats.servers | ForEach-Object { [string]$_ }) }
                if ($nats.PSObject.Properties['bucket'] -and $null -ne $nats.bucket) { $fileBucket = [string]$nats.bucket }
            }
        }
    }
    $source = 'defaults'
    if ($Servers) {
        $list = @($Servers | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
        if ($list.Count -eq 0) { throw '-Servers: no servers' }
        foreach ($s in $list) { $p = Get-ServerProblem $s; if ($p) { throw "-Servers: $p" } }
        $outServers = $list
        $source = '-Servers'
    } else {
        $outServers = $DefaultNatsServers
        $viewer = $null
        $envServers = [Environment]::GetEnvironmentVariable($NatsServersEnv, 'Process')
        if ($envServers -and $envServers.Trim()) { $viewer = @{ From = $NatsServersEnv; List = @($envServers -split ',') } }
        elseif ($null -ne $fileServers) { $viewer = @{ From = 'settings.json: nats.servers'; List = $fileServers } }
        if ($viewer) {
            $list = @($viewer.List | ForEach-Object { $_.Trim() } | Where-Object { $_ })
            $problem = if ($list.Count -eq 0) { 'no servers' } else { @($list | ForEach-Object { Get-ServerProblem $_ } | Where-Object { $_ }) | Select-Object -First 1 }
            if ($problem) { $warnings.Add("$($viewer.From) ignored ($problem); using the default servers, as the viewer does") }
            else { $outServers = $list; $source = $viewer.From }
        }
    }
    if ($Bucket) {
        $p = Get-BucketProblem $Bucket.Trim()
        if ($p) { throw "-Bucket: $p" }
        $outBucket = $Bucket.Trim()
    } else {
        $outBucket = $DefaultNatsBucket
        $viewer = $null
        $envBucket = [Environment]::GetEnvironmentVariable($NatsBucketEnv, 'Process')
        if ($envBucket -and $envBucket.Trim()) { $viewer = @{ From = $NatsBucketEnv; Name = $envBucket.Trim() } }
        elseif ($null -ne $fileBucket) { $viewer = @{ From = 'settings.json: nats.bucket'; Name = $fileBucket.Trim() } }
        if ($viewer) {
            $p = Get-BucketProblem $viewer.Name
            if ($p) { $warnings.Add("$($viewer.From) ignored ($p); using the default bucket $DefaultNatsBucket, as the viewer does") }
            else { $outBucket = $viewer.Name }
        }
    }
    return @{ Servers = @($outServers); Bucket = $outBucket; Source = $source; Warnings = @($warnings) }
}

# The NATS project file, read with a line parser (it is flat YAML): the
# project name and the bucket names it declares. Throws with what to do
# when the file is missing.
function Read-ProjectFile([string]$Path, [string]$Example) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "NATS project file not found: $Path. It is this deployment's own copy (ignored by git): copy $Example to $Path, set its account and the machines' entries, then run this again (or pass -ProjectFile, or -SkipProvision with existing creds)."
    }
    $project = $null
    $buckets = @()
    $section = $null
    foreach ($line in [System.IO.File]::ReadAllLines($Path)) {
        if ($line -match '^\s*(#|$)') { continue }
        if ($line -match '^(\S[^:]*):\s*(.*?)\s*$') {
            $section = $Matches[1]
            if ($section -eq 'project') { $project = $Matches[2].Trim('"', "'") }
            continue
        }
        if ($section -eq 'buckets' -and $line -match '^\s*-\s*name:\s*(.+?)\s*$') { $buckets += $Matches[1].Trim('"', "'") }
    }
    return @{ Project = $project; Buckets = $buckets }
}

# A machine alias: 1 to 31 lowercase letters, digits or dashes, starting with
# a letter or digit (the bundle scripts check the same). Throws otherwise.
function Assert-Alias([string]$Alias) {
    if ($Alias -cnotmatch '^[a-z0-9][a-z0-9-]{0,30}$') {
        throw "invalid alias '$Alias': use 1-31 lowercase letters, digits or dashes, starting with a letter or digit (no dots)"
    }
}

function Test-UnderSyncedFolder([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path).TrimEnd('\') + '\'
    foreach ($root in @($env:OneDrive, $env:OneDriveCommercial, $env:OneDriveConsumer)) {
        if ($root -and $full.StartsWith(([System.IO.Path]::GetFullPath($root).TrimEnd('\') + '\'), [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return ($full -match '\\OneDrive( - [^\\]+)?\\')
}

# Reads a text file and returns its bytes with LF line endings.
function Get-LfBytes([string]$Path) {
    $text = [System.IO.File]::ReadAllText($Path)
    return $Utf8.GetBytes($text.Replace("`r`n", "`n"))
}

function Write-LfFile([string]$Path, [string]$Text) {
    New-Item -ItemType Directory -Force -Path (Split-Path $Path) | Out-Null
    [System.IO.File]::WriteAllBytes($Path, $Utf8.GetBytes($Text.Replace("`r`n", "`n")))
}

# Checks a key file without revealing it: every non-comment line is
# <kid>:<base64 of 32 bytes>. Returns the number of keys.
function Test-KeyFile([string]$Path) {
    $count = 0
    $n = 0
    foreach ($line in [System.IO.File]::ReadAllLines($Path)) {
        $n++
        $t = $line.Trim()
        if ($t -eq '' -or $t.StartsWith('#')) { continue }
        if ($t -notmatch '^[A-Za-z0-9_-]{1,32}:[A-Za-z0-9+/]+=*$') { throw "key file $Path line ${n} is not <kid>:<base64>" }
        $len = ([Convert]::FromBase64String($t.Substring($t.IndexOf(':') + 1))).Length
        if ($len -ne 32) { throw "key file $Path line ${n}: key is $len bytes, expected 32" }
        $count++
    }
    if ($count -eq 0) { throw "key file $Path holds no key" }
    return $count
}

function Set-TarField([byte[]]$Header, [int]$Offset, [string]$Text) {
    $b = [System.Text.Encoding]::ASCII.GetBytes($Text)
    [Array]::Copy($b, 0, $Header, $Offset, $b.Length)
}

# Splits an archive path into the ustar name (at most 100 bytes) and prefix
# (at most 155 bytes) fields: a path that fits the name field goes there
# whole; a longer one is cut at a '/' so that the part after it fits the name
# field and the part before it the prefix field (readers join them with a
# '/'). Returns @(prefix, name); throws for a path no split fits, or one that
# is not printable ASCII.
function Split-TarPath([string]$Path) {
    if ($Path -cnotmatch '^[\x20-\x7e]+$') { throw "path not printable ASCII, which the archive does not carry: $Path" }
    if ($Path.Length -le 100) { return @('', $Path) }
    # A directory's trailing '/' stays with the name part.
    $body = $Path.TrimEnd('/')
    for ($i = $body.IndexOf('/'); $i -ge 0; $i = $body.IndexOf('/', $i + 1)) {
        $prefix = $Path.Substring(0, $i)
        $name = $Path.Substring($i + 1)
        if ($prefix.Length -le 155 -and $name.Length -le 100) { return @($prefix, $name) }
        if ($prefix.Length -gt 155) { break }
    }
    throw "path too long for the archive (a ustar path is a prefix of at most 155 bytes, '/' and a name of at most 100): $Path"
}

# Minimal ustar + gzip writer, so the archive carries exact Unix modes
# (0700 directories, 0600 files, 0700 executables) instead of whatever the
# Windows tar derives from file attributes.
function Write-TarGz([string]$SourceRoot, [string]$TopName, [string]$Dest, [string[]]$ExecutableRel) {
    $entries = New-Object System.Collections.Generic.List[object]
    $entries.Add([pscustomobject]@{ Rel = ''; Dir = $true; Path = $SourceRoot })
    foreach ($item in (Get-ChildItem -LiteralPath $SourceRoot -Recurse | Sort-Object FullName)) {
        $rel = $item.FullName.Substring($SourceRoot.Length).TrimStart('\').Replace('\', '/')
        $entries.Add([pscustomobject]@{ Rel = $rel; Dir = $item.PSIsContainer; Path = $item.FullName })
    }
    $mtime = [long]([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())
    $fs = [System.IO.File]::Create($Dest)
    try {
        $gz = New-Object System.IO.Compression.GZipStream($fs, [System.IO.Compression.CompressionLevel]::Optimal)
        try {
            foreach ($e in $entries) {
                $name = if ($e.Rel) { "$TopName/$($e.Rel)" } else { $TopName }
                if ($e.Dir) { $name += '/' }
                $parts = Split-TarPath $name
                # Assigned in each branch: an empty array returned from an if would become $null.
                if ($e.Dir) { $data = New-Object byte[] 0 } else { $data = [System.IO.File]::ReadAllBytes($e.Path) }
                $mode = if ($e.Dir -or ($ExecutableRel -contains $e.Rel)) { '0000700' } else { '0000600' }
                $h = New-Object byte[] 512
                Set-TarField $h 0 $parts[1]
                Set-TarField $h 100 "$mode`0"
                # uid and gid 0, no names: a non-root tar gives the files to
                # whoever extracts them. Under sudo they would be root's;
                # check-prereqs.sh refuses that folder with a fix hint.
                Set-TarField $h 108 "0000000`0"
                Set-TarField $h 116 "0000000`0"
                Set-TarField $h 124 (([Convert]::ToString([long]$data.Length, 8)).PadLeft(11, '0') + "`0")
                Set-TarField $h 136 (([Convert]::ToString($mtime, 8)).PadLeft(11, '0') + "`0")
                Set-TarField $h 148 '        '
                Set-TarField $h 156 $(if ($e.Dir) { '5' } else { '0' })
                Set-TarField $h 257 "ustar`0"
                Set-TarField $h 263 '00'
                if ($parts[0]) { Set-TarField $h 345 $parts[0] }
                $sum = 0
                foreach ($byte in $h) { $sum += $byte }
                Set-TarField $h 148 (([Convert]::ToString($sum, 8)).PadLeft(6, '0') + "`0 ")
                $gz.Write($h, 0, 512)
                if ($data.Length -gt 0) {
                    $gz.Write($data, 0, $data.Length)
                    $pad = (512 - ($data.Length % 512)) % 512
                    if ($pad -gt 0) { $gz.Write((New-Object byte[] $pad), 0, $pad) }
                }
            }
            $end = New-Object byte[] 1024
            $gz.Write($end, 0, 1024)
        } finally { $gz.Dispose() }
    } finally { $fs.Dispose() }
}

# Runs icacls with $Arguments and returns @(exit code, output). Continue: on
# Windows PowerShell, stderr of a native command under Stop would end the
# script before the exit code is looked at.
function Invoke-Icacls([string[]]$Arguments) {
    $icacls = Join-Path $env:SystemRoot 'System32\icacls.exe'
    $ErrorActionPreference = 'Continue'
    $out = & $icacls ($Arguments + @('/Q')) 2>&1
    $rc = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    return @($rc, "$out")
}

# Makes a folder and what is in it, or a file, readable by the current user
# and SYSTEM only, then checks it. Throws when it cannot: everything it is
# used on holds secrets.
#
# The item itself gets a protected ACL (inheritance off) with full control
# for the two, inheritable on a folder, and explicit entries of anybody else
# removed (new items under %LOCALAPPDATA% can carry explicit Administrators
# entries). On a folder, what is already in it is then reset to inherit
# that ACL and nothing else (icacls <folder>\* /reset /T). Stamping the
# folder's own protected (OI)(CI) entries onto every child with /T instead
# left the files there unreadable, even by their owner.
function Set-PrivateAcl([string]$Path) {
    $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $system = 'S-1-5-18'
    $isDir = Test-Path -LiteralPath $Path -PathType Container
    $perm = if ($isDir) { '(OI)(CI)F' } else { 'F' }
    $r = Invoke-Icacls @($Path, '/inheritance:r', '/grant:r', "*${sid}:$perm", "*${system}:$perm")
    if ($r[0] -ne 0) { throw "could not restrict the ACL of $Path, which holds secrets (icacls exit $($r[0])): $($r[1])" }
    $allowed = @($sid, $system)
    $acl = Get-Acl -LiteralPath $Path
    foreach ($rule in @($acl.GetAccessRules($true, $false, [System.Security.Principal.SecurityIdentifier]))) {
        $s = $rule.IdentityReference.Value
        if ($allowed -contains $s) { continue }
        $r = Invoke-Icacls @($Path, '/remove', "*$s")
        if ($r[0] -ne 0) { throw "could not remove $s from the ACL of $Path, which holds secrets (icacls exit $($r[0])): $($r[1])" }
    }
    $acl = Get-Acl -LiteralPath $Path
    if (-not $acl.AreAccessRulesProtected) { throw "the ACL of $Path still inherits entries from its folder after icacls" }
    $others = @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]) | Where-Object {
            $_.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow -and $allowed -notcontains $_.IdentityReference.Value
        } | ForEach-Object { $_.IdentityReference.Value } | Select-Object -Unique)
    if ($others.Count -gt 0) { throw "the ACL of $Path is not private after icacls: also open to $($others -join ', ')" }
    if (-not $isDir) { return }
    $children = @(Get-ChildItem -LiteralPath $Path -Force -Recurse)
    if ($children.Count -eq 0) { return }
    $r = Invoke-Icacls @((Join-Path $Path '*'), '/reset', '/T')
    if ($r[0] -ne 0) { throw "could not make the contents of $Path inherit its private ACL (icacls exit $($r[0])): $($r[1])" }
    foreach ($child in $children) {
        $open = @((Get-Acl -LiteralPath $child.FullName).GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]) | Where-Object {
                $_.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow -and $allowed -notcontains $_.IdentityReference.Value
            })
        if ($open.Count -gt 0) { throw "the ACL of $($child.FullName) is not private after icacls" }
        try {
            if ($child.PSIsContainer) { [void](Get-ChildItem -LiteralPath $child.FullName -Force) }
            else { [System.IO.File]::OpenRead($child.FullName).Dispose() }
        } catch { throw "$($child.FullName) is not readable by its owner after icacls: $($_.Exception.Message)" }
    }
}

# Creates $Path empty with the private ACL, so whatever is written into it
# later (an archive of the bundle) is never readable by anybody else, not
# even for a moment. Opening an existing file to overwrite or truncate it
# keeps its ACL.
function New-PrivateFile([string]$Path) {
    if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -Force }
    [System.IO.File]::WriteAllBytes($Path, (New-Object byte[] 0))
    Set-PrivateAcl $Path
}

# Zips $SourceRoot into $Dest under the top folder $TopName (entry names with
# forward slashes, files only, like ZipFile.CreateFromDirectory with the base
# directory), writing into a file that is private before any byte goes in.
function Write-PrivateZip([string]$SourceRoot, [string]$TopName, [string]$Dest) {
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    New-PrivateFile $Dest
    $fs = [System.IO.File]::Open($Dest, [System.IO.FileMode]::Truncate, [System.IO.FileAccess]::ReadWrite)
    try {
        $zip = New-Object System.IO.Compression.ZipArchive($fs, [System.IO.Compression.ZipArchiveMode]::Create, $true)
        try {
            foreach ($f in (Get-ChildItem -LiteralPath $SourceRoot -Recurse -File | Sort-Object FullName)) {
                $rel = $f.FullName.Substring($SourceRoot.Length).TrimStart('\').Replace('\', '/')
                [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $f.FullName, "$TopName/$rel", [System.IO.Compression.CompressionLevel]::Optimal)
            }
        } finally { $zip.Dispose() }
    } finally { $fs.Dispose() }
    Set-PrivateAcl $Dest
}

# Whether $Path is the repository or inside it (full paths, compared
# case-insensitively; the repository is the parent of tools\).
function Test-InsideRepository([string]$Path) {
    $repo = [System.IO.Path]::GetFullPath((Split-Path -Parent $ToolsDir)).TrimEnd('\')
    $full = [System.IO.Path]::GetFullPath($Path).TrimEnd('\')
    return ($full.Equals($repo, [System.StringComparison]::OrdinalIgnoreCase) -or
        $full.StartsWith($repo + '\', [System.StringComparison]::OrdinalIgnoreCase))
}
