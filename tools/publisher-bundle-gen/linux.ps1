# The Linux bundle of tools\make-publisher-bundle.ps1 (the default
# platform): the checks of the sources in tools\publisher-bundle and
# New-LinuxBundle, whose steps 2 and 3 (sources, binaries) the update package
# shares before New-UpdatePackage (update.ps1) takes over. Dot-sourced by
# that script; uses its parameters and variables ($Alias, $Target, $Update,
# $Out, $BinDir, $SkipBuild, $NoArchive, $User, $CredsName, $Servers, $Bucket,
# $SourceDir, $Utf8, $ToolsDir, $RepoRoot).

# Checks the Makefile and scripts before they are shipped: recipe lines must
# start with a tab, files must not contain CR, and every script must parse
# with a POSIX shell (dash when present, which rejects bashisms).
function Test-BundleSources {
    $problems = @()
    $mk = Join-Path $SourceDir 'Makefile'
    $lines = [System.IO.File]::ReadAllLines($mk)
    $inRule = $false
    for ($i = 0; $i -lt $lines.Length; $i++) {
        $l = $lines[$i]
        if ($l -match '^[^\s#][^=]*:([^=]|$)' -and $l -notmatch '^\s*(ifn?eq|ifn?def|else|endif|export|include)\b') { $inRule = $true; continue }
        if ($l -eq '' -or $l -match '^\S') { $inRule = $false; continue }
        if ($inRule -and $l -match '^ +\S') { $problems += "Makefile line $($i + 1): recipe line starts with spaces, not a tab" }
    }
    $texts = @(Get-ChildItem -LiteralPath $SourceDir -Recurse -File | Where-Object { $_.Name -eq 'Makefile' -or $_.Extension -in '.sh', '.in', '.md' })
    foreach ($f in $texts) {
        $bytes = [System.IO.File]::ReadAllBytes($f.FullName)
        if ($bytes -contains 13) { $problems += "$($f.Name): has CR line endings (they are converted in the bundle, but fix the source)" }
    }
    $shell = $null
    foreach ($candidate in @('dash', 'sh')) {
        $c = Get-Command $candidate -ErrorAction SilentlyContinue
        if ($c) { $shell = $c.Source; break }
    }
    if (-not $shell) {
        foreach ($p in @("$env:ProgramFiles\Git\usr\bin\dash.exe", "$env:ProgramFiles\Git\usr\bin\sh.exe")) {
            if (Test-Path $p) { $shell = $p; break }
        }
    }
    if ($shell) {
        foreach ($f in @(Get-ChildItem -LiteralPath (Join-Path $SourceDir 'scripts') -Filter '*.sh' -File)) {
            $out = & $shell -n $f.FullName 2>&1
            if ($LASTEXITCODE -ne 0) { $problems += "$($f.Name): sh -n failed: $out" }
        }
    } else {
        Write-Warn 'no POSIX shell found (Git Bash); skipping the sh -n syntax check of the scripts'
    }
    return , $problems
}

# Steps 2 to 8 of the Linux bundle; with -Update, steps 2 and 3, then
# New-UpdatePackage.
function New-LinuxBundle {
    $tgt = Resolve-LinuxTarget $Target
    $BundleName = "cc-usage-publisher-$Alias"
    $BundleDir = Join-Path $Out $BundleName
    $Archive = Join-Path $Out "$BundleName.tar.gz"
    if ($Update) {
        Write-Step "update package ($($tgt.Triple))"
    } else {
        Write-Step "bundle for alias '$Alias' (NATS user $User, $($tgt.Triple))"
    }

    # 2. sources
    Write-Step 'checking the bundle sources (Makefile tabs, line endings, sh -n)'
    $problems = Test-BundleSources
    if ($problems.Count -gt 0) {
        $problems | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        throw 'the bundle sources have problems; fix them first'
    }

    # 3. binaries
    Add-CargoToPath
    if (-not $BinDir) { $BinDir = Join-Path (Get-CargoTargetDir) "$($tgt.Triple)\release" }
    if (-not $SkipBuild) {
        if ((Get-WorkspacePackages) -notcontains 'cc-publish') {
            throw 'cc-publish is not in the workspace yet, so the bundle cannot be complete. Build it first, or pass -SkipBuild -BinDir <dir with cc-hook and cc-publish>.'
        }
        & (Join-Path $ToolsDir 'cross\build-linux.ps1') -Target $tgt.Name -Strict
        if (-not $?) { throw 'the Linux build failed' }
    }
    $Version = $null
    foreach ($b in 'cc-hook', 'cc-publish') {
        $path = Join-Path $BinDir $b
        $e = Assert-StaticElf $path $tgt
        Write-Host ("  {0,-11} static ELF {1}, {2:N0} bytes" -f $b, $tgt.Name, $e.Size)
    }
    $meta = & cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $RepoRoot 'Cargo.toml') | ConvertFrom-Json
    $Version = ($meta.packages | Where-Object { $_.name -eq 'cc-hook' } | Select-Object -First 1).version

    if ($Update) {
        New-UpdatePackage $tgt $BinDir $Version
        return
    }

    # 4. NATS user
    $CredsFile = Invoke-Provision

    # 5. keys
    # The viewer key pair on this machine (viewer.pub goes into
    # the bundle), or -ViewerPub. The machine makes its own key pair when the
    # bundle is installed, so none is made or pinned here.
    $HostPublish = if ($ViewerPub) { $null } else { Get-HostPublishExe $null }
    Write-Step 'envelope keys: the viewer key pair here (the machine makes its own)'
    $viewerPubPath = Get-ViewerPub $HostPublish

    # 6. assemble
    Write-Step "assembling $BundleDir"
    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    if (Test-Path $BundleDir) { Remove-Item -Recurse -Force $BundleDir }
    New-Item -ItemType Directory -Force -Path $BundleDir | Out-Null
    Set-PrivateAcl $BundleDir
    foreach ($d in 'bin', 'secrets', 'config', 'scripts', 'systemd') {
        New-Item -ItemType Directory -Force -Path (Join-Path $BundleDir $d) | Out-Null
    }
    Copy-Item -LiteralPath (Join-Path $BinDir 'cc-hook') -Destination (Join-Path $BundleDir 'bin\cc-hook')
    Copy-Item -LiteralPath (Join-Path $BinDir 'cc-publish') -Destination (Join-Path $BundleDir 'bin\cc-publish')
    Copy-Item -LiteralPath $CredsFile -Destination (Join-Path $BundleDir "secrets\$CredsName")
    Copy-Item -LiteralPath $viewerPubPath -Destination (Join-Path $BundleDir 'config\viewer.pub')
    $textFiles = @('Makefile', 'README.md', 'systemd\cc-publish.service.in')
    $textFiles += @(Get-ChildItem -LiteralPath (Join-Path $SourceDir 'scripts') -Filter '*.sh' -File | ForEach-Object { "scripts\$($_.Name)" })
    foreach ($rel in $textFiles) {
        [System.IO.File]::WriteAllBytes((Join-Path $BundleDir $rel), (Get-LfBytes (Join-Path $SourceDir $rel)))
    }

    # cc-publish's own config, so only members it accepts (it rejects unknown
    # ones): the NATS user is in bundle.env, the data dir in the systemd unit and
    # publisher.env. One member per line: the Linux scripts read it with sed and
    # awk, and install.sh makes creds and key absolute under PREFIX. log_file is
    # off because the journal (or the autostart log) already keeps stderr.
    $serversJson = ($Servers | ForEach-Object { "`"$_`"" }) -join ', '
    Write-LfFile (Join-Path $BundleDir 'config\publish.json') @"
{
  "alias": "$Alias",
  "servers": [$serversJson],
  "bucket": "$Bucket",
  "creds": "nats/$CredsName",
  "publisher_key": "keys/publisher.key",
  "viewer_pub": "keys/viewer.pub",
  "log_file": false
}

"@
    $created = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
    Write-LfFile (Join-Path $BundleDir 'config\bundle.env') @"
BUNDLE_FORMAT=1
ALIAS=$Alias
NATS_USER=$User
ARCH=$($tgt.Name)
TARGET=$($tgt.Triple)
VERSION=$Version
CREATED=$created

"@

    $sums = New-Object System.Text.StringBuilder
    foreach ($f in (Get-ChildItem -LiteralPath $BundleDir -Recurse -File | Sort-Object FullName)) {
        $rel = $f.FullName.Substring($BundleDir.Length + 1).Replace('\', '/')
        [void]$sums.Append("$(Get-Sha256Hex $f.FullName)  $rel`n")
    }
    [System.IO.File]::WriteAllBytes((Join-Path $BundleDir 'SHA256SUMS'), $Utf8.GetBytes($sums.ToString()))

    # 7. archive
    if (-not $NoArchive) {
        Write-Step "writing $Archive (private to this user, like the folder)"
        $exec = @('bin/cc-hook', 'bin/cc-publish') + @(Get-ChildItem -LiteralPath (Join-Path $BundleDir 'scripts') -Filter '*.sh' | ForEach-Object { "scripts/$($_.Name)" })
        # Private before any byte goes in: Write-TarGz overwrites the empty file,
        # which keeps its ACL. Checked again once written.
        New-PrivateFile $Archive
        Write-TarGz $BundleDir $BundleName $Archive $exec
        Set-PrivateAcl $Archive
        $tarExe = Join-Path $env:SystemRoot 'System32\tar.exe'
        if (Test-Path $tarExe) {
            $listing = & $tarExe -tzf $Archive 2>&1
            if ($LASTEXITCODE -ne 0) { throw "the archive does not read back: $listing" }
            $expected = (Get-ChildItem -LiteralPath $BundleDir -Recurse).Count + 1
            if (@($listing).Count -ne $expected) { throw "the archive lists $(@($listing).Count) entries, expected $expected" }
        }
    }

    # 8. summary
    Write-Host ''
    Write-Step "bundle ready: $BundleDir"
    Get-ChildItem -LiteralPath $BundleDir -Recurse -File | Sort-Object FullName | ForEach-Object {
        Write-Host ("  {0,-44} {1,10:N0}" -f $_.FullName.Substring($BundleDir.Length + 1).Replace('\', '/'), $_.Length)
    }
    if (-not $NoArchive) {
        Write-Host ''
        Write-Host "  archive: $Archive"
        Write-Host "  sha256:  $(Get-Sha256Hex $Archive)  (compare it on the Linux machine with sha256sum)"
    }
    Write-Host ''
    Write-IdentityNote
    Write-Host ''
    Write-Host 'WARNING: this bundle contains a secret: the NATS credentials of' -ForegroundColor Yellow
    Write-Host "  $User." -ForegroundColor Yellow
    Write-Host '  Move it over a secure channel only (scp/sftp, not e-mail, chat or a shared' -ForegroundColor Yellow
    Write-Host '  drive), and delete the folder and the archive on both machines once' -ForegroundColor Yellow
    Write-Host '  make verify passes.' -ForegroundColor Yellow
    Write-Host ''
    Write-Host 'On the Linux machine:'
    Write-Host "  scp `"$Archive`" <user>@<host>:"
    Write-Host "  ssh <user>@<host>"
    Write-Host "  tar -xzf $BundleName.tar.gz && cd $BundleName"
    Write-Host '  make install && make verify'
    Write-Host "  cd .. && rm -rf $BundleName $BundleName.tar.gz"
    Write-Host 'make install prints the machine''s public key line; accept it on the viewer when cc-usage asks.'
}
