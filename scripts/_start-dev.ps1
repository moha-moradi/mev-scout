$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# Kill anything already on our ports
foreach ($p in 7600, 5173) {
  Get-NetTCPConnection -LocalPort $p -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
}

$api = Start-Process -FilePath "$root\target\debug\mev-scout-api.exe" `
  -ArgumentList @("--port","7600","--config","mev-scout.toml","--web-dir","web\dist") `
  -WorkingDirectory $root -WindowStyle Hidden -PassThru `
  -RedirectStandardOutput "$env:TEMP\mev-api-out.log" `
  -RedirectStandardError "$env:TEMP\mev-api-err.log"

$env:VITE_API_PORT = "7600"
$npmCmd = (Get-Command npm.cmd -ErrorAction SilentlyContinue).Source
if (-not $npmCmd) { $npmCmd = "npm.cmd" }
$vite = Start-Process -FilePath $npmCmd `
  -ArgumentList @("run","dev","--","--port","5173","--strictPort","--host","127.0.0.1") `
  -WorkingDirectory "$root\web" -WindowStyle Hidden -PassThru `
  -RedirectStandardOutput "$env:TEMP\mev-vite-out.log" `
  -RedirectStandardError "$env:TEMP\mev-vite-err.log"

"API_PID=$($api.Id)"
"VITE_PID=$($vite.Id)"
Start-Sleep -Seconds 3
try { (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7600/api/health).Content } catch { "health_err: $_" }
try { (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:5173/).StatusCode } catch { "vite_err: $_" }
