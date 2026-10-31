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
| `explorer` | Realized revenue report (bare); `index` for ingest | [§3.6](docs/CLI.md#36-explorer--realized-mev-forensics) |

`run` and `paper` were folded into `live`, which detects at chain tip and
always runs its paper ledger (`--initial-balance`, `--reserve`). At session
end it prints and persists cost, remaining balance, net P&L and a
per-strategy breakdown; `report` shows a run's ledger session alongside its
results. `--max-fills-per-block` moved to `[paper]` in the TOML.
Bare `explorer` is the revenue report; `explorer index` covers both historical
backfill (default trailing 7 days) and tip-follow (`--loop`).

Globals on every command: `-f/--config`, `--verbose`, `--quiet`. `live
--blocks` sets the one-shot window (default 64); `--loop` follows the tip.
`discover` takes optional `--blocks` / `--block` / `--from-block`/`--to-block`,
and `--incremental`. Source and enrich live under `[discover]` in TOML.
`explorer` takes `--windows` (`Nh`/`Nd`/`Nw` or `all`) / `--kind` / `--top`. `explorer index` takes
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
via `${ENV_VAR}` placeholders — never commit live keys. Machine-local run
configs (`mev-scout.avalanche-run.toml`, `hist.toml`) are gitignored for the
same reason. Print the fully resolved config with `mev-scout config`. Change
chain, RPC URLs, or `output` in the TOML (or pass `-f` to pick a different
file); those are not clap flags.

To avoid public / free-tier RPCs, run one small tip node for the chain you
are scanning (one chain per box — the disk budgets do not stack) and point
`rpc_urls` at localhost. The guides size that box for `live` and
`explorer index` over SSH, not for a validator or a public RPC:

| Chain | Guide | Local profile | Local RPC |
| --- | --- | --- | --- |
| Avalanche | [`docs/LOCAL_AVALANCHE.md`](docs/LOCAL_AVALANCHE.md) | [`mev-scout.local.toml`](mev-scout.local.toml) | `http://127.0.0.1:9650/ext/bc/C/rpc` |
| Polygon | [`docs/LOCAL_POLYGON.md`](docs/LOCAL_POLYGON.md) | [`mev-scout.local-polygon.toml`](mev-scout.local-polygon.toml) | `http://127.0.0.1:8545` |
| BSC | [`docs/LOCAL_BSC.md`](docs/LOCAL_BSC.md) | [`mev-scout.local-bsc.toml`](mev-scout.local-bsc.toml) | `http://127.0.0.1:8545` |

## Validation

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # unit + integration; #[ignore]d live tests are skipped,
                                # but some non-ignored lib tests still call
                                # GeckoTerminal/DexScreener and need live network
```

### CI

`.github/workflows/ci.yml` runs a subset of these gates on every push to `main`
and on every pull request — it does not run the full workspace suite:

| Job | Gates |
| --- | --- |
| `static` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p mev-scout-core --test explorer_golden` |
| `live` | `cargo test -p mev-scout-core --test explorer_corpus -- --ignored` and `cargo test -p mev-scout-core --test explorer_tag_coverage -- --ignored`, with `MEV_SCOUT_E2E=1`, `RPC_URL`, `MEV_SCOUT_COVERAGE=1`, `MEV_SCOUT_COVERAGE_CONFIG` and `MEV_SCOUT_COVERAGE_STRICT=1` |

The `live` job only runs when the repository variable `RPC_URL` is set; when it
is not, the job reports as skipped — never as a green check. It also requires
the coverage config: `MEV_SCOUT_COVERAGE_CONFIG` comes from the repository
variable `COVERAGE_CONFIG`, falling back to `mev-scout.coverage.toml`, which is
not committed. An opted-in live job that cannot find that file fails loudly
rather than passing as a skip.

Live-RPC tests are `#[ignore]`d, so a plain `cargo test` run skips them; select
them with `--ignored` and supply the environment each one names:

| Test | Environment |
| --- | --- |
| `explorer_corpus` | `MEV_SCOUT_E2E=1`, `RPC_URL` |
| `explorer_tag_coverage` | `MEV_SCOUT_COVERAGE=1`, `MEV_SCOUT_COVERAGE_CONFIG` (path to a coverage TOML), and `MEV_SCOUT_COVERAGE_STRICT=1` so incomplete tag coverage fails |

Once a test is selected, the gate fails loudly instead of skipping: a missing
endpoint or coverage config, and zero reachable chains, are failures, not
passes.
