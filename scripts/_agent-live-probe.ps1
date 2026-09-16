$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Get-NetTCPConnection -LocalPort 7600 -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
Get-Process -Name "mev-scout-api" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

$api = Start-Process -FilePath "$root\target\debug\mev-scout-api.exe" `
  -ArgumentList @("--port","7600","--config","mev-scout.toml","--web-dir","web\dist") `
  -WorkingDirectory $root -WindowStyle Hidden -PassThru `
  -RedirectStandardOutput "$env:TEMP\mev-api-out.log" `
  -RedirectStandardError "$env:TEMP\mev-api-err.log"

Write-Output "API_PID=$($api.Id)"
Start-Sleep -Seconds 2
try {
  $h = Invoke-RestMethod -Uri "http://127.0.0.1:7600/api/health" -TimeoutSec 5
  Write-Output "HEALTH=$($h | ConvertTo-Json -Compress)"
} catch {
  Write-Output "HEALTH_ERR=$($_.Exception.Message)"
  Get-Content "$env:TEMP\mev-api-err.log" -ErrorAction SilentlyContinue | Select-Object -Last 20
  exit 1
}

$jobBody = '{"command":"explorer index","args":["--live"]}'
$job = Invoke-RestMethod -Method POST -Uri "http://127.0.0.1:7600/api/jobs" -ContentType "application/json" -Body $jobBody
Write-Output "JOB=$($job | ConvertTo-Json -Compress)"

Write-Output "WAITING_20s..."
Start-Sleep -Seconds 20

$feed = Invoke-RestMethod -Uri "http://127.0.0.1:7600/api/explorer/feed?limit=10"
$tip = $feed.tip_block
if (-not $tip) { $tip = $feed.tip }
Write-Output "TIP_BLOCK=$tip"
Write-Output "FEED_JSON=$($feed | ConvertTo-Json -Depth 6 -Compress)"

$rows = $feed.rows
if (-not $rows) { $rows = $feed }
if ($rows -isnot [array]) { $rows = @($rows) }

foreach ($r in $rows) {
  $bn = $r.block_number
  $kind = $r.kind
  $np = $r.native_price_usd
  $pu = $r.profit_usd
  $pt = $r.profit_token
  if ($pt -and $pt.Length -gt 12) { $pt = $pt.Substring(0, 6) + ".." + $pt.Substring($pt.Length - 4) }
  $fresh = if ($tip -and $bn) { "$($tip - $bn) blocks behind tip" } else { "n/a" }
  Write-Output "ROW block=$bn kind=$kind native_price_usd=$np profit_usd=$pu profit_token=$pt freshness=$fresh"
}

$hasArb = ($rows | Where-Object { $_.kind -eq "arb_atomic" }).Count -gt 0
$hasNative = ($rows | Where-Object { $null -ne $_.native_price_usd -and $_.native_price_usd -ne 0 }).Count -gt 0
Write-Output "CONFIRM_arb_atomic=$hasArb CONFIRM_native_price_usd=$hasNative"
