# `mev-scout` CLI Reference Guide

**Version:** 0.1.0 | **Binary:** `mev-scout` | **Workspace:** mev-scout (Rust)

MEV Scout is an MEV opportunity scanner, backtester, and live simulator for EVM-compatible blockchains. It detects and quantifies arbitrage, sandwich, JIT liquidity, liquidation, cross-block, and time-bandit opportunities across 4 DEX types (Uniswap V2/V3, Curve, Balancer) on 7 chains.

---

## Table of Contents

1. [Global Flags](#1-global-flags)
2. [`mev-scout run` — Full Backtest](#2-mev-scout-run--full-backtest)
3. [`mev-scout fetch` — Pre-cache Block Data](#3-mev-scout-fetch--pre-cache-block-data)
4. [`mev-scout report` — Re-render Saved Results](#4-mev-scout-report--re-render-saved-results)
5. [`mev-scout config` — Print Resolved Config](#5-mev-scout-config--print-resolved-config)
6. [`mev-scout replay` — Debug a Single Block](#6-mev-scout-replay--debug-a-single-block)
7. [`mev-scout discover` — Pool Discovery](#7-mev-scout-discover--pool-discovery)
8. [`mev-scout fact-check` — Verify Results](#8-mev-scout-fact-check--verify-results)
9. [`mev-scout live` — Live MEV Bot Mode](#9-mev-scout-live--live-mev-bot-mode)
10. [Practical Examples by Chain](#10-practical-examples-by-chain)
11. [Strategy Reference](#11-strategy-reference)
12. [Gas Model Reference](#12-gas-model-reference)
13. [DEX Support Matrix](#13-dex-support-matrix)
14. [Configuration File Reference](#14-configuration-file-reference)

---

## 1. Global Flags

Available on **all** subcommands:

| Flag | Type | Description |
|------|------|-------------|
| `-f, --config FILE` | path | Path to TOML config file |
| `-v, --verbose` | bool | Enable debug-level logging |
| `--quiet` | bool | Suppress all output except final summary |

Usage:

```bash
mev-scout -f my-config.toml -v run --days 7
mev-scout --quiet run --block 50000000 -r <RPC>
```

---

## 2. `mev-scout run` — Full Backtest

This is the primary command. It runs the complete MEV detection pipeline: validate config, discover pools, fetch blocks, replay via `revm` EVM, detect all enabled strategies, aggregate USD prices, optionally fact-check and run PGA simulation.

### Syntax

```bash
mev-scout run [FLAGS]
```

### Block Range (exactly one required)

| Flag | Type | Description |
|------|------|-------------|
| `--days N` | u64 (1–365) | Last N days of blocks |
| `--blocks N` | u64 (≥1) | Last N blocks from chain tip |
| `--block NUMBER` | u64 (≥1) | Single specific block number |
| `--from-block NUMBER --to-block NUMBER` | u64 | Explicit range (requires both) |

### Chain & Connection

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `-n, --chain NAME` | string | `polygon` | Target chain (see chain list below) |
| `-r, --rpc URL` | string | — | Archive node RPC endpoint |
| `--rpc-workers N` | usize | 1 | Concurrent RPC workers (1–3 for public, 10–20 for private) |
| `--rps-limit RPS` | f64 | 500 | RPC requests per second limit (0 = unlimited) |
| `--rpc-urls URLS` | string | — | Additional RPC URLs (comma-separated) for multi-provider |
| `--rpc-rps RPS` | string | — | Per-provider RPS limits (comma-separated, maps 1:1 with URLs) |
| `--no-batch-rpc` | bool | false | Disable JSON-RPC batching |

**Supported chains:** `polygon` (137), `avalanche` (43114), `bsc` (56), `arbitrum` (42161), `base` (8453), `ethereum` (1), `optimism` (10)

### Flash Loan

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--flash-loan-provider PROVIDER` | string | `auto` | `auto`, `balancer`, `aave`, `uniswap` |

### Strategies

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--strategies LIST` | string | `all` | Comma-separated strategy names or `all` |
| `--proximity-window N` | usize | 3 | Tx index window for JIT and JitArb detection |
| `--cross-block-window N` | usize | 0 | Cross-block MEV window (0 = disabled) |

### Gas Model

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--gas-model MODEL` | string | `historical_exact` | `historical_exact`, `p90`, `fixed`, `distribution_N`, `live` |
| `--gas-limit GAS` | u64 | 200000 | Gas limit for arb tx cost estimation |
| `--priority-fee GWEI` | f64 | 0.0 | Priority fee premium in gwei |

### Output

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--output FORMAT` | string | `table` | `table`, `csv`, `json` |
| `--export-path PATH` | string | `./results` | Directory for exports |
| `--db-path PATH` | string | `./cache/mev-scout.sqlite` | SQLite database path |
| `--parquet-dir PATH` | string | — | Parquet intermediate directory (optional) |
| `--fact-check` | bool | false | Print detailed fact-check report |
| `--evm-fact-check` | bool | false | EVM-based fact-check (requires `--fact-check`) |

### PGA (Priority Gas Auction)

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--pga` | bool | false | Enable PGA simulation |
| `--pga-mean-competitors N` | f64 | 3.0 | Mean competing searchers |
| `--pga-intensity F` | f64 | 0.5 | Fraction of surplus dissipated |

### Pricing

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--price-oracle MODE` | string | `coingecko` | `coingecko`, `onchain`, `hybrid` |
| `--token-price PAIRS` | string | — | Per-token prices: `0xADDR=1.50,0xADDR2=1800` |

### Mempool & Competition

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--capture-pending` | bool | false | Capture pending txs from mempool |
| `--competition` | bool | false | Enable competitor extraction analysis |

### Examples

```bash
# Basic: last 7 days on Polygon with all strategies
mev-scout run --days 7 -n polygon -r https://polygon-rpc.publicnode.com

# Single block on Ethereum with arbitrage only
mev-scout run --block 20000000 -n ethereum -r <RPC> --strategies two_hop_arb,multi_hop_arb

# BSC range with JSON output and PGA
mev-scout run --from-block 40000000 --to-block 40000100 -n bsc -r <RPC> --pga --output json

# Cross-block detection on Arbitrum
mev-scout run --blocks 50 -n arbitrum -r <RPC> --cross-block-window 3

# Full config: fixed gas, Aave flash loans, fact-check, CSV export
mev-scout run --days 14 -n ethereum -r <RPC> \
  --gas-model fixed --priority-fee 3.0 \
  --flash-loan-provider aave \
  --output csv --export-path ./eth-results \
  --fact-check --pga

# Sandwich + JIT only on Base with proximity tuning
mev-scout run --blocks 100 -n base -r <RPC> \
  --strategies sandwich,jit,jit_arb \
  --proximity-window 5

# Multi-provider RPC with per-provider rate limits
mev-scout run --days 1 -n polygon -r <RPC1> \
  --rpc-urls <RPC2>,<RPC3> --rpc-rps 10,20,30

# Custom token prices with on-chain oracle
mev-scout run --block 50000000 -n avalanche -r <RPC> \
  --price-oracle onchain \
  --token-price "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7=15.0"

# Fact-check with EVM re-verification after the run
mev-scout run --days 3 -n optimism -r <RPC> \
  --fact-check --evm-fact-check

# Competitor analysis + PGA calibration
mev-scout run --blocks 200 -n ethereum -r <RPC> \
  --competition --pga --output json

# Capture pending mempool txs alongside backtest
mev-scout run --days 1 -n polygon -r <RPC> \
  --capture-pending
```

---

## 3. `mev-scout fetch` — Pre-cache Block Data

Fetches and caches block data **without running any detection strategies**. Useful for warming the cache before a backtest, or for offline analysis.

### Syntax

```bash
mev-scout fetch [FLAGS]
```

### Flags

Same **Block Range**, **Chain & Connection**, `--db-path`, and `--parquet-dir` as `run`.

### Examples

```bash
# Pre-cache 7 days of Polygon blocks
mev-scout fetch --days 7 -n polygon -r https://polygon-rpc.publicnode.com

# Pre-cache a specific Ethereum block range
mev-scout fetch --from-block 19000000 --to-block 19000100 -n ethereum -r <RPC>

# Pre-cache with Parquet export for later analysis
mev-scout fetch --blocks 500 -n bsc -r <RPC> --parquet-dir ./parquet-bsc
```

---

## 4. `mev-scout report` — Re-render Saved Results

Re-renders previously saved JSON results to a different output format without re-running the backtest.

### Syntax

```bash
mev-scout report [FLAGS]
```

### Flags

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--run-id ID` | string | latest | Run ID to report (e.g. `run_1712345678`) |
| `--output FORMAT` | string | `table` | `table`, `csv`, `json` |
| `--export-path PATH` | string | `./results` | Directory containing result files |

### Examples

```bash
# Re-render latest run as table
mev-scout report

# Re-render specific run as CSV
mev-scout report --run-id run_1712345678 --output csv

# Re-render from custom export directory
mev-scout report --run-id run_1712345678 --output json --export-path ./my-results
```

---

## 5. `mev-scout config` — Print Resolved Config

Prints the fully resolved configuration (defaults + config file + CLI overrides) as TOML to stdout. Useful for debugging what settings are active.

### Syntax

```bash
mev-scout config [GLOBAL FLAGS]
```

### Examples

```bash
# Print default config
mev-scout config

# Print config loaded from file
mev-scout -f my-config.toml config

# Print config with CLI overrides merged (verbose to see debug logs too)
mev-scout -f my-config.toml -v config
```

---

## 6. `mev-scout replay` — Debug a Single Block

Replays a single block for debugging purposes. Shows transaction execution traces and optionally analyzes DEX interactions.

### Syntax

```bash
mev-scout replay --block NUMBER [FLAGS]
```

### Flags

| Flag | Type | Required | Description |
|------|------|----------|-------------|
| `--block NUMBER` | u64 | **yes** | Block number to replay |
| `--tx-index INDEX` | usize | no | Replay up to this tx index (default: all) |
| `--analyze` | bool | no | Show DEX interaction analysis per tx |
| `--rpc URL` | string | varies | Archive node RPC endpoint |
| `--db-path PATH` | string | no | Custom SQLite cache path |
| `--parquet-dir PATH` | string | no | Parquet directory |

### Examples

```bash
# Replay a single block on Polygon (shows all txs)
mev-scout replay --block 50000000 -n polygon -r https://polygon-rpc.publicnode.com

# Replay up to tx index 10 on Ethereum
mev-scout replay --block 19000000 -n ethereum -r <RPC> --tx-index 10

# Replay with DEX analysis
mev-scout replay --block 50000000 -n polygon -r <RPC> --analyze

# Replay from cached data
mev-scout replay --block 50000000 -n polygon --db-path ./cache/mev-scout.sqlite
```

---

## 7. `mev-scout discover` — Pool Discovery

Discovers DEX pools from factory events by scanning logs via the RPC endpoint. Found pools are printed to stdout and optionally saved to the SQLite cache.

### Syntax

```bash
mev-scout discover --from-block NUMBER --to-block NUMBER [FLAGS]
```

### Flags

| Flag | Type | Required | Description |
|------|------|----------|-------------|
| `--from-block NUMBER` | u64 | **yes** | Start block for log scanning |
| `--to-block NUMBER` | u64 | **yes** | End block (inclusive) |
| `--v2-factories ADDRS` | string | no | V2 factory addresses (comma-separated, overrides config) |
| `--v3-factory ADDR` | string | no | V3 factory address (overrides config) |
| `--batch-size N` | u64 | 10 | Batch size for `eth_getLogs` requests |
| `--no-save` | bool | false | Skip saving to SQLite |
| `--db-path PATH` | string | no | SQLite database path |
| Chain & Connection flags | — | — | Same as `run` |

### Examples

```bash
# Discover pools from default factories on Polygon
mev-scout discover -n polygon -r <RPC> \
  --from-block 50000000 --to-block 50001000

# Discover with custom V3 factory on Arbitrum, no save
mev-scout discover -n arbitrum -r <RPC> \
  --from-block 200000000 --to-block 200002000 \
  --v3-factory 0x1F98431c8aD98523631AE4a59f267346ea31F984 \
  --no-save

# Discover with small batch size for low-rate-limit RPCs
mev-scout discover -n ethereum -r <RPC> \
  --from-block 19000000 --to-block 19001000 \
  --batch-size 5

# Discover V2 pools only
mev-scout discover -n bsc -r <RPC> \
  --from-block 40000000 --to-block 40000500 \
  --v2-factories 0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73
```

---

## 8. `mev-scout fact-check` — Verify Results

Verifies a previous run's detected opportunities against on-chain state. Structural check verifies pool math; `--re-verify` re-fetches block data from cache and re-runs pool state initialization.

### Syntax

```bash
mev-scout fact-check <RUN_ID> [FLAGS]
```

### Arguments

| Arg | Type | Required | Description |
|-----|------|----------|-------------|
| `RUN_ID` | string | **yes** | Run ID to fact-check (e.g. `run_1712345678`) |

### Flags

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--re-verify` | bool | false | Re-load block data from cache and re-verify |

### Examples

```bash
# Basic fact-check
mev-scout fact-check run_1712345678

# Fact-check with re-verification from cached blocks
mev-scout fact-check run_1712345678 --re-verify
```

---

## 9. `mev-scout live` — Live MEV Bot Mode

Connects to the live chain and runs as a virtual MEV bot with a simulated wallet. Polls the mempool for pending txs, runs detection, and optionally executes virtual trades.

### Syntax

```bash
mev-scout live [FLAGS]
```

### Wallet

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--initial-balance AMOUNT` | f64 | 10.0 | Starting virtual balance (native token) |
| `--min-profit AMOUNT` | f64 | 0.001 | Minimum profit to execute a virtual trade |
| `--max-executions N` | u64 | unlimited | Cap on virtual executions |

### Mempool

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--poll-interval MS` | u64 | 1000 | Mempool poll interval in ms |
| `--resync-interval N` | u64 | 60 | Poll cycles between full pool resyncs |
| `--replay-file PATH` | string | — | Recorded pending-tx JSON for offline replay |

### Strategies

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--strategies LIST` | string | `two_hop_arb,multi_hop_arb` | Comma-separated detection strategies |

### Gas Model

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--gas-limit GAS` | u64 | 200000 | Gas limit per virtual trade |
| `--priority-fee GWEI` | f64 | 1.0 | Priority fee in gwei |
| `--gas-model MODEL` | string | `live` | `live` or `fixed` |

### Pricing & Output

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--price-oracle MODE` | string | `coingecko` | `coingecko`, `onchain`, `hybrid` |
| `--token-price PAIRS` | string | — | Per-token USD prices |
| `--export-path PATH` | string | `./results` | Output directory for execution logs |
| `--db-path PATH` | string | config default | SQLite database path |

### Chain & Connection

Same as `run`: `-n/--chain`, `-r/--rpc`, `--rpc-workers`, `--rps-limit`, `--rpc-urls`, `--rpc-rps`.

### Examples

```bash
# Basic live mode on Polygon (defaults)
mev-scout live -n polygon -r https://polygon-rpc.publicnode.com

# Live mode with custom wallet and fast polling
mev-scout live -n ethereum -r <RPC> \
  --initial-balance 5.0 --min-profit 0.01 \
  --poll-interval 500 --strategies two_hop_arb,sandwich

# Live mode from recorded mempool data (no live RPC needed)
mev-scout live -n polygon --replay-file ./recorded_pending.json

# Live mode with max executions limit
mev-scout live -n bsc -r <RPC> \
  --initial-balance 100.0 --max-executions 10 \
  --gas-limit 300000 --priority-fee 2.0

# Live mode with fixed gas model
mev-scout live -n arbitrum -r <RPC> \
  --gas-model fixed --priority-fee 0.1
```

---

## 10. Practical Examples by Chain

### Polygon (Chain ID: 137)

**Native token:** MATIC | **WMATIC:** `0x0d500b1d8e8ef31e21c99d1db9a6444d3adf1270`
**DEXes:** QuickSwap (V2/V3), Uniswap V3, SushiSwap, Balancer V2, ApeSwap, DFYN, Meshswap

```bash
# Full backtest — last 24 hours, all strategies, table output
mev-scout run --days 1 -n polygon -r https://polygon-bor-rpc.publicnode.com

# 7-day arbitrage scan with PGA
mev-scout run --days 7 -n polygon -r <PRIVATE_RPC> \
  --strategies two_hop_arb,multi_hop_arb \
  --pga --output json

# Single block replay with DEX analysis
mev-scout replay --block 58000000 -n polygon -r <RPC> --analyze

# Cross-block MEV on Polygon with higher proximity window
mev-scout run --blocks 200 -n polygon -r <RPC> \
  --cross-block-window 5 --proximity-window 5

# Live mode with 0.5 MATIC min profit + sandwich detection
mev-scout live -n polygon -r <RPC> \
  --initial-balance 50 --min-profit 0.5 \
  --strategies sandwich,two_hop_arb,multi_hop_arb

# Pool discovery for QuickSwap V2
mev-scout discover -n polygon -r <RPC> \
  --from-block 58000000 --to-block 58001000 \
  --v2-factories 0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32
```

---

### Ethereum (Chain ID: 1)

**Native token:** ETH | **WETH:** `0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2`
**DEXes:** Uniswap V2/V3, SushiSwap, ShibaSwap, Curve, Balancer V2

```bash
# Full backtest — last 3 days, all strategies
mev-scout run --days 3 -n ethereum -r https://ethereum-rpc.publicnode.com

# Sandwich + JIT detection on a recent block
mev-scout run --block 21000000 -n ethereum -r <RPC> \
  --strategies sandwich,jit,jit_arb

# Fact-check with EVM re-verification
mev-scout run --blocks 50 -n ethereum -r <RPC> \
  --fact-check --evm-fact-check --output json

# Competition analysis with PGA
mev-scout run --days 7 -n ethereum -r <RPC> \
  --competition --pga --output json --export-path ./eth-comp

# Live mode arbitrage bot with 1 ETH min profit
mev-scout live -n ethereum -r <RPC> \
  --initial-balance 100 --min-profit 1.0 \
  --strategies two_hop_arb --poll-interval 200

# Use CoinGecko API key for higher rate limits
mev-scout -f config-eth.toml run --days 7 -n ethereum -r <RPC>

# Range scan with gas distribution model
mev-scout run --from-block 21000000 --to-block 21001000 \
  -n ethereum -r <RPC> \
  --gas-model distribution_90 --gas-limit 300000
```

---

### BSC (Chain ID: 56)

**Native token:** BNB | **WBNB:** `0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c`
**DEXes:** PancakeSwap V2/V3, SushiSwap

```bash
# Last 24 hours PancakeSwap-focused scan
mev-scout run --days 1 -n bsc -r https://bsc.publicnode.com

# 100-block arbitrage scan with CSV export
mev-scout run --blocks 100 -n bsc -r <RPC> \
  --strategies two_hop_arb,multi_hop_arb \
  --output csv --export-path ./bsc-arb-results

# Fixed gas model (PancakeSwap uses low fees)
mev-scout run --block 40000000 -n bsc -r <RPC> \
  --gas-model fixed --priority-fee 1.0 --gas-limit 150000

# Live mode with BNB virtual wallet
mev-scout live -n bsc -r <RPC> \
  --initial-balance 500 --min-profit 0.1 \
  --priority-fee 2.0 --gas-limit 250000

# Pre-cache data for later analysis
mev-scout fetch --days 14 -n bsc -r <RPC> \
  --parquet-dir ./bsc-parquet

# Discover PancakeSwap V2 pools
mev-scout discover -n bsc -r <RPC> \
  --from-block 40000000 --to-block 40000500 \
  --v2-factories 0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73
```

---

### Avalanche (Chain ID: 43114)

**Native token:** AVAX | **WAVAX:** `0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7`
**DEXes:** Uniswap V3, SushiSwap, Trader Joe V1, Balancer V2. Aave V3 for liquidations.

```bash
# Full backtest last 7 days
mev-scout run --days 7 -n avalanche -r https://avalanche-c-chain.publicnode.com

# Liquidation-focused scan with Aave V3
mev-scout run --blocks 200 -n avalanche -r <RPC> \
  --strategies liquidation,two_hop_arb

# On-chain pricing (no CoinGecko dependency)
mev-scout run --days 3 -n avalanche -r <RPC> \
  --price-oracle onchain

# Custom AVAX price override
mev-scout run --block 45000000 -n avalanche -r <RPC> \
  --token-price "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7=15.0"

# Live mode with small wallet
mev-scout live -n avalanche -r <RPC> \
  --initial-balance 100 --min-profit 0.5 \
  --strategies two_hop_arb,multi_hop_arb,sandwich
```

---

### Arbitrum (Chain ID: 42161)

**Native token:** ETH | **WETH:** `0x82aF49447D8a07e3bd95BD0d56f35241523fBab1`
**DEXes:** Uniswap V3, Camelot V2. Balancer V2, Aave V3.

```bash
# Full backtest last 7 days (Arbitrum has ~1 sec block times)
mev-scout run --days 1 -n arbitrum -r https://arbitrum-one.publicnode.com

# 500-block scan — high block throughput
mev-scout run --blocks 500 -n arbitrum -r <RPC> \
  --strategies all --pga --output json

# Cross-block detection with wide window
mev-scout run --blocks 1000 -n arbitrum -r <RPC> \
  --cross-block-window 10

# Live mode with aggressive polling
mev-scout live -n arbitrum -r <RPC> \
  --initial-balance 50 --poll-interval 200 \
  --strategies two_hop_arb,multi_hop_arb

# Pre-cache a large range for offline analysis
mev-scout fetch --from-block 250000000 --to-block 250010000 \
  -n arbitrum -r <RPC>
```

---

### Base (Chain ID: 8453)

**Native token:** ETH | **WETH:** `0x4200000000000000000000000000000000000006`
**DEXes:** Aerodrome (V2), Uniswap V3. Balancer V2, Aave V3.

```bash
# Full scan — last 7 days
mev-scout run --days 7 -n base -r https://base.publicnode.com

# Aerodrome-focused arbitrage (Aerodrome uses modified V2)
mev-scout run --blocks 200 -n base -r <RPC> \
  --strategies two_hop_arb,multi_hop_arb

# Sandwich detection with custom proximity window
mev-scout run --block 15000000 -n base -r <RPC> \
  --strategies sandwich --proximity-window 3

# Live mode on Base with low gas
mev-scout live -n base -r <RPC> \
  --initial-balance 10 --min-profit 0.01 \
  --gas-limit 150000 --priority-fee 0.1
```

---

### Optimism (Chain ID: 10)

**Native token:** ETH | **WETH:** `0x4200000000000000000000000000000000000006`
**DEXes:** Uniswap V3, SushiSwap (V2). Balancer V2, Aave V3.

```bash
# Full backtest last 7 days (fast block times)
mev-scout run --days 3 -n optimism -r https://optimism-rpc.publicnode.com

# Fact-checked run with EVM verification
mev-scout run --blocks 100 -n optimism -r <RPC> \
  --fact-check --evm-fact-check

# Hybrid pricing (CoinGecko + on-chain cross-check)
mev-scout run --days 7 -n optimism -r <RPC> \
  --price-oracle hybrid

# Live mode
mev-scout live -n optimism -r <RPC> \
  --initial-balance 20 --min-profit 0.05 \
  --strategies two_hop_arb

# Competitor analysis on recent blocks
mev-scout run --blocks 500 -n optimism -r <RPC> \
  --competition --pga --output json
```

---

### Cross-Chain Scenario Examples

```bash
# Compare profitability across 3 chains in sequence
for chain in polygon arbitrum bsc; do
  mev-scout run --days 7 -n "$chain" -r <RPC_$chain> \
    --output json --export-path "./results/$chain"
done

# Multi-provider RPC for Ethereum (higher throughput)
mev-scout run --days 7 -n ethereum \
  -r https://ethereum-rpc.publicnode.com \
  --rpc-urls https://eth.merkle.io,https://rpc.ankr.com/eth \
  --rpc-rps 1.0,5.0,3.0

# Full pipeline: fetch → discover → run → fact-check
mev-scout fetch --from-block 58000000 --to-block 58001000 -n polygon -r <RPC>
mev-scout discover -n polygon -r <RPC> --from-block 58000000 --to-block 58001000
mev-scout run --from-block 58000000 --to-block 58001000 -n polygon -r <RPC> \
  --fact-check --output json
mev-scout fact-check run_<TIMESTAMP>
```

---

## 11. Strategy Reference

| Strategy | CLI Name | Description | Key Tuning | Default |
|----------|----------|-------------|------------|---------|
| TwoHopArb | `two_hop_arb` | Two-hop triangular arbitrage (A→B→A across 2 pools) | `--proximity-window` | 3 |
| MultiHopArb | `multi_hop_arb` | Multi-hop arbitrage across 3+ pools | — | — |
| Sandwich | `sandwich` | Front-run + back-run a victim swap | `--proximity-window` | 3 |
| JIT | `jit` | Just-In-Time liquidity provision before a large swap | `--proximity-window` | 3 |
| JitArb | `jit_arb` | Combined JIT + arbitrage detection | `--proximity-window` | 3 |
| Liquidation | `liquidation` | Aave V3 liquidation opportunities | — | — |
| CrossBlockArb | `cross_block_arb` | Arbitrage across consecutive blocks using price persistence | `--cross-block-window` | 0 (off) |
| TimeBandit | `time_bandit` | Reorg-based MEV detection | `--cross-block-window` | 0 (off) |

**Strategy selection:**
```bash
# All strategies (default)
--strategies all

# Arbitrage only
--strategies two_hop_arb,multi_hop_arb

# Sandwich + JIT focused
--strategies sandwich,jit,jit_arb

# Liquidation + arbitrage
--strategies liquidation,two_hop_arb,multi_hop_arb

# Cross-block MEV (requires cross_block_window > 0)
--strategies cross_block_arb,time_bandit --cross-block-window 5

# Single strategy
--strategies sandwich
```

---

## 12. Gas Model Reference

| Model | CLI Value | Description | Best For |
|-------|-----------|-------------|----------|
| Historical Exact | `historical_exact` | Uses each block's actual base fee + priority fee. The most accurate for historical backtests. | Historical backtesting |
| P90 | `p90` | 90th percentile effective gas price from recent blocks. Simulates paying above-average prices. | Conservative profit estimates |
| Fixed | `fixed` | Only uses `--priority-fee` (no base fee). Fees don't vary by block. | Quick estimates, L2s with fixed fees |
| Distribution N | `distribution_50` .. `distribution_99` | N-th percentile from the H10 gas price distribution. `distribution_50` = median, `distribution_95` = very conservative. | Fine-grained gas modeling |
| Live | `live` | Fetches real-time base fee + priority fee from chain. | Live mode |

```bash
# Historical exact (default) — most accurate for backtesting
--gas-model historical_exact

# Conservative estimate (90th percentile)
--gas-model p90 --priority-fee 2.0

# Fixed gas (ignore base fee, simulate L2-like)
--gas-model fixed --priority-fee 1.5 --gas-limit 200000

# Custom percentile from H10 distribution
--gas-model distribution_75

# Live chain gas (live mode default)
--gas-model live --priority-fee 1.0
```

**Per-strategy gas limit overrides** (config file only):
```toml
[gas_limits]
two_hop_arb = 150000
sandwich = 300000
multi_hop_arb = 250000
jit = 180000
liquidation = 500000
```

---

## 13. DEX Support Matrix

| DEX | Type | Polygon | Avalanche | BSC | Arbitrum | Base | Ethereum | Optimism |
|-----|------|---------|-----------|-----|----------|------|----------|----------|
| **Uniswap V2** | V2 | — | — | — | — | — | ✓ | — |
| **Uniswap V3** | V3 | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ |
| **QuickSwap V2** | V2 | ✓ | — | — | — | — | — | — |
| **QuickSwap V3** | V3 | ✓ | — | — | — | — | — | — |
| **SushiSwap** | V2 | ✓ | ✓ | ✓ | — | — | ✓ | ✓ |
| **PancakeSwap V2** | V2 | — | — | ✓ | — | — | — | — |
| **PancakeSwap V3** | V3 | — | — | ✓ | — | — | — | — |
| **Trader Joe** | V2 | — | ✓ | — | — | — | — | — |
| **Camelot** | V2 | — | — | — | ✓ | — | — | — |
| **Aerodrome** | V2 | — | — | — | — | ✓ | — | — |
| **ApeSwap** | V2 | ✓ | — | — | — | — | — | — |
| **DFYN** | V2 | ✓ | — | — | — | — | — | — |
| **Meshswap** | V2 | ✓ | — | — | — | — | — | — |
| **ShibaSwap** | V2 | — | — | — | — | — | ✓ | — |
| **Curve** | Stable/Crypto | — | — | — | — | — | ✓ | — |
| **Balancer V2** | Weighted | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| **Aave V3** | Lending | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

---

## 14. Configuration File Reference

A TOML config file can be used to set persistent defaults instead of passing CLI flags every time.

### Minimal Config

```toml
chain = "polygon"
rpc_url = "https://polygon-bor-rpc.publicnode.com"
```

### Full Config (all options)

```toml
# ── Chain & RPC ──────────────────────────────────────────────
chain = "ethereum"
rpc_url = "https://ethereum-rpc.publicnode.com"
rpc_urls = ["https://eth.merkle.io", "https://rpc.ankr.com/eth"]
rpc_rps = [1.0, 5.0, 3.0]
rpc_workers = 3
rps_limit = 500

# ── Strategies ────────────────────────────────────────────────
strategies = "all"
flash_loan_provider = "auto"
proximity_window = 3
cross_block_window = 5
max_pairs_per_token = 50

# ── Gas Model ────────────────────────────────────────────────
gas_model = "historical_exact"
gas_limit = 200000
priority_fee_gwei = 0.0

[gas_limits]
two_hop_arb = 150000
sandwich = 300000
multi_hop_arb = 250000
jit = 180000
liquidation = 500000

# ── Output ────────────────────────────────────────────────────
output = "table"
export_path = "./results"
db_path = "./cache/mev-scout.sqlite"
parquet_dir = "./parquet"

# ── Pricing ──────────────────────────────────────────────────
price_oracle_mode = "coingecko"
coingecko_api_key = "YOUR_COINGECKO_API_KEY"  # Optional, for higher rate limits

# ── PGA ────────────────────────────────────────────────────────
pga_enabled = true
pga_mean_competitors = 3.0
pga_intensity = 0.5

# ── Mempool ──────────────────────────────────────────────────
capture_pending = false

# ── Live Mode ─────────────────────────────────────────────────
initial_balance = 10.0
min_profit_threshold = 0.001
poll_interval_ms = 1000

# ── Per-Chain Overrides ──────────────────────────────────────
[chains.polygon]
chain_id = 137
balancer_vault = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
aave_v3_pool = "0x794a61358D6845594F94dc1DB02A252b5b4814aD"
wrapped_native_token = "0x0d500b1d8e8ef31e21c99d1db9a6444d3adf1270"
uniswap_v3_factories = ["0x1F98431c8aD98523631AE4a59f267346ea31F984", "0x08958a3a1324f4870eb0028f1e93b2e3d8d78e09"]
uniswap_v2_factories = ["0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32", "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"]
pool_discovery_start_block = 49100000
pool_discovery_batch_size = 200
curve_registry = "0x..."
uniswap_v2_default_fee = 30

[chains.polygon.subgraphs]
v2 = [{ url = "https://api.thegraph.com/subgraphs/name/ianlapham/uniswapv2", label = "Uniswap V2", fee = 30 }]
v3 = [{ url = "https://api.thegraph.com/subgraphs/name/ianlapham/uniswap-v3-polygon", label = "Uniswap V3" }]
balancer = [{ url = "https://api.thegraph.com/subgraphs/name/balancer-labs/balancer-polygon-v2", label = "Balancer V2" }]
curve = []

# ── Per-chain configs for other chains follow the same pattern ──
[chains.ethereum]
chain_id = 1
# ... (overrides for Ethereum-specific addresses)
```

---

## Quick Reference: Common Workflows

```bash
# 1. First-time setup — verify config
mev-scout config -v

# 2. Pool discovery for a new chain / range
mev-scout discover -n polygon -r <RPC> --from-block X --to-block Y

# 3. Pre-cache data
mev-scout fetch --days 7 -n polygon -r <RPC>

# 4. Run backtest
mev-scout run --days 7 -n polygon -r <RPC> --output json --fact-check

# 5. Re-render saved results in a different format
mev-scout report --run-id run_1712345678 --output csv

# 6. Fact-check a previous run
mev-scout fact-check run_1712345678 --re-verify

# 7. Debug a suspicious block
mev-scout replay --block 58000000 -n polygon -r <RPC> --analyze

# 8. Go live
mev-scout live -n polygon -r <RPC> --initial-balance 10 --min-profit 0.01
```

---

## Notes

- **RPC requirements:** An archive node RPC is strongly recommended for historical backtesting. Public RPCs work but are rate-limited (use `--rps-limit` and `--rpc-workers` conservatively).
- **Data freshness:** `--days` and `--blocks` resolve at runtime from chain tip. For reproducible results, use explicit `--from-block` / `--to-block` or `--block`.
- **Storage:** Block data is cached in SQLite (`./cache/mev-scout.sqlite` by default). Parquet output is optional and adds ZSTD-compressed columnar storage.
- **Live mode:** Press Ctrl+C to gracefully shut down. Logs are written to `live_<timestamp>.log` when `-v` is used.
- **Performance:** For large ranges, use multiple RPC providers via `--rpc-urls` and increase `--rpc-workers` (10-20 for private RPCs). The tool fetches blocks in parallel using concurrent workers.
