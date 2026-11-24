param([Parameter(Mandatory)][string]$OutputDirectory)
# Draws the setup and uninstaller artwork: a quiet column that runs from a lifted tone into the
# wizard's own background (#F5F5F7 light, #121214 dark, see GrandHonor.iss), the GrandHonor mark
# (Assets\mark.ps1) on a soft shadow, the name in Inter Display and one line in Inter.

. (Join-Path $PSScriptRoot '..\Assets\mark.ps1')
$null = New-Item -ItemType Directory -Path $OutputDirectory -Force

$fonts = [Drawing.Text.PrivateFontCollection]::new()
foreach ($f in 'InterDisplay-SemiBold.ttf', 'Inter-Regular.ttf', 'Inter-Medium.ttf') {
    $fonts.AddFontFile((Resolve-Path (Join-Path $PSScriptRoot "..\Assets\fonts\$f")).Path)
}
function Get-Family([string]$Name) {
    $family = $fonts.Families | Where-Object Name -eq $Name | Select-Object -First 1
    if (!$family) { throw "Font family '$Name' not found in Assets\fonts." }
    return $family
}

function New-Canvas([int]$W, [int]$H, [bool]$Dark, [bool]$Gradient) {
    $b = [Drawing.Bitmap]::new($W, $H, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($b)
    $g.SmoothingMode = 'AntiAlias'; $g.TextRenderingHint = 'AntiAlias'; $g.PixelOffsetMode = 'HighQuality'
    $base = [Drawing.ColorTranslator]::FromHtml($(if ($Dark) { '#121214' } else { '#F5F5F7' }))
    $g.Clear($base)
    if ($Gradient) {
        $lift = [Drawing.ColorTranslator]::FromHtml($(if ($Dark) { '#1E1E22' } else { '#FFFFFF' }))
        $brush = [Drawing.Drawing2D.LinearGradientBrush]::new([Drawing.Rectangle]::new(0, 0, $W, $H), $lift, $base, 90.0)
        $g.FillRectangle($brush, 0, 0, $W, $H); $brush.Dispose()
    }
    return $b, $g
}

function Draw-Shadow([Drawing.Graphics]$G, [single]$X, [single]$Y, [single]$W, [bool]$Dark) {
    # A soft contact shadow: stacked, widening squircles at falling opacity.
    $peak = if ($Dark) { 26 } else { 14 }
    for ($i = 8; $i -ge 1; $i--) {
        $grow = $i * 2.2
        $p = New-Squircle ($X - $grow / 2) ($Y + $W * 0.06 - $grow / 2 + $i) ($W + $grow)
        $b = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb([int]($peak * (9 - $i) / 8), 0, 0, 0))
        $G.FillPath($b, $p); $b.Dispose(); $p.Dispose()
    }
}

function Draw-Hero([string]$Name, [bool]$Dark) {
    $W = 240; $H = 510
    $b, $g = New-Canvas $W $H $Dark $true
    $text = [Drawing.ColorTranslator]::FromHtml($(if ($Dark) { '#F5F5F7' } else { '#1D1D1F' }))
    $muted = [Drawing.ColorTranslator]::FromHtml($(if ($Dark) { '#98989D' } else { '#6E6E73' }))
    $mark = [single]104; $mx = ($W - $mark) / 2; $my = [single]142
    Draw-Shadow $g $mx $my $mark $Dark
    Draw-Mark $g $mx $my $mark
    $center = [Drawing.StringFormat]::new(); $center.Alignment = 'Center'
    $title = [Drawing.Font]::new((Get-Family 'Inter Display SemiBold'), 17, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Point)
    $small = [Drawing.Font]::new((Get-Family 'Inter'), 8.5, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Point)
    $tb = [Drawing.SolidBrush]::new($text); $mb = [Drawing.SolidBrush]::new($muted)
    try {
        $g.DrawString('GrandHonor', $title, $tb, [Drawing.RectangleF]::new(0, $my + $mark + 26, $W, 34), $center)
        $g.DrawString('for HONOR MagicBook', $small, $mb, [Drawing.RectangleF]::new(0, $my + $mark + 62, $W, 20), $center)
        $g.DrawString('Free and open source', $small, $mb, [Drawing.RectangleF]::new(0, $H - 52, $W, 20), $center)
        $b.Save((Join-Path $OutputDirectory $Name), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $tb.Dispose(); $mb.Dispose(); $title.Dispose(); $small.Dispose(); $center.Dispose(); $g.Dispose(); $b.Dispose() }
}

function Draw-Small([string]$Name, [bool]$Dark) {
    $b, $g = New-Canvas 72 72 $Dark $false
    try {
        Draw-Mark $g 8 8 56
        $b.Save((Join-Path $OutputDirectory $Name), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $g.Dispose(); $b.Dispose() }
}

try {
    Draw-Hero 'hero-light.png' $false
    Draw-Hero 'hero-dark.png' $true
    Draw-Small 'mark-light.png' $false
    Draw-Small 'mark-dark.png' $true
} finally { $fonts.Dispose() }
