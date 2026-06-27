# mev-scout Folder Structure Restructuring Plan

## Goals
- Clean Architecture layering in `core/`: **domain / app / infra / adapters**
- Thin binary: CLI owns its args and presentation; core is a pure library
- No behavior changes — pure structural refactor

---

## Phase 1: Extract CLI from Core + Split main.rs

### 1a. Move `core/src/cli.rs` → `cli/src/cli.rs` + `cli/src/args/`

| Source | Dest |
|---|---|
| `core/src/cli.rs` (Cli, Command) | `cli/src/cli.rs` |
| `RunArgs` | `cli/src/args/run.rs` |
| `FetchArgs` | `cli/src/args/fetch.rs` |
| `ReplayArgs` | `cli/src/args/replay.rs` |
| `DiscoverArgs` | `cli/src/args/discover.rs` |
| `FactCheckArgs` | `cli/src/args/fact_check.rs` |
| `ReportArgs` | `cli/src/args/report.rs` |
| `LiveArgs` | `cli/src/args/live.rs` |
| `BlockRangeArgs`, `ChainArgs` | `cli/src/args/mod.rs` (shared) |

**Changes:**
- Remove `pub mod cli` from `core/src/lib.rs`
- Remove `clap` dependency from `core/Cargo.toml`
- Update all `use mev_scout_core::cli::*` → `use crate::*` in CLI crate

### 1b. Split `cli/src/main.rs` (1520 lines)

| Concern | New file(s) |
|---|---|
| Entry point | `cli/src/main.rs` (~30 lines, parse + dispatch) |
| Logging setup | `cli/src/setup.rs` |
| Config merging | `cli/src/overrides.rs` (`build_overrides()`) |
| Table rendering | `cli/src/output/table.rs` |
| CSV rendering | `cli/src/output/csv.rs` |
| JSON rendering | `cli/src/output/json.rs` |
| Progress bars | `cli/src/output/progress.rs` |
| `cmd_run` | `cli/src/commands/run.rs` |
| `cmd_fetch` | `cli/src/commands/fetch.rs` |
| `cmd_replay` | `cli/src/commands/replay.rs` |
| `cmd_discover` | `cli/src/commands/discover.rs` |
| `cmd_factcheck` | `cli/src/commands/fact_check.rs` |
| `cmd_report` | `cli/src/commands/report.rs` |
| `cmd_config` | `cli/src/commands/config.rs` |
| `cmd_live` | `cli/src/commands/live.rs` |

---

## Phase 2: Introduce Layering in Core

### Target structure

```
core/src/
  lib.rs                 # Facade: selective re-exports
  config.rs              # Config, ChainConfig, CliOverrides (unchanged)

  domain/                # Pure business logic — no I/O, no external deps
    mod.rs
    types.rs             # ChainName, Strategy, GasConfig, etc.
    opportunity.rs       # MevOpportunity, ResultsFile
    mev/                 # Detection strategies (as-is)
    pool/                # DEX state & math (as-is)

  infra/                 # Infrastructure — I/O, external systems
    mod.rs
    rpc.rs
    cache.rs
    parquet.rs           # (was parquet_writer.rs)
    coingecko.rs
    resolver.rs
    gas_distribution.rs
    utils.rs

  adapters/              # Thin wrappers around external tools
    mod.rs
    replay.rs            # (was replay.rs, revm adapter)
    data.rs              # BlockData, TxData, ReceiptData
    scan.rs              # ActivityScanner

  app/                   # Use-case orchestration
    mod.rs
    backtest.rs          # (was run.rs)
    live_bot.rs          # (was live.rs)
    fact_check.rs
    aggregate.rs
    validation.rs
```

### File moves

| Current path | New path |
|---|---|
| `core/src/types.rs` | `core/src/domain/types.rs` |
| `core/src/mev/` | `core/src/domain/mev/` |
| `core/src/pool/` | `core/src/domain/pool/` |
| `core/src/rpc.rs` | `core/src/infra/rpc.rs` |
| `core/src/cache.rs` | `core/src/infra/cache.rs` |
| `core/src/parquet_writer.rs` | `core/src/infra/parquet.rs` |
| `core/src/coingecko.rs` | `core/src/infra/coingecko.rs` |
| `core/src/resolver.rs` | `core/src/infra/resolver.rs` |
| `core/src/gas_distribution.rs` | `core/src/infra/gas_distribution.rs` |
| `core/src/utils.rs` | `core/src/infra/utils.rs` |
| `core/src/replay.rs` | `core/src/adapters/replay.rs` |
| `core/src/data.rs` | `core/src/adapters/data.rs` |
| `core/src/scan.rs` | `core/src/adapters/scan.rs` |
| `core/src/run.rs` | `core/src/app/backtest.rs` |
| `core/src/live.rs` | `core/src/app/live_bot.rs` |
| `core/src/fact_check.rs` | `core/src/app/fact_check.rs` |
| `core/src/aggregate.rs` | `core/src/app/aggregate.rs` |
| `core/src/validation.rs` | `core/src/app/validation.rs` |
| `core/src/config.rs` | `core/src/config.rs` (unchanged) |

### Visibility in `lib.rs`

```rust
pub mod config;                          // Public
pub mod domain;                          // Re-exported publicly
pub use domain::{mev, pool, types};

#[doc(hidden)] pub mod app;             // Internal use
#[doc(hidden)] pub mod infra;
#[doc(hidden)] pub mod adapters;
```

---

## Phase 3: Reorganize Tests

### Break monolithic files into directories

| Before | After |
|---|---|
| `core/tests/e2e.rs` (548 lines) | `core/tests/e2e/mod.rs` + `rpc_connectivity.rs`, `fetch_and_cache.rs`, `pool_discovery.rs`, `detection.rs`, `persistence.rs`, `cache_isolation.rs` |
| `core/tests/integration.rs` (1563 lines) | `core/tests/integration/mod.rs` + `backtest.rs`, `detection.rs` |

### Add co-located unit tests

- `#[cfg(test)] mod tests { ... }` in key domain modules
- At minimum: `domain/mev/*.rs`, `domain/pool/math.rs`, `infra/gas_distribution.rs`

---

## Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Circular dependencies | Domain → nothing; App → Domain + Adapters; Infra → nothing. Enforce with `cargo check` |
| Cross-crate pub visibility | `#[doc(hidden)]` on internal modules keeps API surface clean |
| Large diff | Phase 1 and Phase 2 can be separate PRs |
| Breakage during moves | Move files one-by-one, run `cargo build` after each |
| `config.rs` depends on infra types | `config.rs` is pure data structs — keep at root |

---

## Timeline

| Phase | Effort |
|---|---|
| Phase 1 (CLI extraction) | ~2-3 hours |
| Phase 2 (layering) | ~3-4 hours |
| Phase 3 (tests) | ~1-2 hours |
| **Total** | **~6-9 hours** |
