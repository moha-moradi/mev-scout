$ErrorActionPreference = 'Stop'
Set-Location 'd:\gitlab.dte.repo\mev-scout'

# Stop listeners / jobs
Get-NetTCPConnection -LocalPort 7600 -ErrorAction SilentlyContinue |
  Where-Object { $_.State -eq 'Listen' } |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
  Where-Object { $_.CommandLine -match 'mev-scout-api|explorer index' } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

Start-Sleep -Seconds 1

$apiLog = 'target\api-out.log'
$apiErr = 'target\api-err.log'
Remove-Item $apiLog,$apiErr -ErrorAction SilentlyContinue
$api = Start-Process -FilePath 'target\debug\mev-scout-api.exe' `
  -ArgumentList '--port','7600','--config','mev-scout.toml','--web-dir','web\dist' `
  -WorkingDirectory (Get-Location) `
  -RedirectStandardOutput $apiLog -RedirectStandardError $apiErr -PassThru -NoNewWindow
Write-Output "API_PID=$($api.Id)"

$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline) {
  try {
    Invoke-RestMethod -Uri 'http://127.0.0.1:7600/api/sync' -TimeoutSec 2 | Out-Null
    break
  } catch { Start-Sleep -Milliseconds 500 }
}

$jobBody = '{"command":"explorer index","args":["--live"]}'
$job = Invoke-RestMethod -Method POST -Uri 'http://127.0.0.1:7600/api/jobs' -ContentType 'application/json' -Body $jobBody
Write-Output "JOB=$($job | ConvertTo-Json -Compress)"

Start-Sleep -Seconds 15

$sync = Invoke-RestMethod -Uri 'http://127.0.0.1:7600/api/sync'
$feed = Invoke-RestMethod -Uri 'http://127.0.0.1:7600/api/explorer/feed?limit=8'

Write-Output '=== SYNC ==='
$sync | ConvertTo-Json -Depth 6 -Compress
Write-Output '=== FEED ==='
$feed | ConvertTo-Json -Depth 6

$kinds = @{}
$profitPop = 0
$nativePop = 0
foreach ($row in $feed) {
  $k = $row.kind
  if (-not $kinds.ContainsKey($k)) { $kinds[$k] = 0 }
  $kinds[$k]++
  if ($null -ne $row.profit_usd) { $profitPop++ }
  if ($null -ne $row.native_price_usd) { $nativePop++ }
}
Write-Output '=== SUMMARY ==='
Write-Output "feed_count=$($feed.Count) profit_usd_populated=$profitPop native_price_usd_populated=$nativePop"
Write-Output "kinds=$($kinds | ConvertTo-Json -Compress)"
