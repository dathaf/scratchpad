# Generates app.ico (notes-style yellow tile) with PNG-compressed frames.
Add-Type -AssemblyName System.Drawing

$sizes = 256, 64, 48, 32, 24, 16
$frames = foreach ($s in $sizes) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $f = $s / 256.0

    $rect = New-Object System.Drawing.RectangleF (16 * $f), (16 * $f), (224 * $f), (224 * $f)
    $d = 104 * $f
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($rect.X, $rect.Y, $d, $d, 180, 90)
    $path.AddArc($rect.Right - $d, $rect.Y, $d, $d, 270, 90)
    $path.AddArc($rect.Right - $d, $rect.Bottom - $d, $d, $d, 0, 90)
    $path.AddArc($rect.X, $rect.Bottom - $d, $d, $d, 90, 90)
    $path.CloseFigure()

    $fill = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect,
        ([System.Drawing.Color]::FromArgb(255, 255, 224, 102)),
        ([System.Drawing.Color]::FromArgb(255, 255, 182, 0)), 90
    $g.FillPath($fill, $path)

    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(170, 110, 70, 0)), ([Math]::Max(1.4, 15 * $f))
    $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
    foreach ($line in @(@(92, 190), @(132, 190), @(172, 148))) {
        $g.DrawLine($pen, [float](66 * $f), [float]($line[0] * $f), [float]($line[1] * $f), [float]($line[0] * $f))
    }
    $g.Dispose()

    $ms = New-Object System.IO.MemoryStream
    if ($s -ge 256) {
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    } else {
        # Small sizes as plain bitmaps: Windows shows them without starting its PNG decoder,
        # which saves a few milliseconds when the app starts.
        $area = New-Object System.Drawing.Rectangle 0, 0, $s, $s
        $bits = $bmp.LockBits($area, 'ReadOnly', [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $pixels = New-Object byte[] ($s * $s * 4)
        [System.Runtime.InteropServices.Marshal]::Copy($bits.Scan0, $pixels, 0, $pixels.Length)
        $bmp.UnlockBits($bits)
        $maskRow = [int][Math]::Floor(($s + 31) / 32) * 4
        $bw = New-Object System.IO.BinaryWriter $ms
        # BITMAPINFOHEADER; the height counts colour rows plus the 1-bit mask rows
        $bw.Write([UInt32]40); $bw.Write([Int32]$s); $bw.Write([Int32]($s * 2)); $bw.Write([UInt16]1); $bw.Write([UInt16]32)
        $bw.Write([UInt32]0); $bw.Write([UInt32]($pixels.Length + $maskRow * $s))
        $bw.Write([Int32]0); $bw.Write([Int32]0); $bw.Write([UInt32]0); $bw.Write([UInt32]0)
        for ($row = $s - 1; $row -ge 0; $row--) { $bw.Write($pixels, $row * $s * 4, $s * 4) }   # bottom-up
        $bw.Write((New-Object byte[] ($maskRow * $s)))   # mask all zero: transparency comes from the alpha channel
        $bw.Flush()
    }
    $bmp.Dispose()
    , @($s, $ms.ToArray())
}

$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $out
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$frames.Count)
$offset = 6 + 16 * $frames.Count
foreach ($fr in $frames) {
    $s = $fr[0]; $data = $fr[1]
    $dim = if ($s -ge 256) { 0 } else { $s }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([UInt16]1); $w.Write([UInt16]32)
    $w.Write([UInt32]$data.Length); $w.Write([UInt32]$offset)
    $offset += $data.Length
}
foreach ($fr in $frames) { $w.Write([byte[]]$fr[1]) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $PSScriptRoot 'app.ico'), $out.ToArray())
