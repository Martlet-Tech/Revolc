param(
    [string]$Src = "C:\Users\zt\.workbuddy\clipboard-images\clipboard-2026-11-18T01-17-18-647Z-d04c834f.jpg",
    [string]$OutIco = "D:\Projects\file-explorer\Revolc\assets\icon.ico",
    [string]$PreviewDir = "D:\Projects\file-explorer\Revolc\assets\preview"
)

Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = "Stop"

# Sizes embedded in the .ico. 20/40 are the Win10/11 DPI-scaled tray sizes.
$sizes = @(16, 20, 24, 32, 40, 48, 64, 96, 128, 256)

# ---------------------------------------------------------------- source load
$img = [System.Drawing.Image]::FromFile($Src)
$w = $img.Width
$h = $img.Height
Write-Host "source: $Src ($w x $h)"

$bmp = New-Object System.Drawing.Bitmap($w, $h, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
$g.DrawImage($img, 0, 0, $w, $h)
$g.Dispose()
$img.Dispose()

# ------------------------------------------------- white background -> alpha 0
# The art has a white background and a dark-green outline, so keying on
# "low saturation + high luminance" removes the background without eating the
# outline.  Partially transparent pixels get un-blended from white so that the
# later bicubic downscale does not spread a white halo.
$rect = New-Object System.Drawing.Rectangle(0, 0, $w, $h)
$data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadWrite, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$stride = $data.Stride
$buf = New-Object 'byte[]' ($stride * $h)
[System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $buf, 0, $buf.Length)

$kept = 0
for ($y = 0; $y -lt $h; $y++) {
    $row = $y * $stride
    for ($x = 0; $x -lt $w; $x++) {
        $i = $row + $x * 4
        $b = [int]$buf[$i]
        $gc = [int]$buf[$i + 1]
        $r = [int]$buf[$i + 2]
        $mx = [Math]::Max($r, [Math]::Max($gc, $b))
        $mn = [Math]::Min($r, [Math]::Min($gc, $b))
        if (($mx - $mn) -ge 30) {
            $a = 255
        }
        else {
            $a = [int][Math]::Round(($mn - 225) * -255.0 / 55.0)   # mn>=225 -> 0, mn<=170 -> 255
            if ($a -lt 0) { $a = 0 }
            if ($a -gt 255) { $a = 255 }
        }
        if ($a -gt 0 -and $a -lt 255) {
            $inv = 255.0 * (255 - $a) / $a
            $r = [int][Math]::Round(($r - $inv))
            $gc = [int][Math]::Round(($gc - $inv))
            $b = [int][Math]::Round(($b - $inv))
            if ($r -lt 0) { $r = 0 }; if ($r -gt 255) { $r = 255 }
            if ($gc -lt 0) { $gc = 0 }; if ($gc -gt 255) { $gc = 255 }
            if ($b -lt 0) { $b = 0 }; if ($b -gt 255) { $b = 255 }
            $buf[$i] = [byte]$b
            $buf[$i + 1] = [byte]$gc
            $buf[$i + 2] = [byte]$r
        }
        $buf[$i + 3] = [byte]$a
        if ($a -gt 0) { $kept++ }
    }
}
[System.Runtime.InteropServices.Marshal]::Copy($buf, 0, $data.Scan0, $buf.Length)
$bmp.UnlockBits($data)
Write-Host ("keyed out background, opaque-ish pixels: {0} / {1}" -f $kept, ($w * $h))

# ------------------------------------------------- premultiplied resize source
# Resampling must run on premultiplied alpha or the fully transparent pixels
# (which still carry white RGB) bleed into the edges of the leaves.
$pre = New-Object System.Drawing.Bitmap($w, $h, [System.Drawing.Imaging.PixelFormat]::Format32bppPArgb)
$gp = [System.Drawing.Graphics]::FromImage($pre)
$gp.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
$gp.DrawImage($bmp, 0, 0, $w, $h)
$gp.Dispose()

if (-not (Test-Path $PreviewDir)) { New-Item -ItemType Directory -Path $PreviewDir | Out-Null }

function Get-StraightPixels([System.Drawing.Bitmap]$src) {
    $s = $src.Width
    $r = New-Object System.Drawing.Rectangle(0, 0, $s, $s)
    $d = $src.LockBits($r, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppPArgb)
    $st = $d.Stride
    $raw = New-Object 'byte[]' ($st * $s)
    [System.Runtime.InteropServices.Marshal]::Copy($d.Scan0, $raw, 0, $raw.Length)
    $src.UnlockBits($d)
    # premultiplied -> straight BGRA, rows top-down
    $out = New-Object 'byte[]' ($s * $s * 4)
    for ($y = 0; $y -lt $s; $y++) {
        $rs = $y * $st
        $ro = $y * $s * 4
        for ($x = 0; $x -lt $s; $x++) {
            $i = $rs + $x * 4
            $o = $ro + $x * 4
            $a = [int]$raw[$i + 3]
            if ($a -eq 0) {
                $out[$o] = 0; $out[$o + 1] = 0; $out[$o + 2] = 0; $out[$o + 3] = 0
            }
            elseif ($a -eq 255) {
                $out[$o] = $raw[$i]; $out[$o + 1] = $raw[$i + 1]; $out[$o + 2] = $raw[$i + 2]; $out[$o + 3] = 255
            }
            else {
                $f = 255.0 / $a
                foreach ($k in 0..2) {
                    $v = [int][Math]::Round($raw[$i + $k] * $f)
                    if ($v -gt 255) { $v = 255 }
                    $out[$o + $k] = [byte]$v
                }
                $out[$o + 3] = [byte]$a
            }
        }
    }
    return , $out
}

# Unsharp mask on straight-alpha BGRA (alpha untouched; RGB overshoot clamped).
function Get-Sharpened([byte[]]$px, [int]$s, [double]$amount) {
    $k = @(1, 2, 1, 2, 4, 2, 1, 2, 1)
    $out = New-Object 'byte[]' $px.Length
    for ($y = 0; $y -lt $s; $y++) {
        for ($x = 0; $x -lt $s; $x++) {
            $o = ($y * $s + $x) * 4
            $out[$o] = $px[$o]; $out[$o + 1] = $px[$o + 1]; $out[$o + 2] = $px[$o + 2]; $out[$o + 3] = $px[$o + 3]
            if ($px[$o + 3] -eq 0) { continue }
            for ($c = 0; $c -lt 3; $c++) {
                $sum = 0.0
                $ki = 0
                for ($dy = -1; $dy -le 1; $dy++) {
                    for ($dx = -1; $dx -le 1; $dx++) {
                        $yy = $y + $dy; $xx = $x + $dx
                        if ($yy -lt 0) { $yy = 0 }; if ($yy -ge $s) { $yy = $s - 1 }
                        if ($xx -lt 0) { $xx = 0 }; if ($xx -ge $s) { $xx = $s - 1 }
                        $sum += $px[(($yy * $s) + $xx) * 4 + $c] * $k[$ki]
                        $ki++
                    }
                }
                $blur = $sum / 16.0
                $v = [int][Math]::Round($px[$o + $c] + $amount * ($px[$o + $c] - $blur))
                if ($v -lt 0) { $v = 0 }; if ($v -gt 255) { $v = 255 }
                $out[$o + $c] = [byte]$v
            }
        }
    }
    return , $out
}

# --------------------------------------------------------- render every size$entries = @()
foreach ($s in $sizes) {
    $sb = New-Object System.Drawing.Bitmap($s, $s, [System.Drawing.Imaging.PixelFormat]::Format32bppPArgb)
    $gs = [System.Drawing.Graphics]::FromImage($sb)
    $gs.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
    $gs.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $gs.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $gs.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $gs.DrawImage($pre, (New-Object System.Drawing.Rectangle(0, 0, $s, $s)))
    $gs.Dispose()

    $px = Get-StraightPixels $sb
    # Small sizes lose the leaf separation to the resample; a light unsharp mask
    # (3x3 gaussian, amount .7) keeps the four-leaf silhouette readable at 16-48.
    if ($s -le 48) { $px = Get-Sharpened $px $s 0.7 }

    # preview (straight alpha PNG)
    $pb = New-Object System.Drawing.Bitmap($s, $s, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $pr = New-Object System.Drawing.Rectangle(0, 0, $s, $s)
    $pd = $pb.LockBits($pr, [System.Drawing.Imaging.ImageLockMode]::WriteOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    [System.Runtime.InteropServices.Marshal]::Copy($px, 0, $pd.Scan0, $px.Length)
    $pb.UnlockBits($pd)
    $pb.Save((Join-Path $PreviewDir ("icon-{0}.png" -f $s)), [System.Drawing.Imaging.ImageFormat]::Png)
    # 8x neighbour-upscaled preview so tiny sizes can be eyeballed
    $big = New-Object System.Drawing.Bitmap(($s * 8), ($s * 8), [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $gb = [System.Drawing.Graphics]::FromImage($big)
    $gb.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
    $gb.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
    $gb.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
    $gb.DrawImage($pb, (New-Object System.Drawing.Rectangle(0, 0, ($s * 8), ($s * 8))))
    $gb.Dispose()
    $big.Save((Join-Path $PreviewDir ("icon-{0}-x8.png" -f $s)), [System.Drawing.Imaging.ImageFormat]::Png)
    $big.Dispose()
    $pb.Dispose()
    $sb.Dispose()

    # DIB payload: BITMAPINFOHEADER + bottom-up BGRA + 1bpp AND mask
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)
    $bw.Write([int]40)          # biSize
    $bw.Write([int]$s)          # biWidth
    $bw.Write([int]($s * 2))    # biHeight (XOR + AND)
    $bw.Write([int16]1)         # biPlanes
    $bw.Write([int16]32)        # biBitCount
    $bw.Write([int]0)           # biCompression = BI_RGB
    $bw.Write([int]($s * $s * 4))
    $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0)
    for ($y = $s - 1; $y -ge 0; $y--) {
        $bw.Write($px, ($y * $s * 4), ($s * 4))
    }
    $maskRow = [int]([Math]::Ceiling($s / 32.0) * 4)
    $mask = New-Object 'byte[]' ($maskRow * $s)
    # set mask bit where alpha == 0 (transparent)
    for ($y = 0; $y -lt $s; $y++) {
        for ($x = 0; $x -lt $s; $x++) {
            if ($px[(($y * $s) + $x) * 4 + 3] -eq 0) {
                $idx = ($s - 1 - $y) * $maskRow + [int][Math]::Floor($x / 8)
                $mask[$idx] = $mask[$idx] -bor (0x80 -shr ($x % 8))
            }
        }
    }
    $bw.Write($mask)
    $bw.Flush()
    $entries += , @{ Size = $s; Bytes = $ms.ToArray() }
    $bw.Dispose(); $ms.Dispose()
    Write-Host ("  rendered {0}x{0}  ({1} bytes)" -f $s, $entries[-1].Bytes.Length)
}
$pre.Dispose()
$bmp.Dispose()

# ------------------------------------------------------------------ write ICO
$dir = Split-Path -Parent $OutIco
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$fs = [System.IO.File]::Create($OutIco)
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([int16]0)                 # reserved
$bw.Write([int16]1)                 # type = icon
$bw.Write([int16]$entries.Count)
$offset = 6 + 16 * $entries.Count
foreach ($e in $entries) {
    $s = $e.Size
    $bw.Write([byte]$(if ($s -ge 256) { 0 } else { $s }))
    $bw.Write([byte]$(if ($s -ge 256) { 0 } else { $s }))
    $bw.Write([byte]0)              # palette colours
    $bw.Write([byte]0)              # reserved
    $bw.Write([int16]1)             # planes
    $bw.Write([int16]32)            # bit count
    $bw.Write([int]$e.Bytes.Length)
    $bw.Write([int]$offset)
    $offset += $e.Bytes.Length
}
foreach ($e in $entries) { $bw.Write($e.Bytes) }
$bw.Flush(); $bw.Dispose(); $fs.Dispose()

Write-Host ("wrote {0} ({1} bytes, {2} sizes)" -f $OutIco, (Get-Item $OutIco).Length, $entries.Count)
