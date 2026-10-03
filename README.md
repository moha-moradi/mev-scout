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

### 2. Quick start

With no arguments, `mev-scout` bootstraps what it needs and scans the most
recent 64 blocks once, then exits:

```powershell
cargo run -p mev-scout-cli
```

On first run it seeds the token cache from the bundled known-token list and
discovers pools on-chain over the chain's configured lookback window
(~1000 blocks), so there is no separate setup step. Both are skipped once the
caches are warm. Widen the window with `--blocks N`, or keep following the
chain with `--loop`.

### 3. Typical pipeline

Each step is a separate command. The first invocation shows the full
`cargo run` form; later steps use the short `mev-scout` form (same config
and CWD). Full flag coverage lives in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

```powershell
# Detect at the tip → opportunities + paper P&L (ledger is always on)
cargo run -p mev-scout-cli -- --config mev-scout.toml live

# Rebuild the pool universe explicitly (on-chain factory scan)
mev-scout discover --blocks 2000

# Re-render the latest run offline
mev-scout report

# Index realized MEV near tip, then summarize
mev-scout explorer index --duration 15m
mev-scout explorer backfill
```

### 4. Command index

| Command | Purpose | Details |
|---|---|---|
| *(none)* | Default: bootstrap if needed, scan the latest 64 blocks, exit | [§4.2](docs/ARCHITECTURE.md#42-live--the-default-command) |
| `config` | Print fully-resolved TOML | [§4.1](docs/ARCHITECTURE.md#41-config--print-resolved-toml) |
| `discover` | Find pools (on-chain factory scan) | [§4.3](docs/ARCHITECTURE.md#43-discover--build-the-pool-universe) |
| `live` | Detect at tip → opportunities + virtual P&L ledger | [§4.2](docs/ARCHITECTURE.md#42-live--the-default-command) |
| `report` | Re-render a recorded run from SQLite | [§4.5](docs/ARCHITECTURE.md#45-report--re-render-saved-results) |
| `explorer` | Realized-MEV forensics (`index`, `show`, `report`, `backfill`) | [§4.6](docs/ARCHITECTURE.md#46-explorer--realized-mev-forensics) |

`run` and `paper` were folded into `live`, which detects at chain tip and
always runs its paper ledger (`--initial-balance`, `--reserve`,
`--native-usd`). At session end it prints and persists cost, remaining
balance, net P&L and a per-strategy breakdown; `report` shows a run's ledger
session alongside its results. `--max-fills-per-block` moved to `[paper]` in
the TOML, and `explorer stats` is covered by `explorer report`.

`explorer validate` is a research command hidden behind a non-default Cargo
feature — build with `--features validate` to expose it.

Globals on every command: `-f/--config`, `--verbose`, `--quiet`. `live` takes
no range flags (it follows the tip); `discover`, `explorer index` and
`explorer backfill` take exactly one of `--days`, `--blocks`, `--block`, or
`--from-block/--to-block`.

## Layout

| Path | What it is |
|---|---|
| `core/` | Library: pool state, quoting, detectors, cache & explorer stores, config |
| `cli/` | `mev-scout` binary — `live`, `discover`, `report`, `explorer`, … |
| `cache/` | SQLite DBs (per-chain scanner cache + explorer stores), gitignored |

Pool discovery is on-chain only: DEX factory event logs are scanned over the
configured lookback window. Token metadata is seeded offline from the bundled
known-token list, so the default path needs no third-party HTTP service.

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
