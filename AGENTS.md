# AGENTS.md — mev-bot-backtest

## Build & test

```powershell
cargo build --release          # binary: target/release/mev-backtest.exe
cargo run --release -- --help  # run without installing
cargo test                     # all tests (unit + integration, no external services)
cargo clippy                   # lint
```

## Repo structure

- `mev-backtest-core/` — library: cache, fetch, replay (revm), rpc, types, validation, run orchestrator, mev detection, pool state
- `mev-backtest-cli/` — binary (`main.rs`): clap CLI that wires core modules together
- `pools/*.json` — pool registries per chain (Uniswap V2 pairs)
- `cache/` — sled embedded DB (gitignored; delete to reset)

## Commands

```
mev-backtest run --days 7                        # backtest last 7 days
mev-backtest run --block 50000000                # single block
mev-backtest fetch --blocks 1000                 # pre-cache (no strategy execution)
mev-backtest replay --block 50000000             # re-execute cached block, verify receipts
mev-backtest config                              # print resolved config TOML
```

Block range flags (exactly one): `--days N`, `--blocks N`, `--block N`, `--from-block N --to-block N`.
Default config file: `./mev-backtest.toml` (optional — built-in defaults cover all 7 chains).

## Key quirks

- **Only `two_hop_arb` is implemented.** Other strategies (`multi_hop_arb`, `jit`, `jit_arb`, `sandwich`) parse but detect nothing.
- **Pool registries are optional.** Without one, the engine logs a warning and runs no strategies.
- **Public RPCs** are the default (`*.publicnode.com`); they're rate-limited. Use `--parallelism 1` for large ranges.
- **`--fast-mode`** skips token-address widening (matches pool addresses only). Faster, but may miss state changes that affect token prices.
- **`fetch` before `replay`** — `replay` errors if the block isn't cached.
- **Tests use synthetic data** — no RPC or external services needed.

## Config precedence

Defaults ← `mev-backtest.toml` ← CLI flags (highest).

Per-chain addresses (Balancer Vault, Aave V3 Pool, Uniswap V3 Factory, Uniswap V2 factories) are hardcoded in `config.rs:162–271`. To add a new chain, add a `ChainConfig` entry there.

## Architecture

1. CLI parse → config load + merge → validate
2. Resolve block range (binary search for `--days`, chain tip for `--blocks`)
3. Load pool registry → init reserves via `eth_call getReserves()`
4. Per block: load data (cache or RPC) → filtered EVM replay via revm → update pool reserves from Swap/Sync logs → run TwoHopArbDetector
5. Report results (table/CSV/JSON)
