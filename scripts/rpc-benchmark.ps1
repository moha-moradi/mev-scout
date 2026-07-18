#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Benchmarks all RPC URLs from mev-scout.toml across multiple JSON-RPC methods.
.DESCRIPTION
    Tests each RPC endpoint with: eth_blockNumber, eth_chainId, web3_clientVersion,
    eth_gasPrice, eth_getBlockByNumber, eth_getTransactionReceipt, eth_getBalance,
    eth_call, eth_getLogs, eth_getStorageAt, eth_getCode, eth_getTransactionCount,
    eth_getBlockReceipts, eth_getProof, eth_maxPriorityFeePerGas.
    
    Outputs a comparison table with per-method latency and aggregate RPS.
#>

$ErrorActionPreference = "Continue"

# ── RPC URLs (from mev-scout.toml, line 22 has unclosed quote — fixed) ──────
$rpcUrls = @(
    @{ Name = "Tenderly";        Url = "https://polygon.gateway.tenderly.co/58axogDawo53qDU0C5bLWo" }
    @{ Name = "GetBlock-1";      Url = "https://shared.ap-southeast-1.getblock.io/0f8be36949d749a59ee2f8d263ec8600" }
    @{ Name = "GetBlock-2";      Url = "https://shared.ap-southeast-1.getblock.io/ac064dad442e40ceb3ac122654aa5495" }
    @{ Name = "GetBlock-3";      Url = "https://shared.ap-southeast-1.getblock.io/80a93b3d0084480488c75ef568f2970c" }
    @{ Name = "Ankr-1";          Url = "https://rpc.ankr.com/polygon/3b15083a831e18dc4fa8b867dd6a37f1206be7b5b18d438aa5bc8cb3696d5855" }
    @{ Name = "Ankr-2";          Url = "https://rpc.ankr.com/polygon/1f24e5f2e13020f57454d36e0cdedb705a138d84331cd3e632a75876a33724ac" }
    @{ Name = "Ankr-3";          Url = "https://rpc.ankr.com/polygon/3695659a781264a76faf0427d938a4aebd69bf7e3340f50f74488561b2e984fe" }
    @{ Name = "Alchemy-1";       Url = "https://polygon-mainnet.g.alchemy.com/v2/D3LDRulgIGHpGTT2u5zQE" }
    @{ Name = "Alchemy-2";       Url = "https://polygon-mainnet.g.alchemy.com/v2/d4ZKI9Tx9OnDE9E1r7ifs" }
    @{ Name = "Alchemy-3";       Url = "https://polygon-mainnet.g.alchemy.com/v2/booRpg6FM7gc9g8GC0SdN" }
    @{ Name = "Drpc-1";          Url = "https://lb.drpc.live/polygon/AolDryJPNULVk2IKbKzdz_PXsub6gmIR8afhwosiOHdW" }
    @{ Name = "Drpc-2";          Url = "https://lb.drpc.live/polygon/AlEVHe8j40WBrjJdhDUkokDnXjIgaJkR8ZpCVjewFaCJ" }
    @{ Name = "Drpc-3";          Url = "https://lb.drpc.live/polygon/Atek2uI_sEPeiLPQHycqLfsE5emegmUR8afiwosiOHdW" }
    @{ Name = "Nodies";          Url = "https://polygon-public.nodies.app" }
    @{ Name = "PublicNode-1";    Url = "https://polygon-bor-rpc.publicnode.com" }
    @{ Name = "DrpcOrg";         Url = "https://polygon.drpc.org" }
    @{ Name = "PublicNode-2";    Url = "https://polygon.publicnode.com" }
    @{ Name = "TenderlyComm";    Url = "https://tenderly.rpc.polygon.community" }
)

$WarmupRequests = 1
$MeasureRequests = 3
$RpsBurstCount = 15
$RpsBurstTimeout = 20

# ── Known Polygon addresses ────────────────────────────────────────────────
$WMATIC  = "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270"
$USDC    = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"
$WETH    = "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619"
$ZERO_ADDR = "0x0000000000000000000000000000000000000000"

# ── Helpers ─────────────────────────────────────────────────────────────────
function Invoke-RpcRaw {
    param([string]$Url, [string]$Method, [object]$Params, [int]$Id = 1, [int]$TimeoutSec = 10)
    $body = @{ jsonrpc = "2.0"; method = $Method; params = $Params; id = $Id } | ConvertTo-Json -Depth 10 -Compress
    try {
        $resp = Invoke-RestMethod -Uri $Url -Method Post -ContentType "application/json" `
            -Body $body -TimeoutSec $TimeoutSec -ErrorAction Stop
        return $resp
    } catch {
        return $null
    }
}

function Invoke-RpcTimed {
    param([string]$Url, [string]$Method, [object]$Params, [int]$TimeoutSec = 10)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $result = Invoke-RpcRaw -Url $Url -Method $Method -Params $Params -TimeoutSec $TimeoutSec
    $sw.Stop()
    $latencyMs = $sw.Elapsed.TotalMilliseconds
    if ($null -ne $result -and $null -ne $result.result) {
        return @{ Ok = $true; LatencyMs = $latencyMs; Result = $result.result; Error = $null }
    } elseif ($null -ne $result -and $null -ne $result.error) {
        return @{ Ok = $false; LatencyMs = $latencyMs; Result = $null; Error = $result.error.message }
    } else {
        return @{ Ok = $false; LatencyMs = $latencyMs; Result = $null; Error = "timeout or connection failed" }
    }
}

function Get-Median { param([double[]]$Values)
    $sorted = $Values | Sort-Object
    $n = $sorted.Count
    if ($n -eq 0) { return 0 }
    if ($n % 2 -eq 1) { return $sorted[($n - 1) / 2] }
    return ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2
}

function Get-P95 { param([double[]]$Values)
    $sorted = $Values | Sort-Object
    $idx = [math]::Floor($sorted.Count * 0.95)
    return $sorted[[math]::Min($idx, $sorted.Count - 1)]
}

# ── Discover dynamic test parameters ───────────────────────────────────────
Write-Host "`n=== RPC Benchmark Tool for mev-scout ===" -ForegroundColor Cyan
Write-Host "Discovering test parameters from PublicNode..." -ForegroundColor DarkGray

$tipResult = Invoke-RpcTimed -Url "https://polygon-bor-rpc.publicnode.com" -Method "eth_blockNumber" -Params @()
if (-not $tipResult.Ok) {
    Write-Host "FATAL: Cannot reach PublicNode to discover test params" -ForegroundColor Red
    exit 1
}
$tipHex = $tipResult.Result
$tipBlock = [Convert]::ToInt64($tipHex.TrimStart("0x"), 16)
$targetBlock = $tipBlock - 5
$targetBlockHex = "0x{0:X}" -f $targetBlock

Write-Host "  Tip block:  $tipBlock" -ForegroundColor DarkGray
Write-Host "  Test block: $targetBlock" -ForegroundColor DarkGray

# Get a tx hash from the test block for eth_getTransactionReceipt
$blockBody = @{ jsonrpc = "2.0"; method = "eth_getBlockByNumber"; params = @($targetBlockHex, $false); id = 1 }
$blockResp = Invoke-RpcRaw -Url "https://polygon-bor-rpc.publicnode.com" -Method "eth_getBlockByNumber" -Params @($targetBlockHex, $false)
$txHash = "0xf0b9c8c529e99b5c16e5a07eccfe019c3dd80a679a39c62e3ab646b85fc7a5db"  # fallback
if ($null -ne $blockResp -and $null -ne $blockResp.result.transactions) {
    if ($blockResp.result.transactions.Count -gt 0) {
        $txHash = $blockResp.result.transactions[0]
    }
}
Write-Host "  Test tx:    $txHash" -ForegroundColor DarkGray

# getLogs range: single block
$logFromHex = "0x{0:X}" -f ($targetBlock - 1)
$logToHex = $targetBlockHex

# ERC20 Transfer topic
$transferTopic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"

# balanceOf(address(0)) selector on WMATIC = 0x70a08231
$balanceCallData = "0x70a08231000000000000000000000000" + $ZERO_ADDR.Substring(2).ToLower()
# totalSupply() on USDC = 0x18160ddd
$totalSupplyData = "0x18160ddd"

Write-Host "  Log range:  $logFromHex -> $logToHex" -ForegroundColor DarkGray
Write-Host "  Log topic:  $transferTopic (Transfer)" -ForegroundColor DarkGray
Write-Host ""

# ── Define methods to test ──────────────────────────────────────────────────
$methods = @(
    @{
        Name = "eth_blockNumber"
        Method = "eth_blockNumber"
        Params = @()
        Desc = "latest block"
    },
    @{
        Name = "eth_chainId"
        Method = "eth_chainId"
        Params = @()
        Desc = "chain ID"
    },
    @{
        Name = "web3_clientVersion"
        Method = "web3_clientVersion"
        Params = @()
        Desc = "client version"
    },
    @{
        Name = "eth_gasPrice"
        Method = "eth_gasPrice"
        Params = @()
        Desc = "gas price"
    },
    @{
        Name = "eth_maxPriorityFeePerGas"
        Method = "eth_maxPriorityFeePerGas"
        Params = @()
        Desc = "max priority fee"
    },
    @{
        Name = "eth_getBlockByNumber"
        Method = "eth_getBlockByNumber"
        Params = @($targetBlockHex, $true)
        Desc = "full block w/ txs"
    },
    @{
        Name = "eth_getBlockReceipts"
        Method = "eth_getBlockReceipts"
        Params = @($targetBlockHex)
        Desc = "block receipts"
    },
    @{
        Name = "eth_getTransactionReceipt"
        Method = "eth_getTransactionReceipt"
        Params = @($txHash)
        Desc = "tx receipt"
    },
    @{
        Name = "eth_getBalance"
        Method = "eth_getBalance"
        Params = @($ZERO_ADDR, "latest")
        Desc = "zero balance"
    },
    @{
        Name = "eth_getTransactionCount"
        Method = "eth_getTransactionCount"
        Params = @($ZERO_ADDR, "latest")
        Desc = "nonce"
    },
    @{
        Name = "eth_call (balanceOf)"
        Method = "eth_call"
        Params = @(@{ to = $WMATIC; data = $balanceCallData }, "latest")
        Desc = "WMATIC balanceOf(0)"
    },
    @{
        Name = "eth_call (totalSupply)"
        Method = "eth_call"
        Params = @(@{ to = $USDC; data = $totalSupplyData }, "latest")
        Desc = "USDC totalSupply"
    },
    @{
        Name = "eth_getLogs"
        Method = "eth_getLogs"
        Params = @(@{ fromBlock = $logFromHex; toBlock = $logToHex; topics = @($transferTopic) })
        Desc = "Transfer events 1 block"
    },
    @{
        Name = "eth_getStorageAt"
        Method = "eth_getStorageAt"
        Params = @($WMATIC, "0x0", "latest")
        Desc = "WMATIC slot 0"
    },
    @{
        Name = "eth_getCode"
        Method = "eth_getCode"
        Params = @($WMATIC, "latest")
        Desc = "WMATIC bytecode"
    },
    @{
        Name = "eth_getProof"
        Method = "eth_getProof"
        Params = @($ZERO_ADDR, @(), $targetBlockHex)
        Desc = "proof at block"
    }
)

# ── Phase 1: Method-by-method latency benchmark ────────────────────────────
Write-Host "Phase 1: Method latency benchmark ($WarmupRequests warmup + $MeasureRequests measured)" -ForegroundColor Cyan
Write-Host ("-" * 120) -ForegroundColor DarkGray

$results = @{}   # key: "RpcName|MethodName" -> @(latency_ms...)
$rpcStatus = @{} # key: RpcName -> "ok"/"partial"/"failed"

foreach ($rpc in $rpcUrls) {
    $rpcName = $rpc.Name
    $rpcUrl = $rpc.Url
    $okCount = 0
    $totalMethods = $methods.Count

    Write-Host ("`n  [{0}]" -f $rpcName) -ForegroundColor Yellow -NoNewline
    Write-Host " $($rpcUrl.Substring(0, [Math]::Min(60, $rpcUrl.Length)))..." -ForegroundColor DarkGray

    foreach ($m in $methods) {
        $key = "$($rpcName)|$($m.Name)"
        $results[$key] = @()

        # Warmup
        for ($i = 0; $i -lt $WarmupRequests; $i++) {
            $null = Invoke-RpcTimed -Url $rpcUrl -Method $m.Method -Params $m.Params
        }

        # Measured
        $latencies = @()
        for ($i = 0; $i -lt $MeasureRequests; $i++) {
            $r = Invoke-RpcTimed -Url $rpcUrl -Method $m.Method -Params $m.Params
            if ($r.Ok) { $latencies += $r.LatencyMs }
            else { $latencies += -1 }
        }
        $results[$key] = $latencies

        $valid = $latencies | Where-Object { $_ -ge 0 }
        if ($valid.Count -eq $MeasureRequests) { $okCount++ }

        $avg = if ($valid.Count -gt 0) { ($valid | Measure-Object -Average).Average } else { 0 }
        $status = if ($valid.Count -eq $MeasureRequests) { "$([math]::Round($avg, 1))ms" }
                  elseif ($valid.Count -gt 0) { "$([math]::Round($avg, 1))ms ($($valid.Count)/$MeasureRequests)" }
                  else { "FAIL" }

        Write-Host "    $($m.Name): $status" -ForegroundColor $(if ($valid.Count -eq $MeasureRequests) { "Green" } elseif ($valid.Count -gt 0) { "Yellow" } else { "Red" })
    }

    if ($okCount -eq $totalMethods) { $rpcStatus[$rpcName] = "ok" }
    elseif ($okCount -gt 0) { $rpcStatus[$rpcName] = "partial" }
    else { $rpcStatus[$rpcName] = "failed" }
}

# ── Phase 2: RPS burst test (eth_blockNumber, concurrent) ──────────────────
Write-Host "`n`nPhase 2: RPS burst test ($RpsBurstCount concurrent eth_blockNumber requests)" -ForegroundColor Cyan
Write-Host ("-" * 120) -ForegroundColor DarkGray

$rpsResults = @{}

foreach ($rpc in $rpcUrls) {
    $rpcName = $rpc.Name
    $rpcUrl = $rpc.Url

    Write-Host ("  [{0}] " -f $rpcName) -ForegroundColor Yellow -NoNewline

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $tasks = @()
    for ($i = 0; $i -lt $RpsBurstCount; $i++) {
        $tasks += Invoke-RestMethod -Uri $rpcUrl -Method Post -ContentType "application/json" `
            -Body (@{ jsonrpc = "2.0"; method = "eth_blockNumber"; params = @(); id = $i } | ConvertTo-Json -Compress) `
            -TimeoutSec $RpsBurstTimeout -ErrorAction SilentlyContinue
    }
    $sw.Stop()
    $totalMs = $sw.Elapsed.TotalMilliseconds
    $successCount = ($tasks | Where-Object { $null -ne $_ -and $null -ne $_.result }).Count
    $rps = if ($totalMs -gt 0) { [math]::Round($successCount / ($totalMs / 1000), 1) } else { 0 }
    $avgLatency = if ($successCount -gt 0) { [math]::Round($totalMs / $successCount, 1) } else { 0 }

    $rpsResults[$rpcName] = @{ Rps = $rps; Success = $successCount; Total = $RpsBurstCount; AvgMs = $avgLatency; WallMs = [math]::Round($totalMs, 0) }

    $color = if ($rps -ge 50) { "Green" } elseif ($rps -ge 20) { "Yellow" } else { "Red" }
    Write-Host ("{0}/{1} ok | {2} RPS | avg {3}ms | wall {4}ms" -f $successCount, $RpsBurstCount, $rps, $avgLatency, $totalMs) -ForegroundColor $color
}

# ── Output: Comparison tables ───────────────────────────────────────────────
Write-Host "`n`n" -NoNewline

# ── Table 1: Method latency matrix ──────────────────────────────────────────
Write-Host ("=" * 120) -ForegroundColor Cyan
Write-Host "  METHOD LATENCY MATRIX (avg ms, $MeasureRequests samples)" -ForegroundColor Cyan
Write-Host ("=" * 120) -ForegroundColor Cyan

$methodNames = $methods | ForEach-Object { $_.Name }
$rpcNames = $rpcUrls | ForEach-Object { $_.Name }

# Header
$hdr = "{0,-16}" -f "RPC"
foreach ($mn in $methodNames) {
    $hdr += " | {0,14}" -f $mn.Substring(0, [Math]::Min(14, $mn.Length))
}
Write-Host $hdr -ForegroundColor White
Write-Host ("-" * 120) -ForegroundColor DarkGray

foreach ($rpcName in $rpcNames) {
    $row = "{0,-16}" -f $rpcName
    foreach ($mn in $methodNames) {
        $key = "$rpcName|$mn"
        $latencies = $results[$key]
        $valid = @($latencies | Where-Object { $_ -ge 0 })
        if ($valid.Count -gt 0) {
            $avg = [math]::Round(($valid | Measure-Object -Average).Average, 0)
            $p95 = [math]::Round((Get-P95 -Values $valid), 0)
            $cell = "{0}/{1}" -f $avg, $p95
        } else {
            $cell = "FAIL"
        }
        $color = if ($cell -eq "FAIL") { "Red" } elseif ([int]($cell.Split("/")[0]) -lt 100) { "Green" } elseif ([int]($cell.Split("/")[0]) -lt 500) { "Yellow" } else { "Red" }
        Write-Host (" | {0,14}" -f $cell) -ForegroundColor $color -NoNewline
    }
    Write-Host ""
}

Write-Host ""
Write-Host "  Legend: avg/p95 ms  |  Green <100ms  |  Yellow 100-500ms  |  Red >500ms or FAIL" -ForegroundColor DarkGray

# ── Table 2: RPS comparison ─────────────────────────────────────────────────
Write-Host "`n"
Write-Host ("=" * 80) -ForegroundColor Cyan
Write-Host "  RPS COMPARISON (eth_blockNumber burst, $RpsBurstCount requests)" -ForegroundColor Cyan
Write-Host ("=" * 80) -ForegroundColor Cyan
Write-Host ("  {0,-16} {1,10} {2,12} {3,10} {4,10} {5,10}" -f "RPC", "Success", "RPS", "Avg ms", "Wall ms", "Status") -ForegroundColor White
Write-Host ("-" * 80) -ForegroundColor DarkGray

$sortedRps = $rpcNames | Sort-Object { $rpsResults[$_].Rps } -Descending

foreach ($rpcName in $sortedRps) {
    $r = $rpsResults[$rpcName]
    $status = $rpcStatus[$rpcName]
    $color = if ($r.Rps -ge 50) { "Green" } elseif ($r.Rps -ge 20) { "Yellow" } else { "Red" }
    Write-Host ("  {0,-16} {1,10} {2,12} {3,10} {4,10} {5,10}" -f $rpcName, "$($r.Success)/$($r.Total)", $r.Rps, $r.AvgMs, $r.WallMs, $status) -ForegroundColor $color
}

# ── Table 3: Method support matrix ──────────────────────────────────────────
Write-Host "`n"
Write-Host ("=" * 80) -ForegroundColor Cyan
Write-Host "  METHOD SUPPORT MATRIX" -ForegroundColor Cyan
Write-Host ("=" * 80) -ForegroundColor Cyan
Write-Host ("  {0,-16}" -f "RPC") -ForegroundColor White -NoNewline
foreach ($mn in $methodNames) {
    Write-Host (" {0,2}" -f $mn.Substring(0, [Math]::Min(2, $mn.Length))) -ForegroundColor White -NoNewline
}
Write-Host ""

foreach ($rpcName in $rpcNames) {
    Write-Host ("  {0,-16}" -f $rpcName) -ForegroundColor Yellow -NoNewline
    foreach ($mn in $methodNames) {
        $key = "$rpcName|$mn"
        $latencies = $results[$key]
        $valid = @($latencies | Where-Object { $_ -ge 0 })
        $ch = if ($valid.Count -eq $MeasureRequests) { " OK" } elseif ($valid.Count -gt 0) { " ~~" } else { " XX" }
        $clr = if ($valid.Count -eq $MeasureRequests) { "Green" } elseif ($valid.Count -gt 0) { "Yellow" } else { "Red" }
        Write-Host (" {0,2}" -f $ch) -ForegroundColor $clr -NoNewline
    }
    Write-Host ""
}
Write-Host "  Legend: OK = all succeeded, ~~ = partial, XX = failed" -ForegroundColor DarkGray

# ── Summary ─────────────────────────────────────────────────────────────────
Write-Host "`n`n"
Write-Host ("=" * 80) -ForegroundColor Cyan
Write-Host "  SUMMARY" -ForegroundColor Cyan
Write-Host ("=" * 80) -ForegroundColor Cyan

$topRps = $sortedRps[0]
$topR = $rpsResults[$topRps]
Write-Host "  Fastest by RPS:      $topRps ($($topR.Rps) RPS)" -ForegroundColor Green

$avgRps = ($rpcNames | ForEach-Object { $rpsResults[$_].Rps } | Measure-Object -Average).Average
Write-Host "  Average RPS:         $([math]::Round($avgRps, 1))" -ForegroundColor White

$failedRpcs = $rpcNames | Where-Object { $rpcStatus[$_] -eq "failed" }
if ($failedRpcs.Count -gt 0) {
    Write-Host "  Fully failed:        $($failedRpcs -join ', ')" -ForegroundColor Red
}

$partialRpcs = $rpcNames | Where-Object { $rpcStatus[$_] -eq "partial" }
if ($partialRpcs.Count -gt 0) {
    Write-Host "  Partial support:     $($partialRpcs -join ', ')" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "  Note: avg/p95 ms shown in latency matrix. RPS measured with sequential burst." -ForegroundColor DarkGray
Write-Host "  eth_getProof requires archive node - some providers may not support it." -ForegroundColor DarkGray
