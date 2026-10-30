# -Platform windows of tools\make-publisher-bundle.ps1: New-WindowsBundle and
# its checks (the sources in tools\publisher-bundle-windows, the PE machine
# of the binaries). Dot-sourced by that script; uses its parameters and
# variables ($Alias, $Target, $Out, $BinDir, $SkipBuild, $NoArchive, $User,
# $CredsName, $Servers, $Bucket, $Utf8, $ToolsDir, $RepoRoot).

$WinSourceDir = Join-Path $ToolsDir 'publisher-bundle-windows'
# Text files of the Windows bundle, written with CRLF (cmd.exe misreads
# batch files with bare LF line endings).
$WinTextFiles = @('setup.cmd', 'Install.ps1', 'Verify.ps1', 'Uninstall.ps1', 'README.md', 'scripts\common.ps1')
$WinTargets = [ordered]@{
    'x86_64'  = @{ Triple = 'x86_64-pc-windows-msvc'; Machine = 0x8664; Suffix = 'windows' }
    'aarch64' = @{ Triple = 'aarch64-pc-windows-msvc'; Machine = 0xAA64; Suffix = 'windows-arm64' }
}

# Text with CRLF line endings, as bytes.
function Get-CrlfBytes([string]$Path) {
    $text = [System.IO.File]::ReadAllText($Path).Replace("`r`n", "`n").Replace("`n", "`r`n")
    return $Utf8.GetBytes($text)
}

# Checks the Windows bundle sources: every script parses (PowerShell's own
# parser), and every text file is ASCII, since Windows PowerShell 5.1 reads a
# script without a BOM in the ANSI code page.
function Test-WindowsBundleSources {
    $problems = @()
    foreach ($rel in $WinTextFiles) {
        $path = Join-Path $WinSourceDir $rel
        if (-not (Test-Path -LiteralPath $path)) { $problems += "$rel is missing"; continue }
        $bytes = [System.IO.File]::ReadAllBytes($path)
        $bad = @($bytes | Where-Object { $_ -gt 127 }).Count
        if ($bad -gt 0) { $problems += "${rel}: $bad non-ASCII bytes" }
        if ($rel -like '*.ps1') {
            $tokens = $null
            $errors = $null
            [void][System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
            foreach ($e in @($errors)) { $problems += "${rel}:$($e.Extent.StartLineNumber): $($e.Message)" }
        }
    }
    return , $problems
}

# The Machine field of a PE file's COFF header (0x8664 x64, 0xAA64 ARM64),
# or -1 when it is not a PE file.
function Get-PeMachine([string]$Path) {
    $b = [System.IO.File]::ReadAllBytes($Path)
    if ($b.Length -lt 64 -or $b[0] -ne 0x4D -or $b[1] -ne 0x5A) { return -1 }
    $pe = [int](Read-UInt32LE $b 0x3C)
    if ($pe + 6 -gt $b.Length -or $b[$pe] -ne 0x50 -or $b[$pe + 1] -ne 0x45 -or $b[$pe + 2] -ne 0 -or $b[$pe + 3] -ne 0) { return -1 }
    return (Read-UInt16LE $b ($pe + 4))
}

function Test-RustTargetInstalled([string]$Triple) {
    $rustup = Get-Command rustup -ErrorAction SilentlyContinue
    if (-not $rustup) { return $false }
    $installed = & $rustup.Source target list --installed 2>$null
    return (@($installed) -contains $Triple)
}

function New-WindowsBundle {
    $wt = $WinTargets[$Target]
    $bundleName = "cc-usage-publisher-$Alias-$($wt.Suffix)"
    $bundleDir = Join-Path $Out $bundleName
    $zip = Join-Path $Out "$bundleName.zip"
    Write-Step "Windows bundle for alias '$Alias' (NATS user $User, $($wt.Triple))"

    # 2. sources
    Write-Step 'checking the Windows bundle sources (PowerShell parser, ASCII)'
    $problems = Test-WindowsBundleSources
    if ($problems.Count -gt 0) {
        $problems | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        throw 'the Windows bundle sources have problems; fix them first'
    }

    # 3. binaries
    Add-CargoToPath
    $hostTriple = ((& rustc -vV) | Where-Object { $_ -like 'host: *' } | Select-Object -First 1) -replace '^host: ', ''
    $native = ($hostTriple -eq $wt.Triple)
    if (-not $BinDir) {
        $BinDir = if ($native) { Join-Path (Get-CargoTargetDir) 'release' } else { Join-Path (Get-CargoTargetDir) "$($wt.Triple)\release" }
    }
    if (-not $SkipBuild) {
        if (-not $native -and -not (Test-RustTargetInstalled $wt.Triple)) {
            throw "the Rust target $($wt.Triple) is not installed (rustup target add $($wt.Triple), plus the MSVC build tools for it). Windows on ARM runs the x86_64 bundle under emulation: use -Target x86_64."
        }
        # --locked: exactly what Cargo.lock pins; fail rather than update it.
        $cargoArgs = @('build', '--release', '--locked', '-p', 'cc-hook', '-p', 'cc-publish', '--manifest-path', (Join-Path $RepoRoot 'Cargo.toml'))
        if (-not $native) { $cargoArgs += @('--target', $wt.Triple) }
        Write-Step "cargo $($cargoArgs[0..6] -join ' ')$(if (-not $native) { " --target $($wt.Triple)" })"
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo build for $($wt.Triple) failed" }
    }
    foreach ($b in 'cc-hook.exe', 'cc-publish.exe') {
        $path = Join-Path $BinDir $b
        if (-not (Test-Path -LiteralPath $path)) { throw "missing binary $path" }
        $m = Get-PeMachine $path
        if ($m -ne $wt.Machine) { throw ("{0} is for PE machine 0x{1:X}, expected 0x{2:X} ({3})" -f $path, $m, $wt.Machine, $Target) }
        Write-Host ("  {0,-15} PE {1}, {2:N0} bytes" -f $b, $Target, (Get-Item -LiteralPath $path).Length)
    }
    if ($Target -eq 'x86_64') {
        if (Test-RustTargetInstalled $WinTargets['aarch64'].Triple) {
            Write-Host "  note: aarch64-pc-windows-msvc is installed; -Target aarch64 builds a native ARM64 bundle"
        } else {
            Write-Host "  note: no ARM64 bundle (the aarch64-pc-windows-msvc target is not installed); Windows on ARM runs this x64 bundle under emulation"
        }
    }
    $meta = & cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $RepoRoot 'Cargo.toml') | ConvertFrom-Json
    $version = ($meta.packages | Where-Object { $_.name -eq 'cc-hook' } | Select-Object -First 1).version

    # 4. NATS user, 5. keys: as for Linux (the viewer's public key only).
    $credsFile = Invoke-Provision
    $hostPublish = $null
    if (-not $ViewerPub) { $hostPublish = Get-HostPublishExe $(if ($native) { Join-Path $BinDir 'cc-publish.exe' } else { $null }) }
    Write-Step 'envelope keys: the viewer key pair here (the machine makes its own)'
    $viewerPubPath = Get-ViewerPub $hostPublish

    # 6. assemble
    Write-Step "assembling $bundleDir"
    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    if (Test-Path $bundleDir) { Remove-Item -Recurse -Force $bundleDir }
    New-Item -ItemType Directory -Force -Path $bundleDir | Out-Null
    Set-PrivateAcl $bundleDir
    foreach ($d in 'bin', 'secrets', 'config', 'scripts') {
        New-Item -ItemType Directory -Force -Path (Join-Path $bundleDir $d) | Out-Null
    }
    Copy-Item -LiteralPath (Join-Path $BinDir 'cc-hook.exe') -Destination (Join-Path $bundleDir 'bin\cc-hook.exe')
    Copy-Item -LiteralPath (Join-Path $BinDir 'cc-publish.exe') -Destination (Join-Path $bundleDir 'bin\cc-publish.exe')
    Copy-Item -LiteralPath $credsFile -Destination (Join-Path $bundleDir "secrets\$CredsName")
    Copy-Item -LiteralPath $viewerPubPath -Destination (Join-Path $bundleDir 'config\viewer.pub')
    foreach ($rel in $WinTextFiles) {
        [System.IO.File]::WriteAllBytes((Join-Path $bundleDir $rel), (Get-CrlfBytes (Join-Path $WinSourceDir $rel)))
    }
    # cc-publish's config as shipped; Install.ps1 writes the installed one with
    # absolute creds and key paths. log_file on: there is no journal.
    $serversJson = ($Servers | ForEach-Object { "`"$_`"" }) -join ', '
    [System.IO.File]::WriteAllBytes((Join-Path $bundleDir 'config\publish.json'), $Utf8.GetBytes(@"
{
  "alias": "$Alias",
  "servers": [$serversJson],
  "bucket": "$Bucket",
  "creds": "nats/$CredsName",
  "publisher_key": "keys/publisher.key",
  "viewer_pub": "keys/viewer.pub",
  "log_file": true
}

"@.Replace("`r`n", "`n").Replace("`n", "`r`n")))
    $created = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
    [System.IO.File]::WriteAllBytes((Join-Path $bundleDir 'config\bundle.json'), $Utf8.GetBytes(@"
{
  "format": 1,
  "platform": "windows",
  "alias": "$Alias",
  "nats_user": "$User",
  "arch": "$Target",
  "target": "$($wt.Triple)",
  "version": "$version",
  "created": "$created"
}

"@.Replace("`r`n", "`n").Replace("`n", "`r`n")))
    $sums = New-Object System.Text.StringBuilder
    foreach ($f in (Get-ChildItem -LiteralPath $bundleDir -Recurse -File | Sort-Object FullName)) {
        $rel = $f.FullName.Substring($bundleDir.Length + 1).Replace('\', '/')
        [void]$sums.Append("$(Get-Sha256Hex $f.FullName)  $rel`n")
    }
    [System.IO.File]::WriteAllBytes((Join-Path $bundleDir 'SHA256SUMS'), $Utf8.GetBytes($sums.ToString()))

    # 7. archive
    if (-not $NoArchive) {
        Write-Step "writing $zip (private to this user, like the folder)"
        Write-PrivateZip $bundleDir $bundleName $zip
        $expected = @(Get-ChildItem -LiteralPath $bundleDir -Recurse -File).Count
        $z = [System.IO.Compression.ZipFile]::OpenRead($zip)
        try {
            $entries = @($z.Entries | Where-Object { -not $_.FullName.EndsWith('/') })
            $badNames = @($entries | Where-Object { $_.FullName.Contains('\') -or -not $_.FullName.StartsWith("$bundleName/") })
        } finally { $z.Dispose() }
        if ($entries.Count -ne $expected) { throw "the zip lists $($entries.Count) files, expected $expected" }
        if ($badNames.Count -gt 0) { throw "the zip has unexpected entry names, e.g. $($badNames[0].FullName)" }
    }

    # 8. summary
    Write-Host ''
    Write-Step "bundle ready: $bundleDir"
    Get-ChildItem -LiteralPath $bundleDir -Recurse -File | Sort-Object FullName | ForEach-Object {
        Write-Host ("  {0,-44} {1,10:N0}" -f $_.FullName.Substring($bundleDir.Length + 1), $_.Length)
    }
    if (-not $NoArchive) {
        Write-Host ''
        Write-Host "  archive: $zip"
        Write-Host "  sha256:  $(Get-Sha256Hex $zip)  (compare it on the other machine: Get-FileHash <zip>)"
    }
    Write-Host ''
    Write-IdentityNote
    Write-Host ''
    Write-Host 'WARNING: this bundle contains a secret: the NATS credentials of' -ForegroundColor Yellow
    Write-Host "  $User." -ForegroundColor Yellow
    Write-Host '  Move it over a private channel only (your own USB stick, a private share,' -ForegroundColor Yellow
    Write-Host '  scp; not e-mail, chat or a shared drive), and delete the folder and the' -ForegroundColor Yellow
    Write-Host '  .zip on both machines once Verify passes.' -ForegroundColor Yellow
    Write-Host ''
    Write-Host 'On the Windows machine (the one without the cc-usage app):'
    Write-Host "  1. copy $bundleName.zip there; right-click it > Properties > Unblock"
    Write-Host '  2. Extract All, open the folder, double-click setup.cmd'
    Write-Host '     (it installs, prints the machine''s public key line and a PASS/FAIL table,'
    Write-Host '     and waits for a keypress)'
    Write-Host "  3. delete the extracted folder and $bundleName.zip"
    Write-Host "On the viewer, accept $Alias's key when cc-usage asks (compare it with the printed line)."
}
