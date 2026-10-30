# Shared settings and helpers for the Linux cross-compilation scripts
# (setup.ps1, build-linux.ps1) and tools/make-publisher-bundle.ps1.
# Dot-source it: . "$PSScriptRoot\common.ps1"
#
# Everything is portable: zig is unpacked under $CrossRoot (default
# %LOCALAPPDATA%\cc-cross, a per-user folder outside OneDrive; override with
# $env:CC_CROSS_ROOT) and put on PATH only for the build. Nothing is installed
# system-wide. Works on Windows PowerShell 5.1 and PowerShell 7.
#
# The downloaded zip is kept next to the unpacked toolchain: only its hash is
# pinned, so every use re-checks the zip against that hash and the unpacked
# zig.exe against the zip's own copy (Assert-ZigIntegrity). A hash recorded at
# unpack time would sit in the same writable folder as zig.exe and could be
# rewritten along with it; the zip check stays anchored on the pinned value.

Set-StrictMode -Version 3.0

# Pinned toolchain. Bump the three zig values together; the hash is the
# "shasum" of the x86_64-windows zip in https://ziglang.org/download/index.json.
$ZigVersion = '0.15.2'
$ZigZipName = "zig-x86_64-windows-$ZigVersion"
$ZigUrl = "https://ziglang.org/download/$ZigVersion/$ZigZipName.zip"
$ZigSha256 = '3a0ed1e8799a2f8ce2a6e6290a9ff22e6906f8227865911fb7ddedc3cc14cb0c'
$ZigbuildVersion = '0.23.4'

# Linux targets: short name used on the command line and in bundle names, the
# Rust triple, and the ELF e_machine value of its binaries.
$LinuxTargets = [ordered]@{
    'x86_64'  = @{ Triple = 'x86_64-unknown-linux-musl';  Machine = 0x3E; Uname = 'x86_64' }
    'aarch64' = @{ Triple = 'aarch64-unknown-linux-musl'; Machine = 0xB7; Uname = 'aarch64' }
}

$CrossRoot = if ($env:CC_CROSS_ROOT) {
    $env:CC_CROSS_ROOT
} elseif ($env:LOCALAPPDATA) {
    Join-Path $env:LOCALAPPDATA 'cc-cross'
} else {
    Join-Path $env:USERPROFILE 'AppData\Local\cc-cross'
}
$ZigDir = Join-Path $CrossRoot $ZigZipName
$ZigExe = Join-Path $ZigDir 'zig.exe'
$ZigZip = Join-Path $CrossRoot "$ZigZipName.zip"
# zig.exe inside the zip (entry names use forward slashes).
$ZigZipExeEntry = "$ZigZipName/zig.exe"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

# SHA-256 of a stream as lowercase hex.
function Get-StreamSha256Hex([System.IO.Stream]$Stream) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return (-join ($sha.ComputeHash($Stream) | ForEach-Object { $_.ToString('x2') }))
    } finally {
        $sha.Dispose()
    }
}

# SHA-256 of a file as lowercase hex. Plain .NET, so it also works when
# Windows PowerShell 5.1 is started from PowerShell 7 with the wrong module path.
function Get-Sha256Hex([string]$Path) {
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        return (Get-StreamSha256Hex $fs)
    } finally {
        $fs.Dispose()
    }
}

# What is wrong with the unpacked zig, or $null when it can be used: the kept
# zip must match the pinned $ZigSha256 and zig.exe must be byte for byte the
# zip's copy. Runs nothing from the toolchain.
function Test-ZigIntegrity {
    if (-not (Test-Path -LiteralPath $ZigZip -PathType Leaf)) { return "$ZigZip is missing" }
    $zipHash = Get-Sha256Hex $ZigZip
    if ($zipHash -ne $ZigSha256) { return "$ZigZip has SHA-256 $zipHash, expected $ZigSha256" }
    if (-not (Test-Path -LiteralPath $ZigExe -PathType Leaf)) { return "$ZigExe is missing" }
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($ZigZip)
    try {
        # Some zip writers (.NET Framework's among them) store backslashes.
        $entry = @($zip.Entries | Where-Object { $_.FullName.Replace('\', '/') -eq $ZigZipExeEntry }) | Select-Object -First 1
        if (-not $entry) { return "$ZigZip has no $ZigZipExeEntry" }
        $s = $entry.Open()
        try { $want = Get-StreamSha256Hex $s } finally { $s.Dispose() }
    } finally {
        $zip.Dispose()
    }
    $have = Get-Sha256Hex $ZigExe
    if ($have -ne $want) { return "$ZigExe has SHA-256 $have, but the verified zip's zig.exe has $want" }
    return $null
}

# Throws unless the unpacked zig passes Test-ZigIntegrity.
function Assert-ZigIntegrity {
    $problem = Test-ZigIntegrity
    if ($problem) { throw "zig $ZigVersion failed its integrity check: $problem. Run tools\cross\setup.ps1 (it downloads and unpacks it again)." }
}

function Write-Step([string]$Message) {
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Warn([string]$Message) {
    Write-Host "WARNING: $Message" -ForegroundColor Yellow
}

# Puts cargo (rustup's default location) on PATH when it is not there yet.
function Add-CargoToPath {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path $cargoBin)) {
        $env:Path = "$cargoBin;$env:Path"
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'cargo not found. Install Rust with rustup (https://rustup.rs) first.'
    }
}

# Puts the pinned zig first on PATH and tells cargo-zigbuild to use it, after
# checking it again (Assert-ZigIntegrity) on every use.
function Use-Zig {
    if (-not (Test-Path -LiteralPath $ZigExe)) {
        throw "zig $ZigVersion not found at $ZigExe. Run tools\cross\setup.ps1 first."
    }
    Assert-ZigIntegrity
    $env:Path = "$ZigDir;$env:Path"
    $env:CARGO_ZIGBUILD_ZIG_PATH = $ZigExe
    # Keep zig's caches next to the toolchain instead of %LOCALAPPDATA%\zig,
    # and out of OneDrive.
    if (-not $env:ZIG_GLOBAL_CACHE_DIR) { $env:ZIG_GLOBAL_CACHE_DIR = Join-Path $CrossRoot 'zig-cache' }
    if (-not $env:ZIG_LOCAL_CACHE_DIR) { $env:ZIG_LOCAL_CACHE_DIR = Join-Path $CrossRoot 'zig-local-cache' }
}

function Get-ZigbuildVersion {
    $exe = Get-Command cargo-zigbuild -ErrorAction SilentlyContinue
    if (-not $exe) { return $null }
    # `cargo zigbuild --version` is rejected; the binary itself answers it.
    $out = & $exe.Source --version 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $out) { return $null }
    if ("$out" -match '(\d+\.\d+\.\d+)') { return $Matches[1] }
    return $null
}

# Resolves a short target name (x86_64, aarch64) or a full triple.
function Resolve-LinuxTarget([string]$Name) {
    foreach ($k in $LinuxTargets.Keys) {
        if ($Name -eq $k -or $Name -eq $LinuxTargets[$k].Triple) {
            return [pscustomobject]@{
                Name    = $k
                Triple  = $LinuxTargets[$k].Triple
                Machine = $LinuxTargets[$k].Machine
                Uname   = $LinuxTargets[$k].Uname
            }
        }
    }
    throw "unknown target '$Name'; use one of: $($LinuxTargets.Keys -join ', ')"
}

# Cargo's target directory for the workspace (honours CARGO_TARGET_DIR).
function Get-CargoTargetDir {
    if ($env:CARGO_TARGET_DIR) { return $env:CARGO_TARGET_DIR }
    $meta = & cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $RepoRoot 'Cargo.toml') | ConvertFrom-Json
    return $meta.target_directory
}

# Names of the packages in the workspace.
function Get-WorkspacePackages {
    $meta = & cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $RepoRoot 'Cargo.toml') | ConvertFrom-Json
    return @($meta.packages | ForEach-Object { $_.name })
}

function Read-UInt16LE([byte[]]$b, [long]$o) { return [int]$b[$o] -bor ([int]$b[$o + 1] -shl 8) }
function Read-UInt32LE([byte[]]$b, [long]$o) { return [long](Read-UInt16LE $b $o) -bor ([long](Read-UInt16LE $b ($o + 2)) -shl 16) }
function Read-UInt64LE([byte[]]$b, [long]$o) { return [long](Read-UInt32LE $b $o) -bor ([long](Read-UInt32LE $b ($o + 4)) -shl 32) }

# Inspects an ELF file without external tools. Returns an object with
# IsElf, Class (64), Endian, Machine (e_machine), Type (EXEC or DYN),
# HasInterp (PT_INTERP present), Needed (count of DT_NEEDED entries) and
# IsStatic (no interpreter and no shared library dependencies; this is true
# for both static and static-pie executables).
function Get-ElfInfo([string]$Path) {
    $b = [System.IO.File]::ReadAllBytes($Path)
    $info = [ordered]@{ Path = $Path; IsElf = $false; Class = 0; Endian = ''; Machine = 0; Type = ''; HasInterp = $false; Needed = 0; IsStatic = $false; Size = $b.Length }
    if ($b.Length -lt 64 -or $b[0] -ne 0x7F -or $b[1] -ne 0x45 -or $b[2] -ne 0x4C -or $b[3] -ne 0x46) {
        return [pscustomobject]$info
    }
    $info.IsElf = $true
    $info.Class = if ($b[4] -eq 2) { 64 } elseif ($b[4] -eq 1) { 32 } else { 0 }
    $info.Endian = if ($b[5] -eq 1) { 'little' } else { 'big' }
    if ($info.Class -ne 64 -or $info.Endian -ne 'little') { return [pscustomobject]$info }
    $etype = Read-UInt16LE $b 16
    $info.Type = switch ($etype) { 2 { 'EXEC' } 3 { 'DYN' } default { "type$etype" } }
    $info.Machine = Read-UInt16LE $b 18
    $phoff = Read-UInt64LE $b 32
    $phentsize = Read-UInt16LE $b 54
    $phnum = Read-UInt16LE $b 56
    $dynOff = -1; $dynSize = 0
    for ($i = 0; $i -lt $phnum; $i++) {
        $p = $phoff + $i * $phentsize
        $ptype = Read-UInt32LE $b $p
        if ($ptype -eq 3) { $info.HasInterp = $true }            # PT_INTERP
        if ($ptype -eq 2) {                                      # PT_DYNAMIC
            $dynOff = Read-UInt64LE $b ($p + 8)
            $dynSize = Read-UInt64LE $b ($p + 32)
        }
    }
    if ($dynOff -ge 0) {
        for ($o = $dynOff; $o + 16 -le $dynOff + $dynSize -and $o + 16 -le $b.Length; $o += 16) {
            $tag = Read-UInt64LE $b $o
            if ($tag -eq 0) { break }                            # DT_NULL
            if ($tag -eq 1) { $info.Needed++ }                   # DT_NEEDED
        }
    }
    $info.IsStatic = (-not $info.HasInterp) -and ($info.Needed -eq 0)
    return [pscustomobject]$info
}

# Throws unless $Path is a static 64-bit little-endian ELF for $Target.
function Assert-StaticElf([string]$Path, $Target) {
    if (-not (Test-Path $Path)) { throw "missing binary $Path" }
    $e = Get-ElfInfo $Path
    if (-not $e.IsElf) { throw "$Path is not an ELF file" }
    if ($e.Class -ne 64 -or $e.Endian -ne 'little') { throw "$Path is not a 64-bit little-endian ELF" }
    if ($e.Machine -ne $Target.Machine) {
        throw ("{0} is for e_machine 0x{1:X}, expected 0x{2:X} ({3})" -f $Path, $e.Machine, $Target.Machine, $Target.Name)
    }
    if (-not $e.IsStatic) {
        throw "$Path is dynamically linked (interpreter: $($e.HasInterp), DT_NEEDED entries: $($e.Needed))"
    }
    return $e
}
