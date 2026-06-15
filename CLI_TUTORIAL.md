# MEV Scout — Complete CLI Tutorial

A practical guide to using `mev-scout` for MEV opportunity backtesting on EVM chains.

---

## Table of Contents

1. [Building & First Steps](#1-building--first-steps)
2. [Configuration](#2-configuration)
3. [`mev-scout run` — The Main Backtest Command](#3-mev-scout-run--the-main-backtest-command)
4. [`mev-scout fetch` — Pre-Caching Block Data](#4-mev-scout-fetch--pre-caching-block-data)
5. [`mev-scout report` — Re-rendering Saved Results](#5-mev-scout-report--re-rendering-saved-results)
6. [`mev-scout replay` — Debugging Blocks](#6-mev-scout-replay--debugging-blocks)
7. [`mev-scout fact-check` — Verifying Backtest Results](#7-mev-scout-fact-check--verifying-backtest-results)
8. [`mev-scout discover` — On-Chain Pool Discovery](#8-mev-scout-discover--on-chain-pool-discovery)
9. [`mev-scout config` — Inspecting Resolved Configuration](#9-mev-scout-config--inspecting-resolved-configuration)
10. [Common Workflows](#10-common-workflows)
11. [Tips & Best Practices](#11-tips--best-practices)

---

## 1. Building & First Steps

### Build from source

```bash
cargo build --release
```

The binary is at `./target/release/mev-scout` (or `mev-scout.exe` on Windows).

Run directly with cargo:

```bash
cargo run --release -- --help
```

### Check the version

```bash
mev-scout --version
```

### Global flags available on every command

| Flag | Purpose |
|------|---------|
| `-f, --config <FILE>` | Path to TOML config file (default: `mev-scout.toml`) |
| `-v, --verbose` | Enable debug-level logging |
| `--quiet` | Suppress all output except final summary |
| `--help` | Print help |
| `--version` | Print version |

### List all subcommands

```bash
mev-scout --help
```

---

## 2. Configuration

MEV Scout uses a three-layer configuration model:

```
Built-in defaults  ←  TOML config file  ←  CLI flags (highest priority)
```

You **don't need a config file** to get started — sensible defaults exist for all 7 supported
chains. A config file is only needed to override defaults (e.g., custom RPC, contract addresses).

### Minimal config file (`mev-scout.toml`)

```toml
chain = "polygon"
rpc_url = "https://polygon-mainnet.g.alchemy.com/v2/YOUR_KEY"
strategies = "two_hop_arb,multi_hop_arb"
output = "json"
```

### Full example

See `mev-scout.example.toml` in the repo root for all available options, including per-chain
contract addresses (`balancer_vault`, `aave_v3_pool`, `uniswap_v3_factory`), factory addresses
for pool discovery, gas limits per strategy, and CoinGecko API key for USD pricing.

### Built-in chain defaults

Seven chains are pre-configured out of the box:

| Chain | Chain ID | Public RPC |
|-------|----------|------------|
| polygon | 137 | `https://polygon-bor.publicnode.com` |
| avalanche | 43114 | `https://avalanche-c-chain.publicnode.com` |
| bsc | 56 | `https://bsc.publicnode.com` |
| arbitrum | 42161 | `https://arbitrum-one.publicnode.com` |
| base | 8453 | `https://base.publicnode.com` |
| ethereum | 1 | `https://ethereum-rpc.publicnode.com` |
| optimism | 10 | `https://optimism-rpc.publicnode.com` |

Each chain comes with Balancer Vault, Aave V3 Pool, Uniswap V3 Factory, and V2 factory
addresses pre-populated.

---

## 3. `mev-scout run` — The Main Backtest Command

This is the primary command. It fetches block data, replays transactions,
detects MEV opportunities, and prints/saves the results.

### Syntax

```bash
mev-scout run [OPTIONS]
```

### Block range (exactly one required)

You must specify one of these mutually exclusive options:

| Flag | Example | Description |
|------|---------|-------------|
| `--days <N>` | `--days 7` | Last N days (1–365). Uses binary search on block timestamps. |
| `--blocks <N>` | `--blocks 1000` | Last N blocks from chain tip. |
| `--block <N>` | `--block 50000000` | Single specific block. |
| `--from-block <N> --to-block <N>` | `--from-block 50000000 --to-block 50000100` | Explicit inclusive range. |

### Chain & connection

| Flag | Default | Description |
|------|---------|-------------|
| `-n, --chain <NAME>` | `polygon` | One of: `polygon`, `avalanche`, `bsc`, `arbitrum`, `base`, `ethereum`, `optimism` |
| `-r, --rpc <URL>` | public node | Archive node RPC endpoint |

### Strategy selection

| Flag | Default | Description |
|------|---------|-------------|
| `--strategies <LIST>` | `all` | Comma-separated or `all`. Options: `two_hop_arb`, `multi_hop_arb`, `jit`, `jit_arb`, `sandwich` |

### Flash loan

| Flag | Default | Description |
|------|---------|-------------|
| `--flash-loan-provider <P>` | `auto` | `auto`, `balancer`, `aave`, `uniswap` |

### Gas model

| Flag | Default | Description |
|------|---------|-------------|
| `--gas-model <M>` | `historical_exact` | `historical_exact`, `p90`, `fixed` |
| `--gas-limit <GAS>` | `200000` | Gas limit for arb tx cost estimation |
| `--priority-fee <GWEI>` | `0.0` | Priority fee premium in gwei |

### Output

| Flag | Default | Description |
|------|---------|-------------|
| `--output <F>` | `table` | `table`, `csv`, `json` |
| `--export-path <PATH>` | `./results` | Directory for saved JSON/CSV files |
| `--cache-dir <PATH>` | `./cache` | Directory for block/state sled cache |
| `--fact-check` | off | Print detailed fact-check report after the run and save as `{run_id}_factcheck.json` |

### Examples

```bash
# Basic: scan last 100 blocks on Polygon (default chain)
mev-scout run --blocks 100

# Single block with debug logging
mev-scout run --block 50000000 -v

# Last 7 days on BSC with custom RPC
mev-scout run --days 7 -n bsc -r https://bsc.publicnode.com

# Specific range, sandwich detection only, JSON output
mev-scout run --from-block 30000000 --to-block 30000100 \
  --strategies sandwich \
  --output json \
  --export-path ./my_results

# Run with fact-check report
mev-scout run --blocks 500 --fact-check

# Ethereum with custom gas settings
mev-scout run --blocks 500 -n ethereum \
  --gas-limit 300000 \
  --priority-fee 2.0 \
  --gas-model p90

# Arbitrum, multi-hop arb only, quiet mode
mev-scout run --days 1 -n arbitrum --strategies multi_hop_arb --quiet

# Two specific strategies, force flash loan provider
mev-scout run --blocks 1000 \
  --strategies "two_hop_arb,sandwich" \
  --flash-loan-provider aave

# Full options on Base chain
mev-scout run --block 10000000 \
  -n base \
  -r https://base.own-rpc.com \
  --strategies "jit,jit_arb" \
  --gas-model fixed \
  --gas-limit 350000 \
  --priority-fee 1.5 \
  --output csv \
  --export-path ./base_results \
  --cache-dir ./base_cache
```

### What happens during `run`

1. Resolves block range (e.g., converts `--days 7` to actual block numbers)
2. Prints a startup plan with chain, RPC, strategies, gas model
3. Checks RPC connection
4. Opens/creates the sled cache database
5. If `pool_discovery_start_block` is configured, scans for new pools
6. Initializes pool manager (loads pool registry, queries on-chain reserves)
7. For each block in the range:
   - Loads block/txs/receipts (from cache or RPC)
   - Replays transactions through revm
   - Processes swap/sync events to update pool state
   - Runs all enabled MEV detectors
8. Saves results to JSON (with timestamp as run ID)
9. Prints results table

---

## 4. `mev-scout fetch` — Pre-Caching Block Data

Downloads and caches block data **without** running any MEV strategies.
Useful for warming the cache before a large backtest, or for separating
the data fetching from analysis.

### Syntax

```bash
mev-scout fetch [OPTIONS]
```

Accepts the same block range flags as `run` (`--days`, `--blocks`, `--block`,
`--from-block`/`--to-block`) plus `--chain` and `--rpc`.

### Examples

```bash
# Cache the last 5000 blocks on Polygon
mev-scout fetch --blocks 5000

# Cache a specific range on Ethereum
mev-scout fetch --from-block 18000000 --to-block 18001000 -n ethereum

# Cache last 30 days on BSC with custom RPC
mev-scout fetch --days 30 -n bsc -r https://bsc.my-rpc.com

# Cache a single block (for later replay)
mev-scout fetch --block 50000000

# Fetch then run (two-step workflow)
mev-scout fetch --days 7
mev-scout run --days 7
```

The fetch command shows a progress bar and reports:
- Total blocks requested
- Blocks fetched from RPC vs loaded from cache
- Any missing blocks that were auto-refetched

---

## 5. `mev-scout report` — Re-rendering Saved Results

After a `run` saves results to JSON, use `report` to re-display them in
any output format without re-running the backtest.

### Syntax

```bash
mev-scout report [OPTIONS]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--run-id <ID>` | (latest) | Specific run ID (the filename stem, e.g. `run_1718000000`) |
| `--output <F>` | `table` | `table`, `csv`, `json` |
| `--export-path <PATH>` | `./results` | Directory where result JSON files are stored |

### Examples

```bash
# Show latest results as table
mev-scout report

# Show a specific run as CSV
mev-scout report --run-id run_1718000000 --output csv

# Export a specific run as JSON to stdout
mev-scout report --run-id run_1718000000 --output json

# List results from a custom export path
mev-scout report --export-path ./my_results
```

---

## 6. `mev-scout replay` — Debugging Blocks

Re-executes a single cached block through revm and compares the execution
results against the stored receipts. Reports a match rate per transaction.

⚠️ **The block must be cached first** — run `mev-scout fetch --block <N>` before replaying.

### Syntax

```bash
mev-scout replay --block <NUMBER> [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--block <N>` | **(Required)** Block number to replay (must be cached) |
| `--tx-index <I>` | Replay up to this tx index (default: all) |
| `-n, --chain <NAME>` | Chain name (default: `polygon`) |
| `-r, --rpc <URL>` | RPC URL |
| `--cache-dir <PATH>` | Cache directory (default: `./cache`) |
| `--analyze` | Show DEX interaction analysis per transaction (pools must be discovered first) |

### Examples

```bash
# Cache a block, then replay it
mev-scout fetch --block 50000000
mev-scout replay --block 50000000

# Replay first 10 transactions only
mev-scout replay --block 50000000 --tx-index 10

# Replay a block on Ethereum with custom cache
mev-scout replay --block 18000000 -n ethereum --cache-dir ./eth_cache

# Replay with DEX interaction analysis (requires discovered pools in cache)
mev-scout replay --block 50000000 --analyze
```

With `--analyze`, each transaction shows its DEX interactions:
```
  idx  tx_hash                                                           status  gas_used  receipt
  ────  ────────────────────────────────────────────────────────────────  ──────  ────────  ────────
  0    0xabcd...                                                          ok      142000    ✓
         DEX interactions:
           ├ QuickSwap V3 USDC/WETH — Swap
           └ SushiSwap V2 WMATIC/USDC — Sync
  1    0xef01...                                                          ok      21000     ✓
         (no DEX interactions)
```

The output shows per-transaction:
- Index and hash
- Status (ok/fail)
- Gas used
- Receipt match (checkmark or cross)

A summary at the end shows the overall receipt match rate. Rates below 99%
may indicate RPC issues or incorrect chain selection.

---

## 7. `mev-scout fact-check` — Verifying Backtest Results

Loads a saved `run_*.json` results file and re-verifies each detected opportunity.
Reports which opportunities pass sanity checks (profit > gas cost, required fields present).

### Syntax

```bash
mev-scout fact-check <RUN_ID> [OPTIONS]
```

| Argument | Description |
|----------|-------------|
| `RUN_ID` | **(Required)** Run ID to verify (e.g. `run_1718000000`) |

| Flag | Description |
|------|-------------|
| `--re-verify` | Re-load block data from cache and re-verify pool state (requires cached blocks) |
| `--export-path <PATH>` | Directory where result JSON files are stored (default: `./results`) |

### Examples

```bash
# Fact-check a specific run
mev-scout fact-check run_1718000000

# Fact-check with re-verification (loads block data from cache)
mev-scout fact-check run_1718000000 --re-verify

# Fact-check results from a custom directory
mev-scout fact-check run_1718000000 --export-path ./my_results
```

### What gets checked

| Check | Description |
|-------|-------------|
| `profit_gt_gas` | Expected profit exceeds estimated gas cost |
| Sandwich fields | `victim_tx_index` and `backrun_tx_index` are present for sandwich strategies |
| JIT fields | `tick_lower`, `tick_upper`, `liquidity_amount` are present for JIT strategies |

The report is displayed as a table and saved as `{run_id}_factcheck.json`.

---

## 8. `mev-scout discover` — On-Chain Pool Discovery

Scans factory contracts for `PairCreated` (V2) / `PoolCreated` (V3) events
to discover liquidity pools directly from the chain.

### Syntax

```bash
mev-scout discover --from-block <N> --to-block <N> [OPTIONS]
```

| Flag | Default | Description |
|------|---------|-------------|
| `-n, --chain <NAME>` | `polygon` | Chain name |
| `-r, --rpc <URL>` | public node | RPC URL |
| `--v2-factories <ADDRS>` | config defaults | Comma-separated V2 factory addresses (optional — falls back to config) |
| `--v3-factory <ADDR>` | config defaults | V3 factory address (optional — falls back to config) |
| `--from-block <N>` | **(required)** | Start block (inclusive) |
| `--to-block <N>` | **(required)** | End block (inclusive) |
| `--batch-size <N>` | `10` | Blocks per `eth_getLogs` request |
| `--save` | off | Save discovered pools to sled cache |
| `--cache-dir <PATH>` | `./cache` | Cache directory (used with `--save`) |

> If neither `--v2-factories` nor `--v3-factory` is provided, the command falls back to the chain's default or config-file factory addresses. At least one factory address must be available from either source.

### Examples

```bash
# Discover all known V2+V3 pools on Polygon (uses config defaults)
mev-scout discover -n polygon --from-block 0 --to-block 50000000

# Discover V2 pools from a specific factory on Polygon
mev-scout discover \
  -n polygon \
  --v2-factories "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32" \
  --from-block 0 --to-block 50000000

# Discover V2+V3 pools and save to cache
mev-scout discover \
  -n polygon \
  --v2-factories "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32,0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b" \
  --v3-factory "0x1F98431c8aD98523631AE4a59f267346ea31F984" \
  --from-block 50000000 --to-block 51000000 \
  --save

# Discover Uniswap V2 pools on Ethereum
mev-scout discover \
  -n ethereum \
  --v2-factories "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f" \
  --from-block 15000000 --to-block 16000000 \
  --batch-size 1000

# Discover V3 pools on Arbitrum
mev-scout discover \
  -n arbitrum \
  --v3-factory "0x1F98431c8aD98523631AE4a59f267346ea31F984" \
  --from-block 100000000 --to-block 101000000 \
  --save --cache-dir ./arb_cache
```

V2 output format:
```
V2  0xabc...  token0=0x...  token1=0x...
```

V3 output format:
```
V3  0xdef...  token0=0x...  token1=0x...  fee=3000  tickSpacing=60
```

---

## 9. `mev-scout config` — Inspecting Resolved Configuration

Prints the fully resolved configuration (defaults + config file overrides
+ CLI overrides) as TOML to stdout. Useful for debugging what settings
are actually active.

```bash
mev-scout config
mev-scout -f my-config.toml config
mev-scout -v config
```

Sample output:
```toml
chain = "polygon"
flash_loan_provider = "auto"
strategies = "all"
gas_model = "historical_exact"
gas_limit = 200000
priority_fee_gwei = 0.0
output = "table"
export_path = "./results"
cache_dir = "./cache"

[chains.polygon]
chain_id = 137
balancer_vault = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
aave_v3_pool = "0x794a61358D6845594F94dc1DB02A252b5b4814aD"
uniswap_v3_factory = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
uniswap_v2_factories = [
    "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32",
    "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b",
]
wrapped_native_token = "0x0d500b1d8e8ef31e21c99d1db9a6444d3adf1270"
pool_discovery_start_block = 0

# ... chains.ethereum, chains.bsc, etc.
```

---

## 10. Common Workflows

### First-time user: scan last 100 blocks on Polygon

```bash
mev-scout run --blocks 100
```

This uses the public Polygon RPC — no API key required.

### Full workflow: fetch → run → report

```bash
# Step 1: Cache the data (no strategies)
mev-scout fetch --days 7 -n polygon

# Step 2: Run backtest (reads from cache, no RPC needed for blocks)
mev-scout run --days 7 -n polygon

# Step 3: Later, re-display results in different formats
mev-scout report --output csv
mev-scout report --output json
```

### Large backtest on Ethereum

```bash
# Cache first (large ranges = lots of RPC calls)
mev-scout fetch --days 30 -n ethereum -r https://eth.my-archive-node.com

# Then run — faster since most data is cached
mev-scout run --days 30 -n ethereum -r https://eth.my-archive-node.com \
  --strategies two_hop_arb \
  --output json \
  --export-path ./eth_results
```

### Debug a suspicious block

```bash
mev-scout fetch --block 50000000
mev-scout replay --block 50000000 --tx-index 5
```

### Compare two chains

```bash
mev-scout run --blocks 500 -n polygon --output json --export-path ./poly
mev-scout run --blocks 500 -n arbitrum --output json --export-path ./arb
```

### Using a config file for daily runs

Create `mev-scout.toml`:
```toml
chain = "polygon"
rpc_url = "https://polygon.my-rpc.com"
strategies = "two_hop_arb,multi_hop_arb"
flash_loan_provider = "auto"
gas_model = "historical_exact"
gas_limit = 200000
priority_fee = 1.0
output = "table"
export_path = "./results"
cache_dir = "./cache"

[gas_limits]
two_hop_arb = 150000
multi_hop_arb = 350000
```

Then simply:
```bash
mev-scout run --days 1
```

Override any setting on the fly:
```bash
mev-scout run --days 7 --strategies sandwich
```

### Discover pools and use them in a backtest

```bash
# Discover pools and save to cache
mev-scout discover \
  -n polygon \
  --v2-factories "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32" \
  --from-block 0 --to-block 50000000 \
  --save

# Run backtest — pool manager loads cached pools automatically
mev-scout run --blocks 1000
```

---

## 11. Tips & Best Practices

### Public node rate limits

Public RPCs (`*.publicnode.com`) are rate-limited. For large ranges:

1. Use `fetch` first to cache data, then `run` on the same range
2. Provide your own archive node via `-r` or `rpc_url` in config
3. Consider using `--parallelism` (if available in your version)

### Cache management

- Cache is stored in `./cache` by default (sled embedded database)
- Cache is keyed by **chain ID**, so switching chains creates separate namespaces
- Delete the cache directory to start fresh
- Use `--cache-dir` to point to different locations for different chains/projects

### Strategy selection for speed

| Strategy | Rel. Speed | Description |
|----------|-----------|-------------|
| `two_hop_arb` | Fastest | Only checks 2-pool arbitrage pairs |
| `sandwich` | Fast | Sliding window over swap records |
| `jit` | Moderate | Monitors V3 mint/swap/burn patterns |
| `jit_arb` | Moderate | JIT + cross-pool arbitrage |
| `multi_hop_arb` | Slowest | BFS path enumeration up to depth 4 |

For quick tests, use `--strategies two_hop_arb`. For full coverage, use `all`.

### Gas model guide

| Model | Description |
|-------|-------------|
| `historical_exact` | Uses actual `base_fee_per_gas` from each block. Most accurate. |
| `p90` | Uses base_fee × 1.5 (90th percentile approximation). More conservative. |
| `fixed` | Only uses `--priority-fee`. Ignores base fee. Useful for what-if analysis. |

### Output formats

| Format | Use case |
|--------|----------|
| `table` | Interactive terminal use (default) |
| `csv` | Import into spreadsheets or data analysis |
| `json` | Programmatic processing, long-term storage |

### Config file precedence

1. Built-in defaults (always present)
2. `mev-scout.toml` (or `-f <path>`) — overrides defaults
3. CLI flags — override everything

Run `mev-scout config` to see the final merged configuration.

### Pool discovery vs registry

Two ways to provide pool data:
- **Pool registry JSON files** — static files listing pools (default path `./pools/<chain>.json`)
- **On-chain discovery** — `mev-scout discover` scans factory events and stores in cache

Both sources are loaded and merged at runtime.

---

## Quick Reference Card

```
mev-scout run --days <N>                       Last N days
              --blocks <N>                     Last N blocks
              --block <N>                      Single block
              --from-block <A> --to-block <B>  Inclusive range
              -n <chain>                       7 chains supported
              -r <url>                         Custom RPC
              --strategies <list>              Comma-sep or "all"
              --flash-loan-provider <p>        auto/balancer/aave/uniswap
              --gas-model <m>                  historical_exact/p90/fixed
              --gas-limit <N>                  Arb tx gas limit
              --priority-fee <gwei>            Priority fee premium
              --output <f>                     table/csv/json
              --export-path <dir>              Results directory
              --cache-dir <dir>                Cache directory

mev-scout fetch --days/--blocks/--block/--from-block+--to-block
                -n <chain> -r <url>

mev-scout report --run-id <id> --output <f> --export-path <dir>

mev-scout replay --block <N> --tx-index <I> -n <chain> -r <url> --cache-dir <dir>
                --analyze                     DEX interaction analysis per tx

mev-scout fact-check <RUN_ID>                 Verify saved results
                --re-verify                   Re-verify pool state from cache

mev-scout discover --v2-factories <addrs> --v3-factory <addr>
                   --from-block <A> --to-block <B>
                   --save --batch-size <N> -n <chain> -r <url>

mev-scout config                              Print resolved TOML
mev-scout --help                              Show all commands
```
