<#
.SYNOPSIS
    Builds cc-hook in release and copies it to app\staging\cc-hook.exe, where the
    Tauri bundler picks it up (bundle > resources in app\tauri.conf.json).

.DESCRIPTION
    Runs as the installer's beforeBuildCommand. The build output lives outside
    the repo (see .cargo\config.toml), so the bundler cannot point at a fixed
    target\ path; this asks cargo where it builds and stages the binary inside
    the repo, in a git-ignored folder.

    app\build.rs (tauri-build) checks that bundled resources exist even for
    `cargo build` and `cargo test`, so on a fresh clone run this once first.
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

Push-Location $root
try {
    # --locked: build exactly what Cargo.lock pins; fail rather than update it.
    & cargo build -p cc-hook --release --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build -p cc-hook --locked failed ($LASTEXITCODE); if Cargo.lock is out of date, update it on purpose (cargo update -w) and commit it" }
    $meta = & cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    $source = Join-Path $meta.target_directory 'release\cc-hook.exe'
    if (-not (Test-Path $source)) { throw "cc-hook.exe not found at $source" }
    $staging = Join-Path $root 'app\staging'
    New-Item -ItemType Directory -Force -Path $staging | Out-Null
    Copy-Item -Force $source (Join-Path $staging 'cc-hook.exe')
    Write-Host "staged $source -> app\staging\cc-hook.exe"
} finally {
    Pop-Location
}
