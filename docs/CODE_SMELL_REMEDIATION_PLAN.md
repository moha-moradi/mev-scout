# Code Smell Remediation Plan

> Generated from full codebase audit. Ordered by priority (P0–P4) and grouped into
> independent work streams that can be parallelized. Every change includes a
> **safety gate** — the step that must pass before the work is considered done.

---

## Table of Contents

1. [Priority Legend](#priority-legend)
2. [Phase 0 — Critical Safety Fixes (P0)](#phase-0)
3. [Phase 1 — High-Impact Deduplication (P1)](#phase-1)
4. [Phase 2 — Structural Refactors (P2)](#phase-2)
5. [Phase 3 — Quality & Consistency (P3)](#phase-3)
6. [Phase 4 — Nice-to-Have Cleanup (P4)](#phase-4)
7. [Phase 5 — Missing Smell Items (P3–P4)](#phase-5)
8. [Dead Code Audit](#dead-code-audit)
9. [Unreachable / Orphaned Core Code](#unreachable-core-code)
10. [Risk Mitigation](#risk-mitigation)
11. [Tracking](#tracking)

---

## Priority Legend

| Priority | Meaning | When |
|----------|---------|------|
| **P0** | Correctness bug or soundness issue — can produce wrong results or memory-safety violations | Immediate |
| **P1** | High-leverage deduplication that removes the most duplicated code per change | First sprint |
| **P2** | Structural refactors that unlock future improvements | Second sprint |
| **P3** | Consistency, naming, and style improvements | Ongoing |
| **P4** | Nice-to-have cleanup with low urgency | Backlog |

---

<a id="phase-0"></a>
## Phase 0 — Critical Safety Fixes (P0)

### 0.1 Fix unsafe raw-pointer usage in async fetcher

**Files:** `core/src/fetch/fetcher.rs:172,197,472,480`

**Smell:** `let fetch = self as *const Self;` creates a raw pointer from `&self` that is held across `.await` points. If the `Fetcher` is dropped while a future referencing the pointer is still alive, this is undefined behavior.

**Remediation:**
- Wrap the fetcher in `Arc<Fetcher>` and clone the Arc into each spawned future.
- Remove all `unsafe` blocks.

**Safety gate:** `cargo clippy -- -D unsafe_code` passes; `cargo miri test` (if feasible) shows no UB.

---

### 0.2 Fix unsafe `static mut` in main.rs

**Files:** `cli/src/main.rs:17,41`

**Smell:** `static mut _LOG_GUARD` — mutable static accessed via `unsafe`. Deprecated pattern, data-race risk.

**Remediation:**
```rust
use std::sync::OnceLock;
static LOG_GUARD: OnceLock<std::sync::Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>> =
    OnceLock::new();
```
- Store the guard inside the `OnceLock<Mutex<Option<...>>>`.
- Remove the `unsafe` block.

**Safety gate:** `cargo clippy -- -D unsafe_code` passes.

---

### 0.3 Fix cross-validation zero-hash bug

**Files:** `core/src/dune/cross_validate.rs:150,179`

**Smell:** The tx_hash parameter is hardcoded to `\\x0000...0000` instead of the actual opportunity's tx hash. Cross-validation queries never match real data — this is a **functional bug**.

**Remediation:**
- Pass `opp.tx_hash` converted to hex with `\\x` prefix.
- Add a unit test that verifies a non-zero hash is forwarded.

**Safety gate:** Existing tests pass; add a test that a `DuneSandwichCheck` with a real tx_hash produces a non-empty query string.

---

### 0.4 Fix silent migration error swallowing

**Files:** `core/src/cache/store.rs:231-244`

**Smell:** 8 consecutive `let _ = conn.execute_batch("ALTER TABLE ...")` silently discard errors. Only "column already exists" should be ignored.

**Remediation:**
```rust
for stmt in migrations {
    match conn.execute_batch(stmt) {
        Ok(()) => {}
        Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("duplicate column") => {}
        Err(e) => return Err(e.into()),
    }
}
```

**Safety gate:** All existing tests pass; manual test: open DB with existing schema, verify no panic.

---

<a id="phase-1"></a>
## Phase 1 — High-Impact Deduplication (P1)

### 1.1 Extract shared RPC initialization helper

**Files:** `cli/src/commands/run.rs:26-31`, `fetch.rs:15-27`, `live.rs:17-23`, `replay.rs:19-25`, `discover.rs:31-45`

**Smell:** 8–12 line RPC bootstrap sequence (parse chain → get provider configs → create client → set RPS → set archive → check connection) copy-pasted across 5 command files.

**Remediation:**
- Create `cli/src/rpc_setup.rs` with:
  ```rust
  pub async fn init_rpc(config: &Config, chain: &ChainName) -> anyhow::Result<(RpcClient, Vec<ProviderConfig>)>
  ```
- Replace all 5 occurrences with a single call.

**Safety gate:** All command tests pass (`cargo test -p mev-scout-cli`).

---

### 1.2 Deduplicate `retry_call` / `retry_call_archive`

**Files:** `core/src/rpc/client.rs:196-272` vs `280-355`

**Smell:** ~150 lines of near-identical retry logic; only difference is which sorted provider list is used.

**Remediation:**
- Extract `retry_call_inner(sorted_providers_fn, f)` parameterized by a closure that returns the provider list.
- Have `retry_call` and `retry_call_archive` call it.

**Safety gate:** `cargo test -p mev-scout-core` passes; manual test against live RPC.

---

### 1.3 Extract shared `BlockData` construction

**Files:** `core/src/rpc/client.rs:641-686`, `695-740`, `882-901`

**Smell:** Block→`BlockData` conversion duplicated in `get_block`, `get_pending_block`, and `batch_rpc_call`.

**Remediation:**
- Extract `fn block_to_internal(block: &Block) -> (BlockData, Vec<TxData>)`.
- All three call sites use it.

**Safety gate:** Existing tests pass.

---

### 1.4 Consolidate Dune utility functions

**Files:** `dune/audit.rs:13-39`, `dune/pool_discovery.rs:11-46`, `dune/cross_validate.rs:197-201`

**Smell:** `render_query()`, `approx_block_month_min()`, and `dune_chain_label()` are each copy-pasted in 2–3 files.

**Remediation:**
- Create `core/src/dune/util.rs` with these three functions.
- All three modules import from `dune::util`.

**Safety gate:** `cargo test -p mev-scout-core` passes.

---

### 1.5 Unify chain genesis parameter tables

**Files:** `cli/src/commands/dune_find_blocks.rs:6-22,32-43,46-60,64-86` + `cli/src/commands/dune_query.rs:680-696`

**Smell:** Chain→(genesis_ts, secs_per_block, blocks_per_day, lag) mapping defined in 4+ match blocks.

**Remediation:**
- Create `ChainTimingParams` struct in `core/src/types/chain.rs` or a new `core/src/dune/chain_params.rs`.
- All match blocks look up from this single source.

**Safety gate:** `cargo test` passes; verify `dune-find-blocks` output unchanged.

---

### 1.6 Unify V3/V4 pool state types

**Files:** `core/src/pool/state/pool_types.rs:228-284`

**Smell:** `UniswapV3PoolState` and `UniswapV4PoolState` are identical 7-field structs with a field-by-field `From` impl.

**Remediation:**
- Rename to `ConcentratedLiquidityState` (or keep `UniswapV3PoolState` and type-alias `UniswapV4PoolState = UniswapV3PoolState`).
- Remove the `From` impl; both variants use the same type directly.
- Update `apply.rs:62-109` to handle both with a single code path.

**Safety gate:** `cargo test` passes; run backtest on a block containing V4 swaps.

---

### 1.7 Deduplicate `apply.rs` V3/V4 swap handling

**Files:** `core/src/pool/state/apply.rs:62-109`

**Smell:** V3 and V4 branches in `apply_v3_swap` are identical ~20-line blocks.

**Remediation:**
- After 1.6 unifies the types, collapse into one match arm:
  ```rust
  if let Some(PoolState::UniswapV3(state) | PoolState::UniswapV4(state)) = self.pools.get_mut(address) {
  ```

**Safety gate:** Same as 1.6.

---

### 1.8 Extract pool deserialization helper in SqliteStore

**Files:** `core/src/cache/store.rs:1003-1078` vs `1080-1152`

**Smell:** ~140 lines of duplicated row→`PoolInfo` mapping.

**Remediation:**
- Extract `fn row_to_pool(row: &Row) -> rusqlite::Result<PoolInfo>`.
- Both `get_discovered_pool` and `list_discovered_pools` call it.

**Safety gate:** Existing cache tests pass.

---

### 1.9 Deduplicate overrides.rs chain/block-range copying

**Files:** `cli/src/overrides.rs:7-123`

**Smell:** Chain args (chain, rpc_url, rpc_urls, rpc_rps, rps_limit) copied in 7 match arms; block-range args in 3 arms.

**Remediation:**
```rust
fn apply_chain_args(o: &mut CliOverrides, c: &ChainArgs) {
    o.chain = Some(c.chain.clone());
    o.rpc_url = c.rpc_url.clone();
    o.rpc_urls = c.rpc_urls.clone();
    o.rpc_rps = c.rpc_rps.clone();
    o.rps_limit = Some(c.rps_limit);
}

fn apply_block_range(o: &mut CliOverrides, b: &BlockRangeArgs) {
    o.days = b.days;
    o.blocks = b.blocks;
    o.block = b.block;
    o.from_block = b.from_block;
    o.to_block = b.to_block;
}
```

**Safety gate:** `cargo test -p mev-scout-cli` passes.

---

### 1.10 Merge `dune_query.rs` two-source-of-truth

**Files:** `cli/src/commands/dune_query.rs:14-527` (all_queries) + `529-671` (get_query_sql)

**Smell:** 500-line `Vec<QueryInfo>` and 140-arm match must be kept in sync manually.

**Remediation:**
- Add a `sql: &'static str` field to `QueryInfo`.
- `get_query_sql` becomes `all_queries().iter().find(|q| q.name == name).map(|q| q.sql)`.
- Delete the match statement (~140 lines saved).

**Safety gate:** `cargo test -p mev-scout-cli`; manually verify `--list` and `--all` output unchanged.

---

<a id="phase-2"></a>
## Phase 2 — Structural Refactors (P2)

### 2.1 Decompose `Config` into sub-structs

**Files:** `core/src/config/settings.rs:21-140`

**Smell:** 30+ field God Object mixing backtest, live, Dune, RPC, gas, output, and per-chain concerns.

**Remediation:**
```rust
pub struct Config {
    pub chain: String,
    pub rpc: RpcConfig,        // rpc_url, rpc_urls, rpc_rps, rps_limit
    pub backtest: BacktestConfig,
    pub live: LiveConfig,
    pub gas: GasConfig,
    pub output: OutputConfig,
    pub dune: DuneConfig,
    // ...
}
```
- Update `merge_cli` and `CliOverrides` accordingly.
- Use `#[serde(flatten)]` for backward-compatible TOML.

**Safety gate:** `cargo test -p mev-scout-core`; existing TOML configs still parse.

---

### 2.2 Introduce `MevOpportunity` builder

**Files:** `core/src/types/opportunity.rs` (all detectors)

**Smell:** 22-field struct literal with many `None` defaults repeated across every detector.

**Remediation:**
```rust
impl MevOpportunity {
    pub fn builder() -> MevOpportunityBuilder { MevOpportunityBuilder::default() }
}
```
- Each detector calls `MevOpportunity::builder().pool_a(...).profit(...).build()`.
- `Default` derive handles all `None`/`ZERO` fields.

**Safety gate:** `cargo test` passes; each detector's output unchanged.

---

### 2.3 Decompose `SqliteStore` into domain modules

**Files:** `core/src/cache/store.rs` (1286 lines, 40+ methods)

**Smell:** Single struct with 40+ methods spanning blocks, pools, accounts, manifests, integrity.

**Remediation:**
- Keep `SqliteStore` as the connection holder.
- Move methods into domain-specific trait impls or free functions:
  - `store::blocks` — block/tx/receipt/log CRUD
  - `store::pools` — pool discovery CRUD
  - `store::accounts` — account/storage/code CRUD
  - `store::manifests` — run manifest CRUD
  - `store::integrity` — missing block checks
- All functions take `&SqliteStore` as a parameter.

**Safety gate:** `cargo test` passes; public API unchanged.

---

### 2.4 Add SQLite schema versioning

**Files:** `core/src/cache/store.rs:71-256`

**Smell:** Ad-hoc ALTER TABLE migrations with error suppression; no version tracking.

**Remediation:**
- Create `schema_version` table with a single `version INTEGER` row.
- On open, read version; apply numbered migration scripts in order.
- Remove all `let _ = conn.execute_batch("ALTER TABLE...")` lines.

**Safety gate:** `cargo test`; manual test: open old DB, verify migration runs cleanly.

---

### 2.5 Extract per-DEX scanners from `discover_pools_shard`

**Files:** `core/src/pool/discovery.rs:634-1156` (522 lines)

**Smell:** All DEX factory scanning in one monolith.

**Remediation:**
- Create `pool/discovery/` module with:
  - `v2.rs`, `v3.rs`, `balancer.rs`, `curve.rs`, `solidly.rs`, `camelot.rs`, `v4.rs`, `pendle.rs`
- Each exports `async fn scan_shard(...) -> Vec<DiscoveredPool>`.
- `discover_pools_shard` becomes a dispatcher that fans out to the per-DEX scanners.

**Safety gate:** `cargo test -p mev-scout-core`; run `discover` command and verify pool count unchanged.

---

### 2.6 Extract per-DEX initialization from `init_from_rpc`

**Files:** `core/src/pool/state/factory.rs:145-421` (276 lines)

**Smell:** All pool-type initialization in one function.

**Remediation:**
- Create `init_v2_pool`, `init_v3_pool`, `init_balancer_pool`, etc.
- `init_from_rpc` dispatches to the appropriate init function.

**Safety gate:** Same as 2.5.

---

### 2.7 Extract `Command` trait

**Files:** `cli/src/main.rs:80-93`, all command files

**Smell:** Each command is a free function with no shared interface.

**Remediation:**
```rust
#[async_trait]
trait Command {
    async fn execute(&self, config: &Config) -> anyhow::Result<()>;
}
```
- Implement for each `*Args` struct.
- `main.rs` becomes `cli.command.execute(&config).await`.

**Safety gate:** `cargo test -p mev-scout-cli`.

---

### 2.8 Extract shared `estimate_gas` helper across detectors

**Files:** All detectors in `core/src/mev/detectors/`

**Smell:** `40_000 + calldata + pool_gas` pattern repeated in every detector with the same magic numbers.

**Remediation:**
- Create `core/src/mev/gas.rs`:
  ```rust
  pub const BASE_TX_GAS: u64 = 40_000;
  pub const DEFAULT_POOL_GAS: u64 = 80_000;
  pub fn estimate_multi_swap_gas(slot_count: usize, pool_gases: &[u64]) -> u64 { ... }
  ```
- All detectors import from `mev::gas`.

**Safety gate:** `cargo test` passes; gas estimates unchanged.

---

<a id="phase-3"></a>
## Phase 3 — Quality & Consistency (P3)

### 3.1 Replace magic numbers with named constants

**Files:** Throughout codebase (45+ instances)

**Groups:**
- **Gas constants:** `sandwich.rs:465,467`, `multi_hop.rs:26,27`, `jit.rs:190,264`, `liquidation.rs:456` → `core/src/mev/gas.rs`
- **Fee/slip constants:** `two_hop.rs:302-306`, `apply.rs:70,78` → `core/src/pool/math/consts.rs`
- **RPC constants:** `client.rs:77,201`, `middleware.rs:167,169,174,177` → `core/src/rpc/consts.rs`
- **Dune constants:** `dune/client.rs:111,165`, `dune_find_blocks.rs:205,249,332,377` → `core/src/dune/consts.rs`
- **Discovery constants:** `discovery.rs:280-282,292,301` → `core/src/pool/discovery/consts.rs`

**Safety gate:** `cargo test` passes; no behavioral change.

---

### 3.2 Replace glob re-exports with explicit ones

**Files:** `types/mod.rs`, `error/mod.rs`, `config/mod.rs`, `pool/mod.rs`, `pool/math/mod.rs`, `mev/detectors/mod.rs`, `rpc/mod.rs`

**Smell:** `pub use submodule::*` in every `mod.rs`.

**Remediation:** Replace with selective re-exports. Example:
```rust
// Before
pub use chain::*;
pub use strategy::*;
pub use opportunity::*;
// After
pub use chain::ChainName;
pub use strategy::{Strategy, GasModel, GasConfig};
pub use opportunity::MevOpportunity;
```

**Safety gate:** `cargo build` passes; fix any new "unresolved import" errors.

---

### 3.3 Fix `debug_assert!` usage in public API

**Files:** `core/src/types/opportunity.rs:171-174,184-187,196-199`

**Smell:** `debug_assert!` only fires in debug builds; release silently produces garbage.

**Remediation:** Return `Result` from these methods or use `cfg!(debug_assertions)` with `tracing::warn!`.

**Safety gate:** `cargo test` passes.

---

### 3.4 Add `is_empty()` to `TokenCache`

**Files:** `core/src/cache/token_cache.rs:239-241`

**Smell:** `len()` implemented but `is_empty()` missing (clippy lint).

**Remediation:**
```rust
pub fn is_empty(&self) -> bool { self.inner.is_empty() }
```

**Safety gate:** `cargo clippy` passes.

---

### 3.5 Add structured error variants for RPC errors

**Files:** `core/src/error/rpc.rs`, `core/src/rpc/client.rs` (25+ `.map_err(|e| anyhow::anyhow!("{}", e))`)

**Smell:** Typed alloy errors flattened to opaque strings at 25+ call sites.

**Remediation:**
- Add structured fields to `RpcError` variants:
  ```rust
  pub enum RpcError {
      CallFailed { url: String, method: String, attempts: usize, source: anyhow::Error },
      NoProvidersAvailable,
      // ...
  }
  ```
- Remove dead variants (`AllProvidersFailed`, `RateLimited`) or wire them in.

**Safety gate:** `cargo test` passes.

---

### 3.6 Fix inconsistent return types in `ChainName`

**Files:** `core/src/types/chain.rs:97,128`

**Smell:** `default_uniswap_v2_factories()` returns `Vec<&'static str>` while V3 returns `&'static [&'static str]`.

**Remediation:** Unify to `&'static [&'static str]` for all factory methods.

**Safety gate:** `cargo build` passes; fix call sites.

---

### 3.7 Fix `_burned` misleading underscore prefix

**Files:** `core/src/mev/detectors/jit.rs:222`

**Smell:** `_burned: bool` parameter IS used in the function body but prefixed with `_`.

**Remediation:** Remove the underscore prefix.

**Safety gate:** `cargo clippy` passes.

---

### 3.8 Standardize error message formatting

**Files:** Multiple CLI files

**Smell:** Inconsistent error prefixing (`"Error: ..."` vs bare messages).

**Remediation:** Standardize on `anyhow::Context`:
```rust
// Before
bail!("Error: failed to connect: {}", e);
// After
e.context("Failed to connect to RPC")?;
```

**Safety gate:** `cargo test -p mev-scout-cli`.

---

<a id="phase-4"></a>
## Phase 4 — Nice-to-Have Cleanup (P4)

### 4.1 Remove dead code

> **Note:** Items verified by codebase-wide grep trace. Some items initially flagged
> were confirmed alive (see [Dead Code Audit](#dead-code-audit) for full traceability).

**Confirmed dead — safe to remove:**
- `core/src/config/defaults.rs:235-241` — `default_executor_addresses()` (returns empty HashMaps for every chain)
- `core/src/config/settings.rs:277-279` — `default_with_chains()` (one-liner delegating to `Config::default()`, never called)
- `core/src/config/validation.rs:265-267` — `_output` parsed into `OutputFormat` then immediately discarded
- `core/src/cache/store.rs:43-44` — `#[allow(dead_code)] chain_id: u64` (stored but never read)
- `core/src/pool/math/v3.rs:428-438` — `get_tick_spacing_from_fee` (`#[allow(dead_code)]`, no call sites)
- `core/src/pool/math/v3.rs:440-460` — `get_tick_at_sqrt_ratio` (`#[allow(dead_code)]`, no call sites)
- `core/src/mev/detectors/mempool.rs:465-496` — `parse_v3_exact_output` (`#[allow(dead_code)]`, never called)
- `core/src/pool/state/factory.rs:829-836` — `decode_i128_from_be_bytes` (`#[allow(dead_code)]`, no call sites)
- `core/src/pool/state/factory.rs:50-51` — `CURVE_PRICE_ORACLE_SELECTOR` (`#[allow(dead_code)]`, never referenced)
- `core/src/dune/types.rs:98-103` — `DuneTokenInfo` struct (defined, re-exported, but never constructed or referenced)
- `core/src/mev/detectors/liquidation.rs:30-33` — `FALLBACK_LTV_BPS` constant (`#[allow(dead_code)]`)
- `core/src/mev/detectors/liquidation.rs:148` — `LiquidationEvent.user` field (written at construction, never read)
- `core/src/pool/math/curve.rs:143` — `_x_in_new` variable (computed, never used)

**Corrected — NOT dead (initially flagged, confirmed alive):**
- ~~`optimal_two_hop_arb`~~ — **Used** in `two_hop.rs:185`
- ~~`eval_profit`~~ — **Used** internally by `optimal_two_hop_arb_generic`
- ~~`extract_selector`~~ — **Used** in `fetch/fetcher.rs:625`
- ~~`estimate_v3_exact_out`~~ — **Used** at `mempool.rs:537` (only `parse_v3_exact_output` is dead)
- ~~`DISCOVER_*` constants~~ — **Used** extensively in `dune_query.rs:606-646`
- ~~`BlockSnapshot` annotation~~ — Struct is used; annotation is misleading but not dead

**Safety gate:** `cargo build` and `cargo test` pass after each individual removal.

---

### 4.2 Move hardcoded data to external files

**Files:**
- `core/src/config/defaults.rs:45-231` — chain configs → `data/chains.toml`
- `core/src/cache/token_cache.rs:127-212` — warm token list → `data/known_tokens.json`
- `core/src/sigs/downloader.rs:50-487` — signature hex pairs → `data/signatures.csv` (loaded via `include_str!`)
- `core/src/types/pool_types.rs:10-34` — FoT/rebase token lists → `data/fot_tokens.json`

**Safety gate:** `cargo test` passes; `include_str!` makes these still compile-time verified.

---

### 4.3 Unify V3/V4 tick crossing logic

**Files:** `core/src/pool/math/v3.rs:568-632` vs `703-766`

**Smell:** Tick-crossing and liquidity update logic (~30 lines) duplicated between exact-in and exact-out.

**Remediation:** Extract `fn cross_tick(state, tick) -> (i32, u128)` helper.

**Safety gate:** `cargo test -p mev-scout-core`.

---

### 4.4 Extract `row_to_log` helper in SqliteStore

**Files:** `core/src/cache/store.rs:612-627` vs `642-657`

**Smell:** Identical 15-line row→`NormalizedLog` mapping in two functions.

**Remediation:** Extract `fn row_to_log(row: &Row) -> rusqlite::Result<NormalizedLog>`.

**Safety gate:** Existing tests pass.

---

### 4.5 Fix f64 precision in Balancer math

**Files:** `core/src/pool/math/balancer.rs:46-47,55-62`

**Smell:** `1e18 as u128` = 999999999999999872 (not 10^18); `as_limbs()[0] as f64` truncates U256.

**Remediation:**
- Use `1_000_000_000_000_000_000u128` directly.
- For the power operation, document precision bounds or use integer exponentiation.

**Safety gate:** `cargo test`; compare output against known Balancer pools.

---

### 4.6 Extract `epoch_secs()` helper

**Files:** `main.rs:54-57`, `run.rs:42-45,53-56,191-194`

**Smell:** `SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()` repeated 5+ times.

**Remediation:**
```rust
pub fn epoch_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
```

**Safety gate:** `cargo test`.

---

### 4.7 Use `strum` for enum Display/FromStr

**Files:** `core/src/types/strategy.rs` (7 enums with manual Display/FromStr)

**Smell:** ~200 lines of repetitive boilerplate.

**Remediation:** Add `strum = "0.26"` dependency; derive `#[derive(Display, EnumString)]`.

**Safety gate:** `cargo test`; verify CLI parsing unchanged.

---

<a id="phase-5"></a>
## Phase 5 — Missing Smell Items (P3–P4)

> Items identified in the full audit but not covered by Phases 0–4.

### 5.1 Deduplicate coingecko cache-check-fetch pattern

**Files:** `core/src/coingecko.rs:82-111` (`usd_price`) vs `184-211` (`token_usd`)

**Smell:** Both methods follow identical flow: check cache → fetch → store → fallback to stale.

**Remediation:** Extract generic `get_or_fetch(&mut self, key, fetch_fn)` helper.

---

### 5.2 Deduplicate coingecko HTTP request setup

**Files:** `core/src/coingecko.rs:214-236` vs `239-263`

**Smell:** `fetch_native_price` and `fetch_token_price` build identical `reqwest::Request` + API key header + status check + JSON parse.

**Remediation:** Extract `fn execute_price_request(&self, url: &str) -> Result<f64>`.

---

### 5.3 Make `PriceCache` concurrency-safe

**Files:** `core/src/coingecko.rs:82,121,184` — all `&mut self`

**Smell:** Taking `&mut self` for cache mutation prevents sharing across async tasks.

**Remediation:** Use `tokio::sync::Mutex` or `DashMap` for the entries HashMap.

---

### 5.4 Deduplicate RPC URL merging in Config

**Files:** `core/src/config/settings.rs:285-298` vs `392-405`

**Smell:** `effective_rpc_urls()` and `user_rpc_urls()` contain nearly identical clone+dedup logic.

**Remediation:** Extract shared `fn merge_rpc_urls(base: &[String], extra: &Option<String>) -> Vec<String>`.

---

### 5.5 Fix config validation gaps

**Files:** `core/src/config/validation.rs:162-208`, `settings.rs:564-580`

**Smell:** `validate_replay` reimplements range-conflict detection; `parse_token_prices` silently ignores invalid entries.

**Remediation:** `validate_replay` should call `resolve_block_range` first; `parse_token_prices` should log warnings for unparseable pairs.

---

### 5.6 Unify V3/V4 in `init_from_rpc` result handling

**Files:** `core/src/pool/state/factory.rs:232-262`

**Smell:** `V3State` and `V4State` match arms are ~15 lines of near-identical result processing.

**Remediation:** Extract shared `fn process_concentrated_result(state: ConcentratedLiquidityState)`.

---

### 5.7 Convert `PoolInitResult` tuple variants to named structs

**Files:** `core/src/pool/state/factory.rs:13-26`

**Smell:** `BalancerState` has 9 unlabeled tuple fields; `CurveState` has 8. Reading `result.3` is opaque.

**Remediation:** Convert to named struct fields (e.g., `BalancerState { amp, balances, tokens, ... }`).

---

### 5.8 Deduplicate integrity/missing-block queries in SqliteStore

**Files:** `core/src/cache/store.rs:681-685`, `728-749`, `751-782`

**Smell:** Three functions (`missing_blocks_in_range`, `check_integrity`, `check_integrity_range`) all query cached blocks in a range and find the complement.

**Remediation:** Unify into `fn find_uncached_blocks(range: impl IntoIterator<Item = u64>) -> Vec<u64>`.

---

### 5.9 Deduplicate `access_list` serialization in SqliteStore

**Files:** `core/src/cache/store.rs:386-390`, `487-491`, `1214-1218`

**Smell:** `if tx.access_list.is_empty() { None } else { Some(Self::serialize(&tx.access_list)?) }` appears 3 times.

**Remediation:** Extract `fn serialize_access_list(list: &[AccessListItem]) -> Option<Vec<u8>>`.

---

### 5.10 Deduplicate `DiscoveredPool` construction

**Files:** `core/src/pool/discovery.rs` (9 occurrences), `core/src/dune/pool_discovery.rs` (2 occurrences)

**Smell:** ~11 near-identical struct literals; adding a field requires editing all 11.

**Remediation:** Create `DiscoveredPool::new(address, token0, token1, fee, dex_type, creation_block)` constructor with chained setters for optional fields.

---

### 5.11 Deduplicate `apply.rs` V2 Solidly/Camelot cross-type handling

**Files:** `core/src/pool/state/apply.rs:23-33`, `37-49`

**Smell:** Both `apply_v2_swap` and `apply_v2_sync` have a secondary `if let PoolState::Curve` branch to handle Solidly/Camelot pools stored as CurvePoolState. Cross-type mutation is fragile.

**Remediation:** Consider a unified pool trait or a dedicated Solidly variant in `PoolState`.

---

### 5.12 Add `apply_v3_mint_burn` support for V4

**Files:** `core/src/pool/state/apply.rs:113-129`

**Smell:** `apply_v3_mint_burn` only handles `PoolState::UniswapV3`, not `UniswapV4`. V4 pools also have ticks and liquidity.

**Remediation:** Add `PoolState::UniswapV4` arm (trivial after 1.6/1.7 unify the types).

---

### 5.13 Deduplicate Dune fetch functions in audit.rs

**Files:** `core/src/dune/audit.rs:121-207`

**Smell:** `fetch_sandwiches_from_dune`, `fetch_arbitrages_from_dune`, `fetch_flash_loans_from_dune` share identical structure: call SQL → match result → iterate rows → push struct.

**Remediation:** Generic `fetch_events<T: DeserializeOwned>(client, sql) -> Vec<T>` helper.

---

### 5.14 Replace `eprintln!` with `tracing` in Dune client

**Files:** `core/src/dune/client.rs:129,182`

**Smell:** Rate-limit logging uses `eprintln!` instead of `tracing::warn!`, bypassing structured logging.

**Remediation:** Replace with `tracing::warn!(...)`.

---

### 5.15 Fix `_burned` misleading underscore prefix

**Files:** `core/src/mev/detectors/jit.rs:222`

**Smell:** `_burned: bool` IS used at line 290 but prefixed with `_`, misleading readers and clippy.

**Remediation:** Remove underscore prefix.

---

### 5.16 Deduplicate `discovery.rs` log processing blocks

**Files:** `core/src/pool/discovery.rs:700-750`

**Smell:** Fast-path and full-topic-set fallback branches contain identical log-processing loop bodies.

**Remediation:** Extract `fn process_discovery_log(log, active_blocks, pool_hits)`.

---

### 5.17 Deduplicate `replay.rs` EVM setup and TX execution

**Files:** `core/src/replay/replayer.rs:452-454,549-551,626-628` (EVM setup) and `465-496,563-585,640-672` (TX loop)

**Smell:** Polygon precompile registration + EVM context setup repeated in all 3 replay methods; TX execution loop also triplicated.

**Remediation:** Extract `setup_evm(block_num) -> (CacheDB, Evm)` and a shared TX iteration helper.

---

### 5.18 Deduplicate `DatabaseRef` vs `Database` impl in replay/db.rs

**Files:** `core/src/replay/db.rs:267-349` vs `135-265`

**Smell:** ~80 lines of near-duplicate lookup logic between the two trait impls.

**Remediation:** Implement `Database` in terms of `DatabaseRef` or extract shared lookup helper.

---

### 5.19 Deduplicate `discovery.rs` `merge_from` manual field copy

**Files:** `core/src/pool/discovery.rs:151-196`

**Smell:** 14 fields manually copied with `if self.X.is_none()` checks.

**Remediation:** Use a `merge_option!` macro or `typed-builder` crate.

---

### 5.20 Fix inconsistent `StaticLock` vs `const` usage in decoders

**Files:** `core/src/pool/decoders.rs:9-25`

**Smell:** Some topic constants are `const` with `b256!()`, others use `LazyLock` for no apparent reason.

**Remediation:** Make all topic constants `const` with `b256!()` (keccak256 of string literals is compile-time computable).

---

### 5.21 Fix `dune/pool_discovery.rs` leading space bug

**Files:** `core/src/dune/pool_discovery.rs:238`

**Smell:** `" Ramses"` has a leading space — string comparison will fail for `"ramses"` (lowercase input from line 211).

**Remediation:** Remove leading space.

---

### 5.22 Deduplicate `call` / `call_latest` in RPC client

**Files:** `core/src/rpc/client.rs:1090-1105` vs `1113-1128`

**Smell:** Both build identical `TransactionRequest`, only differ by block tag.

**Remediation:** Extract `fn call_at(to, data, block) -> Result`.

---

### 5.23 Deduplicate `get_gas_price` / `get_max_priority_fee`

**Files:** `core/src/rpc/client.rs:1133-1143` vs `1148-1158`

**Smell:** Identical structure: raw U256 request → map_err → to::<u128>.

**Remediation:** Extract `fn fetch_u128_metric(method: &str) -> Result<u128>`.

---

### 5.24 Fix `ProviderState` encapsulation

**Files:** `core/src/rpc/middleware.rs:117-132`

**Smell:** 11 public fields; `client.rs` directly mutates `is_alive`, `cooldown_until`, `consecutive_failures`, bypassing invariant-enforcing methods.

**Remediation:** Make fields private; expose via methods only.

---

### 5.25 Fix mixed `std::time::Instant` / `tokio::time::Instant`

**Files:** `core/src/rpc/middleware.rs:123` (std) vs `24` (tokio)

**Smell:** Two different `Instant` types used for timing in the same module.

**Remediation:** Standardize on `tokio::time::Instant` for async code.

---

### 5.26 Add `PoolState::info()` method

**Files:** `core/src/pool/state/pool_types.rs:444-479`, `cli/src/display.rs:52-60`

**Smell:** Three methods (`address()`, `info()`, `info_mut()`) each have a 7-arm match accessing `.info`. Display code also manually matches every variant to extract `&s.info`.

**Remediation:** Add `fn info(&self) -> &PoolInfo` (and `info_mut`) to `PoolState` directly.

---

### 5.27 Deduplicate `PoolManager` V3/V4 price calculation

**Files:** `core/src/pool/state/manager.rs:365-414`

**Smell:** V3 and V4 branches contain identical sqrt-price-to-price calculations (~25 lines each).

**Remediation:** Since V4 converts to V3 state via `From`, use the V3 branch for both.

---

### 5.28 Deduplicate `persistence` serialization pattern in replay/db.rs

**Files:** `core/src/replay/db.rs:59-69`

**Smell:** `CachedRpcDb` has 8 fields including 4 HashMaps that must all be cleared in `set_block_number`.

**Remediation:** Group caches into a `CacheState` struct with a single `clear()` method.

---

<a id="dead-code-audit"></a>
## Dead Code Audit

> Full traceability of every item flagged as dead/unused. Verified by codebase-wide
> grep. Items are categorized as **Confirmed Dead** (safe to remove), **Misleadingly
> Named** (alive but poorly named), or **Alive** (initially flagged, verified used).

### Confirmed Dead — Safe to Remove

| Item | File:Line | Evidence |
|------|-----------|----------|
| `default_executor_addresses()` | `config/defaults.rs:235` | No call sites anywhere |
| `default_with_chains()` | `config/settings.rs:277` | No call sites anywhere |
| `_output` parsed then discarded | `config/validation.rs:265` | Bound to `_`, never used |
| `chain_id` field (`#[allow(dead_code)]`) | `cache/store.rs:44` | Stored at line 64, never read |
| `get_tick_spacing_from_fee` | `pool/math/v3.rs:428` | `#[allow(dead_code)]`, no call sites |
| `get_tick_at_sqrt_ratio` | `pool/math/v3.rs:441` | `#[allow(dead_code)]`, no call sites |
| `parse_v3_exact_output` | `mev/detectors/mempool.rs:466` | `#[allow(dead_code)]`, no call sites |
| `decode_i128_from_be_bytes` | `pool/state/factory.rs:830` | `#[allow(dead_code)]`, no call sites |
| `CURVE_PRICE_ORACLE_SELECTOR` | `pool/state/factory.rs:51` | Defined, never referenced |
| `DuneTokenInfo` | `dune/types.rs:98` | Defined, re-exported, never constructed |
| `FALLBACK_LTV_BPS` | `mev/detectors/liquidation.rs:33` | `#[allow(dead_code)]`, no references |
| `LiquidationEvent.user` field | `mev/detectors/liquidation.rs:148` | Written at construction, never read |
| `_x_in_new` variable | `pool/math/curve.rs:143` | Assigned, never used afterward |
| `RpcError::AllProvidersFailed` | `error/rpc.rs:8` | Defined, never constructed |
| `RpcError::RateLimited` | `error/rpc.rs:10` | Defined, never constructed |
| `EtagStore` | `rpc/middleware.rs:85` | Fully implemented, never instantiated |
| `cross_validate_opportunities()` | `dune/cross_validate.rs` | Function defined, never called from anywhere |

### Alive — Incorrectly Flagged (Do NOT Remove)

| Item | File:Line | Actually Used At |
|------|-----------|-----------------|
| `optimal_two_hop_arb` (non-generic) | `pool/math/core.rs:154` | `mev/detectors/two_hop.rs:185` |
| `eval_profit` | `pool/math/core.rs:231` | Internal to `optimal_two_hop_arb_generic` (lines 259–284) |
| `extract_selector` | `rpc/client.rs:1164` | `fetch/fetcher.rs:625` |
| `estimate_v3_exact_out` | `mev/detectors/mempool.rs:501` | `mempool.rs:537` (the `0xf28c0498` branch) |
| `DISCOVER_*` constants | `dune/queries.rs:2033+` | `dune_query.rs:606-646` (used via `queries::DISCOVER_*`) |
| `BlockSnapshot` struct | `cross_block.rs:26` | Used throughout `cross_block.rs` (lines 21, 56, 202) |
| `pool_hits` | `discovery.rs` | Used in shard merge and final output (lines 596–1613) |
| `CURVE_*_UNDERLYING` topics | `discovery.rs:92,95` | `discovery.rs:434-435,666-667` + `pipeline/scanner.rs:49-51` |

### Misleadingly Named — Alive But Poorly Named

| Item | File:Line | Issue |
|------|-----------|-------|
| `_burned` parameter | `jit.rs:223` | Prefixed `_` but IS used at line 290 — remove underscore |
| `_expected_chain_id` param | `rpc/client.rs:491` | Unused param — should be removed, not just prefixed |
| `_gas_config` param | `cross_block.rs:96` | Unused param — should be removed |
| `_token_in` / `_token_out` params | `cross_block.rs:205-206` | Unused params — should be removed |
| `_tick_spacing` param | `pool/math/v3.rs:653` | Unused param — should be removed |

---

<a id="unreachable-core-code"></a>
## Unreachable / Orphaned Core Code

> Code that exists in `core/src/` but is **not reachable from any CLI command**.
> The CLI layer was verified to have 100% dispatch coverage (12 enum variants → 12
> match arms → 12 module imports → 12 `cmd_*` functions). However, some core library
> functions are never called from any CLI path.

### Confirmed Orphaned — No CLI Command Reaches These

| Function/Type | File:Line | Notes |
|---------------|-----------|-------|
| `cross_validate_opportunities()` | `dune/cross_validate.rs` | No caller anywhere in the codebase. The `DuneTradeCheck` type is also only used within this unreachable function. |
| `DuneSandwichEvent` | `dune/audit.rs:43` | Used within `audit.rs` internally, but `audit.rs` functions are only called from `cmd_audit`. The type itself has no external consumers beyond `audit.rs`. |

### Partially Reachable — Only One Code Path Exercises These

| Function | File:Line | Reachable From |
|----------|-----------|---------------|
| `DuneClient::col_as_*` utilities | `dune/client.rs:237-275` | Only via `dune/pool_discovery.rs`, `dune/token_discovery.rs`, `dune/audit.rs` — never directly from CLI |
| `resolve_method` / `resolve_event` | `sigs/resolver.rs:56-108` | Only via `pipeline/runner.rs` during backtest runs — `run` command only |
| `fetch_relevant` | `fetch/fetcher.rs` | Only via `commands/fetch.rs` — `fetch` command only |

### Data-Only Code — Present But Not Computationally Active

| Item | File:Line | Notes |
|------|-----------|-------|
| `DISCOVER_*` constants | `dune/queries.rs:2033-2172` | These ARE referenced by `dune_query.rs` via `queries::DISCOVER_*`, but they are debug/exploration queries likely only used interactively via `--query DISCOVER_*`. They are not part of any automated pipeline. Consider gating behind `#[cfg(test)]` or a feature flag. |
| `default_executor_addresses()` | `config/defaults.rs:235` | Returns empty HashMaps — even if called, it provides no data. |

### CLI Coverage Summary

| Check | Result |
|-------|--------|
| All 12 `Command` variants dispatched? | ✅ Yes |
| All 12 `cmd_*` functions exist? | ✅ Yes |
| All 12 module files imported in `mod.rs`? | ✅ Yes |
| Any orphaned `.rs` files? | ✅ None found |
| Any dead helper functions in CLI? | ✅ None found |
| `Report` command fully implemented? | ✅ Yes (88 lines, 3 output formats) |
| `Config` command fully implemented? | ✅ Yes (intentionally minimal — 7 lines) |

**Bottom line:** The CLI dispatch layer is clean. Dead code is concentrated in
`core/src/` library code, particularly in `dune/cross_validate.rs` (entire module
unreachable) and various `#[allow(dead_code)]` utility functions that were likely
written for future use but never wired in.

---

| Risk | Mitigation |
|------|-----------|
| Behavioral regression from refactoring | Every phase includes a safety gate with `cargo test`; Phase 1–2 changes also require manual testing against live RPC or a known-good block |
| TOML config breakage from Config decomposition (2.1) | Use `#[serde(flatten)]` for backward compatibility; add a test that parses existing `mev-scout.toml` |
| V3/V4 type unification breaks pool quoting (1.6) | Run backtest on a block containing both V3 and V4 swaps; compare profit numbers pre/post |
| SQLite migration breaks existing DBs (2.4) | Test with an existing DB file; verify `schema_version` is set after migration |
| Dead code removal breaks hidden dependencies (4.1) | Each removal is a separate commit; `cargo build` after each |

---

## Tracking

| Phase | Items | Est. Effort | Status |
|-------|-------|-------------|--------|
| Phase 0 | 0.1–0.4 | 1–2 days | Not started |
| Phase 1 | 1.1–1.10 | 3–5 days | Not started |
| Phase 2 | 2.1–2.8 | 5–8 days | Not started |
| Phase 3 | 3.1–3.8 | 2–3 days | Not started |
| Phase 4 | 4.1–4.7 | 2–3 days | Not started |
| Phase 5 | 5.1–5.28 | 4–6 days | Not started |
| **Total** | **65 items** | **17–27 days** | |
