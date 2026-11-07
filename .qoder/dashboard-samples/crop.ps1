param(
  [string]$Src,
  [string]$Dst,
  [int]$X = 0,
  [int]$Y = 0,
  [int]$W = 0,
  [int]$H = 0,
  [int]$Scale = 0
)
Add-Type -AssemblyName System.Drawing
$img = [System.Drawing.Image]::FromFile($Src)
Write-Output ("source: {0}x{1}" -f $img.Width, $img.Height)
if ($W -le 0) { $W = $img.Width - $X }
if ($H -le 0) { $H = $img.Height - $Y }
if ($Scale -gt 0) {
  $dw = [int]($W * $Scale / 100)
  $dh = [int]($H * $Scale / 100)
  $bmp = New-Object System.Drawing.Bitmap($dw, $dh)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.DrawImage($img, (New-Object System.Drawing.Rectangle(0, 0, $dw, $dh)), (New-Object System.Drawing.Rectangle($X, $Y, $W, $H)), [System.Drawing.GraphicsUnit]::Pixel)
} else {
  $bmp = New-Object System.Drawing.Bitmap($W, $H)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.DrawImage($img, (New-Object System.Drawing.Rectangle(0, 0, $W, $H)), (New-Object System.Drawing.Rectangle($X, $Y, $W, $H)), [System.Drawing.GraphicsUnit]::Pixel)
}
$bmp.Save($Dst, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose(); $img.Dispose()
Write-Output ("wrote: {0}" -f $Dst)
