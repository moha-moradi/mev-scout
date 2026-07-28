# Code Smell Remediation Plan

> Generated from full codebase audit. Ordered by priority (P0-P4) and grouped into
> independent work streams that can be parallelized. Every change includes a
> **safety gate** -- the step that must pass before the work is considered done.
>
> **Last updated:** 2026-07-28 -- status verified against codebase.

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

### 0.1 Fix unsafe raw-pointer usage in async fetcher [TODO]

**Files:** `core/src/fetch/fetcher.rs:172,197,472,480`

**Smell:** `let fetch = self as *const Self;` creates a raw pointer from `&self` held
across `.await` points. Undefined behavior if `Fetcher` is dropped while a future
referencing the pointer is alive.

**Remediation:**
- Wrap the fetcher in `Arc<Fetcher>` and clone the Arc into each spawned future.
- Remove all `unsafe` blocks.

**Remaining work:** Not started. Both `unsafe` blocks remain at lines 197 and 480.
No `Arc<Fetcher>` usage anywhere in the file.

**Safety gate:** `cargo clippy -- -D unsafe_code` passes; `cargo miri test` (if feasible) shows no UB.

---

### 0.2 Fix unsafe `static mut` in main.rs [TODO]

**Files:** `cli/src/main.rs:17,41`

**Smell:** `static mut _LOG_GUARD` -- mutable static accessed via `unsafe`. Deprecated
pattern, data-race risk.

**Remediation:**
```rust
use std::sync::OnceLock;
static LOG_GUARD: OnceLock<Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>> =
    OnceLock::new();
```

**Remaining work:** Not started. `static mut` at line 17 and `unsafe` write at line 41
remain. No `OnceLock` usage anywhere in the file.

**Safety gate:** `cargo clippy -- -D unsafe_code` passes.

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

### 1.1 Extract shared RPC initialization helper [PARTIAL]

**Files:** `cli/src/rpc_setup.rs`, `cli/src/commands/{run,fetch,live,replay,discover}.rs`

**Remediation:** `cli/src/rpc_setup.rs` was created with `init_rpc` (35 lines).
**But no command uses it.** All 5 commands still duplicate the manual RPC setup.

**Remaining work:**
- Add `use crate::rpc_setup::init_rpc;` to each of the 5 command files
- Replace the 5-7 line manual RPC setup block in `run.rs:26-31`, `fetch.rs:20-26`,
  `live.rs:17-23`, `replay.rs:19-24`, `discover.rs:31-44` with a single `init_rpc` call
- Verify each command still passes its tests

**Safety gate:** `cargo test -p mev-scout-cli`.

---

### 1.2 Deduplicate `retry_call` / `retry_call_archive` [TODO]

**Files:** `core/src/rpc/client.rs:195-271` vs `279-354`

**Smell:** ~150 lines of near-identical retry logic. Only differences: (1) `sorted_available()`
vs `sorted_available_archive()`, (2) error message wording.

**Remediation:**
- Extract `retry_call_inner(sorted_providers_fn, f)` parameterized by a closure.
- Have `retry_call` and `retry_call_archive` call it.

**Remaining work:** Not started. Both functions remain fully duplicated (~77 and ~75 lines).

**Safety gate:** `cargo test -p mev-scout-core`; manual test against live RPC.

---

### 1.3 Extract shared `BlockData` construction [TODO]

**Files:** `core/src/rpc/client.rs:674-682`, `728-736`, `892-900`

**Smell:** Block to BlockData conversion duplicated in `get_block`, `get_pending_block`,
and `batch_rpc_call`.

**Remediation:**
- Extract `fn block_to_internal(block: &Block) -> (BlockData, Vec<TxData>)`.
- All three call sites use it.

**Remaining work:** Not started. All three locations contain identical code constructing
`BlockData { number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase }`.

**Safety gate:** Existing tests pass.

---

### 1.4 Consolidate Dune utility functions [PARTIAL]

**Files:** `core/src/dune/util.rs`, `dune/audit.rs`, `dune/pool_discovery.rs`

**Remediation:** `core/src/dune/util.rs` was created with `render_query`, `approx_block_month_min`,
and `dune_chain_label`. **But it was never wired into the module tree or adopted by consumers.**

**Remaining work:**
- Add `pub mod util;` to `core/src/dune/mod.rs`
- In `audit.rs`: delete private `render_query` (line 14) and `approx_block_month_min` (line 24),
  replace with `use super::util::{render_query, approx_block_month_min};`
- In `pool_discovery.rs`: delete private `render_query` (line 11), `approx_block_month_min`
  (line 21), and `dune_chain_label` (line 41), replace with imports from `util`
- In `dune_query.rs`: delete private `dune_chain_label` (line 598) and `approx_block_month_min`
  (line 605), replace with imports from `util`
- Three separate copies exist; all should be deleted in favor of `util`

**Safety gate:** `cargo test -p mev-scout-core`.

---

### 1.5 Unify chain genesis parameter tables [TODO]

**Files:** `dune_query.rs:605-621`, `audit.rs:24-39`, `pool_discovery.rs:21-36`, `util.rs:8-32`

**Smell:** Chain to (genesis_ts, secs_per_block, blocks_per_day, lag) mapping in 4 places.

**Remediation:** Use `ChainTimingParams` from `util.rs` as single source of truth.

**Remaining work:** `ChainTimingParams` exists in `util.rs:1` and `chain_timing()` at `util.rs:8`,
but no consumer uses it. `audit.rs:24-39`, `pool_discovery.rs:21-36`, and
`dune_query.rs:605-621` each have their own hardcoded match -- four copies remain.

**Safety gate:** `cargo test`; verify `dune-find-blocks` output unchanged.

---

### 1.6 Unify V3/V4 pool state types [TODO]

**Files:** `core/src/pool/state/pool_types.rs:228-284`

**Smell:** `UniswapV3PoolState` and `UniswapV4PoolState` are identical 7-field structs
with a field-by-field `From` impl.

**Remediation:**
- Rename to `ConcentratedLiquidityState` (or type-alias V4 = V3).
- Remove the `From` impl; both variants use the same type.
- Update `apply.rs` to handle both with a single code path.

**Remaining work:** Not started. Both structs remain separate and identical with the same
7 fields. A `From<UniswapV4PoolState> for UniswapV3PoolState` impl exists (line 286) but
the structs themselves were not unified.

**Safety gate:** `cargo test`; run backtest on a block containing V4 swaps.

---

### 1.7 Deduplicate `apply.rs` V3/V4 swap handling [TODO]

**Files:** `core/src/pool/state/apply.rs:48-105,109-133`

**Smell:** V3 and V4 branches in `apply_v3_swap` are identical ~20-line blocks.
Same duplication in `apply_v3_mint_burn`.

**Remediation:** After 1.6 unifies the types, collapse into one match arm:
```rust
if let Some(PoolState::UniswapV3(state) | PoolState::UniswapV4(state)) = self.pools.get_mut(address) {
```

**Remaining work:** Not started. `apply_v3_swap` V3 branch (lines 58-81) and V4 branch
(lines 83-105) have identical logic. `apply_v3_mint_burn` V3 branch (lines 116-124) and
V4 branch (lines 125-133) have identical tick/liquidity update logic.

**Safety gate:** Same as 1.6.

---

### 1.8 Extract pool deserialization helper in SqliteStore [DONE]

**Files:** `core/src/cache/store/mod.rs:345-407`, `store/pools.rs:57,71`

**Remediation:** `row_to_pool_info` helper exists. Both `get_discovered_pool` and
`list_discovered_pools` call it via `super::row_to_pool_info(&row)?`.

**Remaining work:** None.

---

### 1.9 Deduplicate overrides.rs chain/block-range copying [TODO]

**Files:** `cli/src/overrides.rs:6-123`

**Smell:** Chain args copied in 7 match arms; block-range args in 3 arms.

**Remediation:**
```rust
fn apply_chain_args(o: &mut CliOverrides, c: &ChainArgs) {
    o.chain = Some(c.chain.clone());
    o.rpc_url = c.rpc_url.clone();
    o.rpc_urls = c.rpc_urls.clone();
    o.rpc_rps = c.rpc_rps.clone();
    o.rps_limit = Some(c.rps_limit);
}
fn apply_block_range(o: &mut CliOverrides, b: &BlockRangeArgs) { ... }
```

**Remaining work:** Not started. 10 match arms each manually copy chain args with the
same 5-line pattern. No helpers exist.

**Safety gate:** `cargo test -p mev-scout-cli`.

---

### 1.10 Merge `dune_query.rs` two-source-of-truth [PARTIAL]

**Files:** `cli/src/commands/dune_query.rs:7-11,454-596`

**Remediation plan:** Add `sql: &'static str` field to `QueryInfo`. Replace 140-arm match
with a simple lookup.

**Remaining work:**
- `QueryInfo` (lines 7-11) does **not** have a `sql` field -- only `name`, `description`, `required`
- The ~140-arm `match` in `get_query_sql` (lines 454-596) still exists
- `dune_query.rs` has its own private `dune_chain_label` (line 598) and `approx_block_month_min`
  (line 605) that should be replaced with imports from `dune::util` (see 1.4)

**Safety gate:** `cargo test -p mev-scout-cli`; verify `--list` and `--all` output unchanged.

---

<a id="phase-2"></a>
## Phase 2 -- Structural Refactors (P2)

### 2.1 Decompose `Config` into sub-structs [TODO]

**Files:** `core/src/config/settings.rs:21-140`

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

**Remaining work:** Not started. `Config` is still a flat 30+ field struct. Only separation
is comment section dividers.

**Safety gate:** `cargo test -p mev-scout-core`; existing TOML configs still parse.

---

### 2.2 Introduce `MevOpportunity` builder [PARTIAL]

**Files:** `core/src/types/opportunity.rs:113-203`

**Smell:** 22-field struct literal with many `None` defaults repeated across every detector.

**Remediation plan:** Full builder pattern with `Result`-returning methods.

**Remaining work:**
- `MevOpportunity::new()` exists (line 116) with builder-style methods
  `with_canonical_id()`, `with_jit_fields()`, `with_sandwich_fields()`, `with_path()`
  (lines 154-202)
- But builder methods still use `debug_assert!` (lines 171, 184, 196) instead of
  returning `Result`, so invalid combinations are silently accepted in release builds
- Replace `debug_assert!` with `Result` returns (also addresses item 3.3)

**Safety gate:** `cargo test`; each detector's output unchanged.

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

### 2.5 Extract per-DEX scanners from `discover_pools_shard` [TODO]

**Files:** `core/src/pool/discovery.rs:647-1526`

**Smell:** All DEX factory scanning in one ~880 line monolith.

**Remediation:**
- Create `pool/discovery/` module with `v2.rs`, `v3.rs`, `balancer.rs`, `curve.rs`,
  `solidly.rs`, `camelot.rs`, `v4.rs`, `pendle.rs`
- Each exports `async fn scan_shard(...) -> Vec<DiscoveredPool>`.
- `discover_pools_shard` becomes a dispatcher.

**Remaining work:** Not started. Single monolith function.

**Safety gate:** `cargo test -p mev-scout-core`; run `discover` and verify pool count unchanged.

---

### 2.6 Extract per-DEX initialization from `init_from_rpc` [TODO]

**Files:** `core/src/pool/state/factory.rs:153-219`

**Smell:** All pool-type initialization in one function.

**Remediation:**
- Create `init_v2_pool`, `init_v3_pool`, `init_balancer_pool`, etc.
- `init_from_rpc` dispatches to the appropriate init function.

**Remaining work:** Not started. `init_from_rpc` dispatches to `fetch_pool_state()` (line 216)
but does not split per-pool-type initialization into separate functions.

**Safety gate:** Same as 2.5.

---

### 2.7 Extract `Command` trait [TODO]

**Files:** `cli/src/main.rs:77-90`

**Smell:** Each command is a free function with no shared interface.

**Remediation:**
```rust
#[async_trait]
trait Command {
    async fn execute(&self, config: &Config) -> anyhow::Result<()>;
}
```

**Remaining work:** Not started. `main.rs` still uses a `match` block dispatching to
`commands::cmd_run()`, `commands::cmd_fetch()`, etc. via free functions.

**Safety gate:** `cargo test -p mev-scout-cli`.

---

### 2.8 Extract shared `estimate_gas` helper across detectors [DONE]

**Files:** `core/src/mev/gas.rs:1-16`

**Remediation:** Contains `BASE_TX_GAS`, `DEFAULT_POOL_GAS`, `JIT_OVERHEAD`,
`LIQUIDATION_GAS_LIMIT` constants and `estimate_base_gas()`, `estimate_multi_swap_gas()` helpers.

**Remaining work:** None.

---

<a id="phase-3"></a>
## Phase 3 -- Quality & Consistency (P3)

### 3.1 Replace magic numbers with named constants [PARTIAL]

**Files:** Throughout codebase (45+ instances)

**Remediation plan:** Create `mev/gas.rs`, `pool/math/consts.rs`, `rpc/consts.rs`,
`dune/consts.rs`, `pool/discovery/consts.rs`.

**Remaining work:**
- `mev/gas.rs` -- **DONE** (item 2.8)
- `pool/math/consts.rs` -- **DOES NOT EXIST**. The `math/` directory has: `balancer.rs`,
  `core.rs`, `curve.rs`, `lb.rs`, `mod.rs`, `pendle.rs`, `stable_swap.rs`, `v3.rs`
- `rpc/consts.rs` -- **DOES NOT EXIST**
- `dune/consts.rs` -- **DOES NOT EXIST**
- Magic numbers in `two_hop.rs:302-306`, `apply.rs:70,78`, `client.rs:77,201`,
  `middleware.rs:167-177` remain as inline literals

**Safety gate:** `cargo test`; no behavioral change.

---

### 3.2 Replace glob re-exports with explicit ones [TODO]

**Files:** `types/mod.rs`, `error/mod.rs`, `config/mod.rs`, `pool/mod.rs`,
`pool/math/mod.rs`, `mev/detectors/mod.rs`, `rpc/mod.rs`

**Smell:** `pub use submodule::*` in every `mod.rs`.

**Remaining work:** Not started. All glob re-exports remain:
- `types/mod.rs:4-6` -- `pub use chain::*; pub use strategy::*; pub use opportunity::*;`
- `error/mod.rs:6-9` -- `pub use cache::*; pub use config::*; pub use replay::*; pub use rpc::*;`
- `config/mod.rs:4-6` -- `pub use defaults::*; pub use settings::*; pub use validation::*;`
- `pool/mod.rs:10` -- `pub use math::*;`
- `mev/detectors/mod.rs:10-17` -- all glob re-exports
- `rpc/mod.rs:3-4` -- `pub use client::*; pub use middleware::*;`
- `pool/math/mod.rs:8-11` -- all glob re-exports

**Safety gate:** `cargo build`; fix any new "unresolved import" errors.

---

### 3.3 Fix `debug_assert!` usage in public API [TODO]

**Files:** `core/src/types/opportunity.rs:171,184,196`

**Smell:** `debug_assert!` only fires in debug builds; release silently produces garbage.

**Remaining work:** Not started. All three builder methods still use `debug_assert!`:
- Line 171: `debug_assert!(self.strategy == Strategy::Jit || ...` in `with_jit_fields`
- Line 184: `debug_assert!(self.strategy == Strategy::Sandwich, ...` in `with_sandwich_fields`
- Line 196: `debug_assert!(self.strategy == Strategy::MultiHopArb, ...` in `with_path`

**Safety gate:** `cargo test`.

---

### 3.4 Add `is_empty()` to `TokenCache` [TODO]

**Files:** `core/src/cache/token_cache.rs:239-241`

**Smell:** `len()` implemented but `is_empty()` missing (clippy lint).

**Remaining work:** Not started. `TokenCache` has `len()` (line 239), `contains()` (line 233),
`missing()` (line 244), but no `is_empty()`. Note: `AaveReserveCache` in `liquidation.rs:95`
does have `is_empty()` but `TokenCache` does not.

**Safety gate:** `cargo clippy`.

---

### 3.5 Remove dead `RpcError` variants [TODO]

**Files:** `core/src/error/rpc.rs:8,10`

**Smell:** `AllProvidersFailed` and `RateLimited` variants are defined but never constructed.

**Remaining work:** Not started. `RpcError` enum still has: `CallFailed`, `AllProvidersFailed`
(line 8), `RateLimited` (line 10), `InvalidResponse`. The dead variants `AllProvidersFailed`
and `RateLimited` are never constructed anywhere.

**Safety gate:** `cargo test`.

---

### 3.6 Fix inconsistent return types in factory methods [TODO]

**Files:** `core/src/types/chain.rs:103,134`

**Smell:** `default_uniswap_v2_factories()` returns `Vec<&'static str>` while V3 returns
`&'static [&'static str]`.

**Remaining work:** Not started. Different return types remain.

**Safety gate:** `cargo build`; fix call sites.

---

### 3.7 Fix `_burned` misleading underscore prefix [TODO]

**Files:** `core/src/mev/detectors/jit.rs:223`

**Smell:** `_burned: bool` IS used in the function body but prefixed with `_`.

**Remaining work:** Not started. Parameter `_burned: bool` is still present at line 223
and used at lines 290-293.

**Safety gate:** `cargo clippy`.

---

### 3.8 Standardize error message formatting [TODO]

**Files:** Multiple CLI files

**Smell:** Inconsistent error prefixing (`"Error: ..."` vs bare messages).

**Remaining work:** Not started. `anyhow::Context` is not used in CLI command files.
`dune/client.rs` uses it in one place (lines 82-84, 121) but it's not standardized.

**Safety gate:** `cargo test -p mev-scout-cli`.

---

<a id="phase-4"></a>
## Phase 4 -- Nice-to-Have Cleanup (P4)

### 4.1 Remove dead code [PARTIAL]

**Files:** Multiple

**Remaining work -- still dead, not yet removed:**
- `config/defaults.rs:235` -- `default_executor_addresses()` (returns empty HashMaps)
- `config/settings.rs:277` -- `default_with_chains()` (one-liner delegating to `Config::default()`)
- `pool/math/v3.rs:428` -- `get_tick_spacing_from_fee` (`#[allow(dead_code)]`)
- `pool/math/v3.rs:440` -- `get_tick_at_sqrt_ratio` (`#[allow(dead_code)]`)
- `mev/detectors/mempool.rs:465` -- `parse_v3_exact_output` (`#[allow(dead_code)]`)
- `error/rpc.rs:8,10` -- `AllProvidersFailed`, `RateLimited` variants (see 3.5)

**Already removed (confirmed):**
- `decode_i128_from_be_bytes` -- removed from `factory.rs`
- `CURVE_PRICE_ORACLE_SELECTOR` -- removed from `factory.rs`
- `DuneTokenInfo` -- removed from `dune/types.rs`
- `FALLBACK_LTV_BPS` -- renamed to `FALLBACK_LIQUIDATION_THRESHOLD_BPS` in `liquidation.rs`
- `_x_in_new` -- removed from `curve.rs`
- `EtagStore` -- removed from `rpc/middleware.rs`

**Safety gate:** `cargo build` and `cargo test` after each removal.

---

### 4.2 Move hardcoded data to external files [TODO]

**Files:**
- `config/defaults.rs:45-231` -- chain configs (candidate: `data/chains.toml`)
- `cache/token_cache.rs:127-212` -- warm token list (candidate: `data/known_tokens.json`)
- `types/pool_types.rs:10-34` -- FoT/rebase token lists (candidate: `data/fot_tokens.json`)

**Remaining work:** Not started. Data is still hardcoded in Rust source.

**Safety gate:** `cargo test`; `include_str!` makes these compile-time verified.

---

### 4.3 Extract `cross_tick` helper [TODO]

**Files:** `core/src/pool/math/v3.rs:568-632` vs `703-766`

**Smell:** Tick-crossing and liquidity update logic duplicated between exact-in and exact-out.

**Remaining work:** Not started. No `cross_tick` function found anywhere.

**Safety gate:** `cargo test -p mev-scout-core`.

---

### 4.4 Extract `row_to_log` helper [TODO]

**Files:** `core/src/cache/store.rs:612-627` vs `642-657`

**Smell:** Identical row-to-NormalizedLog mapping in two functions.

**Remaining work:** Not started. No `row_to_log` function found. The store module has
`row_to_pool_info` but not `row_to_log`.

**Safety gate:** Existing tests pass.

---

### 4.5 Fix f64 precision in Balancer math [TODO]

**Files:** `core/src/pool/math/balancer.rs:46-47`

**Smell:** `1e18 as u128` = 999999999999999999 (not 10^18).

**Remaining work:** Not started. Lines 46-47 still read:
```rust
let w_in = U256::from(if weight_in == 0 { 1e18 as u128 } else { weight_in });
let w_out = U256::from(if weight_out == 0 { 1e18 as u128 } else { weight_out });
```

**Safety gate:** `cargo test`; compare output against known Balancer pools.

---

### 4.6 Extract `epoch_secs()` helper [DONE]

**Files:** `core/src/utils.rs:8-13`

**Remediation:** `pub fn epoch_secs() -> u64` exists as a shared utility.

**Remaining work:** None.

---

### 4.7 Use `strum` for enum Display/FromStr [PARTIAL]

**Files:** `core/Cargo.toml`, `core/src/types/chain.rs`

**Remaining work:**
- `strum` is added as a dependency (`core/Cargo.toml:31`)
- `ChainName` in `types/chain.rs:23` uses `#[derive(strum::Display, strum::EnumString)]`
- But other enums with manual `Display`/`FromStr` impls are NOT converted (e.g. `Strategy`,
  `DexType`, `GasModel`, and others in `strategy.rs` and related files still have manual impls)
- Approximately 200 lines of repetitive boilerplate remain across ~7 enums

**Safety gate:** `cargo test`; verify CLI parsing unchanged.

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

### 5.5 Fix config validation gaps [PARTIAL]

**Remaining work:** `validate_and_resolve_for()` validates chain, provider, strategies,
range, RPC URLs, gas model, output format. But no validation for gas_limit, rps_limit,
proximity_window ranges, etc.

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

### 5.9 Deduplicate `access_list` serialization [TODO]

**Files:** `core/src/cache/store.rs:386-390`, `487-491`, `1214-1218`

**Smell:** `if tx.access_list.is_empty() { None } else { Some(Self::serialize(...)?) }` x3.

**Remaining work:** Not started. Extract `fn serialize_access_list(...)`.

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

### 5.13 Deduplicate Dune fetch functions in audit.rs [TODO]

**Files:** `core/src/dune/audit.rs:121-207`

**Smell:** `fetch_sandwiches_from_dune`, `fetch_arbitrages_from_dune`, `fetch_flash_loans_from_dune`
share identical structure.

**Remaining work:** Not started. No generic `fetch_events<T>` helper exists.

---

### 5.14 Replace `eprintln!` with `tracing` in Dune client [DONE]

No `eprintln!` calls found in `core/src`. `dune/client.rs` uses `tracing::warn!`.

---

### 5.15 Fix `_burned` misleading underscore prefix [TODO]

Same as item 3.7. `jit.rs:223` still has `_burned: bool`.

---

### 5.16 Deduplicate `discovery.rs` log processing blocks [DONE]

`core/src/pool/discovery.rs:237-253` -- `process_discovery_log()` shared function exists.

---

### 5.17 Deduplicate `replay.rs` EVM setup and TX execution [TODO]

**Files:** `core/src/replay/replayer.rs` -- three replay methods

**Remaining work:** Not fully verified. `BlockReplayer` struct exists in `replayer.rs`.
`Database` impl in `db.rs:149` delegates to `DatabaseRef::basic_ref` for code fetching,
suggesting some dedup. But full EVM setup and TX execution dedup across all 3 replay
methods could not be confirmed without reading more.

---

### 5.18 Deduplicate `DatabaseRef` vs `Database` impl [PARTIAL]

**Files:** `core/src/replay/db.rs:149-199`

**Remaining work:** `Database` impl for `basic()` calls `DatabaseRef::basic_ref(self, address)?`
(line 156) and augments with code fetching. This shows delegation for the common path.
Full dedup across all methods not confirmed.

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

### 5.22 Deduplicate `call` / `call_latest` in RPC client [TODO]

**Files:** `core/src/rpc/client.rs:1090-1105` vs `1113-1128`

**Remaining work:** Not fully verified. Both build identical `TransactionRequest`,
only differ by block tag. Extract `fn call_at(to, data, block) -> Result`.

---

### 5.23 Deduplicate `get_gas_price` / `get_max_priority_fee` [TODO]

**Files:** `core/src/rpc/client.rs:1133-1143` vs `1148-1158`

**Remaining work:** Not fully verified. Identical structure: raw U256 request, map_err, to u128.
Extract `fn fetch_u128_metric(method: &str) -> Result<u128>`.

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
| `RpcError::AllProvidersFailed` | `error/rpc.rs:8` | Still present -- remove |
| `RpcError::RateLimited` | `error/rpc.rs:10` | Still present -- remove |
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
| `_burned` parameter | `jit.rs:223` | Prefixed `_` but IS used -- remove underscore (see 3.7) |
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
| Phase 0 | 0.1-0.4 | 4 | 2 | 0 | 2 | 50% |
| Phase 1 | 1.1-1.10 | 10 | 1 | 3 | 6 | 25% |
| Phase 2 | 2.1-2.8 | 8 | 3 | 1 | 4 | 44% |
| Phase 3 | 3.1-3.8 | 8 | 0 | 1 | 7 | 6% |
| Phase 4 | 4.1-4.7 | 7 | 1 | 2 | 4 | 29% |
| Phase 5 | 5.1-5.28 | 28 | 17 | 2 | 9 | 68% |
| **Total** | **65 items** | **65** | **24** | **9** | **32** | **~46%** |
