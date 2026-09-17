<#
.SYNOPSIS
  Start the local mev-scout API + Vite HMR stack.

.PARAMETER buildFirst
  Build the API (cargo) and ensure web deps (npm install) before starting.

.PARAMETER noApi
  Skip the API process.

.PARAMETER noWeb
  Skip the Vite dev server.

.PARAMETER port
  API listen port (default 7600). Passed to Vite as VITE_API_PORT for the /api proxy.

.PARAMETER webPort
  Vite listen port (default 5173).

.PARAMETER apiBinary
  Path to mev-scout-api.exe (default: target\debug\mev-scout-api.exe).

.PARAMETER webDir
  Static web dir served by the API (default: web\dist).

.PARAMETER config
  Path to mev-scout.toml (default: mev-scout.toml).
#>
param(
  [switch]$buildFirst,
  [switch]$noApi,
  [switch]$noWeb,
  [int]$port = 7600,
  [int]$webPort = 5173,
  [string]$apiBinary = "",
  [string]$webDir = "web\dist",
  [string]$config = "mev-scout.toml"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $apiBinary) {
  $apiBinary = Join-Path $root "target\debug\mev-scout-api.exe"
}

function Free-Port([int]$Port) {
  Get-NetTCPConnection -LocalPort $Port -ErrorAction SilentlyContinue |
    ForEach-Object {
      Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue
    }
}

function Resolve-Npm {
  $cmd = (Get-Command npm.cmd -ErrorAction SilentlyContinue).Source
  if ($cmd) { return $cmd }
  $cmd = (Get-Command npm -ErrorAction SilentlyContinue).Source
  if ($cmd) { return $cmd }
  throw "npm not found on PATH (need Node 20+)."
}

if ($buildFirst) {
  Write-Host "==> cargo build -p mev-scout-api"
  cargo build -p mev-scout-api
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }

  Write-Host "==> npm install (web/)"
  Push-Location (Join-Path $root "web")
  try {
    & (Resolve-Npm) install
    if ($LASTEXITCODE -ne 0) { throw "npm install failed (exit $LASTEXITCODE)" }
  } finally {
    Pop-Location
  }
}

$api = $null
$vite = $null
$apiLogOut = Join-Path $env:TEMP "mev-api-out.log"
$apiLogErr = Join-Path $env:TEMP "mev-api-err.log"
$viteLogOut = Join-Path $env:TEMP "mev-vite-out.log"
$viteLogErr = Join-Path $env:TEMP "mev-vite-err.log"

function Stop-DevStack {
  foreach ($proc in @($vite, $api)) {
    if ($null -ne $proc -and -not $proc.HasExited) {
      Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
  }
}

try {
  if (-not $noApi) { Free-Port $port }
  if (-not $noWeb) { Free-Port $webPort }

  if (-not $noApi) {
    if (-not (Test-Path $apiBinary)) {
      throw "API binary not found: $apiBinary`nRun with -buildFirst or: cargo build -p mev-scout-api"
    }
    Write-Host "==> API  http://127.0.0.1:$port  ($apiBinary)"
    $api = Start-Process -FilePath $apiBinary `
      -ArgumentList @(
        "--port", "$port",
        "--config", $config,
        "--web-dir", $webDir,
        "--cors-origin", "http://127.0.0.1:$webPort"
      ) `
      -WorkingDirectory $root -WindowStyle Hidden -PassThru `
      -RedirectStandardOutput $apiLogOut `
      -RedirectStandardError $apiLogErr
    Write-Host "    PID $($api.Id)  logs: $apiLogOut / $apiLogErr"
  }

  if (-not $noWeb) {
    $npmCmd = Resolve-Npm
    if (-not (Test-Path (Join-Path $root "web\node_modules"))) {
      throw "web\node_modules missing. Run with -buildFirst or: npm --prefix web install"
    }
    $env:VITE_API_PORT = "$port"
    Write-Host "==> Vite http://127.0.0.1:$webPort  (proxies /api -> :$port)"
    $vite = Start-Process -FilePath $npmCmd `
      -ArgumentList @("run", "dev", "--", "--port", "$webPort", "--strictPort", "--host", "127.0.0.1") `
      -WorkingDirectory (Join-Path $root "web") -WindowStyle Hidden -PassThru `
      -RedirectStandardOutput $viteLogOut `
      -RedirectStandardError $viteLogErr
    Write-Host "    PID $($vite.Id)  logs: $viteLogOut / $viteLogErr"
  }

  Start-Sleep -Seconds 3

  if (-not $noApi) {
    try {
      $health = (Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$port/api/health").Content
      Write-Host "==> health: $health"
    } catch {
      Write-Warning "API health check failed: $_"
      if (Test-Path $apiLogErr) { Get-Content $apiLogErr -Tail 20 | ForEach-Object { Write-Host "  $_" } }
    }
  }

  if (-not $noWeb) {
    try {
      $code = (Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$webPort/").StatusCode
      Write-Host "==> vite: HTTP $code"
    } catch {
      Write-Warning "Vite check failed: $_"
      if (Test-Path $viteLogErr) { Get-Content $viteLogErr -Tail 20 | ForEach-Object { Write-Host "  $_" } }
    }
  }

  Write-Host ""
  Write-Host "Dev stack running. Press Ctrl+C to stop."
  while ($true) {
    if (($null -ne $api -and $api.HasExited) -or ($null -ne $vite -and $vite.HasExited)) {
      Write-Warning "A child process exited; shutting down."
      break
    }
    Start-Sleep -Seconds 1
  }
}
finally {
  Stop-DevStack
  Write-Host "Stopped."
}
