# Step 5 of tools\make-publisher-bundle.ps1: the envelope keys, that is
# the viewer's public key (made with a Windows cc-publish.exe that runs here
# when this machine is the viewer), and the summary's note on the
# publisher's own key, which the machine makes itself. Dot-sourced by that
# script; uses its parameters and variables ($ViewerPub, $SkipBuild, $Alias,
# $DataDir, $Utf8, $RepoRoot).

# A Windows cc-publish.exe that runs on this machine, for the key commands:
# $Candidate when given (the Windows bundle's own, just built), else the
# host build in cargo's target dir, built now unless -SkipBuild finds one.
function Get-HostPublishExe([string]$Candidate) {
    if ($Candidate) { return $Candidate }
    $exe = Join-Path (Get-CargoTargetDir) 'release\cc-publish.exe'
    if ($SkipBuild -and (Test-Path -LiteralPath $exe)) { return $exe }
    Write-Step 'cargo build --release --locked -p cc-publish (this machine, for the key commands)'
    & cargo build --release --locked -p cc-publish --manifest-path (Join-Path $RepoRoot 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --locked -p cc-publish (Windows) failed' }
    return $exe
}

# Runs cc-publish with arguments; returns its stdout lines. Throws on a
# non-zero exit. stderr (paths and key ids only) goes to the console.
function Invoke-PublishExe([string]$Exe, [string[]]$Arguments) {
    $out = & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cc-publish $($Arguments -join ' ') failed (exit $LASTEXITCODE); a stale cc-publish.exe? Build without -SkipBuild"
    }
    return @($out)
}

# A public key line as cc-publish prints it: <kid>:<base64 of 32 bytes>.
function Assert-PublicLine([string]$Line, [string]$What) {
    if ($Line -notmatch '^[A-Za-z0-9_-]{1,32}:[A-Za-z0-9+/]{43}=$') { throw "$What is not a public key line: '$Line'" }
    return $Line
}

# Step 5: the viewer's key pair on this machine, the viewer:
# keys\viewer.key and keys\viewer.pub under the data dir, created when
# missing (`cc-publish viewer-keygen --if-missing`; the app makes the same
# files on its first start). Returns the path of viewer.pub, which every
# publisher bundle carries. Only the public key is printed.
function Confirm-ViewerKeys([string]$PublishExe) {
    $line = Invoke-PublishExe $PublishExe @('--data-dir', $DataDir, 'viewer-keygen', '--if-missing') | Select-Object -Last 1
    $line = Assert-PublicLine ([string]$line) 'viewer-keygen output'
    $pub = Join-Path $DataDir 'keys\viewer.pub'
    if (-not (Test-Path -LiteralPath $pub)) { throw "viewer-keygen did not write $pub" }
    Write-Host "  viewer key: $line (in $pub; viewer.key next to it never leaves this machine)"
    return $pub
}

# The viewer's public key for a bundle or package: -ViewerPub (the viewer
# is another PC; the file is its keys\viewer.pub, copied here) or this
# machine's own pair (Confirm-ViewerKeys), which only a viewer may use.
# Returns the path of the viewer.pub to ship. Only public keys are printed.
function Get-ViewerPub([string]$PublishExe) {
    if ($ViewerPub) {
        $path = Assert-ViewerPubFile $ViewerPub
        $n = Test-KeyFile $path
        Write-Host "  viewer key: $path ($n key(s), from -ViewerPub; compare with keys\viewer.pub on the viewer):"
        foreach ($line in [System.IO.File]::ReadAllLines($path)) {
            $t = $line.Trim()
            if ($t -ne '' -and -not $t.StartsWith('#')) { Write-Host "    $t" }
        }
        return $path
    }
    Assert-ViewerHere
    return Confirm-ViewerKeys $PublishExe
}

# The -ViewerPub file: it must exist and be named *.pub. A private key file
# (viewer.key) has the same <kid>:<base64> lines, so only the name tells
# them apart, as for the app's Import viewer key...; shipping one would hand
# the viewer's private key to every publisher. Returns the full path.
function Assert-ViewerPubFile([string]$File) {
    if (-not (Test-Path -LiteralPath $File -PathType Leaf)) { throw "-ViewerPub: $File does not exist" }
    $path = (Resolve-Path -LiteralPath $File).Path
    if ([System.IO.Path]::GetExtension($path) -ne '.pub') {
        throw "-ViewerPub: $path is not a .pub file; pass a copy of keys\viewer.pub (never viewer.key, the viewer's private key)"
    }
    return $path
}

# Without -ViewerPub the keys come from this machine, so it must be the
# viewer: on a PC in the app's Publisher role, viewer-keygen --if-missing
# would make a viewer key pair nobody reads with, and every bundle would
# seal to it.
function Assert-ViewerHere {
    $settings = Get-ViewerSettingsPath
    if (-not $settings -or -not (Test-Path -LiteralPath $settings)) { return }
    $role = try { (Get-Content -LiteralPath $settings -Raw | ConvertFrom-Json).role } catch { $null }
    if ($role -eq 'publisher') {
        throw "this PC is in the Publisher role ($settings), not the viewer: pass -ViewerPub <a copy of keys\viewer.pub from the viewer>"
    }
}

# The summary's note on the machine's identity. No publisher key travels in
# a bundle: the machine makes its key pair on its first install, prints its
# public key line and keeps it on every reinstall and update; the viewer
# asks to accept it with the first value it receives.
function Write-IdentityNote {
    Write-Host "  identity: no private key in the bundle. $Alias makes its own key pair on the first"
    Write-Host '  install, prints its public key line and keeps it on every reinstall; the viewer'
    Write-Host '  asks to accept that key (compare its id and fingerprint).'
    if (-not $ViewerPub) {
        $pin = Join-Path $DataDir "keys\publishers\$Alias.pub"
        if (Test-Path -LiteralPath $pin) {
            Write-Host "  $pin already holds key(s) of ${Alias}: a reinstall there keeps its key; a new"
            Write-Host '  machine with this alias makes a new one, which the viewer asks to accept again.'
        }
    }
}
