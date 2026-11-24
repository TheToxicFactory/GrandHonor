# The GrandHonor mark, shared by the icon generator (GrandHonor.ico) and installer\theme.ps1
# (setup artwork): a graphite squircle (superellipse, n = 5) shaded from #3A3A3F at the top to
# #121214 at the bottom, a lit rim that fades from the top edge, and a strict white H whose stems
# snap to whole pixels. The tray icon (tray.rs) and the panel header (ui/src/main.rs) follow it.
Add-Type -AssemblyName System.Drawing

function New-Squircle([single]$X, [single]$Y, [single]$W) {
    $p = [Drawing.Drawing2D.GraphicsPath]::new()
    $n = 5.0; $a = $W / 2; $cx = $X + $a; $cy = $Y + $a
    $pts = [Collections.Generic.List[Drawing.PointF]]::new()
    for ($i = 0; $i -lt 256; $i++) {
        $t = 2 * [math]::PI * $i / 256
        $c = [math]::Cos($t); $s = [math]::Sin($t)
        $px = [math]::Sign($c) * [math]::Pow([math]::Abs($c), 2 / $n)
        $py = [math]::Sign($s) * [math]::Pow([math]::Abs($s), 2 / $n)
        $pts.Add([Drawing.PointF]::new([single]($cx + $a * $px), [single]($cy + $a * $py)))
    }
    $p.AddPolygon($pts.ToArray())
    return $p
}

function Draw-Mark([Drawing.Graphics]$G, [single]$X, [single]$Y, [single]$W) {
    $path = New-Squircle $X $Y $W
    $rect = [Drawing.RectangleF]::new($X, $Y, $W, $W)
    $fill = [Drawing.Drawing2D.LinearGradientBrush]::new($rect,
        [Drawing.Color]::FromArgb(255, 58, 58, 63), [Drawing.Color]::FromArgb(255, 18, 18, 20), 90.0)
    $rimBrush = [Drawing.Drawing2D.LinearGradientBrush]::new($rect,
        [Drawing.Color]::FromArgb(130, 255, 255, 255), [Drawing.Color]::FromArgb(0, 255, 255, 255), 90.0)
    $blend = [Drawing.Drawing2D.Blend]::new(3)
    $blend.Factors = [single[]](0, 1, 1); $blend.Positions = [single[]](0, 0.55, 1)
    $rimBrush.Blend = $blend
    $rim = [Drawing.Pen]::new($rimBrush, [single][math]::Max(1, $W * 0.012))
    $white = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(255, 245, 245, 247))
    try {
        $G.FillPath($fill, $path)
        $inner = New-Squircle ($X + $rim.Width / 2) ($Y + $rim.Width / 2) ($W - $rim.Width)
        $G.DrawPath($rim, $inner); $inner.Dispose()
        # Pixel-snapped H (stems 10.5 % of the side, 43 % tall).
        $t = [math]::Max(2, [math]::Round($W * 0.105))
        $left = [math]::Round($X + $W * 0.305); $right = [math]::Round($X + $W * 0.695) - $t
        $top = [math]::Round($Y + $W * 0.285); $bottom = [math]::Round($Y + $W * 0.715)
        $bar = [math]::Round(($top + $bottom - $t) / 2)
        $G.FillRectangle($white, [single]$left, [single]$top, [single]$t, [single]($bottom - $top))
        $G.FillRectangle($white, [single]$right, [single]$top, [single]$t, [single]($bottom - $top))
        $G.FillRectangle($white, [single]$left, [single]$bar, [single]($right + $t - $left), [single]$t)
    } finally { $path.Dispose(); $fill.Dispose(); $rimBrush.Dispose(); $rim.Dispose(); $white.Dispose() }
}
