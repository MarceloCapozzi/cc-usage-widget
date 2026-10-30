<#
.SYNOPSIS
    Seeds a synthetic history to develop the flyout's chart.

.DESCRIPTION
    The widget stores one sample per refresh, with a 5-minute minimum between
    samples. To see a 6 h curve you have to wait 6 h; for the 30-day one, a
    month. This writes the file directly.

    It writes history.json in $env:CC_USAGE_DATA_DIR, which the app also
    honours: start the app with the same CC_USAGE_DATA_DIR to see it. The
    real data dir, %LOCALAPPDATA%\cc-usage, holds the real usage history, so
    it is refused (CC_USAGE_DATA_DIR unset, or pointing there) unless -Force
    is given. An existing history.json is copied to
    history.json.bak-<timestamp> first, unless -NoBackup is given. The
    series use the app's limit labels ("5-hour session", "All models -
    weekly" with a middle dot).

.PARAMETER Hours
    How many hours of history to generate. Defaults to 6.

.PARAMETER NoBackup
    Overwrites an existing history.json without keeping a copy.

.PARAMETER Force
    Writes into the real data dir, %LOCALAPPDATA%\cc-usage (a copy of the
    real history is kept unless -NoBackup is given).

.EXAMPLE
    $env:CC_USAGE_DATA_DIR = "$env:TEMP\cc-usage-dev"; pwsh -File tools\seed-history.ps1 -Hours 24
#>

param(
    [int] $Hours = 6,
    [switch] $NoBackup,
    [switch] $Force
)

$ErrorActionPreference = 'Stop'

# The app's limit labels. The middle dot is built from its code point so this
# file stays ASCII (Windows PowerShell reads a script without a BOM in the
# ANSI code page).
$LabelSession = '5-hour session'
$LabelWeekly = "All models $([char]0x00B7) weekly"

# The app's data dir: CC_USAGE_DATA_DIR when set (as app/src/paths.rs reads
# it), else %LOCALAPPDATA%\cc-usage.
$realDir = Join-Path $env:LOCALAPPDATA 'cc-usage'
$dir = if ($env:CC_USAGE_DATA_DIR) { $env:CC_USAGE_DATA_DIR } else { $realDir }
$target = Join-Path $dir 'history.json'
$isReal = [System.IO.Path]::GetFullPath($dir).TrimEnd('\') -ieq [System.IO.Path]::GetFullPath($realDir).TrimEnd('\')
if ($isReal -and -not $Force) {
    Write-Host ("ERROR: refusing to overwrite the real usage history in $target. Set CC_USAGE_DATA_DIR to another folder " +
        '(and start the app with it), or pass -Force to write there anyway (a backup is kept unless -NoBackup).') -ForegroundColor Red
    exit 1
}
New-Item -ItemType Directory -Path $dir -Force | Out-Null

if (-not $NoBackup -and (Test-Path -LiteralPath $target)) {
    $copy = "$target.bak-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
    Copy-Item -LiteralPath $target -Destination $copy
    Write-Output "backup: $copy"
}

$now = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$stepMs = 300000            # 5 min, the history's minimum interval
$n = [int](($Hours * 3600000) / $stepMs)

# The accumulators are typed as [double] on purpose: without that,
# [math]::Min(99, 58.5) resolves to the integer overload and truncates to 58,
# so the series gets stuck and looks like a chart bug.
[double] $session = 3
[double] $weekly = 41
$sessionReset = $now - 2 * 3600000
$weeklyReset = $now + 4 * 86400000

$samples = New-Object System.Collections.Generic.List[object]
for ($i = $n - 1; $i -ge 0; $i--) {
    $t = $now - $i * $stepMs

    # The session resets every ~5 h: it is marked with a new resets_at, which is
    # what makes the chart break the line instead of drawing a drop.
    if ($t -gt $sessionReset) {
        $session = 1 + (Get-Random -Minimum 0 -Maximum 30) / 10
        $sessionReset = $t + 5 * 3600000
    }
    else {
        $session = [math]::Min([double]98, $session + (Get-Random -Minimum 2 -Maximum 32) / 10)
    }
    $weekly = [math]::Min([double]99, $weekly + (Get-Random -Minimum 0 -Maximum 9) / 10)

    # The series keys are the limit labels the app builds (LABEL_SESSION and
    # LABEL_WEEKLY in app/src/model.rs), so they must match them exactly.
    $samples.Add([ordered]@{
        t = $t
        v = @(
            [ordered]@{ k = $LabelSession; p = [math]::Round($session, 1); r = $sessionReset },
            [ordered]@{ k = $LabelWeekly;  p = [math]::Round($weekly, 1); r = $weeklyReset }
        )
    })
}

# UTF-8 without a BOM: the app's JSON parser rejects one, and Windows
# PowerShell's Set-Content -Encoding utf8 would write it.
$json = @{ samples = $samples } | ConvertTo-Json -Depth 6 -Compress
[System.IO.File]::WriteAllText($target, $json, (New-Object System.Text.UTF8Encoding($false)))

$first = $samples[0].v
$last = $samples[$samples.Count - 1].v
Write-Output "seeded: $($samples.Count) samples over $Hours h in $target"
Write-Output ("  5h session {0,5} -> {1,5}" -f $first[0].p, $last[0].p)
Write-Output ("  weekly     {0,5} -> {1,5}" -f $first[1].p, $last[1].p)
Write-Output "Restart the widget so it loads it."
