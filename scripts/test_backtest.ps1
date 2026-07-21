<#
.SYNOPSIS
    Dune-guided backtest tester for MEV Scout.

.DESCRIPTION
    Uses Dune Analytics to find blocks with known MEV opportunities (arbitrages,
    sandwiches), then runs the full MEV Scout pipeline (discover, fetch, run, audit)
    against those blocks to verify detection accuracy.

.PARAMETER Chain
    Chain name (default: polygon).

.PARAMETER Days
    Look back N days for candidate blocks (default: 7).

.PARAMETER MevType
    MEV type to search for: arbitrage, sandwich, or both (default: both).

.PARAMETER Top
    Number of candidate blocks to test (default: 3).

.PARAMETER Config
    Path to MEV Scout config file (default: mev-scout.toml).

.PARAMETER SkipDiscover
    Skip the pool discovery step (use if pools are already cached).

.PARAMETER SkipAudit
    Skip the Dune audit step (faster, but no cross-validation).

.PARAMETER DuneApiKey
    Dune API key override (overrides config file).

.EXAMPLE
    .\scripts\test_backtest.ps1 -Chain polygon -Days 7 -MevType arbitrage -Top 3

.EXAMPLE
    .\scripts\test_backtest.ps1 -Chain polygon -Days 3 -MevType sandwich -Top 1 -SkipAudit
#>
param(
    [string]$Chain = "polygon",
    [int]$Days = 7,
    [string]$MevType = "both",
    [int]$Top = 3,
    [string]$Config = "mev-scout.toml",
    [switch]$SkipDiscover,
    [switch]$SkipAudit,
    [string]$DuneApiKey
)

$ErrorActionPreference = "Stop"

function Write-Step {
    param([string]$Message)
    Write-Host "`n=== $Message ===" -ForegroundColor Cyan
}

function Write-Success {
    param([string]$Message)
    Write-Host "  $Message" -ForegroundColor Green
}

function Write-Fail {
    param([string]$Message)
    Write-Host "  $Message" -ForegroundColor Red
}

function Invoke-Scout {
    param([string[]]$Arguments)
    $configArgs = @()
    if (Test-Path $Config) {
        $configArgs = @("-f", $Config)
    }
    $allArgs = $Arguments + $configArgs
    & cargo run --release -- $allArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "mev-scout command failed: $($allArgs -join ' ')"
    }
}

# ── Preflight checks ──
Write-Host "MEV Scout Dune-Guided Backtest" -ForegroundColor Yellow
Write-Host "Chain: $Chain | Days: $Days | Type: $MevType | Top: $Top"

if (-not (Test-Path $Config)) {
    Write-Host "Warning: Config file '$Config' not found, using defaults" -ForegroundColor Yellow
}

# ── Step 1: Find candidate blocks via Dune ──
Write-Step "Step 1: Finding blocks with $MevType opportunities via Dune"

$findBlocksArgs = @("dune-find-blocks", "--chain", $Chain, "--days", $Days.ToString(), "--mev-type", $MevType, "--top", $Top.ToString())
if ($DuneApiKey) {
    $findBlocksArgs += @("--dune-api-key", $DuneApiKey)
}

$output = Invoke-Scout -Arguments $findBlocksArgs
$blocks = $output | Where-Object { $_ -match '^\d+$' }

if (-not $blocks -or $blocks.Count -eq 0) {
    Write-Fail "No candidate blocks found."
    Write-Host "Check your Dune API key and network connectivity." -ForegroundColor Yellow
    Write-Host "`nRaw output:" -ForegroundColor DarkGray
    $output | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
    exit 1
}

Write-Success "Found $($blocks.Count) candidate block(s): $($blocks -join ', ')"

# ── Step 2: Discover pools ──
if (-not $SkipDiscover) {
    Write-Step "Step 2: Discovering pools (Dune + on-chain)"
    try {
        $discoverArgs = @("discover", "--chain", $Chain, "--source", "all")
        Invoke-Scout -Arguments $discoverArgs | Out-Null
        Write-Success "Pool discovery complete"
    } catch {
        Write-Host "  Pool discovery failed (continuing with cached pools): $_" -ForegroundColor Yellow
    }
} else {
    Write-Step "Step 2: Pool discovery (SKIPPED)"
}

# ── Step 3-4: For each candidate block ──
$stepNum = 3
$results = @()

foreach ($block in $blocks) {
    Write-Host ""
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor DarkCyan
    Write-Step "Step $stepNum: Testing block $block"

    # Fetch block data
    Write-Host "  Fetching block $block..." -ForegroundColor Gray
    try {
        Invoke-Scout -Arguments @("fetch", "--block", $block, "--chain", $Chain) | Out-Null
        Write-Success "Block $block fetched"
    } catch {
        Write-Fail "Fetch failed for block $block`: $_"
        continue
    }

    # Run backtest
    Write-Host "  Running backtest with --fact-check..." -ForegroundColor Gray
    $runOutput = Invoke-Scout -Arguments @("run", "--block", $block, "--chain", $Chain, "--fact-check")
    $runOutput | ForEach-Object { Write-Host "  $_" -ForegroundColor Gray }

    # Parse opportunities from run output
    $oppLine = $runOutput | Where-Object { $_ -match 'Detected (\d+) MEV opportunity' }
    if ($oppLine -match 'Detected (\d+) MEV opportunity') {
        $oppCount = [int]$Matches[1]
        Write-Success "Block $block`: $oppCount opportunities detected"
    } else {
        Write-Host "  Block $block`: No opportunities detected" -ForegroundColor Yellow
        $oppCount = 0
    }

    # Audit against Dune
    if (-not $SkipAudit) {
        Write-Host "  Auditing against Dune..." -ForegroundColor Gray
        try {
            $auditOutput = Invoke-Scout -Arguments @("audit", "--from-block", $block, "--to-block", $block, "--chain", $Chain)
            $auditOutput | ForEach-Object { Write-Host "  $_" -ForegroundColor Gray }
            Write-Success "Audit complete for block $block"
        } catch {
            Write-Host "  Audit failed for block $block`: $_" -ForegroundColor Yellow
        }
    }

    $results += [PSCustomObject]@{
        Block       = $block
        Opportunities = $oppCount
    }

    $stepNum++
}

# ── Summary ──
Write-Host "`n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor DarkCyan
Write-Host "`nTest Summary:" -ForegroundColor Yellow
$results | ForEach-Object {
    $status = if ($_.Opportunities -gt 0) { "+" } else { "-" }
    $color = if ($_.Opportunities -gt 0) { "Green" } else { "Yellow" }
    Write-Host "  [$status] Block $($_.Block): $($_.Opportunities) opportunities" -ForegroundColor $color
}

$totalOpps = ($results | Measure-Object -Property Opportunities -Sum).Sum
Write-Host "`nTotal: $($results.Count) blocks tested, $totalOpps opportunities found" -ForegroundColor Yellow
