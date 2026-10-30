<#
.SYNOPSIS
    Sets up cross-compilation from Windows to static Linux binaries (musl).

.DESCRIPTION
    Portable, no administrator rights, no system install:
      1. downloads the pinned zig release zip from ziglang.org into
         %LOCALAPPDATA%\cc-cross (or $env:CC_CROSS_ROOT), checks its SHA-256
         and unpacks it there. The zip stays: build-linux.ps1 checks it
         against the pinned hash, and zig.exe against the zip's copy, before
         every use (and so does this script before it runs zig);
      2. installs the pinned cargo-zigbuild with `cargo install --locked`
         (into ~/.cargo/bin, like any cargo tool);
      3. adds the musl targets with `rustup target add`.
    Safe to run again: each step is skipped when already done.

.PARAMETER Targets
    Short target names to add (default: x86_64 and aarch64).

.EXAMPLE
    pwsh -File tools\cross\setup.ps1
#>
[CmdletBinding()]
param(
    [string[]]$Targets = @('x86_64', 'aarch64')
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\common.ps1"

Add-CargoToPath
New-Item -ItemType Directory -Force -Path $CrossRoot | Out-Null

# 1. zig. Nothing from the toolchain runs before Test-ZigIntegrity passes:
# the kept zip matches the pinned hash and zig.exe is the zip's copy.
$haveZig = $false
if ((Test-Path -LiteralPath $ZigExe) -and -not (Test-ZigIntegrity)) {
    $v = (& $ZigExe version 2>$null)
    if ("$v".Trim() -eq $ZigVersion) { $haveZig = $true }
}
if ($haveZig) {
    Write-Step "zig $ZigVersion already at $ZigDir (checked against $ZigZip)"
} else {
    $zip = $ZigZip
    $needDownload = $true
    if (Test-Path -LiteralPath $zip) {
        if ((Get-Sha256Hex $zip) -eq $ZigSha256) { $needDownload = $false }
    }
    if ($needDownload) {
        Write-Step "downloading zig $ZigVersion ($ZigUrl)"
        $partial = "$zip.partial"
        # WebClient rather than Invoke-WebRequest: no progress bar slowdown on
        # 5.1, and no dependency on the Utility module being loadable.
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        $wc = New-Object System.Net.WebClient
        try { $wc.DownloadFile($ZigUrl, $partial) } finally { $wc.Dispose() }
        $hash = Get-Sha256Hex $partial
        if ($hash -ne $ZigSha256) {
            Remove-Item -Force $partial
            throw "zig download has SHA-256 $hash, expected $ZigSha256. Not unpacking it."
        }
        Move-Item -Force $partial $zip
    }
    Write-Step "unpacking zig into $CrossRoot"
    if (Test-Path -LiteralPath $ZigDir) { Remove-Item -LiteralPath $ZigDir -Recurse -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zip, $CrossRoot)
    if (-not (Test-Path -LiteralPath $ZigExe)) { throw "zig.exe not found at $ZigExe after unpacking" }
    # The zip is kept: every later use checks zig.exe against it again.
    Assert-ZigIntegrity
    $v = (& $ZigExe version).Trim()
    if ($v -ne $ZigVersion) { throw "unpacked zig reports version $v, expected $ZigVersion" }
}

# 2. cargo-zigbuild
$zb = Get-ZigbuildVersion
if ($zb -eq $ZigbuildVersion) {
    Write-Step "cargo-zigbuild $ZigbuildVersion already installed"
} else {
    if ($zb) { Write-Step "cargo-zigbuild $zb found; installing the pinned $ZigbuildVersion" }
    else { Write-Step "installing cargo-zigbuild $ZigbuildVersion (a few minutes the first time)" }
    & cargo install cargo-zigbuild --locked --version $ZigbuildVersion
    if ($LASTEXITCODE -ne 0) { throw "cargo install cargo-zigbuild failed ($LASTEXITCODE)" }
    $zb = Get-ZigbuildVersion
    if ($zb -ne $ZigbuildVersion) { throw "cargo-zigbuild --version reports '$zb', expected $ZigbuildVersion" }
}

# 3. Rust targets
$installed = @(& rustup target list --installed)
foreach ($t in $Targets) {
    $triple = (Resolve-LinuxTarget $t).Triple
    if ($installed -contains $triple) {
        Write-Step "rust target $triple already installed"
    } else {
        Write-Step "rustup target add $triple"
        & rustup target add $triple
        if ($LASTEXITCODE -ne 0) { throw "rustup target add $triple failed ($LASTEXITCODE)" }
    }
}

Write-Step "cross toolchain ready: zig $ZigVersion at $ZigDir, cargo-zigbuild $ZigbuildVersion"
Write-Host "Next: pwsh -File tools\cross\build-linux.ps1 [-Target x86_64|aarch64]"
