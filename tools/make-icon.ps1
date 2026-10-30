<#
.SYNOPSIS
    Generates app/icons/icon.png and icon.ico: a 3x3 grid of activity greens.

.DESCRIPTION
    The app icon (Start menu, installer, executable, Alt+Tab), plus the two
    tray images (tray-grid.png and tray-grid-alert.png, the latter with the
    red "needs you" badge).

    Design: nine rounded cells in the GitHub contribution greens of the usage
    view's Activity calendar, on a white tile: how much you use Claude Code,
    at a glance (docs/icons/final.png).

    The .ico carries several resolutions because Windows picks a different one
    per context (tray and title bar 16/20/24, Alt+Tab 32/48, installer 256).
    Small sizes are laid out on whole pixels, not scaled down: at 16, 20 and
    24 px each cell and gap is a whole number of pixels, otherwise the cells
    blur into one green smudge. A light grey outline keeps the white tile
    visible on a light taskbar.

    The greens match the calendar (the --act-* tokens in app/ui/styles/tokens.css,
    drawn by app/ui/scripts/activity.js).

.PARAMETER OutDir
    Output folder. Defaults to app\icons.
#>

param([string] $OutDir)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

if (-not $OutDir) {
    $OutDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'app\icons'
}
if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Path $OutDir -Force | Out-Null }

function Hex([string] $h) { [System.Drawing.ColorTranslator]::FromHtml($h) }
$WHITE = Hex '#ffffff'
$OUTLINE = Hex '#d7dbe6'
$NEED = Hex '#d13f3a'
# The calendar's levels, no activity first.
$L0 = Hex '#ebedf0'; $L1 = Hex '#9be9a8'; $L2 = Hex '#40c463'; $L3 = Hex '#30a14e'; $L4 = Hex '#216e39'
# The nine cells, row by row: a busy week, not a pattern.
$CELLS = @($L2, $L4, $L1, $L0, $L3, $L2, $L1, $L3, $L4)

function Add-RoundRect($path, [single] $x, [single] $y, [single] $w, [single] $h, [single] $r) {
    $d = [math]::Max(0.5, 2 * $r)
    $path.AddArc($x, $y, $d, $d, 180, 90)
    $path.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $path.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $path.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $path.CloseFigure()
}

# The grid's geometry for a size: [margin, cell, gap] in pixels. Small sizes
# get whole pixels (16 = 1 + 3x4 + 2x1 + 1); larger ones the round-5
# proportions.
function Get-Grid([int] $Size) {
    switch ($Size) {
        16 { return @{ m = 1.0; cell = 4.0; gap = 1.0 } }
        20 { return @{ m = 1.0; cell = 5.0; gap = 1.5 } }
        24 { return @{ m = 2.0; cell = 6.0; gap = 1.0 } }
        default {
            $m = $Size * 0.16; $gap = $Size * 0.05
            return @{ m = $m; cell = ($Size - 2 * $m - 2 * $gap) / 3; gap = $gap }
        }
    }
}

# -Compact uses the 16 px layout scaled to the size (the tray image is 32 px
# but shown at 16-24); -Badge adds the red "needs you" dot of the tray icon.
function New-IconBitmap([int] $Size, [switch] $Compact, [switch] $Badge) {
    $s = [single] $Size
    if ($Compact) {
        $k = $Size / 16.0
        $grid = @{ m = 1.0 * $k; cell = 4.0 * $k; gap = 1.0 * $k }
    } else {
        $grid = Get-Grid $Size
    }
    $small = $Compact -or $Size -le 24

    $bmp = [System.Drawing.Bitmap]::new($Size, $Size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.PixelOffsetMode = 'HighQuality'
    $g.Clear([System.Drawing.Color]::Transparent)

    # White tile with a light outline (invisible on white otherwise).
    $tile = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $inset = if ($small) { 0.5 } else { $s * 0.01 }
    Add-RoundRect $tile $inset $inset ($s - 2 * $inset) ($s - 2 * $inset) ($s * 0.20)
    $g.FillPath([System.Drawing.SolidBrush]::new($WHITE), $tile)
    $g.DrawPath([System.Drawing.Pen]::new($OUTLINE, [single][math]::Max(1, $s * 0.012)), $tile)

    # The nine cells.
    for ($i = 0; $i -lt 9; $i++) {
        $x = $grid.m + ($i % 3) * ($grid.cell + $grid.gap)
        $y = $grid.m + [math]::Floor($i / 3) * ($grid.cell + $grid.gap)
        $cell = [System.Drawing.Drawing2D.GraphicsPath]::new()
        Add-RoundRect $cell $x $y $grid.cell $grid.cell ($grid.cell * 0.22)
        $g.FillPath([System.Drawing.SolidBrush]::new($CELLS[$i]), $cell)
        $cell.Dispose()
    }

    if ($Badge) {
        # A red dot with a white ring at the top right: some session needs you.
        $bd = $s * 0.46; $ring = [math]::Max(1, $s * 0.07)
        $bxp = $s - $bd; $byp = 0
        $g.FillEllipse([System.Drawing.SolidBrush]::new($WHITE), [single]$bxp, [single]$byp, [single]$bd, [single]$bd)
        $g.FillEllipse([System.Drawing.SolidBrush]::new($NEED), [single]($bxp + $ring), [single]($byp + $ring), [single]($bd - 2 * $ring), [single]($bd - 2 * $ring))
    }

    $tile.Dispose(); $g.Dispose()
    return $bmp
}

# Tray icons (32 px, compact proportions so they stay legible when Windows
# shows them at 16-24 px): plain, and with the "needs you" badge. Embedded by
# app/src/tray.rs.
foreach ($t in @(@{ name = 'tray-grid.png'; badge = $false }, @{ name = 'tray-grid-alert.png'; badge = $true })) {
    $bmp = New-IconBitmap 32 -Compact -Badge:$t.badge
    $bmp.Save((Join-Path $OutDir $t.name), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

# Large PNG, for the bundler.
$png = New-IconBitmap 256
$png.Save((Join-Path $OutDir 'icon.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$png.Dispose()

# Multi-resolution ICO, assembled by hand (System.Drawing cannot write them).
$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$blobs = @()
foreach ($s in $sizes) {
    $bmp = New-IconBitmap $s
    $mem = [System.IO.MemoryStream]::new()
    $bmp.Save($mem, [System.Drawing.Imaging.ImageFormat]::Png)
    $blobs += , $mem.ToArray()
    $bmp.Dispose(); $mem.Dispose()
}

$out = [System.IO.MemoryStream]::new()
$w = [System.IO.BinaryWriter]::new($out)
$w.Write([uint16] 0)             # reserved
$w.Write([uint16] 1)             # type: icon
$w.Write([uint16] $sizes.Count)

$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    # 256 is encoded as 0 in the directory: the field is a single byte.
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $w.Write([byte] $dim); $w.Write([byte] $dim)
    $w.Write([byte] 0); $w.Write([byte] 0)
    $w.Write([uint16] 1); $w.Write([uint16] 32)
    $w.Write([uint32] $blobs[$i].Length)
    $w.Write([uint32] $offset)
    $offset += $blobs[$i].Length
}
foreach ($b in $blobs) { $w.Write($b) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $OutDir 'icon.ico'), $out.ToArray())
$w.Dispose(); $out.Dispose()

Get-ChildItem $OutDir | ForEach-Object { '{0,-10} {1,8:N0} bytes' -f $_.Name, $_.Length }
