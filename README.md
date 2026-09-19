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

### 2. Run a backtest

```powershell
cargo run -p mev-scout-cli -- --config mev-scout.toml run --blocks 100
```

Other common commands:

```powershell
cargo run -p mev-scout-cli -- --config mev-scout.toml discover --source hybrid
cargo run -p mev-scout-cli -- --config mev-scout.toml live --loop
cargo run -p mev-scout-cli -- --config mev-scout.toml explorer index --days 7
cargo run -p mev-scout-cli -- --config mev-scout.toml explorer stats --since 7d
```

See `docs/ARCHITECTURE.md` for the full command map.

## Layout

| Path | What it is |
|---|---|
| `core/` | Library: pool state, quoting, detectors, cache & explorer stores, config |
| `cli/` | `mev-scout` binary — `run`, `live`, `discover`, `tokens`, `scan`, `report`, `explorer`, … |
| `cache/` | SQLite DBs (per-chain scanner cache + explorer stores), gitignored |

Pool discovery remotes are **GeckoTerminal + DexScreener** (plus on-chain
factory scans). DefiLlama yields is intentionally **not** a pool source
(UUID ids, no AMM pool contract addresses). Token metadata can be enriched
with `mev-scout tokens --enrich` (DefiLlama coins for symbol/decimals,
CoinGecko for name/icon URL).

## Configuration & secrets

Copy `mev-scout.example.toml` to `mev-scout.toml` and reference API keys
via `${ENV_VAR}` placeholders — never commit live keys. Print the fully
resolved config with `mev-scout config`. CLI flags such as `--rpc-urls`
override the TOML for a single invocation.

## Validation

```powershell
cargo test --workspace          # unit + integration
cargo clippy --workspace --all-targets -- -D warnings
```
