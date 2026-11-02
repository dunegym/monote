# 生成 MoNote 应用图标（PNG + ICO）。无第三方依赖，使用 .NET System.Drawing。
Add-Type -AssemblyName System.Drawing

$iconDir = Join-Path $PSScriptRoot '..\src-tauri\icons'
$iconDir = [System.IO.Path]::GetFullPath($iconDir)
New-Item -ItemType Directory -Force -Path $iconDir | Out-Null

function New-MonoteBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAlias
    $rect = New-Object System.Drawing.Rectangle(0, 0, $size, $size)
    # 底色：墨色深蓝渐变（呼应“墨记”）
    $c1 = [System.Drawing.Color]::FromArgb(255, 30, 41, 59)
    $c2 = [System.Drawing.Color]::FromArgb(255, 55, 72, 108)
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $c1, $c2, 45.0)
    $radius = [int]($size * 0.22)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $radius * 2
    $path.AddArc(0, 0, $d, $d, 180, 90)
    $path.AddArc($size - $d, 0, $d, $d, 270, 90)
    $path.AddArc($size - $d, $size - $d, $d, $d, 0, 90)
    $path.AddArc(0, $size - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    $g.FillPath($brush, $path)
    # 纸页
    $mx = [int]($size * 0.24)
    $my = [int]($size * 0.18)
    $mw = [int]($size * 0.52)
    $mh = [int]($size * 0.64)
    $page = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r2 = [int]($size * 0.06)
    $page.AddArc($mx, $my, $r2 * 2, $r2 * 2, 180, 90)
    $page.AddArc($mx + $mw - $r2 * 2, $my, $r2 * 2, $r2 * 2, 270, 90)
    $page.AddArc($mx + $mw - $r2 * 2, $my + $mh - $r2 * 2, $r2 * 2, $r2 * 2, 0, 90)
    $page.AddArc($mx, $my + $mh - $r2 * 2, $r2 * 2, $r2 * 2, 90, 90)
    $page.CloseFigure()
    $paper = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 241, 245, 249))
    $g.FillPath($paper, $page)
    # 文字 “墨”
    $fontSize = [float]($size * 0.34)
    try {
        $font = New-Object System.Drawing.Font('Microsoft YaHei', $fontSize, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    } catch {
        $font = New-Object System.Drawing.Font('Segoe UI', $fontSize, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    }
    $ink = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 15, 23, 42))
    $fmt = New-Object System.Drawing.StringFormat
    $fmt.Alignment = [System.Drawing.StringAlignment]::Center
    $fmt.LineAlignment = [System.Drawing.StringAlignment]::Center
    $textRect = New-Object System.Drawing.RectangleF($mx, $my, $mw, $mh)
    $g.DrawString('墨', $font, $ink, $textRect, $fmt)
    $g.Dispose()
    return $bmp
}

$sizes = @(32, 128, 256, 512)
$pngData = @{}
foreach ($s in $sizes) {
    $bmp = New-MonoteBitmap $s
    $name = switch ($s) {
        32 { '32x32.png' }
        128 { '128x128.png' }
        256 { '128x128@2x.png' }
        512 { 'icon.png' }
    }
    $bmp.Save((Join-Path $iconDir $name), [System.Drawing.Imaging.ImageFormat]::Png)
    if ($s -eq 256) {
        $ms = New-Object System.IO.MemoryStream
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $pngData['ico'] = $ms.ToArray()
        $ms.Dispose()
    }
    $bmp.Dispose()
}

# 组装 ICO（单条目，内嵌 PNG）
$png = $pngData['ico']
$fs = [System.IO.File]::Create((Join-Path $iconDir 'icon.ico'))
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([uint16]0)      # reserved
$bw.Write([uint16]1)      # type = icon
$bw.Write([uint16]1)      # count
$bw.Write([byte]0)        # width 0 = 256
$bw.Write([byte]0)        # height 0 = 256
$bw.Write([byte]0)        # palette
$bw.Write([byte]0)        # reserved
$bw.Write([uint16]1)      # planes
$bw.Write([uint16]32)     # bpp
$bw.Write([uint32]$png.Length)
$bw.Write([uint32]22)     # offset
$bw.Write($png)
$bw.Close()
$fs.Close()

Write-Host "Icons written to $iconDir"
Get-ChildItem $iconDir | Select-Object Name, Length
