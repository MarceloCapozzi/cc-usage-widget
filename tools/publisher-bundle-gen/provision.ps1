# Step 4 of tools\make-publisher-bundle.ps1: the NATS user of the alias,
# declared and applied with toolctl, and its creds file. Dot-sourced by that
# script; uses its parameters and variables ($Toolctl, $SkipProvision,
# $ProjectFile, $CredsDir, $CredsName, $User, $Alias, $RepoRoot,
# $LocalAppData).

# toolctl, the maintainer's internal tool (a separate private repository):
# -Toolctl, $env:TOOLCTL, toolctl on PATH, or built with go from a checkout at
# ..\tooling-ctl into %LOCALAPPDATA%\toolctl\bin (outside OneDrive; the
# binary holds no secret, the folder is just a cache).
function Resolve-Toolctl {
    foreach ($candidate in @($Toolctl, $env:TOOLCTL)) {
        if (-not $candidate) { continue }
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { throw "toolctl not found at $candidate" }
        return (Resolve-Path -LiteralPath $candidate).Path
    }
    $onPath = Get-Command toolctl -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) { return $onPath.Source }
    $src = Join-Path (Split-Path -Parent $RepoRoot) 'tooling-ctl'
    if (-not (Test-Path -LiteralPath (Join-Path $src 'cmd\toolctl'))) {
        throw "toolctl not found: pass -Toolctl, set TOOLCTL to its path, put it on PATH, or check it out next to this repository ($src); without toolctl, make the creds by hand (docs\remote.md, By hand) and pass -SkipProvision"
    }
    $go = Get-Command go -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $go) { throw "toolctl is not built and go is not on PATH; build it in $src (go build -o <dir>\toolctl.exe ./cmd/toolctl) and pass -Toolctl" }
    $exe = Join-Path $LocalAppData 'toolctl\bin\toolctl.exe'
    Write-Step "building toolctl from $src into $exe"
    & $go.Source -C $src build -o $exe ./cmd/toolctl
    if ($LASTEXITCODE -ne 0) { throw "go build of toolctl failed (exit $LASTEXITCODE)" }
    return $exe
}

# The Synadia token in the repository's .env, or $null. Never printed.
function Get-DotEnvToken {
    $envFile = Join-Path $RepoRoot '.env'
    if (-not (Test-Path -LiteralPath $envFile)) { return $null }
    foreach ($line in [System.IO.File]::ReadAllLines($envFile)) {
        $t = $line.Trim()
        $prefix = 'SYNADIA_CLOUD_ACCESS_TOKEN='
        if ($t.StartsWith($prefix)) {
            $v = $t.Substring($prefix.Length).Trim().Trim('"', "'")
            if ($v) { return $v }
        }
    }
    return $null
}

# Runs toolctl and returns its exit code; its output goes to the console
# (Write-Host: inside a function it would become the return value). When the
# environment has no SYNADIA_CLOUD_ACCESS_TOKEN, the one in .env is set for
# the call only, in this process's environment, which toolctl inherits; it
# is never on a command line or printed, and toolctl redacts it from errors.
# Without either, toolctl uses its OS credential store.
function Invoke-Toolctl([string]$Exe, [string[]]$Arguments) {
    $name = 'SYNADIA_CLOUD_ACCESS_TOKEN'
    $restore = $false
    if (-not [Environment]::GetEnvironmentVariable($name, 'Process')) {
        $token = Get-DotEnvToken
        if ($token) {
            [Environment]::SetEnvironmentVariable($name, $token, 'Process')
            $restore = $true
        }
        Remove-Variable token
    }
    try {
        & $Exe @Arguments | ForEach-Object { Write-Host "  $_" }
        return $LASTEXITCODE
    } finally {
        if ($restore) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
    }
}

# Step 4 of both platforms: provisions the NATS user (unless -SkipProvision)
# and returns the path of its creds file, checked to look like one and made
# private (current user and SYSTEM only).
function Invoke-Provision {
    $CredsFile = Join-Path $CredsDir $CredsName
    if ($SkipProvision) {
        Write-Step "not provisioning (-SkipProvision); using $CredsFile"
    } else {
        [void](Read-ProjectFile $ProjectFile (Join-Path $RepoRoot 'infra\nats\cc-sessions.example.yaml'))
        $exe = Resolve-Toolctl
        $hint = "If the Synadia token was rejected (HTTP 401/403), renew SYNADIA_CLOUD_ACCESS_TOKEN in the repository's .env (or run: toolctl auth login)."
        $before = Get-Sha256Hex $ProjectFile
        Write-Step "declaring $User in $ProjectFile (toolctl add-publisher, idempotent)"
        $rc = Invoke-Toolctl $exe @('add-publisher', $Alias, '-f', $ProjectFile)
        if ($rc -ne 0) { throw "toolctl add-publisher failed (exit $rc)" }
        if ((Get-Sha256Hex $ProjectFile) -ne $before) {
            Write-Warn "$ProjectFile now declares $User; commit that change so the account stays reproducible"
        }
        Write-Step "applying $ProjectFile (toolctl apply --no-creds; never prunes)"
        $rc = Invoke-Toolctl $exe @('apply', '-f', $ProjectFile, '--no-creds')
        if ($rc -ne 0) { throw "toolctl apply failed (exit $rc). $hint" }
        Write-Step "downloading the creds of $User (never printed)"
        $rc = Invoke-Toolctl $exe @('creds', $User, '-o', $CredsFile)
        if ($rc -ne 0) { throw "toolctl creds failed (exit $rc). $hint" }
    }
    if (-not (Test-Path $CredsFile)) { throw "creds file not found: $CredsFile" }
    $credsText = [System.IO.File]::ReadAllText($CredsFile)
    $credsOk = $credsText.Contains('-----BEGIN NATS USER JWT-----') -and $credsText.Contains('-----BEGIN USER NKEY SEED-----')
    Remove-Variable credsText
    if (-not $credsOk) { throw "$CredsFile does not look like a NATS creds file" }
    # toolctl leaves the file with its folder's inherited ACL; it holds the
    # publisher's write credentials (JWT and nkey seed), so this machine's
    # copy is made private like the bundle's.
    Set-PrivateAcl $CredsFile
    return $CredsFile
}
