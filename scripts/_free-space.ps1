# Free C: temp space so Cursor Shell can run again.
$ErrorActionPreference = 'SilentlyContinue'
$targets = @(
  "$env:TEMP\cursor-sandbox-cache",
  "$env:TEMP\cargo-target",
  "$env:LOCALAPPDATA\Temp\cursor-sandbox-cache"
)
foreach ($t in $targets) {
  if (Test-Path $t) {
    Write-Output "Removing $t"
    Remove-Item -LiteralPath $t -Recurse -Force
  }
}
Get-ChildItem $env:TEMP -Filter 'ps-script-*.ps1' -ErrorAction SilentlyContinue | Remove-Item -Force
Get-ChildItem $env:TEMP -Filter 'ps-state-*.txt' -ErrorAction SilentlyContinue | Remove-Item -Force
Get-PSDrive C | Select-Object Used,Free
Write-Output "DONE"
