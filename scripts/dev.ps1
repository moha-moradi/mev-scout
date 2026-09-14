# Phase 6 dev launcher — runs the API (:7600) and the Vite dev server (:5173)
# concurrently. The Vite dev server proxies /api → 127.0.0.1:7600.
#
# Prereqs:
#   1. `cargo build -p mev-scout-cli` (job manager spawns it) — the API
#      picks the sibling `target\debug` binary automatically; pass
#      `-apiBinary path\to\mev-scout.exe` to point elsewhere.
#   2. `npm install` inside web/ (node 20+ on PATH).
#
# Flags:
#   -config        path to the live mev-scout.toml (default mev-scout.toml)
#   -port          API port (default 7600) / -webPort Vite port (default 5173)
#   -apiBinary     explicit CLI binary path
#   -noApi         skip starting the API (already running)
#   -noWeb         skip starting Vite (static build already served by API)
#   -webDir        static bundle dir (default web\dist)
#   -buildFirst    run `cargo build` + `npm run build` before starting
#
# Example:
#   powershell -ExecutionPolicy Bypass -File scripts\dev.ps1 -buildFirst
param(
  [string]$config = "mev-scout.toml",
  [int]$port = 7600,
  [int]$webPort = 5173,
  [string]$apiBinary = "",
  [switch]$noApi,
  [switch]$noWeb,
  [string]$webDir = "web\dist",
  [switch]$buildFirst
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

function Run-Foreground($Name, $Args, $WorkDir) {
  Write-Host "`n▶ $Name:" -ForegroundColor Cyan
  Push-Location
  try {
    if ($WorkDir) { Set-Location $WorkDir }
    & $Name @Args
  }
  finally { Pop-Location }
}

if ($buildFirst) {
  Run-Foreground "cargo build" @("-p", "mev-scout-cli") $root
  Run-Foreground "npm" @("run", "build") (Join-Path $root "web")
}

Write-Host "`nStarting mev-scout dev environment" -ForegroundColor Green
Write-Host "  API  -> http://127.0.0.1:$port"
if (-not $noWeb) {
  Write-Host "  Web  -> http://127.0.0.1:$webPort (Vite dev, proxied /api)"
}

$jobs = @()

if (-not $noApi) {
  $apiArgs = @(
    "--port", "$port",
    "--config", $config,
    "--web-dir", $webDir
  )
  if ($apiBinary) { $apiArgs += @("--binary", $apiBinary) }
  $jobs += Start-Process -FilePath "cargo" `
    -ArgumentList @("run", "-p", "mev-scout-api", "--", @$apiArgs) `
    -WorkingDirectory $root `-RedirectStandardOutput (Join-Path $env:TEMP "mev-scout-api-out.log") `
    -RedirectStandardError (Join-Path $env:TEMP "mev-scout-api-err.log") `
    -NoNewWindow -PassThru
}

if (-not $noWeb) {
  $env:VITE_API_PORT = "$port"
  $vite = Start-Process -FilePath "npm" `
    -ArgumentList @("run", "dev", "--", "--port", "$webPort", "--strictPort") `
    -WorkingDirectory (Join-Path $root "web") `
    -RedirectStandardOutput (Join-Path $env:TEMP "mev-scout-vite-out.log") `
    -RedirectStandardError (Join-Path $env:TEMP "mev-scout-vite-err.log") `
    -NoNewWindow -PassThru
  $jobs += $vite
}

Write-Host "`nPress Ctrl+C to stop both. Logs in $env:TEMP\mev-scout-*-*.log" -ForegroundColor Yellow

try {
  foreach ($j in $jobs) { $j.WaitForExit() }
} finally {
  foreach ($j in $jobs) {
    if (-not $j.HasExited) { $j.Kill() }
  }
}
