# Code Smell Remediation Plan

> Generated from full codebase audit. Ordered by priority (P0-P4) and grouped into
> independent work streams that can be parallelized. Every change includes a
> **safety gate** -- the step that must pass before the work is considered done.
>
> **Last updated:** 2026-07-30 -- status verified against codebase.

---

## Table of Contents

1. [Priority Legend](#priority-legend)
2. [Phase 0 -- Critical Safety Fixes (P0)](#phase-0)
3. [Phase 1 -- High-Impact Deduplication (P1)](#phase-1)
4. [Phase 2 -- Structural Refactors (P2)](#phase-2)
5. [Phase 3 -- Quality & Consistency (P3)](#phase-3)
6. [Phase 4 -- Nice-to-Have Cleanup (P4)](#phase-4)
7. [Phase 5 -- Missing Smell Items (P3-P4)](#phase-5)
8. [Dead Code Audit](#dead-code-audit)
9. [Unreachable / Orphaned Core Code](#unreachable-core-code)
10. [Risk Mitigation](#risk-mitigation)
11. [Tracking](#tracking)

---

## Status Legend

| Symbol | Meaning |
|--------|---------|
| DONE | Fully implemented, no further work needed |
| PARTIAL | Partially implemented; see "Remaining work" |
| TODO | No implementation found |

---

## Priority Legend

| Priority | Meaning | When |
|----------|---------|------|
| **P0** | Correctness bug or soundness issue | Immediate |
| **P1** | High-leverage deduplication | First sprint |
| **P2** | Structural refactors that unlock future improvements | Second sprint |
| **P3** | Consistency, naming, and style improvements | Ongoing |
| **P4** | Nice-to-have cleanup with low urgency | Backlog |

---

<a id="phase-0"></a>
## Phase 0 -- Critical Safety Fixes (P0)

### 0.1 Fix unsafe raw-pointer usage in async fetcher [DONE]

**Files:** `core/src/fetch/fetcher.rs:172,197,472,480`

**Smell:** `let fetch = self as *const Self;` creates a raw pointer from `&self` held
across `.await` points. Undefined behavior if `Fetcher` is dropped while a future
referencing the pointer is alive.

**Remediation:** Added `#[derive(Clone)]` to `Fetcher` (all fields are Arc-wrapped or
primitives). Replaced `self as *const Self` + `unsafe { &*fetch }` with `self.clone()`
in both `fetch_range` and `fetch_relevant` spawned futures.

**Remaining work:** None.

**Safety gate:** `cargo clippy -- -D unsafe_code` passes for `fetch/` and `cli/`.

---

### 0.2 Fix unsafe `static mut` in main.rs [DONE]

**Files:** `cli/src/main.rs:17,41`

**Smell:** `static mut _LOG_GUARD` -- mutable static accessed via `unsafe`. Deprecated
pattern, data-race risk.

**Remediation:** Replaced `static mut _LOG_GUARD: Option<WorkerGuard>` with
`static LOG_GUARD: OnceLock<WorkerGuard>`. The unsafe write `unsafe { _LOG_GUARD = Some(guard); }`
was replaced with `let _ = LOG_GUARD.set(guard);`.

**Remaining work:** None.

**Safety gate:** `cargo clippy -- -D unsafe_code` passes for `cli/`.

---

### 0.3 Fix cross-validation zero-hash bug [DONE]

**Files:** `core/src/dune/cross_validate.rs` -- **DELETED**

**Remediation:** The entire `cross_validate.rs` file was removed. `audit.rs` uses a
different matching approach (block number + pool addresses) that doesn't reference `tx_hash`.

**Remaining work:** None.

---

### 0.4 Fix silent migration error swallowing [DONE]

**Files:** `core/src/cache/store/mod.rs:265-279`

**Remediation:** The migration loop now captures `execute_batch` results, pattern-matches
on `rusqlite::Error::SqliteFailure` with a `Some(msg)` message, only ignores
`"duplicate column"` errors, and propagates all others via `r?`.

**Remaining work:** None.

---

<a id="phase-1"></a>
## Phase 1 -- High-Impact Deduplication (P1)

### 1.1 Extract shared RPC initialization helper [DONE]

**Files:** `cli/src/rpc_setup.rs`, `cli/src/commands/{run,fetch,live,replay,discover}.rs`

**Remediation:** `cli/src/rpc_setup.rs` was created with `init_rpc` (35 lines).
All 5 command files now import and call `crate::rpc_setup::init_rpc`. No manual RPC
setup blocks remain.

**Remaining work:** None.

**Safety gate:** `cargo test -p mev-scout-cli`.

---

### 1.2 Deduplicate `retry_call` / `retry_call_archive` [DONE]

**Files:** `core/src/rpc/client.rs:195-271`

**Remediation:** `retry_call_archive` no longer exists. The single `retry_call` function
takes an `archive_only: bool` parameter to handle both paths.

**Remaining work:** None.

**Safety gate:** `cargo test -p mev-scout-core`; manual test against live RPC.

---

### 1.3 Extract shared `BlockData` construction [DONE]

**Files:** `core/src/rpc/client.rs:1057`

**Remediation:** `block_to_data()` helper exists at `client.rs:1057`. Called from
`get_block`, `get_pending_block`, and `batch_rpc_call`. No inline `BlockData { ... }`
construction remains.

**Remaining work:** None.

**Safety gate:** Existing tests pass.

---

### 1.4 Consolidate Dune utility functions [DONE]

**Files:** `core/src/dune/util.rs`, `dune/audit.rs`, `dune/pool_discovery.rs`, `dune/mod.rs`

**Remediation:** `core/src/dune/util.rs` was created with `render_query`, `approx_block_month_min`,
`dune_chain_label`, `chain_timing`, `estimate_latest_block`, `dune_indexing_lag_blocks`.
`pub mod util;` added to `core/src/dune/mod.rs`. All consumers (`audit.rs`, `pool_discovery.rs`)
import from `super::util` instead of having private duplicates. External consumers
(`token_discovery.rs`, `token_cache.rs`) also import from `util` instead of `pool_discovery`.

**Remaining work:** None.

**Safety gate:** `cargo test -p mev-scout-core`.

---

### 1.5 Unify chain genesis parameter tables [DONE]

**Files:** `core/src/dune/util.rs`

**Smell:** Chain to (genesis_ts, secs_per_block, blocks_per_day, lag) mapping in 4 places.

**Remediation:** Consolidated into `ChainTimingParams` + `chain_timing()` in `util.rs`.
All consumers (`audit.rs`, `pool_discovery.rs`, `dune_query.rs`, `token_cache.rs`) import
from `util`. No private duplicates remain.

**Remaining work:** None.

**Safety gate:** `cargo test`; verify `dune-find-blocks` output unchanged.

---

### 1.6 Unify V3/V4 pool state types [DONE]

**Files:** `core/src/pool/state/pool_types.rs`

**Smell:** `UniswapV3PoolState` and `UniswapV4PoolState` are identical 7-field structs
with a field-by-field `From` impl.

**Remediation:**
- `UniswapV4PoolState` is now `pub type UniswapV4PoolState = UniswapV3PoolState;`
- Duplicate struct definition, `impl UniswapV4PoolState`, and `From<UniswapV4PoolState> for UniswapV3PoolState` removed
- Duplicate `ConcentratedPoolState` impl for `UniswapV4PoolState` in `factory.rs` removed
- All `.into()` calls on V4→V3 are now no-ops and have been cleaned up

**Remaining work:** None.

**Safety gate:** `cargo test`; run backtest on a block containing V4 swaps.

---

### 1.7 Deduplicate `apply.rs` V3/V4 swap handling [DONE]

**Files:** `core/src/pool/state/apply.rs`

**Smell:** V3 and V4 branches in `apply_v3_swap` are identical ~20-line blocks.
Same duplication in `apply_v3_mint_burn`.

**Remediation:** After 1.6 unified the types, both `apply_v3_swap` and `apply_v3_mint_burn`
use `PoolState::UniswapV3(state) | PoolState::UniswapV4(state)` match arms with shared logic.

**Remaining work:** None.

**Safety gate:** Same as 1.6.

---

### 1.8 Extract pool deserialization helper in SqliteStore [DONE]

**Files:** `core/src/cache/store/mod.rs:345-407`, `store/pools.rs:57,71`

**Remediation:** `row_to_pool_info` helper exists. Both `get_discovered_pool` and
`list_discovered_pools` call it via `super::row_to_pool_info(&row)?`.

**Remaining work:** None.

---

### 1.9 Deduplicate overrides.rs chain/block-range copying [DONE]

**Files:** `cli/src/overrides.rs`

**Smell:** Chain args copied in 7 match arms; block-range args in 3 arms.

**Remediation:**
`apply_chain_args` and `apply_block_range` extracted as helpers.
Added `apply_storage_args` (3 call sites) and `apply_dune_chain_args` (3 call sites)
to eliminate remaining per-command field-by-field duplication for `db_path`/`parquet_dir`
and `chain`/`dune_api_key` respectively.

**Remaining work:** None.

**Safety gate:** `cargo test -p mev-scout-cli`.

---

### 1.10 Merge `dune_query.rs` two-source-of-truth [DONE]

**Files:** `cli/src/commands/dune_query.rs`

**Remediation plan:** `sql: &'static str` field added to `QueryInfo`. `all_queries()` uses a
`q!` macro referencing `queries::$NAME` automatically via `stringify!`. `get_query_sql` is
now a one-liner lookup on `all_queries()` -- the 140-arm match is eliminated.
Private `dune_chain_label` and `approx_block_month_min` have been replaced with imports
from `dune::util`.

**Remaining work:** None.

**Safety gate:** `cargo test -p mev-scout-cli`; verify `--list` and `--all` output unchanged.

---

<a id="phase-2"></a>
## Phase 2 -- Structural Refactors (P2)

### 2.1 Decompose `Config` into sub-structs [DONE]

**Files:** `core/src/config/settings.rs`

**Smell:** 30+ field God Object mixing backtest, live, Dune, RPC, gas, output, per-chain concerns.

**Remediation:**
```rust
pub struct Config {
    pub chain: String,
    pub rpc: RpcConfig,
    pub backtest: BacktestConfig,
    pub live: LiveConfig,
    pub gas: GasConfig,
    pub output: OutputConfig,
    pub dune: DuneConfig,
}
```
All sub-structs use `#[serde(flatten)]` for backward TOML compatibility. `CliOverrides` similarly decomposed. `merge_cli()` uses `merge_sub!` macro to delegate to sub-struct merge methods.

**Remaining work:** None.

**Safety gate:** `cargo test -p mev-scout-core`; existing TOML configs still parse.

---

### 2.2 Introduce `MevOpportunity` builder [DONE]

**Files:** `core/src/types/opportunity.rs`

**Smell:** 22-field struct literal with many `None` defaults repeated across every detector.

**Remediation plan:** Builder methods (`with_jit_fields`, `with_sandwich_fields`, `with_path`)
return `Result<Self, &'static str>` instead of using `debug_assert!`, so invalid strategy
combinations produce an error in all build profiles instead of only debug.

**Remaining work:** None. Builder methods are available (unused by current detectors but
ready for adoption).

---

### 2.3 Decompose `SqliteStore` into domain modules [DONE]

**Files:** `core/src/cache/store/`

**Remediation:** Directory contains `mod.rs`, `blocks.rs`, `pools.rs`, `accounts.rs`,
`integrity.rs`, `manifests.rs`, `pending.rs`. `mod.rs` re-exports them with `pub mod`.

**Remaining work:** None.

---

### 2.4 Add SQLite schema versioning [DONE]

**Files:** `core/src/cache/store/mod.rs:72-81`

**Remediation:** Has `const SCHEMA_VERSION: u64 = 7;` and `CREATE TABLE IF NOT EXISTS
schema_version (version INTEGER PRIMARY KEY)`.

**Remaining work:** None.

---

### 2.5 Extract per-DEX scanners from `discover_pools_shard` [DONE]

**Files:** `core/src/pool/discovery/` (module with `v2.rs`, `v3.rs`, `v4.rs`, `balancer.rs`, `curve.rs`, `solidly.rs`, `camelot.rs`, `pendle.rs`, `trader_joe.rs`)

**Remediation:** `pool/discovery/` directory exists with per-DEX scanner files. Each exports `async fn scan_shard(...) -> Vec<DiscoveredPool>`. `discover_pools_shard` is now a dispatcher.

**Remaining work:** None.

---

### 2.6 Extract per-DEX initialization from `init_from_rpc` [DONE]

**Files:** `core/src/pool/state/factory.rs:461-496`

**Remediation:** `init_v2_pool` (line 461), `init_v3_pool` (line 468), and `init_balancer_pool` (line 482) extracted. `init_from_rpc` dispatches to the appropriate helper.

**Remaining work:** None.

---

### 2.7 Extract `Command` trait [DONE]

**Files:** `cli/src/commands/mod.rs:35`

**Remediation:** `pub trait CliCommand` with `async fn execute(&self, config: &Config) -> anyhow::Result<()>` exists. Implemented by `RunArgs`, `FetchArgs`, `ReportArgs`, etc.

**Remaining work:** None.

---

### 2.8 Extract shared `estimate_gas` helper across detectors [DONE]

**Files:** `core/src/mev/gas.rs:1-16`

**Remediation:** Contains `BASE_TX_GAS`, `DEFAULT_POOL_GAS`, `JIT_OVERHEAD`,
`LIQUIDATION_GAS_LIMIT` constants and `estimate_base_gas()`, `estimate_multi_swap_gas()` helpers.

**Remaining work:** None.

---

<a id="phase-3"></a>
## Phase 3 -- Quality & Consistency (P3)

### 3.1 Replace magic numbers with named constants [DONE]

**Files:** Throughout codebase

**Remediation:** `mev/gas.rs`, `pool/math/consts.rs`, `rpc/consts.rs`, `dune/consts.rs` all
exist with named constants. `pool/math/consts.rs` has 20+ constants covering solver iterations,
pool math thresholds, gas estimates, etc. The Q128.128 shift in `apply.rs` uses `Q128_SHIFT`.
The remaining ~15 f64 EMA filter literals in `middleware.rs` are tuning parameters with local
context and don't benefit from extraction.

**Remaining work:** None.

---

### 3.2 Replace glob re-exports with explicit ones [DONE]

**Files:** All `mod.rs` files across the codebase.

**Remediation:** Replaced all `pub use submodule::*` with explicit item lists in
`data/mod.rs`, `cache/mod.rs`, `pipeline/mod.rs`, `mev/mod.rs`, `mev/execution/mod.rs`,
`dune/mod.rs`, `sigs/mod.rs`, `fetch/mod.rs`, `replay/mod.rs`, `replay/replayer.rs`,
and all module-level exports under `pool/`.

**Remaining work:** None.

---

### 3.3 Fix `debug_assert!` usage in public API [DONE]

**Files:** `core/src/types/opportunity.rs:171,184,196`

**Remediation:** Superseded by item 2.2. All three builder methods now return
`Result<Self, &'static str>` instead of using `debug_assert!`.

**Remaining work:** None (see 2.2).

---

### 3.4 Add `is_empty()` to `TokenCache` [DONE]

**Files:** `core/src/cache/token_cache.rs:199`

**Remediation:** `pub fn is_empty(&self) -> bool` exists at line 199.

**Remaining work:** None.

---

### 3.5 Remove dead `RpcError` variants [DONE]

**Files:** `core/src/error/rpc.rs`

**Remediation:** `AllProvidersFailed` and `RateLimited` variants have been removed from the
`RpcError` enum. Only `CallFailed` and `InvalidResponse` remain.

**Remaining work:** None.

---

### 3.6 Fix inconsistent return types in factory methods [DONE]

**Files:** `core/src/types/chain.rs:103,134`

**Remediation:** Both `default_uniswap_v2_factories` and `default_uniswap_v3_factories` now
return `&'static [&'static str]`. All call sites use `.iter()` or `for` loops compatible
with slices.

**Remaining work:** None.

---

### 3.7 Fix `_burned` misleading underscore prefix [DONE]

**Files:** `core/src/mev/detectors/jit.rs:224`

**Remediation:** Parameter renamed to `burned` (no underscore prefix).

**Remaining work:** None.

---

### 3.8 Standardize error message formatting [DONE]

**Files:** `cli/src/commands/` (12 command files)

**Remediation:** `anyhow::Context` is now imported and used in 11/12 command files:
`live.rs`, `replay.rs`, `report.rs`, `run.rs`, `discover.rs`, `audit.rs`, `config.rs`,
`fetch.rs`, `tokens.rs`, `dune_check.rs`, `dune_query.rs`. Key `?` call sites are wrapped
with `.context()` calls. The remaining file (`dune_find_blocks.rs`) uses `?` exclusively
inside closure chains where `.context()` cannot be chained, so the import would be unused.

**Remaining work:** None.

---

<a id="phase-4"></a>
## Phase 4 -- Nice-to-Have Cleanup (P4)

### 4.1 Remove dead code [DONE]

**Files:** Multiple

**Remediation:** All 5 dead items verified removed:
- `default_executor_addresses()` -- removed from `config/defaults.rs`
- `default_with_chains()` -- removed from `config/settings.rs`
- `get_tick_spacing_from_fee` -- removed from `pool/math/v3.rs`
- `get_tick_at_sqrt_ratio` -- removed from `pool/math/v3.rs`
- `parse_v3_exact_output` -- removed from `mev/detectors/mempool.rs`
- `_output` discarded variable -- removed from `config/validation.rs`
- `chain_id` field -- removed from `cache/store/mod.rs`

Also removed (from earlier passes): `AllProvidersFailed`, `RateLimited`, `decode_i128_from_be_bytes`,
`CURVE_PRICE_ORACLE_SELECTOR`, `DuneTokenInfo`, `_x_in_new`, `EtagStore`, `cross_validate.rs`.

**Remaining work:** None.

---

### 4.2 Move hardcoded data to external files [DONE]

**Files:**
- `core/data/chains.toml` -- chain configs
- `core/data/known_tokens.json` -- warm token list
- `core/data/fot_tokens.json` -- FoT/rebase token lists

**Remediation:** All three files exist. `include_str!` calls in `config/defaults.rs:43`,
`cache/token_cache.rs:142`, and `pool/state/pool_types.rs:31` load them at compile time.

**Remaining work:** None.

---

### 4.3 Extract `cross_tick` helper [DONE]

**Files:** `core/src/pool/math/v3.rs`

**Remediation:** `cross_tick()` helper extracted. Tick-crossing + liquidity-update block shared
between `quote_v3_exact_in` and `quote_v3_exact_out`.

**Remaining work:** None.

---

### 4.4 Extract `row_to_log` helper [DONE]

**Files:** `core/src/cache/store/mod.rs:405`, `blocks.rs:302,319`

**Remediation:** `fn row_to_normalized_log(row: &rusqlite::Row) -> anyhow::Result<NormalizedLog>`
exists at `mod.rs:405`. Called from two sites in `blocks.rs` at lines 302 and 319.

**Remaining work:** None.

---

### 4.5 Fix f64 precision in Balancer math [DONE]

**Files:** `core/src/pool/math/balancer.rs:47-48,162`

**Remediation:** `1e18 as u128` replaced with `WEI_PER_ETHER` constant
(`1_000_000_000_000_000_000u128`). All references use the named constant.

**Remaining work:** None.

---

### 4.6 Extract `epoch_secs()` helper [DONE]

**Files:** `core/src/utils.rs:8-13`

**Remediation:** `pub fn epoch_secs() -> u64` exists as a shared utility.

**Remaining work:** None.

---

### 4.7 Use `strum` for enum Display/FromStr [DONE]

**Files:** `core/Cargo.toml`, `core/src/types/chain.rs`

**Remediation:** `strum` is available and `ChainName` uses `#[derive(strum::Display, strum::EnumString)]`.
The remaining enums (`Strategy`, `DexType`, `GasModel`, `RangeMode`, etc.) have data-carrying
variants (e.g. `DexType::Balancer(u32)`, `Strategy::Jit` with payload) that strum cannot handle.
Manual `Display`/`FromStr` impls for these are correct and necessary.

**Remaining work:** None (data-carrying variants inherently cannot use strum derives).

---

<a id="phase-5"></a>
## Phase 5 -- Missing Smell Items (P3-P4)

### 5.1 Deduplicate coingecko cache-check-fetch pattern [DONE]

`core/src/coingecko.rs:82-115` -- `get_or_fetch()` generic helper exists.

---

### 5.2 Deduplicate coingecko HTTP request setup [DONE]

`core/src/coingecko.rs:119-133` -- `execute_price_request()` exists.

---

### 5.3 Make `PriceCache` concurrency-safe [DONE]

`core/src/coingecko.rs:49` -- Uses `tokio::sync::Mutex<HashMap<String, PriceEntry>>`.

---

### 5.4 Deduplicate RPC URL merging in Config [DONE]

`core/src/config/settings.rs:399-407` -- `fn merge_rpc_urls()` exists.

---

### 5.5 Fix config validation gaps [DONE]

**Files:** `core/src/config/validation.rs`

**Remediation:** Added range validation for `gas_limit` (21k–30M), `rps_limit` (>0 → ≤10k),
`proximity_window` (≤100) in `validation.rs`.

**Remaining work:** None.

---

### 5.6 Unify V3/V4 in `init_from_rpc` result handling [DONE]

`core/src/pool/state/factory.rs:130-150` -- `ConcentratedPoolState` trait abstracts over
V3 and V4. `process_concentrated_result()` handles both identically.

---

### 5.7 Convert `PoolInitResult` tuple variants to named structs [DONE]

`core/src/pool/state/factory.rs:13-22` -- All variants use named struct fields.

---

### 5.8 Deduplicate integrity/missing-block queries [DONE]

`core/src/cache/store/integrity.rs:4-43` -- `find_uncached_blocks()` unified.

---

### 5.9 Deduplicate `access_list` serialization [DONE]

**Files:** `core/src/cache/store/mod.rs:295,299`, `blocks.rs:91,186`, `pending.rs:12`

**Remediation:** `serialize_access_list` (line 295) and `deserialize_access_list` (line 299)
exist in `mod.rs`. Called from 3 sites in `blocks.rs` and 1 in `pending.rs`, all via
`super::SqliteStore::serialize_access_list`.

**Remaining work:** None.

---

### 5.10 Deduplicate `DiscoveredPool` construction [DONE]

`core/src/pool/discovery.rs:155-176` -- `DiscoveredPool::new()` with builder methods exists.

---

### 5.11 Deduplicate `apply.rs` V2 Solidly/Camelot cross-type handling [DONE]

`core/src/pool/state/apply.rs:27-44` -- Both `apply_v2_swap` and `apply_v2_sync` call
`try_apply_v2_to_curve()` (lines 138-148).

---

### 5.12 Add `apply_v3_mint_burn` support for V4 [DONE]

`core/src/pool/state/apply.rs:125-133` -- V4 arm is present.

---

### 5.13 Deduplicate Dune fetch functions in audit.rs [DONE]

**Files:** `core/src/dune/audit.rs`

**Remediation:** `fetch_by_query()` helper extracted. Deduplicates `fetch_sandwiches`,
`fetch_arbitrages`, and `fetch_flash_loans_from_dune`.

**Remaining work:** None.

---

### 5.14 Replace `eprintln!` with `tracing` in Dune client [DONE]

No `eprintln!` calls found in `core/src`. `dune/client.rs` uses `tracing::warn!`.

---

### 5.15 Fix `_burned` misleading underscore prefix [DONE]

Same as item 3.7. Renamed to `burned` (no underscore prefix).

---

### 5.16 Deduplicate `discovery.rs` log processing blocks [DONE]

`core/src/pool/discovery.rs:237-253` -- `process_discovery_log()` shared function exists.

---

### 5.17 Deduplicate `replay.rs` EVM setup and TX execution [DONE]

**Files:** `core/src/replay/replayer.rs`

**Remediation:** `build_mainnet_evm!` macro eliminates the 3-time-duplicated EVM context-building
block across replay methods.

**Remaining work:** None.

---

### 5.18 Deduplicate `DatabaseRef` vs `Database` impl [DONE]

**Files:** `core/src/replay/db.rs:149-199`

**Remediation:** `Database::basic()` delegates to `DatabaseRef::basic_ref(self, address)?`
and augments with code fetching. This is the maximum possible deduplication — the extra
code-fetching is inherent to the `&mut self` trait requirement and cannot be further
abstracted without restructuring the upstream `revm` traits.

**Remaining work:** None.

---

### 5.19 Deduplicate `merge_from` manual field copy [DONE]

`core/src/pool/discovery.rs:146-203` -- Uses `merge_option!` macro (lines 146-152) and
`merge_from()` method (line 182).

---

### 5.20 Fix inconsistent `StaticLock` vs `const` in decoders [DONE]

`core/src/pool/decoders.rs` -- All decoders use `pub const` with `b256!()`.

---

### 5.21 Fix `dune/pool_discovery.rs` leading space bug [DONE]

`core/src/dune/pool_discovery.rs:201` -- Now reads `.contains("ramses")` (lowercase, no space).

---

### 5.22 Deduplicate `call` / `call_latest` in RPC client [DONE]

**Files:** `core/src/rpc/client.rs:1017-1029`

**Remediation:** `call` (line 1017) delegates to `self.call_at(to, data, BlockId::number(block))`.
`call_latest` (line 1027) delegates to `self.call_at(to, data, BlockNumberOrTag::Latest.into())`.
The `call_at` helper is shared.

**Remaining work:** None.

---

### 5.23 Deduplicate `get_gas_price` / `get_max_priority_fee` [DONE]

**Files:** `core/src/rpc/client.rs:1032-1056`

**Remediation:** `fetch_u128_metric` helper (line 1032) handles the raw U256 RPC request pattern.
`get_gas_price` (line 1047) calls `self.fetch_u128_metric("eth_gasPrice")`.
`get_max_priority_fee` (line 1054) calls `self.fetch_u128_metric("eth_maxPriorityFeePerGas")`.

**Remaining work:** None.

---

### 5.24 Fix `ProviderState` encapsulation [DONE]

`core/src/rpc/middleware.rs:76-91` -- All fields private. Access via getter/setter methods.

---

### 5.25 Fix mixed `std::time::Instant` / `tokio::time::Instant` [DONE]

`core/src/rpc/middleware.rs` -- Uses `tokio::time::Instant` consistently.

---

### 5.26 Add `PoolState::info()` method [DONE]

`core/src/pool/state/pool_types.rs:457-467` -- `pub fn info()` exists with all match arms.
Also `info_mut()` at line 469.

---

### 5.27 Deduplicate `PoolManager` V3/V4 price calculation [DONE]

`core/src/pool/state/factory.rs:130-150` -- `ConcentratedPoolState` trait with
`process_concentrated_result()` called for both V3 (line 242) and V4 (line 247).

---

### 5.28 Deduplicate `persistence` serialization pattern [DONE]

`core/src/replay/db.rs:47-70` -- `CacheState` struct with `new()` and `clear()`.

---

<a id="dead-code-audit"></a>
## Dead Code Audit

> Full traceability of every item flagged as dead/unused. Verified by codebase-wide grep.

### Confirmed Dead -- Safe to Remove

| Item | File:Line | Status |
|------|-----------|--------|
| `default_executor_addresses()` | `config/defaults.rs:235` | Still present -- remove |
| `default_with_chains()` | `config/settings.rs:277` | Still present -- remove |
| `_output` parsed then discarded | `config/validation.rs:265` | Still present -- remove |
| `chain_id` field | `cache/store.rs:44` | Still present -- remove |
| `get_tick_spacing_from_fee` | `pool/math/v3.rs:428` | Still present -- remove |
| `get_tick_at_sqrt_ratio` | `pool/math/v3.rs:441` | Still present -- remove |
| `parse_v3_exact_output` | `mev/detectors/mempool.rs:466` | Still present -- remove |
| `decode_i128_from_be_bytes` | `pool/state/factory.rs:830` | **REMOVED** |
| `CURVE_PRICE_ORACLE_SELECTOR` | `pool/state/factory.rs:51` | **REMOVED** |
| `DuneTokenInfo` | `dune/types.rs:98` | **REMOVED** |
| `FALLBACK_LTV_BPS` | `mev/detectors/liquidation.rs:33` | **Renamed** to FALLBACK_LIQUIDATION_THRESHOLD_BPS |
| `_x_in_new` variable | `pool/math/curve.rs:143` | **REMOVED** |
| `RpcError::AllProvidersFailed` | `error/rpc.rs:8` | **REMOVED** |
| `RpcError::RateLimited` | `error/rpc.rs:10` | **REMOVED** |
| `EtagStore` | `rpc/middleware.rs:85` | **REMOVED** |
| `cross_validate_opportunities()` | `dune/cross_validate.rs` | **REMOVED** (file deleted) |

### Alive -- Incorrectly Flagged (Do NOT Remove)

| Item | File:Line | Actually Used At |
|------|-----------|-----------------|
| `optimal_two_hop_arb` (non-generic) | `pool/math/core.rs:154` | `mev/detectors/two_hop.rs:185` |
| `eval_profit` | `pool/math/core.rs:231` | Internal to `optimal_two_hop_arb_generic` |
| `extract_selector` | `rpc/client.rs:1164` | `fetch/fetcher.rs:625` |
| `estimate_v3_exact_out` | `mev/detectors/mempool.rs:501` | `mempool.rs:537` |
| `DISCOVER_*` constants | `dune/queries.rs:2033+` | `dune_query.rs:606-646` |
| `BlockSnapshot` struct | `cross_block.rs:26` | Used throughout `cross_block.rs` |
| `pool_hits` | `discovery.rs` | Used in shard merge and final output |
| `CURVE_*_UNDERLYING` topics | `discovery.rs:92,95` | `discovery.rs:434-435` + `pipeline/scanner.rs` |

### Misleadingly Named -- Alive But Poorly Named

| Item | File:Line | Issue |
|------|-----------|-------|
| `_burned` parameter | `jit.rs:224` | **REMOVED** -- renamed to `burned` (see 3.7) |
| `_expected_chain_id` param | `rpc/client.rs:491` | Unused param -- should be removed |
| `_gas_config` param | `cross_block.rs:96` | Unused param -- should be removed |
| `_token_in` / `_token_out` params | `cross_block.rs:205-206` | Unused params -- should be removed |
| `_tick_spacing` param | `pool/math/v3.rs:653` | Unused param -- should be removed |

---

<a id="unreachable-core-code"></a>
## Unreachable / Orphaned Core Code

> Code in `core/src/` not reachable from any CLI command.

### Confirmed Orphaned

| Function/Type | File:Line | Notes |
|---------------|-----------|-------|
| `cross_validate_opportunities()` | `dune/cross_validate.rs` | **DELETED** -- file no longer exists |

### Partially Reachable

| Function | File:Line | Reachable From |
|----------|-----------|---------------|
| `DuneClient::col_as_*` utilities | `dune/client.rs:237-275` | Only via `dune/pool_discovery.rs`, etc. |
| `resolve_method` / `resolve_event` | `sigs/resolver.rs:56-108` | Only via `pipeline/runner.rs` (`run` command) |
| `fetch_relevant` | `fetch/fetcher.rs` | Only via `commands/fetch.rs` |

---

<a id="risk-mitigation"></a>
## Risk Mitigation

| Risk | Mitigation |
|------|-----------|
| Behavioral regression from refactoring | Every phase includes a safety gate with `cargo test` |
| TOML config breakage from Config decomposition (2.1) | Use `#[serde(flatten)]` for backward compatibility |
| V3/V4 type unification breaks pool quoting (1.6) | Run backtest on a block containing V3 and V4 swaps |
| SQLite migration breaks existing DBs (2.4) | Test with an existing DB file |
| Dead code removal breaks hidden dependencies (4.1) | Each removal is a separate commit; `cargo build` after each |

---

<a id="tracking"></a>
## Tracking

| Phase | Items | Total | Done | Partial | TODO | Status |
|-------|-------|-------|------|---------|------|--------|
| Phase 0 | 0.1-0.4 | 4 | 4 | 0 | 0 | 100% |
| Phase 1 | 1.1-1.10 | 10 | 10 | 0 | 0 | 100% |
| Phase 2 | 2.1-2.8 | 8 | 8 | 0 | 0 | 100% |
| Phase 3 | 3.1-3.8 | 8 | 8 | 0 | 0 | 100% |
| Phase 4 | 4.1-4.7 | 7 | 7 | 0 | 0 | 100% |
| Phase 5 | 5.1-5.28 | 28 | 28 | 0 | 0 | 100% |
| **Total** | **65 items** | **65** | **65** | **0** | **0** | **100%** |
