# Start Vite only (API assumed running on 7600)
$root = Split-Path -Parent $PSScriptRoot
$env:VITE_API_PORT = "7600"
$npmCmd = (Get-Command npm.cmd -ErrorAction SilentlyContinue).Source
if (-not $npmCmd) { $npmCmd = "$env:ProgramFiles\nodejs\npm.cmd" }
$vite = Start-Process -FilePath $npmCmd `
  -ArgumentList @("run","dev","--","--port","5173","--strictPort","--host","127.0.0.1") `
  -WorkingDirectory "$root\web" -WindowStyle Hidden -PassThru `
  -RedirectStandardOutput "$env:TEMP\mev-vite-out.log" `
  -RedirectStandardError "$env:TEMP\mev-vite-err.log"
"VITE_PID=$($vite.Id)"
Start-Sleep -Seconds 4
try { (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:5173/).StatusCode } catch { "vite_err: $_" }
try { (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7600/api/health).Content } catch { "health_err: $_" }
