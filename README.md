# mev-scout

MEV scanner and backtester for 7 EVM chains (polygon, avalanche, bsc,
arbitrum, base, ethereum, optimism), with an explorer/forensics layer
grounded in SQLite. Interface is the `mev-scout` CLI only.

## Prerequisites

| Tool | Version | Check |
|---|---|---|
| [Rust](https://rustup.rs/) (`rustc` + `cargo`) | stable | `cargo --version` |

All commands below assume you are in the **repo root**.

## Quick start

### 1. Create config

```powershell
copy mev-scout.example.toml mev-scout.toml
```

Edit `mev-scout.toml` and set your RPC endpoints. Prefer `${ENV_VAR}`
placeholders (see the example file), then export keys in the same shell
before starting anything:

```powershell
$env:ALCHEMY_API_KEY = "..."
$env:DRPC_KEY        = "..."
$env:GETBLOCK_KEY    = "..."
```

Never commit live API keys. Unset placeholders stay literal and fail
loudly at the provider.

Chain (`chain = "polygon"`), RPC URLs, strategies, and listing format
(`output = "table"|"csv"|"json"`) live in the TOML file — they are not CLI
flags. Inspect the fully resolved config with:

```powershell
cargo run -p mev-scout-cli -- --config mev-scout.toml config
```

### 2. Typical pipeline

Each step is a separate command. The first invocation shows the full
`cargo run` form; later steps use the short `mev-scout` form (same config
and CWD). Full flag coverage lives in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

```powershell
# Discover pools (hybrid = on-chain factories ∪ GeckoTerminal/DexScreener)
cargo run -p mev-scout-cli -- --config mev-scout.toml discover --source hybrid --days 30

# Enrich token metadata (DefiLlama coins + CoinGecko name/icon)
mev-scout tokens --enrich

# Backtest the last 100 blocks → opportunities in the explorer store
# (fetches and caches blocks as part of the run)
mev-scout run --blocks 100

# Re-render the latest run offline
mev-scout report

# Index realized MEV near tip, then summarize
mev-scout explorer index --duration 15m
mev-scout explorer stats --since 7d

# Virtual-fund P&L over a backtest (theoretical, no competition)
mev-scout paper run --blocks 100
mev-scout paper stats
```

### 3. Command index

| Command | Purpose | Details |
|---|---|---|
| `config` | Print fully-resolved TOML | [§4.1](docs/ARCHITECTURE.md#41-config--print-resolved-toml) |
| `discover` | Find pools (on-chain / remote / hybrid) | [§4.2](docs/ARCHITECTURE.md#42-discover--build-the-pool-universe) |
| `tokens` | Populate / view token metadata cache | [§4.3](docs/ARCHITECTURE.md#43-tokens--token-metadata-cache) |
| `run` | Full backtest → opportunities | [§4.4](docs/ARCHITECTURE.md#44-run--the-full-backtest) |
| `live` | Stream tip blocks and detect | [§4.5](docs/ARCHITECTURE.md#45-live--real-time-streaming-detection) |
| `report` | Re-render a recorded run from SQLite | [§4.6](docs/ARCHITECTURE.md#46-report--re-render-saved-results) |
| `explorer` | Realized-MEV forensics (`index`, `stats`, `show`, `report`, `backfill`, `validate`) | [§4.7](docs/ARCHITECTURE.md#47-explorer--realized-mev-forensics) |
| `paper` | Virtual-fund bot P&L (`run`, `live`, `sim`, `stats`) | [§4.8](docs/ARCHITECTURE.md#48-paper--virtual-fund-bot-pl) |

Globals on every command: `-f/--config`, `--verbose`, `--quiet`. Block-range
commands take exactly one of `--days`, `--blocks`, `--block`, or
`--from-block/--to-block`.

## Layout

| Path | What it is |
|---|---|
| `core/` | Library: pool state, quoting, detectors, cache & explorer stores, config |
| `cli/` | `mev-scout` binary — `run`, `live`, `discover`, `tokens`, `report`, `explorer`, `paper`, … |
| `cache/` | SQLite DBs (per-chain scanner cache + explorer stores), gitignored |

Pool discovery remotes are **GeckoTerminal + DexScreener** (plus on-chain
factory scans). DefiLlama yields is intentionally **not** a pool source
(UUID ids, no AMM pool contract addresses). Token metadata can be enriched
with `mev-scout tokens --enrich` (DefiLlama coins for symbol/decimals,
CoinGecko for name/icon URL).

## Configuration & secrets

Copy `mev-scout.example.toml` to `mev-scout.toml` and reference API keys
via `${ENV_VAR}` placeholders — never commit live keys. Print the fully
resolved config with `mev-scout config`. Change chain, RPC URLs, or
`output` in the TOML (or pass `-f` to pick a different file); those are
not clap flags.

## Validation

```powershell
cargo test --workspace          # unit + integration
cargo clippy --workspace --all-targets -- -D warnings
```
