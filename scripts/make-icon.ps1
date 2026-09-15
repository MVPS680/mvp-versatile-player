# Generates assets/icon.ico — the icon Explorer shows for the player and for the
# file types it is associated with.
#
# The logo is drawn with GDI+ rather than committed as a binary blob so it can be
# tweaked by editing this file, and so the repository carries no opaque assets.
# Each size is rendered separately (rather than downscaling one bitmap) so the
# 16 px tray icon stays crisp.

[CmdletBinding()]
param(
    [string] $Output = (Join-Path (Split-Path -Parent $PSScriptRoot) 'assets\icon.ico')
)

Add-Type -AssemblyName System.Drawing

$sizes = @(16, 24, 32, 48, 64, 128, 256)

function New-LogoBitmap {
    param([int] $Size)

    $bmp = [System.Drawing.Bitmap]::new(
        $Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    $pad = [Math]::Max(0.5, $Size * 0.03)
    $side = [single]($Size - 2.0 * $pad)
    $rect = [System.Drawing.RectangleF]::new([single]$pad, [single]$pad, $side, $side)
    $radius = [single]([Math]::Max(2.0, $Size * 0.24))
    $d = [single]($radius * 2.0)

    # Rounded-square body in the player's accent violet.
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $path.AddArc($rect.X, $rect.Y, $d, $d, 180.0, 90.0)
    $path.AddArc($rect.Right - $d, $rect.Y, $d, $d, 270.0, 90.0)
    $path.AddArc($rect.Right - $d, $rect.Bottom - $d, $d, $d, 0.0, 90.0)
    $path.AddArc($rect.X, $rect.Bottom - $d, $d, $d, 90.0, 90.0)
    $path.CloseFigure()

    $top = [System.Drawing.Color]::FromArgb(255, 0x8F, 0x74, 0xFF)
    $bottom = [System.Drawing.Color]::FromArgb(255, 0x5B, 0x3D, 0xD9)
    $brush = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
        $rect, $top, $bottom, [single]90.0)
    $g.FillPath($brush, $path)

    # Play triangle, optically centred: a triangle's visual centre sits slightly
    # right of its bounding box centre, so nudge it.
    $cx = [single]($Size * 0.535)
    $cy = [single]($Size * 0.5)
    $half = [single]($Size * 0.19)
    $points = [System.Drawing.PointF[]]@(
        [System.Drawing.PointF]::new([single]($cx - $half * 0.9), [single]($cy - $half)),
        [System.Drawing.PointF]::new([single]($cx - $half * 0.9), [single]($cy + $half)),
        [System.Drawing.PointF]::new([single]($cx + $half * 1.1), $cy)
    )
    $white = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
    $g.FillPolygon($white, $points)

    $g.Dispose()
    $brush.Dispose()
    $white.Dispose()
    $path.Dispose()
    return $bmp
}

$entries = @()
foreach ($size in $sizes) {
    $bmp = New-LogoBitmap -Size $size
    $ms = [System.IO.MemoryStream]::new()
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $entries += [pscustomobject]@{ Size = $size; Bytes = $ms.ToArray() }
    $ms.Dispose()
    $bmp.Dispose()
}

$dir = Split-Path -Parent $Output
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }

$stream = [System.IO.File]::Create($Output)
$writer = [System.IO.BinaryWriter]::new($stream)

# ICONDIR
$writer.Write([uint16]0)                 # reserved
$writer.Write([uint16]1)                 # type: icon
$writer.Write([uint16]$entries.Count)

# ICONDIRENTRY table
$offset = 6 + 16 * $entries.Count
foreach ($entry in $entries) {
    $dim = if ($entry.Size -ge 256) { 0 } else { $entry.Size }
    $writer.Write([byte]$dim)            # width (0 means 256)
    $writer.Write([byte]$dim)            # height
    $writer.Write([byte]0)               # palette entries
    $writer.Write([byte]0)               # reserved
    $writer.Write([uint16]1)             # colour planes
    $writer.Write([uint16]32)            # bits per pixel
    $writer.Write([uint32]$entry.Bytes.Length)
    $writer.Write([uint32]$offset)
    $offset += $entry.Bytes.Length
}

foreach ($entry in $entries) {
    $writer.Write($entry.Bytes)
}

$writer.Flush()
$writer.Dispose()
$stream.Dispose()

Write-Output ("Wrote {0} ({1} bytes, sizes: {2})" -f `
    $Output, (Get-Item $Output).Length, ($sizes -join ', '))
