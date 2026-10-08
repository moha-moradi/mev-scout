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
discovers an explorer-like pool universe (`hybrid`: on-chain lookback plus
aggregator TVL ranking, default `min_tvl = 25000`). Both are skipped once the
caches are warm. Set `[discover].source = "onchain"` for a pure factory scan.
Widen the live window with `--blocks N`, or keep following the chain with
`--loop`.

### 3. Typical pipeline

Each step is a separate command. The first invocation shows the full
`cargo run` form; later steps use the short `mev-scout` form (same config
and CWD). Full flag coverage lives in
[`docs/CLI.md`](docs/CLI.md).

```powershell
# Detect at the tip → opportunities + paper P&L (ledger is always on)
cargo run -p mev-scout-cli -- --config mev-scout.toml live

# Rebuild the pool universe (hybrid TVL ranking by default)
mev-scout discover
mev-scout discover --blocks 2000

# Re-render the latest run offline
mev-scout report

# Index realized MEV (trailing 7 days), follow tip, then summarize
mev-scout explorer index
mev-scout explorer index --loop --duration 15m
mev-scout explorer
```

### 4. Command index

| Command | Purpose | Details |
|---|---|---|
| *(none)* | Default: bootstrap if needed, scan the latest 64 blocks, exit | [§3.2](docs/CLI.md#32-live--the-default-command) |
| `config` | Print fully-resolved TOML | [§3.1](docs/CLI.md#31-config--print-resolved-toml) |
| `discover` | Find pools (hybrid TVL ranking; `[discover].source` for factory-only) | [§3.3](docs/CLI.md#33-discover--build-the-pool-universe) |
| `live` | Detect at tip → opportunities + virtual P&L ledger | [§3.2](docs/CLI.md#32-live--the-default-command) |
| `report` | Re-render a recorded run from SQLite | [§3.5](docs/CLI.md#35-report--re-render-saved-results) |
| `explorer` | Realized revenue report (bare); `index` / `show` for ingest and detail | [§3.6](docs/CLI.md#36-explorer--realized-mev-forensics) |

`run` and `paper` were folded into `live`, which detects at chain tip and
always runs its paper ledger (`--initial-balance`, `--reserve`). At session
end it prints and persists cost, remaining balance, net P&L and a
per-strategy breakdown; `report` shows a run's ledger session alongside its
results. `--max-fills-per-block` moved to `[paper]` in the TOML.
Bare `explorer` is the revenue report; `explorer index` covers both historical
backfill (default trailing 7 days) and tip-follow (`--loop`).

`explorer validate` is a research command hidden behind a non-default Cargo
feature — build with `--features validate` to expose it.

Globals on every command: `-f/--config`, `--verbose`, `--quiet`. `live
--blocks` sets the one-shot window (default 64); `--loop` follows the tip.
`discover` takes optional `--blocks` / `--block` / `--from-block`/`--to-block`,
and `--incremental`. Source and enrich live under `[discover]` in TOML.
`explorer` takes `--windows` / `--kind` / `--top`. `explorer index` takes
`--days` or `--from-block`/`--to-block` (bounded), or `--loop [--duration]`.

## Layout

| Path | What it is |
|---|---|
| `core/` | Library: pool state, quoting, detectors, cache & explorer stores, config |
| `cli/` | `mev-scout` binary — `live`, `discover`, `report`, `explorer`, … |
| `cache/` | SQLite DBs (per-chain scanner cache + explorer stores), gitignored |

Pool discovery defaults to `hybrid`: factory events over the lookback window
merged with GeckoTerminal/DexScreener, then ranked by TVL (`min_tvl` /
`max_pools`). `[discover].source = "onchain"` disables aggregator HTTP. Token
metadata is seeded offline from the bundled known-token list.

## Configuration & secrets

Copy `mev-scout.example.toml` to `mev-scout.toml` and reference API keys
via `${ENV_VAR}` placeholders — never commit live keys. Print the fully
resolved config with `mev-scout config`. Change chain, RPC URLs, or
`output` in the TOML (or pass `-f` to pick a different file); those are
not clap flags.

To avoid public / free-tier RPCs, run AvalancheGo natively on Ubuntu and
point `[chains.avalanche.rpc]` at `http://127.0.0.1:9650/ext/bc/C/rpc`
(C-Chain WebSocket is also on the same node at
`ws://127.0.0.1:9650/ext/bc/C/ws`) — see
[`docs/LOCAL_AVALANCHE.md`](docs/LOCAL_AVALANCHE.md).

## Validation

```powershell
cargo test --workspace          # unit + integration
cargo clippy --workspace --all-targets -- -D warnings
```
