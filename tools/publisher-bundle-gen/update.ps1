# -Update of tools\make-publisher-bundle.ps1: New-UpdatePackage, the Linux
# update package, called by New-LinuxBundle (linux.ps1) once the binaries
# are checked. Dot-sourced by that script; uses its parameters and variables
# ($Out, $NoArchive, $SourceDir, $Utf8).

# Assembles the update package from the checked binaries in $BinDir of
# $tgt (a Linux target of tools\cross\common.ps1) at $Version, writes its
# archive and prints how to install it.
function New-UpdatePackage($tgt, [string]$BinDir, [string]$Version) {
    $BundleName = "cc-usage-publisher-update-$Version-$($tgt.Name)"
    $BundleDir = Join-Path $Out $BundleName
    $Archive = Join-Path $Out "$BundleName.tar.gz"
    Write-Step "assembling $BundleDir"
    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    if (Test-Path $BundleDir) { Remove-Item -Recurse -Force $BundleDir }
    New-Item -ItemType Directory -Force -Path (Join-Path $BundleDir 'bin') | Out-Null
    Copy-Item -LiteralPath (Join-Path $BinDir 'cc-hook') -Destination (Join-Path $BundleDir 'bin\cc-hook')
    Copy-Item -LiteralPath (Join-Path $BinDir 'cc-publish') -Destination (Join-Path $BundleDir 'bin\cc-publish')
    [System.IO.File]::WriteAllBytes((Join-Path $BundleDir 'update.sh'), (Get-LfBytes (Join-Path $SourceDir 'scripts\update.sh')))
    # The viewer's public key (no secret; update.sh installs it
    # and keeps the machine's own key pair) and a fresh copy of the tooling
    # for <prefix>/bundle.
    $HostPublish = if ($ViewerPub) { $null } else { Get-HostPublishExe $null }
    Write-Step 'envelope keys: the viewer key pair here (its public key goes into the package)'
    $viewerPubPath = Get-ViewerPub $HostPublish
    Copy-Item -LiteralPath $viewerPubPath -Destination (Join-Path $BundleDir 'viewer.pub')
    $toolFiles = @('Makefile', 'README.md', 'systemd\cc-publish.service.in')
    $toolFiles += @(Get-ChildItem -LiteralPath (Join-Path $SourceDir 'scripts') -Filter '*.sh' -File | ForEach-Object { "scripts\$($_.Name)" })
    foreach ($rel in $toolFiles) {
        $to = Join-Path $BundleDir "bundle\$rel"
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $to) | Out-Null
        [System.IO.File]::WriteAllBytes($to, (Get-LfBytes (Join-Path $SourceDir $rel)))
    }
    $sums = New-Object System.Text.StringBuilder
    foreach ($item in (Get-ChildItem -LiteralPath $BundleDir -Recurse -File | Sort-Object FullName)) {
        $rel = $item.FullName.Substring($BundleDir.Length + 1).Replace('\', '/')
        [void]$sums.Append("$(Get-Sha256Hex $item.FullName)  $rel`n")
    }
    [System.IO.File]::WriteAllBytes((Join-Path $BundleDir 'SHA256SUMS'), $Utf8.GetBytes($sums.ToString()))
    if (-not $NoArchive) {
        Write-Step "writing $Archive"
        if (Test-Path $Archive) { Remove-Item -Force $Archive }
        Write-TarGz $BundleDir $BundleName $Archive @('bin/cc-hook', 'bin/cc-publish', 'update.sh')
    }
    Write-Host ''
    Write-Step "update package ready: $BundleDir (no secrets)"
    Get-ChildItem -LiteralPath $BundleDir -Recurse -File | Sort-Object FullName | ForEach-Object {
        Write-Host ("  {0,-44} {1,10:N0}" -f $_.FullName.Substring($BundleDir.Length + 1).Replace('\', '/'), $_.Length)
    }
    if (-not $NoArchive) {
        Write-Host ''
        Write-Host "  archive: $Archive"
        Write-Host "  sha256:  $(Get-Sha256Hex $Archive)"
    }
    Write-Host ''
    Write-Host 'On each Linux machine that already has the publisher:'
    Write-Host "  scp `"$Archive`" <user>@<host>:"
    Write-Host "  ssh <user>@<host>"
    Write-Host "  tar -xzf $BundleName.tar.gz && sh $BundleName/update.sh"
    Write-Host "  rm -rf $BundleName $BundleName.tar.gz"
    Write-Host 'update.sh installs viewer.pub and keeps the machine''s own key pair. A machine'
    Write-Host 'without one gets a new key only when confirmed (sh update.sh CONFIRM_ALIAS=<alias>); it'
    Write-Host 'prints that key''s public line, and the viewer asks to accept it.'
}
