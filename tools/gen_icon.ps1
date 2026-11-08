# Genera assets/destello.ico (16..256 px, PNG para 128/256 y BMP 32bpp para el resto).
# Uso:  powershell -ExecutionPolicy Bypass -File tools/gen_icon.ps1
param(
    [string]$Out = (Join-Path $PSScriptRoot '..\assets\destello.ico'),
    [string]$Preview = (Join-Path $PSScriptRoot '..\assets\destello-256.png')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

function New-RoundedRectPath {
    param([single]$x, [single]$y, [single]$w, [single]$h, [single]$r)
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $d = $r * 2
    $path.AddArc($x, $y, $d, $d, 180, 90)
    $path.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $path.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $path.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

function New-SparklePoints {
    param([single]$cx, [single]$cy, [single]$outer, [single]$inner)
    $points = [System.Drawing.PointF[]]::new(8)
    $points[0] = [System.Drawing.PointF]::new($cx, ($cy - $outer))
    $points[1] = [System.Drawing.PointF]::new(($cx + $inner), ($cy - $inner))
    $points[2] = [System.Drawing.PointF]::new(($cx + $outer), $cy)
    $points[3] = [System.Drawing.PointF]::new(($cx + $inner), ($cy + $inner))
    $points[4] = [System.Drawing.PointF]::new($cx, ($cy + $outer))
    $points[5] = [System.Drawing.PointF]::new(($cx - $inner), ($cy + $inner))
    $points[6] = [System.Drawing.PointF]::new(($cx - $outer), $cy)
    $points[7] = [System.Drawing.PointF]::new(($cx - $inner), ($cy - $inner))
    return $points
}

function New-IconBitmap {
    param([int]$size)

    $bmp = [System.Drawing.Bitmap]::new($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    $pad = [single]([Math]::Max(0.5, $size * 0.035))
    $side = [single]($size - 2 * $pad)
    $radius = [single]($side * 0.26)
    $path = New-RoundedRectPath $pad $pad $side $side $radius

    $grad = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
        ([System.Drawing.PointF]::new($pad, $pad)),
        ([System.Drawing.PointF]::new(($size - $pad), ($size - $pad))),
        [System.Drawing.Color]::FromArgb(255, 109, 91, 255),
        [System.Drawing.Color]::FromArgb(255, 0, 196, 255))
    $g.FillPath($grad, $path)

    # Brillo sutil en la esquina superior izquierda.
    $glow = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(38, 255, 255, 255))
    $glowPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $glowPath.AddEllipse([single]($pad - $side * 0.15), [single]($pad - $side * 0.35), [single]($side * 0.95), [single]($side * 0.75))
    $g.FillPath($glow, $glowPath)

    # Borde interior claro.
    if ($size -ge 20) {
        $pen = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(55, 255, 255, 255), [single]([Math]::Max(1.0, $size * 0.018)))
        $inset = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $inner = [single]($side - 2 * [Math]::Max(1.0, $size * 0.03))
        $inset = New-RoundedRectPath ($pad + $size * 0.03) ($pad + $size * 0.03) $inner $inner ([single]($inner * 0.24))
        $g.DrawPath($pen, $inset)
        $pen.Dispose(); $inset.Dispose()
    }

    # Destello principal.
    $cx = [single]($size * 0.5)
    $cy = [single]($size * 0.52)
    $outer = [single]($size * 0.32)
    $innerR = [single]($size * 0.082)
    if ($size -le 20) { $outer = [single]($size * 0.36); $innerR = [single]($size * 0.10) }
    $white = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
    $g.FillPolygon($white, (New-SparklePoints $cx $cy $outer $innerR))

    if ($size -ge 32) {
        $sx = [single]($size * 0.755)
        $sy = [single]($size * 0.235)
        $g.FillPolygon($white, (New-SparklePoints $sx $sy ([single]($size * 0.085)) ([single]($size * 0.024))))
        $dot = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(235, 255, 255, 255))
        $g.FillEllipse($dot, [single]($size * 0.235), [single]($size * 0.735), [single]($size * 0.05), [single]($size * 0.05))
        $dot.Dispose()
    }

    $white.Dispose(); $glow.Dispose(); $glowPath.Dispose(); $grad.Dispose(); $path.Dispose()
    $g.Dispose()
    return $bmp
}

function ConvertTo-PngBytes {
    param([System.Drawing.Bitmap]$bmp)
    $ms = [System.IO.MemoryStream]::new()
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bytes = $ms.ToArray()
    $ms.Dispose()
    return $bytes
}

function ConvertTo-IcoBmpBytes {
    param([System.Drawing.Bitmap]$bmp, [int]$size)
    $rect = [System.Drawing.Rectangle]::new(0, 0, $size, $size)
    $locked = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = [Math]::Abs($locked.Stride)
    $pixels = New-Object byte[] ($stride * $size)
    [System.Runtime.InteropServices.Marshal]::Copy($locked.Scan0, $pixels, 0, $pixels.Length)
    $bmp.UnlockBits($locked)

    $ms = [System.IO.MemoryStream]::new()
    $w = [System.IO.BinaryWriter]::new($ms)
    $w.Write([UInt32]40)                       # biSize
    $w.Write([Int32]$size)                     # biWidth
    $w.Write([Int32]($size * 2))               # biHeight (XOR + AND)
    $w.Write([UInt16]1)                        # biPlanes
    $w.Write([UInt16]32)                       # biBitCount
    $w.Write([UInt32]0)                        # BI_RGB
    $w.Write([UInt32]($size * $size * 4))      # biSizeImage
    $w.Write([Int32]0); $w.Write([Int32]0)
    $w.Write([UInt32]0); $w.Write([UInt32]0)

    for ($y = $size - 1; $y -ge 0; $y--) {
        $w.Write($pixels, $y * $stride, $size * 4)
    }

    # MÃ¡scara AND a cero (todo opaco), filas alineadas a 32 bits.
    $maskRow = [int]([Math]::Floor(($size + 31) / 32) * 4)
    $zero = New-Object byte[] ($maskRow * $size)
    $w.Write($zero)
    $w.Flush()
    $bytes = $ms.ToArray()
    $w.Dispose(); $ms.Dispose()
    return $bytes
}

$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$entries = @()

foreach ($size in $sizes) {
    $bmp = New-IconBitmap $size
    if ($size -le 64) {
        $data = ConvertTo-IcoBmpBytes $bmp $size
    } else {
        $data = ConvertTo-PngBytes $bmp
    }
    $entries += [pscustomobject]@{ Size = $size; Data = $data }

    if ($size -eq 256) {
        $bmp.Save($Preview, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    $bmp.Dispose()
}

$ms = [System.IO.MemoryStream]::new()
$w = [System.IO.BinaryWriter]::new($ms)
$w.Write([UInt16]0)                 # reserved
$w.Write([UInt16]1)                 # type: icon
$w.Write([UInt16]$entries.Count)

$offset = 6 + (16 * $entries.Count)
foreach ($entry in $entries) {
    $dim = if ($entry.Size -ge 256) { 0 } else { $entry.Size }
    $w.Write([byte]$dim)
    $w.Write([byte]$dim)
    $w.Write([byte]0)               # color count
    $w.Write([byte]0)               # reserved
    $w.Write([UInt16]1)             # planes
    $w.Write([UInt16]32)            # bit count
    $w.Write([UInt32]$entry.Data.Length)
    $w.Write([UInt32]$offset)
    $offset += $entry.Data.Length
}
foreach ($entry in $entries) {
    $w.Write([byte[]]$entry.Data)
}
$w.Flush()

$dir = Split-Path -Parent $Out
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
[System.IO.File]::WriteAllBytes((Resolve-Path -LiteralPath $dir).Path + '\' + (Split-Path -Leaf $Out), $ms.ToArray())
$w.Dispose(); $ms.Dispose()

Write-Host ("Icono generado: {0} ({1:N0} bytes, {2} tamaÃ±os)" -f $Out, (Get-Item $Out).Length, $entries.Count)

