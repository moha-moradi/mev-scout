# MEV Bot Backtest Engine — User Guide

A high-fidelity historical backtesting engine for detecting Maximal Extractable Value (MEV) opportunities on EVM-compatible blockchains. Currently supports 7 chains with a focus on Polygon.

**Design philosophy:** No API-key-gated services. Relies entirely on public RPC endpoints (`*.publicnode.com`) and on-chain data. All block data is cached locally in an embedded `sled` database for fast replay.

---

## Table of Contents

1. [Installation](#installation)
2. [CLI Reference](#cli-reference)
3. [Configuration](#configuration)
4. [Architecture & Execution Flow](#architecture--execution-flow)
5. [Features](#features)
6. [Pool Registry](#pool-registry)
7. [Output Formats](#output-formats)
8. [Chains](#chains)
9. [Troubleshooting](#troubleshooting)

---

## Installation

### Prerequisites

- Rust toolchain (edition 2021, minimum Rust 1.75+)
- Access to an EVM archive node (public nodes from `publicnode.com` work, but rate limits apply)

### Build from source

```bash
# Clone the repository
git clone <repo-url>
cd mev-bot-backtest

# Build in release mode
cargo build --release

# The binary is at:
#   ./target/release/mev-backtest.exe   (Windows)
#   ./target/release/mev-backtest       (Linux/macOS)
```

Alternatively, run directly with cargo:

```bash
cargo run --release -- --help
```

---

## CLI Reference

### Global flags

| Flag | Description |
|------|-------------|
| `-f, --config <FILE>` | Path to TOML config file (default: `./mev-backtest.toml`) |
| `-v, --verbose` | Enable debug-level logging |
| `--quiet` | Suppress all output except the final summary |
| `--help` | Print help information |
| `--version` | Print version information |

### Subcommands

#### `run` — Execute the full backtest

```bash
mev-backtest run [OPTIONS]
```

**Block range (exactly one required):**

| Flag | Description |
|------|-------------|
| `--days <N>` | Last N days of blocks (1–365). Resolved via binary search on block timestamps. |
| `--blocks <N>` | Last N blocks from chain tip (≥1). |
| `--block <NUMBER>` | Single specific block (>0). |
| `--from-block <N> --to-block <N>` | Explicit inclusive range. |

**Chain & connection:**

| Flag | Default | Description |
|------|---------|-------------|
| `-n, --chain <NAME>` | `polygon` | Chain name. Supported: `polygon`, `avalanche`, `bsc`, `arbitrum`, `base`, `ethereum`, `optimism` |
| `-r, --rpc <URL>` | Public node | Archive node RPC URL. Overrides the chain's default public endpoint. |

**Flash loan:**

| Flag | Default | Description |
|------|---------|-------------|
| `--flash-loan-provider <PROVIDER>` | `auto` | Flash loan source. `auto` probes Balancer V2 → Aave V3 → Uniswap V3 (flash swap). Also accepts `balancer`, `aave`, `uniswap` to force a specific provider. |

**Strategies:**

| Flag | Default | Description |
|------|---------|-------------|
| `--strategies <LIST>` | `all` | Comma-separated strategy list or `all`. Available: `two_hop_arb`, `multi_hop_arb`, `jit`, `jit_arb`, `sandwich`. `two_hop_arb`, `multi_hop_arb`, and `jit` are implemented. |

**Gas model:**

| Flag | Default | Description |
|------|---------|-------------|
| `--gas-model <MODEL>` | `historical_exact` | `historical_exact` (use actual base fee from block), `p90`, or `fixed`. |
| `--priority-fee <GWEI>` | `0.0` | Priority fee premium in gwei, added on top of the base fee. |
| `--gas-limit <GAS>` | `200000` | Gas limit for arb transaction cost estimation. |

**Output:**

| Flag | Default | Description |
|------|---------|-------------|
| `--output <FORMAT>` | `table` | Output format: `table`, `csv`, `json`. |
| `--export-path <PATH>` | `./results` | Directory for CSV/JSON exports. |
| `--cache-dir <PATH>` | `./cache` | Block/state cache directory (sled database). |

**Example:**

```bash
# Backtest the last 7 days on Polygon with default settings
mev-backtest run --days 7

# Backtest a specific block range on BSC with JSON output
mev-backtest run --from-block 30000000 --to-block 30000100 -n bsc -r https://bsc.publicnode.com --output json

# Single-block backtest with custom gas settings
mev-backtest run --block 45000000 --gas-limit 300000 --priority-fee 2.0 --min-profit-usd 0.01
```

#### `fetch` — Pre-cache block data without running strategies

```bash
mev-backtest fetch [OPTIONS]
```

Shares the same block range flags as `run` (`--days`, `--blocks`, `--block`, `--from-block/--to-block`) plus `--chain`, `--rpc`, and `--parallelism`.

Useful for warming the cache before running strategies, or for separating the data-fetching phase from the analysis phase.

```bash
# Cache the last 1000 blocks on Polygon
mev-backtest fetch --blocks 1000
```

#### `replay` — Debug a specific block

```bash
mev-backtest replay --block <NUMBER> [OPTIONS]
```

Replays a single cached block through revm, comparing execution results against the stored receipts. Reports a per-transaction match rate.

| Flag | Description |
|------|-------------|
| `--block <NUMBER>` | (Required) Block number to replay. Must be cached first via `fetch`. |
| `--tx-index <INDEX>` | Replay up to this transaction index (default: all). |
| `-n, --chain <NAME>` | Chain name (default: `polygon`). |
| `-r, --rpc <URL>` | RPC URL. |
| `--cache-dir <PATH>` | Cache directory (default: `./cache`). |

```bash
# Cache a block first, then replay it
mev-backtest fetch --block 50000000
mev-backtest replay --block 50000000
```

#### `config` — Print resolved configuration

```bash
mev-backtest config
```

Prints the full resolved configuration as TOML after merging config file and CLI overrides. Useful for debugging what settings are active.

#### `report` — Re-render tables from saved JSON

```bash
mev-backtest report [OPTIONS]
```

Re-renders saved results from a previous `run` execution. By default loads the latest results file from the export directory.

| Flag | Default | Description |
|------|---------|-------------|
| `--run-id <ID>` | latest | Specific run ID to load (the filename without extension) |
| `--output <FORMAT>` | `table` | Output format: `table`, `csv`, `json`. |
| `--export-path <PATH>` | `./results` | Directory where result files are stored |

```bash
# Show the latest run results
mev-backtest report

# Show a specific run as CSV
mev-backtest report --run-id run_1712345678 --output csv
```

#### `discover` — On-chain pool discovery

```bash
mev-backtest discover [OPTIONS] --from-block <N> --to-block <N>
```

Scans factory contract events (`PairCreated` / `PoolCreated`) to discover liquidity pools directly from the chain.

| Flag | Default | Description |
|------|---------|-------------|
| `-n, --chain <NAME>` | `polygon` | Chain name |
| `-r, --rpc <URL>` | Public node | Archive node RPC URL |
| `--v2-factories <ADDRS>` | — | Comma-separated Uniswap V2 factory addresses |
| `--v3-factory <ADDR>` | — | Uniswap V3 factory address |
| `--from-block <N>` | (required) | Start block for scanning |
| `--to-block <N>` | (required) | End block for scanning |
| `--batch-size <N>` | `50000` | Block range per `eth_getLogs` request |
| `--save` | off | Save discovered pools to the sled cache |
| `--cache-dir <PATH>` | `./cache` | Cache directory (used with `--save`) |

```bash
# Discover V2 pools on Ethereum: QuickSwap factory, blocks 15M–16M
mev-backtest discover -n ethereum --v2-factories "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f" --from-block 15000000 --to-block 16000000

# Discover V2+V3 pools and save to cache
mev-backtest discover -n polygon \
  --v2-factories "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32,0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b" \
  --v3-factory "0x1F98431c8aD98523631AE4a59f267346ea31F984" \
  --from-block 50000000 --to-block 51000000 --save
```

---

## Examples

### Basic run on Polygon (last 100 blocks)
```bash
mev-backtest run --blocks 100 --chain polygon
```

### Run with custom gas settings
```bash
mev-backtest run --block 50000000 \
  --gas-limit 300000 \
  --priority-fee 2.0 \
  --gas-model p90
```

### Run with specific strategies
```bash
mev-backtest run --days 7 --strategies "two_hop_arb,multi_hop_arb"
```

### Run multi-hop arbitrage only (Polygon archive node)
```bash
mev-backtest run --blocks 1000 --chain polygon --strategies multi_hop_arb
```

### Fetch block data first, then run backtest
```bash
mev-backtest fetch --days 30 --chain polygon
mev-backtest run --days 30 --chain polygon
```

### Replay a specific block for debugging
```bash
mev-backtest replay --block 50000000 --chain polygon
```

### Report from saved JSON results
```bash
mev-backtest report
mev-backtest report --output csv
mev-backtest report --run-id run_1718000000
```

### Discover pools on a new chain
```bash
mev-backtest discover \
  --chain polygon \
  --v2-factories 0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32 \
  --from-block 0 --to-block 50000000 \
  --save
```

### Full TOML configuration
Create `mev-backtest.toml`:
```toml
chain = "polygon"
rpc_url = "https://polygon-rpc.com"
flash_loan_provider = "auto"
strategies = "all"
gas_model = "historical_exact"
gas_limit = 200000
priority_fee_gwei = 0.0
output = "table"
export_path = "./results"
cache_dir = "./cache"
```

---

## Configuration

The engine uses a three-layer configuration model:

```
Defaults  ←  TOML config file  ←  CLI flags (highest precedence)
```

### Config file (`mev-backtest.toml`)

```toml
# Chain (default: "polygon")
chain = "polygon"

# Optional RPC URL override (falls back to public node for the chain)
rpc_url = "https://polygon-bor.publicnode.com"

# Flash loan provider: auto, balancer, aave, uniswap
flash_loan_provider = "auto"

# Strategies: comma-separated or "all"
strategies = "all"

# Gas model: historical_exact, p90, fixed
gas_model = "historical_exact"

# Priority fee premium in gwei
priority_fee = 1.0

# Coinbase bribe percentage (0-100)
coinbase_bribe = 10

# Minimum profit in USD
min_profit_usd = 0.0

# Output format: table, csv, json
output = "table"

# Export directory
export_path = "./results"

# Cache directory (sled database)
cache_dir = "./cache"

# Fast mode (skip token widening in tx filter)
fast_mode = false

# Gas limit for arb cost estimation
gas_limit = 200000

# Per-chain configurations
[chains.polygon]
chain_id = 137
balancer_vault = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
aave_v3_pool = "0x794a61358D6845594F94dc1DB02A252b5b4814aD"
uniswap_v3_factory = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
pools_registry_path = "./pools/polygon.json"
uniswap_v2_factories = [
    "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32",  # QuickSwap
    "0x9e5A52f57b3038F1B8EeE45F28b3C1960e1fC6b",   # SushiSwap
]

# Start pool discovery from this block (inclusive). Comment out to disable.
# pool_discovery_start_block = 0

# Blocks per getLogs request during discovery (default: 50000)
# pool_discovery_batch_size = 100000
```

### Built-in chain defaults

Seven chains are preconfigured with chain IDs, contract addresses, factory addresses, and pool registry paths. You only need a config file if you want to override defaults.

---

## Architecture & Execution Flow


### Detailed execution walkthrough

1. **CLI Parsing** — `clap` parses the command and flags into a typed struct.
2. **Config Loading** — Reads `mev-backtest.toml` (or the path given by `-f`). Falls back to built-in defaults if the file doesn't exist.
3. **CLI Merge** — Every CLI flag overrides the corresponding config field.
4. **Validation** — Validates chain name, resolves strategies, checks range conflicts (e.g., `--days` and `--block` cannot be used together), parses gas model, output format, etc.
5. **Startup Plan** — Prints a summary table showing the resolved configuration before any work begins.
6. **Block Range Resolution**:
   - `--days N`: Binary searches block timestamps to find the block closest to N days ago.
   - `--blocks N`: Queries the chain tip and computes `tip - N + 1`.
   - `--block N` / `--from-block/to-block`: Direct integer ranges.
7. **Pool Initialization**:
   - Loads the pool registry JSON file for the target chain.
   - Calls `eth_call getReserves()` for each pool at `(start_block - 1)` to capture pre-backtest state.
   - Parallel fetches with a semaphore cap of 20 concurrent RPC calls.
8. **Per-Block Loop**:
   - Loads block header, transactions, and receipts from the sled cache (fetches from RPC if not cached).
   - Builds a revm execution context with `CachedRpcDb` — a lazy-loading database that checks cache first, then falls back to RPC.
   - **Transaction filter:** For each tx, checks if the `to` address or any log emitter address is a tracked pool or token. If not, skips EVM execution and builds the result directly from the cached receipt (fast path). In `--fast-mode`, only pool addresses are matched (tokens are skipped).
   - After each tx, processes Swap and Sync events to update pool reserves.
   - Runs `TwoHopArbDetector` and `MultiHopArbDetector` on the updated pool state.
   - Detected opportunities are collected per-block.
9. **Result Reporting** — Prints a table with columns: Block, Tx Index, Strategy, Profit (USD), Gas (USD), Net (USD). Can also export to CSV or JSON.

---

## Features

### Block Data Fetching & Caching

- Parallel block fetching using `tokio` with configurable concurrency.
- Progress bar display during fetch operations.
- Integrity verification — auto-detects and refetches missing blocks.
- All data stored in a sled embedded database: blocks, transactions, receipts, accounts, storage slots, contract code.
- Run manifests stored with each execution for traceability.

### EVM State Replay

- Full transaction re-execution using **revm** (Rust EVM).
- `CachedRpcDb` implements revm's `Database` trait with a lazy-fetch strategy: check sled cache → query RPC → cache result.
- Polygon fork support: BLS12-377 precompile addresses registered, state receiver system contract accounted for, spec ID selection by block number (Berlin / London / Cancun).
- **Filtered replay:** Transactions that don't interact with tracked pools or tokens skip EVM execution entirely, using the cached receipt directly. This dramatically speeds up backtests over large ranges.
- Receipt verification compares re-execution results against cached receipts (status, gas used, logs).

### Two-Hop Arbitrage Detection

Discovers arbitrage opportunities between pairs of Uniswap V2-style constant-product pools that share a common token.

- `PoolManager.arbitrage_pairs()` enumerates all `(pool_a, pool_b, shared_token)` tuples.
- For each pair, **both directions** are checked:
  - Direction 1: buy the shared token from pool A, sell to pool B.
  - Direction 2: buy the shared token from pool B, sell to pool A.
- **Optimal input search** via ternary search (80 iterations) over the concave profit function to find the input amount that maximizes profit.

### Multi-Hop Arbitrage Detection

Discovers N-pool (3+) arbitrage opportunities by enumerating pool paths through the token-pool graph.

- BFS-limited walk up to depth 4, seeded from existing arbitrage pairs in both directions.
- For each path, a composed quote function chains per-pool quoting through all pools.
- `optimal_n_hop_generic` ternary search finds the optimal input amount for any N-pool chain.
- Both Uniswap V2 (constant-product) and V3 (concentrated liquidity) pools are supported.
- Paths are stored in the optional `path` field on `MevOpportunity`.

### JIT Liquidity Detection

The JIT (Just-In-Time) liquidity detector identifies Uniswap V3 positions where an LP:
1. Mints concentrated liquidity in a specific tick range (Mint)
2. A swapper trades against this liquidity (Swap)
3. The LP removes the position (Burn)

This happens within the same block — the LP uses transaction ordering to capture swap fees without providing meaningful liquidity.

**Patterns detected:**
- **Full JIT** (Mint → Swap → Burn): Strong signal — LP deployed, captured fees, and removed
- **Partial JIT** (Mint → Swap): Moderate signal — liquidity deployed and traded against, but not yet removed

**Output fields:**
- `strategy`: `"jit"`
- `pool_a`: The V3 pool where JIT occurred
- `tick_lower`, `tick_upper`: The concentrated tick range
- `liquidity_amount`: Amount of liquidity deployed

Note: JIT detection is always active when running backtests. No separate CLI flag is needed — the detector runs alongside arbitrage detectors and emits opportunities when patterns are found.

**Current limitations:**
- Expected profit and gas cost are not estimated (set to 0 in v1)
- Only V3 concentrated liquidity pools are monitored
- Requires a complete block replay (not snapshot-based)

### Sandwich Detection

The sandwich detector identifies frontrunning attacks on Uniswap V2 pools where a searcher exploits a user's pending swap. The pattern spans three consecutive (or nearby) transactions interacting with the same pool:

1. **Frontrun (tx N):** The searcher buys/sells tokens on pool P, moving the price.
2. **Victim (tx N+1):** The user's swap executes on pool P at the worsened price.
3. **Backrun (tx N+2):** The searcher reverses their position on pool P at a profit.

All three transactions must interact with the same pool. The frontrun and backrun must come from the same EOA (the searcher).

**Pattern matched:**
- Same pool for all three transactions
- Frontrun and backrun share the same sender address
- Victim swaps in the same direction as the frontrun
- Backrun swaps in the opposite direction (reversal)
- Sliding window over swap records grouped by pool

**Output fields:**
- `strategy`: `"sandwich"`
- `pool_a`: The V2 pool where the sandwich occurred
- `tx_index`: Transaction index of the frontrun
- `victim_tx_index`: Transaction index of the victim
- `backrun_tx_index`: Transaction index of the backrun
- `token_in`, `token_out`: Tokens involved (resolved from pool metadata)

Note: Sandwich detection is always active when running backtests. No separate CLI flag is needed.

**Current limitations:**
- Only Uniswap V2 pools are monitored (V3 support planned)
- Expected profit and gas cost are not estimated (set to 0 in v1)
- Only detects strict consecutive triples on the same pool (no gap handling)
- Does not verify actual price impact — relies on direction matching

### Performance

MultiHopArb enumerates all pool paths up to depth 4. For Polygon (~100 pools), this evaluates ~1,600 paths per block, each running 80 iterations of ternary search. Expected overhead: 50–200ms per block.

To reduce detection time:
- Use `--strategies two_hop_arb` to skip multi-hop detection.
- Reduce path depth (hardcoded default: 4).
- Fewer pools = faster detection (use a slim pool registry).

### AMM Math

- Constant product formula: `x * y = k`
- `constant_product_output_amount` — output given input (with fee accounting).
- `constant_product_input_amount` — input required for desired output.
- `optimal_two_hop_arb` — ternary search for maximum-profit two-hop arbitrage.
- `simulate_two_hop` — computes intermediate and output amounts for a given input.

### Pool Management

- `PoolManager` maintains runtime state for all tracked pools.
- **Reserve initialization** via `eth_call getReserves()` at a reference block.
- **Swap event processing:** Decodes Uniswap V2 Swap events (`0xd78ad95fa...`) and applies reserve updates.
- **Sync event processing:** Decodes Sync events (`0x1c411e9a9...`) for direct reserve sync.
- Token index maintained for fast `arbitrage_pairs()` enumeration.

### USD Pricing

Two-tier pricing system:

1. **Hardcoded prices** — A static map of ~20 major Polygon tokens (WMATIC, USDC, USDT, WETH, WBTC, DAI, LINK, CRV, AAVE, FRAX, BAL, stMATIC, MaticX, GHST, QUICK, SUSHI, CAKE, TEL, agEUR, EURS) with their USD values.
2. **On-chain fallback** — For tokens not in the hardcoded map, derives USD price by finding a WMATIC pair in the pool manager and computing the exchange rate.

### Flash Loan Model

- Math-based simulation (no callback execution).
- `auto` mode probes providers in priority order: Balancer V2 → Aave V3 → Uniswap V3 (flash swap).
- Can be forced to a specific provider.

### Gas Cost Estimation

- `historical_exact` — uses the actual `base_fee_per_gas` from the block header.
- `p90` — uses the 90th percentile of recent block base fees (future feature).
- `fixed` — uses a constant gas price (future feature).
- Configurable priority fee (gwei) added on top of base fee.
- Configurable gas limit for arb transactions (default 200,000).
- Coinbase bribe model: configurable percentage of gross profit allocated as validator tip.
- Total gas cost = `gas_limit * (base_fee + priority_fee)` converted to USD.

### On-Chain Pool Discovery

- Scans Uniswap V2 factory contracts for `PairCreated` events and V3 factory for `PoolCreated` events.
- Configurable batch size (`pool_discovery_batch_size`, default 50,000 blocks per `eth_getLogs` request).
- Saves and resumes cursor position per factory (supports incremental discovery).
- Deduplicates against existing pools in the registry.
- Configured via `uniswap_v2_factories`, `uniswap_v3_factory`, and `pool_discovery_start_block` in chain config.
- Enable by setting `pool_discovery_start_block` in the chain config. On `run`, the engine scans from that block (or the last saved cursor) to `start_block - 1` before initializing pool reserves.
- Standalone CLI command: `mev-backtest discover` for ad-hoc discovery with optional `--save` to cache.
- Discovered pools persist in the sled cache and are loaded on subsequent runs alongside the registry JSON files.

### Multi-Chain Support

| Chain | Chain ID | Public RPC |
|-------|----------|------------|
| Polygon | 137 | `https://polygon-bor.publicnode.com` |
| Avalanche | 43114 | `https://avalanche-c-chain.publicnode.com` |
| BSC | 56 | `https://bsc.publicnode.com` |
| Arbitrum | 42161 | `https://arbitrum-one.publicnode.com` |
| Base | 8453 | `https://base.publicnode.com` |
| Ethereum | 1 | `https://ethereum-rpc.publicnode.com` |
| Optimism | 10 | `https://optimism-rpc.publicnode.com` |

Each chain comes preconfigured with Balancer Vault, Aave V3 Pool, Uniswap V3 Factory addresses, and Uniswap V2 factory addresses for pool discovery.

---

## Pool Registry

Pool registries are JSON files listing the liquidity pools to track during the backtest.

### File location

- Polygon: `./pools/polygon.json`
- Other chains: Configured via `pools_registry_path` in the per-chain config.

### JSON format

```json
[
  {
    "address": "0xa1c57f48f0db89d34f9e8c4e7a8c5f5c5d5e5f5a",
    "type": "uniswap_v2",
    "token0": "0x0d500b1d8e8ef31e21c99d1db9a6444d3adf1270",
    "token1": "0x2791bca1f2de4661ed88a30c99a7a9449aa84174",
    "fee": 30,
    "name": "QuickSwap WMATIC/USDC"
  }
]
```

| Field | Type | Description |
|-------|------|-------------|
| `address` | hex string | Pool contract address (checksummed) |
| `type` | string | Pool type (currently only `"uniswap_v2"`) |
| `token0` | hex string | First token address |
| `token1` | hex string | Second token address |
| `fee` | integer | Fee in basis points (e.g., 30 = 0.3%) |
| `name` | string (optional) | Human-readable pool name |

You can add custom pools by editing or creating new registry JSON files. Pools that are not in the registry will not have their reserves tracked or be considered for arbitrage detection.

---

## Output Formats

### Table (default)

A terminal-formatted table printed via `comfy-table`:

```
 Block    | Tx | Strategy    | Profit (USD) | Gas (USD) | Net (USD)
----------|----|-------------|--------------|-----------|-----------
 50000000 | 42 | two_hop_arb | 12.5431      | 0.8923    | 11.65
```

Negative net profit values are shown in parentheses.

### CSV

Exported to `--export-path` (default `./results/`). Each row contains the same fields as the table.

### JSON

Exported to `--export-path`. Each opportunity includes:
- `block_number`, `tx_index`
- `strategy`
- `expected_profit_usd`, `gas_cost_usd`, `net_profit_usd`
- Pool addresses, token addresses, amounts
- Timestamp

---

## Troubleshooting

### Public node rate limits

Public RPC endpoints (`*.publicnode.com`) are free but rate-limited. For large backtests:

1. Use `--parallelism 1` to reduce concurrent requests.
2. Pre-cache data with `fetch` before running strategies, so the strategy phase reads from the local cache.
3. Consider using a private archive node for production workloads.

### Cache directory

- The cache is stored in `./cache` by default as a sled database.
- If the cache becomes corrupted or you want a fresh start, delete the cache directory.
- Cache is keyed by chain ID, so switching chains creates separate cache namespaces.

### Missing pool registry

If no pool registry file is found for the target chain, the engine logs a warning and skips arbitrage detection. Create a pool registry JSON or enable on-chain pool discovery.

### Receipt verification failures

If `replay` shows a low receipt match rate (below 99%):

1. Verify the RPC endpoint is an archive node (must support `eth_getProof`, `eth_getStorageAt`, etc.).
2. Check that the correct chain is selected.
3. The block range must be fully cached before replaying.

### `mev-backtest.toml` not found

If the config file doesn't exist, the engine uses built-in defaults. This is not an error. Create the file only if you need custom overrides.

### Strategies not detected

`two_hop_arb`, `multi_hop_arb`, and `jit` are implemented. Other strategies (`jit_arb`, `sandwich`) are parsed and accepted but produce no opportunities. Selecting `"all"` is safe and runs both `two_hop_arb` and `multi_hop_arb` detection.
