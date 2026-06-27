# MEV Scout — Folder Structure Improvement Plan

## Current State

**Workspace**: Rust monorepo with 2 crates (`core` library + `cli` binary)
**Source files**: 45 `.rs` files across the workspace
**Key technology**: revm (EVM), alloy (Ethereum SDK), clap, rusqlite, tokio

### Current directory tree

```
mev-scout/
├── Cargo.toml                  # workspace root
├── cli/
│   └── src/main.rs             # 1,381 lines — CLI dispatch + rendering + I/O
├── core/
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs              # 22 flat `pub mod` declarations
│   │   ├── aggregate.rs
│   │   ├── cache.rs
│   │   ├── cli.rs              # clap defs living in library crate
│   │   ├── coingecko.rs
│   │   ├── config.rs           # ~937 lines — Config + chain defaults + overrides
│   │   ├── data.rs
│   │   ├── fact_check.rs
│   │   ├── fetch.rs
│   │   ├── gas_distribution.rs
│   │   ├── live.rs
│   │   ├── parquet_writer.rs
│   │   ├── replay.rs           # ~1,176 lines — BlockReplayer + CachedRpcDb
│   │   ├── resolver.rs
│   │   ├── rpc.rs
│   │   ├── run.rs
│   │   ├── scan.rs
│   │   ├── types.rs            # ~835 lines — catch-all: ChainName, Strategy, GasConfig, API keys
│   │   ├── utils.rs
│   │   ├── validation.rs
│   │   ├── mev/                # 11 modules — all flat
│   │   │   ├── mod.rs
│   │   │   ├── block_builder.rs
│   │   │   ├── cross_block.rs
│   │   │   ├── jit.rs
│   │   │   ├── jit_arb.rs
│   │   │   ├── liquidation.rs
│   │   │   ├── mempool.rs
│   │   │   ├── multi_hop.rs
│   │   │   ├── opportunity.rs
│   │   │   ├── pga.rs
│   │   │   ├── sandwich.rs
│   │   │   └── two_hop.rs
│   │   └── pool/               # 9 modules — mixed concerns
│   │       ├── mod.rs
│   │       ├── balancer_math.rs
│   │       ├── curve_math.rs
│   │       ├── decoders.rs
│   │       ├── dex_type.rs
│   │       ├── discovery.rs
│   │       ├── math.rs
│   │       ├── state.rs        # 2,255 lines — largest file in project
│   │       ├── subgraph_discovery.rs
│   │       └── v3_quote.rs
│   └── tests/
│       ├── integration.rs      # 1,324 lines — monolithic
│       └── e2e.rs              # 492 lines
├── cache/
├── results/
└── target/
```

---

## File Size Hotspots (>800 lines)

| File | Lines | Issue |
|------|-------|-------|
| `pool/state.rs` | 2,255 | Massive — PoolManager + all pool state + event application |
| `cli/src/main.rs` | 1,381 | Too many responsibilities |
| `fact_check.rs` | 1,225 | Standalone, belongs in `mev/` domain |
| `replay.rs` | 1,176 | BlockReplayer + CachedRpcDb merged |
| `cache.rs` | 1,151 | Large but cohesive |
| `pool/v3_quote.rs` | 938 | V3 quoting engine |
| `config.rs` | 937 | Config + chain defaults merged |
| `rpc.rs` | 887 | RPC client + rate limiter + URL rotation merged |
| `types.rs` | 835 | Catch-all + hardcoded API keys |
| `mev/liquidation.rs` | 820 | Large but cohesive |

---

## Key Issues

| # | Problem | Location | Severity |
|---|---------|----------|----------|
| 1 | 22 flat modules in `core/src/` | `core/src/lib.rs` | Medium |
| 2 | `pool/state.rs` is 2,255 lines | `core/src/pool/state.rs` | **High** |
| 3 | `main.rs` does everything (dispatch, render, I/O) | `cli/src/main.rs` | High |
| 4 | Hardcoded Infura/Alchemy API keys in source | `core/src/types.rs` | **Security** |
| 5 | `cli.rs` (clap defs) lives in `core/` crate | `core/src/cli.rs` | Medium |
| 6 | No structured error types (all `anyhow`) | Throughout | Medium |
| 7 | `mev/` has 11 flat modules, no sub-grouping | `core/src/mev/` | Low-Medium |
| 8 | Barrel files inconsistent (`pool/` re-exports, `mev/` doesn't) | `pool/mod.rs`, `mev/mod.rs` | Low |
| 9 | `types.rs` is an 835-line catch-all | `core/src/types.rs` | Medium |
| 10 | Integration tests monolithic (1,324 lines) | `core/tests/integration.rs` | Low |

---

## Recommended Target Structure

```
core/src/
├── lib.rs
├── config/                          # was config.rs + validation.rs + cli.rs (CLI types moved)
│   ├── mod.rs
│   ├── settings.rs                  # Config struct, CliOverrides, merge logic
│   ├── defaults.rs                  # Chain defaults, API keys from env (NOT hardcoded)
│   └── validation.rs               # Config validation
├── types/                           # was types.rs + mev/opportunity.rs
│   ├── mod.rs
│   ├── chain.rs                     # ChainName enum + chain-specific constants
│   ├── strategy.rs                  # Strategy, GasConfig, FlashLoanProvider, etc.
│   └── opportunity.rs              # MevOpportunity, ResultsFile
├── rpc/                             # was rpc.rs
│   ├── mod.rs
│   ├── client.rs                    # RpcClient — multi-provider
│   └── middleware.rs               # Rate limiter, URL rotation
├── cache/                           # was cache.rs
│   ├── mod.rs
│   └── store.rs                     # SqliteStore
├── data/                            # was data.rs
│   ├── mod.rs
│   └── types.rs                     # BlockData, TxData, ReceiptData, LogData
├── fetch/                           # was fetch.rs + parquet_writer.rs
│   ├── mod.rs
│   ├── fetcher.rs                   # Fetcher
│   └── parquet.rs                   # ParquetWriter
├── replay/                          # was replay.rs
│   ├── mod.rs
│   ├── replayer.rs                  # BlockReplayer
│   └── db.rs                        # CachedRpcDb (revm Database trait)
├── pool/                            # restructured with state/ subdir + math/ subdir
│   ├── mod.rs
│   ├── state/                       # was pool/state.rs — split 2,255-line file
│   │   ├── mod.rs
│   │   ├── manager.rs               # PoolManager — orchestrator
│   │   ├── pool_types.rs            # PoolState enum + variant structs
│   │   ├── apply.rs                 # Event application (swap, mint, burn, sync)
│   │   └── factory.rs               # Pool creation from factory events
│   ├── math/                        # was math.rs, v3_quote.rs, curve_math.rs, balancer_math.rs
│   │   ├── mod.rs
│   │   ├── core.rs                  # quote_exact_in, TwoHopArbResult
│   │   ├── v3.rs                    # V3 tick quoting
│   │   ├── curve.rs                 # Curve AMM formulas
│   │   └── balancer.rs             # Balancer AMM formulas
│   ├── decoders.rs                  # Event log decoders
│   ├── discovery.rs                 # On-chain pool discovery
│   ├── subgraph_discovery.rs        # Subgraph-based discovery
│   └── dex_type.rs                  # DexType enum
├── mev/                             # restructured with sub-groups
│   ├── mod.rs
│   ├── detectors/                   # was 9 files at mev/ top level
│   │   ├── mod.rs
│   │   ├── two_hop.rs
│   │   ├── multi_hop.rs
│   │   ├── sandwich.rs
│   │   ├── jit.rs
│   │   ├── jit_arb.rs
│   │   ├── liquidation.rs
│   │   ├── cross_block.rs
│   │   ├── mempool.rs
│   │   └── pga.rs
│   ├── verify/                      # was fact_check.rs at top level
│   │   ├── mod.rs
│   │   └── fact_check.rs           # On-chain opportunity verification
│   └── execution/                   # was live.rs + block_builder.rs at top level
│       ├── mod.rs
│       ├── live.rs                  # LiveRunner
│       └── block_builder.rs        # Bundle packing
├── pipeline/                        # was run.rs + scan.rs + aggregate.rs + gas_distribution.rs
│   ├── mod.rs
│   ├── runner.rs                    # BacktestRunner
│   ├── scanner.rs                   # ActivityScanner
│   ├── aggregate.rs                 # USD aggregation + metrics
│   └── gas.rs                       # Gas price distribution / H10
├── coingecko.rs                     # stays — small, cohesive
├── resolver.rs                      # stays — small, cohesive
├── error/                           # NEW: structured error types
│   ├── mod.rs
│   ├── config.rs                    # ConfigError (was ValidationError)
│   ├── rpc.rs                       # RpcError
│   ├── replay.rs                    # ReplayError
│   └── cache.rs                     # CacheError, SqliteError
└── utils.rs                         # stays — small, single function

cli/src/                             # restructured
├── main.rs                          # ~50 lines — just entry + dispatch
├── cli.rs                           # moved from core/src/cli.rs
├── commands/                        # one file per subcommand
│   ├── mod.rs
│   ├── run.rs
│   ├── fetch.rs
│   ├── report.rs
│   ├── config.rs
│   ├── replay.rs
│   ├── discover.rs
│   ├── fact_check.rs
│   └── live.rs
├── display.rs                       # Table rendering, progress bars
└── overrides.rs                     # build_overrides() extracted + simplified

core/tests/                          # split by domain
├── mod.rs
├── common/
│   ├── mod.rs
│   └── setup.rs                     # Test helpers, mock data
├── arbitrage.rs                     # two-hop, multi-hop tests
├── sandwich.rs
├── liquidation.rs
├── replay.rs
├── config.rs
└── e2e.rs                           # already separate
```

---

## Detailed Rationale

### 1. Group flat top-level modules into domain directories

**Problem**: `core/src/` has 20 flat `.rs` files and 2 subdirectories — too much breadth in a single namespace. Hard to navigate.

**Solution**: Organize into domain directories with `mod.rs` barrel files.

**Benefit**:
- Reduces cognitive load — directory name tells you the domain
- Makes module boundaries explicit
- Follows Rust convention of grouping related functionality into directories
- Enables future crate splitting (e.g., `mev-scout-pipeline` as a separate crate)

### 2. Split `pool/state.rs` (2,255 lines)

**Problem**: One file contains `PoolManager`, all `PoolState` variants, pool update logic, pool creation, and event application.

**Solution**: Split into `state/manager.rs`, `state/pool_types.rs`, `state/apply.rs`, `state/factory.rs`.

**Impact**: ~500 lines per file. Makes event application changes independent of pool management.

### 3. Move `cli.rs` (clap definitions) from `core/` to `cli/` crate

**Problem**: `core/src/cli.rs` (282 lines) defines the CLAP argument structures in the library crate. The CLI crate imports its own CLI definitions from the library — an odd inversion.

**Solution**: Move `core/src/cli.rs` → `cli/src/cli.rs`. CLI crate imports from `crate::cli`. Core crate exports only the types needed (e.g., `CliOverrides` stays in `core/src/config/`).

### 4. Split `cli/src/main.rs` (1,381 lines)

**Problem**: `main.rs` does everything: dispatch 8 subcommands, render tables, manage files, build overrides.

**Solution**: One file per subcommand in `commands/`, plus `display.rs` for rendering and `overrides.rs` for config mapping.

### 5. Remove hardcoded API keys

**Problem**: `types.rs` contains hardcoded Infura and Alchemy keys.

**Solution**: Read API keys from environment variables at process start. Fall back to config file. Remove hardcoded keys entirely.

### 6. Extract structured error types

**Problem**: Nearly all functions return `anyhow::Result<T>`. Only `validation.rs` defines a dedicated error type.

**Solution**: Create `core/src/error/` with `ConfigError`, `RpcError`, `ReplayError`, `CacheError`.

**Benefit**: Callers can match on specific errors. Better error messages. Easier debugging.

### 7. Consistent barrel file patterns

**Problem**: `pool/mod.rs` re-exports types extensively; `mev/mod.rs` re-exports nothing.

**Solution**: Every directory `mod.rs` re-exports the **primary public API** of its submodules. Internal details remain at submodule path.

### 8. Split integration tests

**Problem**: `core/tests/integration.rs` is 1,324 lines — one monolithic test file.

**Solution**: Split by domain: `arbitrage.rs`, `sandwich.rs`, `liquidation.rs`, `replay.rs`, `config.rs`.

---

## File Migration Map

| Current Path | Target Path | Rationale |
|---|---|---|
| `core/src/config.rs` | `core/src/config/settings.rs` | Domain grouping |
| `core/src/validation.rs` | `core/src/config/validation.rs` | Config validation |
| `core/src/cli.rs` | `cli/src/cli.rs` | Co-location with binary |
| `core/src/types.rs` | `core/src/types/chain.rs` + `strategy.rs` | Split catch-all |
| `core/src/mev/opportunity.rs` | `core/src/types/opportunity.rs` | Types belong in `types/` |
| `core/src/data.rs` | `core/src/data/types.rs` | Domain grouping |
| `core/src/cache.rs` | `core/src/cache/store.rs` | Domain grouping |
| `core/src/rpc.rs` | `core/src/rpc/client.rs` + `middleware.rs` | Separate concerns |
| `core/src/replay.rs` | `core/src/replay/replayer.rs` + `db.rs` | Split responsibilities |
| `core/src/fetch.rs` | `core/src/fetch/fetcher.rs` | Domain grouping |
| `core/src/parquet_writer.rs` | `core/src/fetch/parquet.rs` | Related to fetch pipeline |
| `core/src/scan.rs` | `core/src/pipeline/scanner.rs` | Part of run pipeline |
| `core/src/run.rs` | `core/src/pipeline/runner.rs` | Part of run pipeline |
| `core/src/aggregate.rs` | `core/src/pipeline/aggregate.rs` | Part of run pipeline |
| `core/src/gas_distribution.rs` | `core/src/pipeline/gas.rs` | Part of run pipeline |
| `core/src/live.rs` | `core/src/mev/execution/live.rs` | Live mode is MEV execution |
| `core/src/fact_check.rs` | `core/src/mev/verify/fact_check.rs` | MEV verification |
| `core/src/pool/state.rs` | `core/src/pool/state/*.rs` | Split 2,255-line file |
| `core/src/pool/v3_quote.rs` | `core/src/pool/math/v3.rs` | Math belongs in `math/` |
| `core/src/pool/curve_math.rs` | `core/src/pool/math/curve.rs` | Math belongs in `math/` |
| `core/src/pool/balancer_math.rs` | `core/src/pool/math/balancer.rs` | Math belongs in `math/` |
| `core/src/pool/math.rs` | `core/src/pool/math/core.rs` | Math belongs in `math/` |
| `cli/src/main.rs` | `cli/src/main.rs` + `commands/*` + `display.rs` + `overrides.rs` | Split 1,381-line file |
| `core/tests/integration.rs` | `core/tests/*.rs` (split by domain) | Monolithic tests |

---

## Migration Strategy (Incremental Phases)

Each phase is self-contained, testable, and reversible if issues arise.

### Phase 1 — Low Risk, High Value
- Extract hardcoded API keys to environment variables
- Split `pool/state.rs` into `state/manager.rs`, `state/pool_types.rs`, `state/apply.rs`, `state/factory.rs`
- Update all imports

### Phase 2 — Structural Domain Grouping
- Create domain directories: `config/`, `types/`, `data/`, `rpc/`, `cache/`
- Move existing `.rs` files into their new directories
- Create `mod.rs` barrel files with appropriate re-exports
- Update all `use crate::` imports in all files

### Phase 3 — Pipeline Bundling
- Create `pipeline/` directory
- Move `run.rs`, `scan.rs`, `aggregate.rs`, `gas_distribution.rs` into it
- Update lib.rs and imports

### Phase 4 — MEV Restructure
- Create `detectors/`, `verify/`, `execution/` subdirectories under `mev/`
- Move `fact_check.rs` into `verify/`
- Move `live.rs`, `block_builder.rs` into `execution/`
- Update barrel files and imports

### Phase 5 — CLI Refactor
- Move `core/src/cli.rs` → `cli/src/cli.rs`
- Split `cli/src/main.rs` into command files
- Create `display.rs` and `overrides.rs`
- Extract and simplify `build_overrides()` with a builder pattern

### Phase 6 — Structured Errors (Optional)
- Create `core/src/error/` module
- Define `ConfigError`, `RpcError`, `ReplayError`, `CacheError`
- Migrate key functions from `anyhow::Result` to specific error types
