<#
.SYNOPSIS
    Builds static Linux binaries of cc-hook and cc-publish with cargo-zigbuild.

.DESCRIPTION
    Runs `cargo zigbuild --release --locked --target <triple> -p cc-hook -p cc-publish`
    with the pinned zig from tools\cross\setup.ps1 on PATH (its zip and
    zig.exe are checked against the pinned SHA-256 first), then checks that
    every binary is a static ELF for the target's architecture (header bytes,
    no PT_INTERP, no DT_NEEDED; no `file` command needed).

    Packages that are not in the workspace yet are skipped with a warning,
    unless -Strict is given. Output: <target dir>\<triple>\release\<name>.

    Why zig: the musl targets need a C cross compiler and linker for crates
    with C or assembly parts (ring). cargo-zigbuild sets CC_<triple>,
    AR_<triple> and CARGO_TARGET_<TRIPLE>_LINKER to zig wrappers itself, so no
    extra environment is needed; see docs\publisher-linux.md for the manual
    fallback.

.PARAMETER Target
    x86_64 (default), aarch64, a full triple, or several of them.

.PARAMETER Packages
    Packages to build (default: cc-hook, cc-publish).

.PARAMETER Strict
    Fail when a requested package is not in the workspace.

.EXAMPLE
    pwsh -File tools\cross\build-linux.ps1
    pwsh -File tools\cross\build-linux.ps1 -Target x86_64,aarch64
#>
[CmdletBinding()]
param(
    [string[]]$Target = @('x86_64'),
    [string[]]$Packages = @('cc-hook', 'cc-publish'),
    [switch]$Strict
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\common.ps1"

# pwsh -File passes "-Target x86_64,aarch64" as one string; accept both forms.
$Target = @($Target | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$Packages = @($Packages | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

Add-CargoToPath
Use-Zig
if (-not (Get-ZigbuildVersion)) { throw 'cargo-zigbuild not found. Run tools\cross\setup.ps1 first.' }

$available = Get-WorkspacePackages
$build = @()
foreach ($p in $Packages) {
    if ($available -contains $p) { $build += $p }
    elseif ($Strict) { throw "package $p is not in the workspace" }
    else { Write-Warn "package $p is not in the workspace yet; skipping it" }
}
if ($build.Count -eq 0) { throw 'nothing to build' }

$targetDir = Get-CargoTargetDir
$results = @()
Push-Location $RepoRoot
try {
    foreach ($t in $Target) {
        $tgt = Resolve-LinuxTarget $t
        $pkgArgs = @()
        foreach ($p in $build) { $pkgArgs += @('-p', $p) }
        # --locked: exactly what Cargo.lock pins; fail rather than update it.
        Write-Step "cargo zigbuild --release --locked --target $($tgt.Triple) $($pkgArgs -join ' ')"
        & cargo zigbuild --release --locked --target $tgt.Triple @pkgArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo zigbuild failed for $($tgt.Triple) ($LASTEXITCODE)" }
        foreach ($p in $build) {
            $bin = Join-Path $targetDir "$($tgt.Triple)\release\$p"
            $e = Assert-StaticElf $bin $tgt
            $results += [pscustomobject]@{
                Target = $tgt.Name
                Binary = $p
                Size   = '{0:N0}' -f $e.Size
                ELF    = ('ELF64 {0} e_machine=0x{1:X2} static' -f $e.Type, $e.Machine)
                Path   = $bin
            }
        }
    }
} finally {
    Pop-Location
}

Write-Step 'built and checked'
$results | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
