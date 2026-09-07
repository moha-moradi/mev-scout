# Dead Code Cleanup Plan

**Goal:** Remove confirmed dead code (~1,230–1,340 lines), deduplicate gas constants and event
topic tables, shrink the public API surface, and add guardrails so dead code cannot silently
accumulate again. (Original estimate was ~800–900 lines; the review found ~250 more; a second
pass found ~180 more — mainly the mempool `PendingPoolEffect` subsystem.)

**Method behind this plan:** Every item below was verified by cross-referencing the definition
against *all* call sites in the workspace (core lib, CLI bin, `core/tests`, `cli/tests`,
`core/examples`). "No callers" means: no `rg` match for the identifier outside its own
definition, its re-export lines, and its own in-file unit tests.

> **Review note (2026-09):** A reviewer re-verified this plan against the codebase and ran
> `cargo clippy --workspace --all-targets -- -W dead_code -W unused` as a cross-check. Two
> important findings shape the plan:
> 1. **The compiler cannot catch dead `pub` items in a lib crate** — `mev-scout-core`'s 71 lib
>    warnings are all style lints; zero dead-code warnings surfaced for public API items. So the
>    `rg`-based method here is the *only* way to find this dead code; the items below were all
>    individually re-confirmed.
> 2. The clippy dead-code noise that *does* show up is all in shared test helpers
>    (`core/tests/common/setup.rs`, `cli/tests/common/mod.rs`) — but those are per-test-binary
>    warnings, not global dead code (each binary compiles the shared module separately). Do not
>    mass-delete those during this cleanup.
> All items added by the review are tagged `(review)` so they are distinguishable from the
> original list.

**Ground rules:**
- Execute phase by phase. Build + test after **every phase** before moving on.
- Do **not** delete anything marked `KEEP` (has a caller or a concrete future plan).
- Prefer deletion over `#[allow(dead_code)]`. If an item is genuinely needed later, git
  history preserves it.
- Items tagged `DECISION` need a product call (delete vs. wire up) before touching.
- **(third-pass)** File line numbers have drifted since this plan was written (e.g.
  `parse_token_prices` is now at `settings.rs:683`, not `:632`; the `merge_cli` entries
  shifted by roughly 50 lines). **Trust identifiers, not line numbers** — re-locate each
  item by name (`rg "\b<ident>\b"`) right before deleting it.

---

## Phase 0 — Baseline (do first)

Confirm the current state is green and capture compiler evidence for the items below.

```powershell
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -W dead_code -W unused 2>&1 | Tee-Object clippy-baseline.txt
cargo test -p mev-scout-core --test config --test sandwich --test liquidation --test arbitrage
cargo test -p mev-scout-cli --test cli_args
```

Note: `core/tests/e2e.rs`, `backtest.rs`, `replay.rs` hit live RPC by default (they fall back
to the first URL in `mev-scout.toml`). Phase 6 makes them opt-in; until then skip them or set
a reachable `RPC_URL`.

- [x] Baseline build green
- [x] Baseline tests green
- [x] `clippy-baseline.txt` saved

---

## Phase 1 — Orphaned file & gas constant deduplication (zero risk)

### 1.1 Delete the orphaned module `core/src/mev/gas.rs` entirely

Evidence: `core/src/mev/mod.rs` never declares `mod gas;` — this file is **not even compiled**.
It contains `FLASH_LOAN_OVERHEAD_GAS`, `estimate_base_gas()`, `estimate_multi_swap_gas()`, and
duplicate copies of `BASE_TX_GAS`, `DEFAULT_POOL_GAS`, `JIT_OVERHEAD`, `LIQUIDATION_GAS_LIMIT`
(the canonical copies live in `core/src/pool/math/consts.rs:12-17` and are what the codebase
actually uses).

- [x] Delete `core/src/mev/gas.rs` — done (2026-09-07); `cargo build` confirmed the orphan claim
      (build stayed green; `calldata_gas_estimate` untouched).

> **(review)** File is only ~20 lines. Note: it imports `crate::pool::state::calldata_gas_estimate`
> — that function is **used** (detectors/jit.rs:290, jit_arb.rs:231, liquidation.rs:473,
> multi_hop.rs:968, two_hop.rs:902, sandwich.rs:467) and must NOT be removed; it is unrelated to
> this orphaned file.

### 1.2 Deduplicate `LIQUIDATION_GAS_LIMIT` in `core/src/mev/detectors/liquidation.rs:36`

The file re-declares a private `const LIQUIDATION_GAS_LIMIT: u64 = 180_000;` even though
`crate::pool::math::consts::LIQUIDATION_GAS_LIMIT` is identical and already used everywhere else.

- [x] Replace the local const with `use crate::pool::math::consts::LIQUIDATION_GAS_LIMIT;`
      — done; local const removed, import added to the existing `consts` use list (2026-09-07).

**Verify:** `cargo build --workspace --all-targets` (deleting an uncompiled file cannot break
the build — this also proves the orphan claim; if the build breaks, stop and re-check).

---

## Phase 2 — Dead modules (each needs a DECISION)

### 2.1 `core/src/coingecko.rs` (~220 lines) — DECISION: delete vs. wire up

Evidence: `PriceCache`, `usd_price()`, `token_usd()`, `resolve_native_price()`, `with_ttl()`
have **no callers anywhere**; nothing imports `mev_scout_core::coingecko`. The whole CoinGecko
integration was never wired into the pipeline. Dead plumbing attached to it:

| Item | Location | Action if deleting |
|---|---|---|
| `pub mod coingecko;` | `core/src/lib.rs:7` | remove |
| `PriceOracleMode` enum | `core/src/types/strategy.rs:347` | remove |
| `PriceSource` enum (never referenced) | `core/src/types/strategy.rs:324-331` | remove |
| `ExecutorType` + `from_strategy()` (self-referencing only) | `core/src/types/strategy.rs:397-417` | remove |
| `onchain_native_price()` (only caller was coingecko) | `core/src/pool/state/manager.rs:879` | remove |
| `rpc.coingecko_api_key` field + default + env-expansion block + `BacktestOverrides`/`merge_cli` entry + the `expands_plain_urls_and_coingecko_key_unchanged` test fixture | `core/src/config/settings.rs` (~34-36, 127, 255, 292-293, 514, 656, 738-749 — **drifted, locate by name**) | remove (see third-pass note below) |
| `backtest.price_oracle_mode` field + default + `merge_cli` entry | `core/src/config/settings.rs` (~72-74, 151, 531, 669 — **drifted, locate by name**) | remove |

⚠️ Removing config fields changes the TOML schema for users who have them set (serde with
`#[serde(default)]` tolerates *unknown* fields only if `deny_unknown_fields` is NOT set —
verified: no `deny_unknown_fields` anywhere in `core/src/config`, so removal is non-breaking
for old TOML files).
Also note a latent bug if wiring up later: `resolve_onchain_price()` hardcodes Ethereum
mainnet stable-token addresses regardless of the `chain` parameter (coingecko.rs:191-195).

**Alternative (wire up):** only worth it if USD pricing becomes a near-term feature; otherwise
delete — git history keeps it.

⚠️ **DECISION context (review):** the explorer planning docs name `core/src/coingecko.rs`
as the explorer's USD-pricing source. ⚠️ **(third-pass)** `docs/EXPLORER_PLAN.md` and
`docs/EXPLORER_EXECUTION_PLAN.md` no longer exist — they were consolidated (commit `63e9f6f`)
into **`docs/EXPLORER_UNIFIED_PLAN.md`**, which still names `coingecko.rs` (lines ~208, ~444,
~1186) and `pipeline/aggregate.rs` (lines ~110, ~173, ~1178). The decision logic is unchanged.
This is the same "reserved for explorer" treatment the plan grants `pipeline/aggregate.rs`
(Phase 2.2). If the explorer command is moving forward, mark coingecko.rs `KEEP` (reserved)
instead of deleting — otherwise Phase 2.2 and this deletion are inconsistent. If deleting,
everything inside coingecko.rs goes with it (including `PriceEntry`, `coingecko_asset_id`,
`coingecko_platform`, `get_or_fetch`), and remove the `PriceOracleMode`/`PriceSource`/
`ExecutorType` entries from the `types/mod.rs:9-10` re-export list.

**(third-pass) coingecko deletion has two extra coupling points the table above misses:**
- `core/src/config/settings.rs` env-expansion: the `expand_secrets` block expands
  `rpc.coingecko_api_key` (~lines 292-293) and a doc comment above `expand_secrets`
  (~line 255) lists the key — both must go with the field.
- `core/src/config/settings.rs` unit test `expands_plain_urls_and_coingecko_key_unchanged`
  (~lines 738-749) writes `coingecko_api_key = "CG-1"` into a TOML fixture and asserts on
  it — the test will not compile/pass after field removal; rewrite the fixture without
  that key (keep the URL-expansion assertions).

- [x] Decision recorded: **delete** (2026-09-07; product call — explorer plan's USD pricing is
      deferred alongside the `explorer` command; git history retains coingecko.rs)
- [x] Executed per decision: `coingecko.rs` deleted; `lib.rs` module line, `PriceSource`/
      `PriceOracleMode`/`ExecutorType` enums + `from_strategy`, `types/mod.rs` re-exports,
      `onchain_native_price`, `rpc.coingecko_api_key` (field/default/env-expansion/override/
      merge/test), `backtest.price_oracle_mode` (field/default/override/merge) all removed.
      Build + tests green.

### 2.2 `core/src/pipeline/aggregate.rs` (~360 lines) — KEEP for now (explorer plan)

Evidence: `aggregate()`, `aggregate_with_prices()`, `AggregationResult`, `DexMeta`,
`DexMetrics`, `StrategyMetrics`, `SummaryMetrics` are only re-exported in
`core/src/pipeline/mod.rs:6-9`; no caller exists today. **However** `docs/EXPLORER_UNIFIED_PLAN.md`
(lines ~110, ~173, ~1178; formerly `EXPLORER_PLAN.md` — see 2.1) names `pipeline/aggregate.rs`
as the aggregation layer for the planned `explorer` command.

Action: **keep**, but mark as intentionally-unused so it doesn't trip the Phase 7 guardrail:

- [x] Add `#![allow(dead_code)]` marker at top of `aggregate.rs` with explanatory comment
      — done (2026-09-07).

### 2.3 `core/src/cache/store/pending.rs` + `pending_txs` table — DECISION: delete vs. keep

Evidence: `put_pending_txs()`, `count_pending_txs()`, `total_pending_txs()` have no callers.
The `pending_txs` table is created in `core/src/cache/store/mod.rs:212-227` but never written
or read (mempool processing (`mempool.rs::capture_pending_block` + runner) is in-memory only).

- **Delete:** remove `pending.rs`, remove `pub mod pending;` (`store/mod.rs:12`), remove the
  `pending` entry from the re-export list in `cache/mod.rs:4`, and remove the
  `CREATE TABLE IF NOT EXISTS pending_txs (...)` block from `mod.rs`. Existing DBs keep the
  orphan table harmlessly (no migration needed).
- **Keep:** only if persisting mempool captures is a near-term goal (live mode currently
  discards them on restart).

- [x] Decision recorded: **delete** (2026-09-07)
- [x] Executed per decision: `pending.rs`, `pub mod pending;`, `cache/mod.rs` re-export, and
      the `pending_txs` CREATE TABLE all removed. Build + tests green.

---

## Phase 3 — Dead methods & functions (mechanical removals)

All items verified caller-free (apart from their own in-file tests, which get removed too).
Remove in small groups and build after each group.

| # | Item | Location | Notes |
|---|---|---|---|
| 3.1 | `MevOpportunity::with_canonical_id()` | `core/src/types/opportunity.rs:157` | runner sets `canonical_id` directly via `compute_canonical_id` (runner.rs:524,709) |
| 3.2 | `MevOpportunity::with_jit_fields()` | `core/src/types/opportunity.rs:173` | |
| 3.3 | `MevOpportunity::with_sandwich_fields()` | `core/src/types/opportunity.rs:190` | |
| 3.4 | `MevOpportunity::with_path()` | `core/src/types/opportunity.rs:205` | |
| 3.5 | `GasPriceDistribution::forecast_base_fee()` | `core/src/pipeline/gas.rs:84` | |
| 3.6 | `GasPriceDistribution::clear()` | `core/src/pipeline/gas.rs:104` | |
| 3.7 | `BacktestRunner::with_persistence_scoring()` | `core/src/pipeline/runner.rs:146` | field stays (default `true` used by run_block/sync_block_from_logs) |
| 3.8 | `BacktestRunner::with_aave_reserve_cache()` | `core/src/pipeline/runner.rs:116` | field stays (populated via `prefetch_aave_reserves`) |
| 3.9 | `BacktestRunner::aave_reserve_cache()` getter | `core/src/pipeline/runner.rs:152` | |
| 3.10 | `BlockReplayer::replay_block()` | `core/src/replay/replayer.rs:585` | |
| 3.11 | `BlockReplayer::replay_each()` | `core/src/replay/replayer.rs:601` | runner uses `replay_each_filtered`; **(third-pass)** also update the module doc at `replayer.rs:201`, which still describes `replay_each()` |
| 3.12 | `StateSnapshot` struct + `new/db/db_mut/fork` | `core/src/replay/replayer.rs:743-767` | also remove from re-export in `core/src/replay/mod.rs:5` and the module doc mention (replayer.rs:11) |
| 3.13 | `quote_v3_exact_out()` | `core/src/pool/math/v3.rs:756` | **DONE (2026-09-07):** removed `quote_v3_exact_out`, plus `compute_swap_step_exact_out` (was **only** used by it — plan's original note was imprecise) and `get_next_sqrt_price_from_output` (also orphaned) in `v3.rs`; kept `get_swap_target_for_tick` / `find_next_initialized_tick` (genuinely shared with `quote_v3_exact_in`); removed from re-exports in `pool/math/mod.rs:31` and `pool/mod.rs:15` |
| 3.14 | `optimal_n_hop_generic()` | `core/src/pool/math/core.rs:386` | remove re-exports in `pool/math/mod.rs:22` and `pool/mod.rs:13` |
| 3.15 | `v2_router_for_factory()` | `core/src/types/chain.rs:258` | doc claims "Used by M3" — no such caller; remove re-export in `types/mod.rs:5` |
| 3.16 | `ChainName::public_rpc_urls()` | `core/src/types/chain.rs:146` | `public_rpc_url()` + `public_rpc_endpoints()` are the used ones |
| 3.17 | `RangeResolver::rpc_client()` | `core/src/resolver.rs:44` | |
| 3.18 | `Fetcher::rpc_client()` | `core/src/fetch/fetcher.rs:123` | `cache_store()` has a test caller — keep |
| 3.19 | `SqliteStore::get_logs_for_block()` | `core/src/cache/store/blocks.rs:315` | **DONE (2026-09-07):** after removing 3.19/3.20, the shared private helper `row_to_normalized_log` (`store/mod.rs:419`) also became orphaned and was removed |
| 3.20 | `SqliteStore::get_logs_for_tx()` | `core/src/cache/store/blocks.rs:329` | |
| 3.21 | `SqliteStore::get_cached_blocks_in_range()` | `core/src/cache/store/blocks.rs:346` | `missing_blocks_in_range()` / `has_block()` are used — keep |
| 3.22 | `SqliteStore::count_discovered_pools()` | `core/src/cache/store/pools.rs:94` | |
| 3.23 | `SqliteStore::put_discovery_cursor()` / `get_discovery_cursor()` | `core/src/cache/store/pools.rs:102,111` | `discovery_cursors` table (mod.rs:178) becomes unused → remove the CREATE TABLE too |
| 3.24 | `TokenCache::save_one()` | `core/src/cache/token_cache.rs:190` | |
| 3.25 | `TokenCache::get_full()` | `core/src/cache/token_cache.rs:110` | |
| 3.26 | `TokenCache::missing()` | `core/src/cache/token_cache.rs:138` | only its own test uses it — remove test too |
| 3.27 | `block_timestamp_secs()`, `estimate_latest_block()` | `core/src/chain/timing.rs:35,45` | in-file tests removed too; **keep `chain_timing()`** — used by `core/tests/backtest.rs:71` |

### Group D — additional dead code found during review (all verified caller-free) `(review)`

> **Why it's safe to delete the DEX-related entries (3.28/3.29) — verified during review:**
> the DEX feature surface is **already complete and fully wired**; the dead constants are pure
> duplication from an older architecture, not unfinished features:
> - **Pool discovery for every supported DEX is wired** — `pool/discovery/{v2,v3,v4,balancer,curve,
>   solidly,camelot,trader_joe,pendle}.rs` are all called from `discover_pools` (discovery/mod.rs),
>   and `pool/discovery/mod.rs` owns the canonical *pair-creation* topics (incl. Algebra/QuickSwap
>   V3, which `chain/events.rs` doesn't even have).
> - **Swap/Mint/Burn decoding is wired** — `pool/decoders.rs` (V3 mint/burn, Balancer swap, Curve
>   swap) is used by `jit.rs`, `jit_arb.rs`, `sandwich.rs`, `apply.rs`, `manager.rs`; the
>   `chain/events.rs` swap decoders are used by `chain/{trades,flashloans,liquidations,transfers}.rs`
>   → CLI `scan`.
> - **DEX quote math is wired** — `math/lb.rs`, `math/pendle.rs`, `math/stable_swap.rs`,
>   `math/curve.rs`, `math/balancer.rs` are all reachable via `quote_exact_in` (math/core.rs:110,120).
>
> Deleting the duplicated consts loses **zero** functionality. The genuinely-*unfinished* feature
> areas (the real "wire up vs. delete" decisions) are the non-DEX ones already flagged as
> `DECISION` in this plan: coingecko/USD pricing (2.1), `pending_txs` persistence (2.3),
> `output.parquet_dir` (4.5), explorer aggregation (2.2). If anything DEX-related looks incomplete,
> it's the *hexic duplication itself* — addressed by the Phase 7 topic-table consolidation, not by
> wiring up the dead copies.

| # | Item | Location | Notes |
|---|---|---|---|
| 3.28 | orphaned topic consts: `V2_SYNC_TOPIC`, `V2_PAIR_CREATED_TOPIC`, `V3_MINT_TOPIC`, `V3_BURN_TOPIC`, `V3_POOL_CREATED_TOPIC`, `V4_INITIALIZE_TOPIC`, `BALANCER_SWAP_TOPIC`, `BALANCER_POOL_REGISTERED_TOPIC`, `SOLIDLY_PAIR_CREATED_TOPIC`, `CAMELOT_PAIR_CREATED_TOPIC`, `CURVE_POOL_ADDED_TOPIC`, `PENDLE_NEW_MARKET_TOPIC`, `TRADER_JOE_LB_PAIR_CREATED_TOPIC` | `core/src/chain/events.rs:21-139` | canonical copies live in `pool/decoders.rs`, `pool/discovery/mod.rs`, and `pipeline/scanner.rs::topics` — the chain copies have zero referencers (even in-file tests don't touch them). Keep the swap/flash/transfer topics + all `decode_*` fns (used by `trades.rs`/`flashloans.rs`/`liquidations.rs`/`transfers.rs` + `cli scan`) |
| 3.29 | `CURVE_TOKEN_EXCHANGE_UNDERLYING`, `CURVE_V2_TOKEN_EXCHANGE_UNDERLYING` | `core/src/pool/discovery/mod.rs:118,121` | the used copies are `pipeline::scanner::topics` (referenced at mod.rs:582-583,825-826) |
| 3.30 | `RpcClient::reset()` | `core/src/rpc/client.rs:184` | no callers |
| 3.31 | `ProviderState::{mark_failed, rate_limiter, url, reset}` | `core/src/rpc/middleware.rs:200,128,160,206` | **DONE (2026-09-07):** removed all four methods; also removed the now-write-only `url` **field** + its `new()` param + call site in `client.rs`; the live paths use `record_rate_limited`/`set_rate_limiter`/`label` |
| 3.32 | `CacheError`, `SqliteError`, `RpcError`, `ReplayError` enums + `Error::{Rpc, Replay, Cache}` variants + `#[from]` impls | `core/src/error/{cache,rpc,replay}.rs`; `error/mod.rs:6-9,20-24` | never constructed — no `?`/`return` path builds them. Delete all three files (keep `error/config.rs`, used by validation.rs), drop the `pub use` lines and the three `Error` variants. Safe: deleting variants also removes the `From` impls, which nothing relies on |
| 3.33 | `SqliteStore::list_manifests()` | `core/src/cache/store/manifests.rs:36` | `put_manifest`/`get_manifest` are used by cli `run.rs`/`fetch.rs` |
| 3.34 | `TokenCache::save_all_to_sqlite()` | `core/src/cache/token_cache.rs:172` | cli uses `save_batch`/`load`/`merge` instead |
| 3.35 | `PoolManager::{with_capacity, set_token_max_pairs, with_use_latest, get_v2_state}` | `core/src/pool/state/manager.rs:118,142,167,810` | live knobs: `new`, `set_max_pairs_per_token`, `set_use_latest`, `get_v3_state` (jit.rs). **DONE (2026-09-07):** also removed the orphaned private `sqrt_price_reserves()` helper (TVL-oracle) that surfaced as a lib dead_code warning during the sweep |
| 3.36 | `V4HookFlags` + `classify()`/`modifies_swap()`/`modifies_liquidity()` | `core/src/pool/state/pool_types.rs:79-143` | also remove `V4HookFlags` from the re-export at `state/mod.rs:10` |
| 3.37 | `FlashLoanProvider::priority_list()` | `core/src/types/strategy.rs:57` | ⚠️ **`gas_overhead()` is NOT dead — keep it** (used by `two_hop.rs:162`, `multi_hop.rs:340`) |

### Group E — additional dead code found in second pass `(second-pass)`

> Most of these are methods/helpers that became orphaned once the mempool subsystem stopped
> being wired in, plus a few overlooked getters.

| # | Item | Location | Notes |
|---|---|---|---|
| 3.38 | `PendingPoolEffect` struct + `simulate_pending_tx_pool_impact()` + `estimate_pending_tx_pool_impact()` + ~15 private helpers (V2ExactInParams, V3Hop, parse_v2_swap_*, estimate_v2_hop_*, v2_swap_effects, estimate_v2_exact_in/out, parse_v3_path, parse_v3_exact_input, estimate_v3_exact_in/out) | `core/src/mev/detectors/mempool.rs:100–546` | ~150–200 lines. The *live* mempool fns (`capture_pending_block` L31, `detect_pending_opportunities` L56, `PendingBlockCapture`) are **used** — keep them. Only the pending-pool-effect simulation subsystem is dead. ⚠️ **(third-pass)** also remove the re-exports of `PendingPoolEffect`, `simulate_pending_tx_pool_impact`, `estimate_pending_tx_pool_impact` from `mev/detectors/mod.rs:13-14` **and** `mev/mod.rs:5-11`, otherwise the build breaks. **DONE (2026-09-07):** removed lines 98–546 (entire subsystem) + both re-export lists (also pruned the now-unused imports `Address`/`PoolState`/`BlockRef`/`constant_product_output_amount`/`abi_decode_*`); `mempool.rs` trimmed to 96 live lines |
| 3.39 | `balancer_output_amount()` | `core/src/mev/detectors/two_hop.rs:639` | zero callers; two_hop uses `curve_output_amount` + `balancer_quote_exact_in` internally. ⚠️ **(third-pass)** also remove it from the re-export lists in `mev/detectors/mod.rs` and `mev/mod.rs:6`. **DONE (2026-09-07):** removed wrapper + both re-exports; the canonical `pool/math/balancer.rs::balancer_output_amount` stays (in-file use + pool-level re-exports) |
| 3.40 | `AaveReserveCache::is_empty()` | `core/src/mev/detectors/liquidation.rs:96` | `get()` and `len()` are used — keep those. **DONE (2026-09-07)** |
| 3.41 | `CachedRpcDb::block_number()` / `set_block_number()` | `core/src/replay/replayer.rs:126,130` | no callers. **DONE (2026-09-07):** also removed the now-orphaned `CacheState::clear()` (db.rs:64, only caller was `set_block_number`) + stale doc note; `block_number` field + `rpc()`/`handle()` getters remain (used) |
| 3.42 | `PoolInfo::is_concentrated_liquidity()` | `core/src/pool/state/pool_types.rs:245` | **DONE (2026-09-07):** also removed the orphaned `DexType::is_concentrated_liquidity()` (dex_type.rs:41 ─ its only caller was the PoolInfo wrapper; zero workspace refs) |
| 3.43 | `LabelDb::merge()` / `is_empty()` | `core/src/chain/labels.rs:51,63` | `load()`/`get()`/`len()` are used — keep those. **DONE (2026-09-07)** |
| 3.44 | `constant_product_input_amount()` | `core/src/pool/math/core.rs:157` | only reachable from the dead mempool effect subsystem (3.38); also remove from re-export in `pool/math/mod.rs` and `pool/mod.rs`. **DONE (2026-09-07):** removed fn + both re-export entries; verified `constant_product_output_amount` (sibling) still heavily used — kept |

- [x] Group E: 3.38–3.44 removed, build green

- [x] Group A: 3.1–3.9 removed, build green
- [x] Group B: 3.10–3.14 removed, build green
- [x] Group C: 3.15–3.27 removed, build green
- [x] Group D: 3.28–3.37 removed, build green

---

## Phase 4 — Config surface cleanup

| # | Item | Location | Action |
|---|---|---|---|
| 4.1 | `Config::parse_token_prices()` | `core/src/config/settings.rs:683` (was `:632` — drifted) | **DONE (2026-09-07)** — delete — no callers ⇒ the whole `backtest.token_prices` knob is dead plumbing |
| 4.2 | `backtest.token_prices` field chain | settings.rs (see 4.1) | **DONE (2026-09-07)** — removed field, default, `BacktestOverrides` entry, `merge_cli` entry. **(third-pass)** ⚠️ when grepping `token_prices` afterwards, ignore hits inside `pipeline/aggregate.rs` (~:96-249) — that `token_prices` is a live internal parameter of the KEEP'd aggregation API, unrelated to the config knob — **verified: only aggregate.rs hits remain** |
| 4.3 | `user_rpc_urls()` | `core/src/config/settings.rs:359` | **DONE (2026-09-07)** — delete; kept shared `merge_rpc_urls` (still called from effective-rpc path) |
| 4.4 | `ConfigBuilder::with_days/with_blocks/with_block/with_from_block/with_to_block/with_rpc/with_gas/with_backtest` | `core/src/config/settings.rs:555-569` | **DONE (2026-09-07)** — removed all 8 unused methods; kept `with_chain`, `with_output`, `build()` (test callers); pruned the builder's now-settable-nowhere `days..to_block/rpc/gas/backtest` fields + fixed doc example (drop `with_rpc`) |
| 4.5 | `output.parquet_dir` | `core/src/config/settings.rs` | **DONE (2026-09-07)** — DECISION: **delete** (no parquet functionality exists). Removed field + default + `OutputOverrides` entry + `merge_cli` entry + `plan_summary` line |
| 4.6 | `config/mod.rs` re-exports of `validate_rpc_url`, `validate_rpc_urls` | `core/src/config/mod.rs:6` | **DONE (2026-09-07)** — removed from re-export list (functions stay — used internally by validation.rs) |
| 4.7 | `backtest.price_oracle_mode` | settings.rs | **already covered** — gone with 2.1 (no references anywhere) |

⚠️ After 4.1/4.2/4.7 double-check `core/tests/config.rs` and `cli/tests/*` for references to
removed config keys (grep `token_prices|price_oracle_mode|parquet_dir`).

- [x] Executed, build + tests green

---

## Phase 5 — Small quality fixes

- [x] **5.1** `core/examples/dump_logs.rs:7` — hardcoded `r"D:\gitlab.dte.repo\...\cache\polygon-mev-scout.sqlite"`.
      **DONE (2026-09-07):** DB path now read from `env::args().nth(1)` (block becomes arg #2), like `trace_tx.rs`/`diag_gas.rs`.
- [x] **5.2** `core/src/fetch/fetcher.rs:99` — **DONE (2026-09-07):** `Fetcher::new` default `batch_rpc` changed `true` → `false`,
      matching the CLI-documented opt-in default (`--batch-rpc`; `run.rs`/`fetch.rs` pass it explicitly). `live.rs` now uses the
      (parallel) non-batched path — single-block fetch, perf-neutral.
- [x] **5.3** `cli/src/display.rs:52-53` — **DONE (2026-09-07):** dropped the redundant `Some(...)` wrap + `if let Some(info) = info`;
      `pool_name` uses `ps.info()` (a `&PoolInfo`) directly.
- [x] **5.4** `core/src/pool/math/consts.rs:6` — **DONE (2026-09-07):** verified every const still referenced
      (incl. `Q128_SHIFT` → apply.rs, `MAX_V2_RESERVE_RATIO` → factory.rs, solver-iteration consts → curve/stable_swap/pendle);
      no dead consts remain.
- [x] **5.5** `(review)` `core/src/fetch/fetcher.rs:163,473` — **DONE (2026-09-07):** `parallelism.min(30).max(1)` →
      `parallelism.clamp(1, 30)` (both spots).
- [x] **5.6** `(review)` `core/tests/backtest.rs:198,359,290` — **DONE (2026-09-07):** `let mut fetcher` →
      `let fetcher` (fetch_range takes `&self`), unused outer `stats` → `_stats`.

---

## Phase 6 — Test gating consistency (live-RPC core tests)

`cli/tests` is well-gated (`MEV_SCOUT_E2E=1` + `rpc_ready()` probe), but core integration
tests silently fall back to the repo's `mev-scout.toml` RPCs and do live network I/O by
default. Make them consistent.

⚠️ **(review)** The `config_rpc_url()` fallback is **duplicated locally in each test file**, not
just in `common/setup.rs`:
- `core/tests/e2e.rs:169-190` — own `rpc_url()` + `config_rpc_url()` (+ `public_rpc_url()` fallback, line 203)
- `core/tests/backtest.rs:28-50` — own `rpc_url()` + `config_rpc_url()` (+ own `temp_test_dir`)
- `core/tests/replay.rs`, `arbitrage.rs`, `sandwich.rs` — use `common::setup::rpc_url()` (which wraps `env_rpc_url()` + `config_rpc_url()`)

So Phase 6 is bigger than one line: gate **e2e.rs and backtest.rs in-place**, then either
convert `common/setup.rs::rpc_url()` to the gated form **or** delete the shared copies once no
one uses them. Prefer: add `MEV_SCOUT_E2E=1`-style gating in each of the 4 entry points and
delete the now-dead `config_rpc_url()`/`env_rpc_url()`/`rpc_url()` from `setup.rs` (and the
duplicate local copies in e2e.rs/backtest.rs), keeping a single gated `rpc_url()` helper.

- [x] `core/tests/e2e.rs`, `core/tests/backtest.rs`, `core/tests/replay.rs` — **DONE (2026-09-07):**
      `common/setup.rs::rpc_url()` now returns `RPC_URL` **only** when `MEV_SCOUT_E2E=1` (no config
      fallback); `e2e.rs::try_rpc()` requires the gate (`RPC_URL` preferred, public Polygon endpoint
      only reachable while gated); `backtest.rs` local `rpc_url()` gated the same way. Local
      `config_rpc_url()` copies deleted (e2e.rs:173-188, backtest.rs:35-50, setup.rs:23-46).
      `arbitrage.rs:408` and `sandwich.rs:211` (both `common::rpc_url()` callers) and the three
      replay activity-scanner tests inherited the gate and self-skip. Gated suites verified:
      previously failing `test_activity_scanner_finds_active_blocks` and the >10-min `backtest`
      suite now skip instantly; lib 96 / arbitrage 17 / replay 7 / backtest 9 / sandwich 7 /
      rps_management 3 / config 4 / cli_args 25 all pass.
- [x] Remove/repurpose `config_rpc_url()` / `env_rpc_url()` / `rpc_url()` in
      `core/tests/common/setup.rs:19-51` after the gating lands. — **DONE (2026-09-07):**
      `env_rpc_url()`/`config_rpc_url()` deleted; single gated `rpc_url()` kept.
- [x] Document the gate at the top of each test file like `cli/tests/cli_e2e.rs` does. —
      **DONE (2026-09-07):** `//!` headers added to `e2e.rs`, `backtest.rs`, `replay.rs`,
      `arbitrage.rs`, `sandwich.rs` describing the `MEV_SCOUT_E2E=1` + `RPC_URL` gate.

---

## Phase 7 — Guardrails so this doesn't come back

- [x] Add a clippy job (script, CI, or pre-push hook) with:
      `cargo clippy --workspace --all-targets -- -W dead_code -D warnings`
      **DONE (2026-09-07):** shipped `scripts/clippy.ps1` (+ `scripts/clippy.sh`) — no CI exists
      (third-pass note), so a script guardrail was chosen. Scope corrected: the plan's literal
      `-W dead_code -D warnings` fails on ~70 pre-existing clippy *style* lints
      (too_many_arguments/precedence/unnecessary_cast/...) unrelated to dead code, so the script
      errors only on dead_code: `cargo clippy --workspace --lib --bins -- -W dead_code -D dead_code`
      (test binaries excluded — each `--test` target legitimately carries per-binary unused
      helpers from `common/setup.rs`). Verified passing: zero dead_code findings in lib+bins
      after the whole cleanup; cli/src (a binary crate, where dead_code covers `pub` items too)
      is therefore fully audited — the ⚠️ review note about `pub` blind spots only applies to
      lib crates, whose surface was manually rg-audited across Phases 3–6.
      ⚠️ **(review)** this only catches dead *private* items — `pub` items in a lib crate
      never warn. The rg-based audit in Phase 7's next bullet is the real guardrail for
      the public API.
      ⚠️ **(third-pass)** there is **no CI at all** (no `.github/` directory) — the job
      cannot be "added to CI"; either create a minimal workflow from scratch or ship a
      pre-push hook / `scripts/clippy.sh` first.
- [x] **(third-pass) rg-audit `cli/src`** — **DONE (2026-09-07):** covered by the guardrail run
      above: `mev-scout-cli` is a binary crate, so rustc's dead_code lint flags caller-free
      `pub` items in its modules (unlike a lib). Zero findings.
- [x] Add `cargo machete` (or `cargo +nightly udeps`) to check unused deps
      (suspects to verify: `alloy` feature `signers` in core, `futures` breadth, `url`).
      **DONE (2026-09-07):** machete/udeps not installed; manual suspects check instead — all
      three used: `signers` → `alloy::signers::Either` (replay/replayer.rs:38), `futures` →
      fetcher/client/discovery/factory/replayer/multicall, `url` → `Url::parse` (rpc/client.rs:157).
      No unused deps removed.
      **(third-pass)** after 2.1, `strum` is *still* used (Strategy, OutputFormat, DexType,
      ChainName, …) — do not remove it.
- [x] **Consolidate event-topic tables** `(review)` — **DONE (2026-09-07):** after 3.28/3.29 the
      four remaining tables (`chain/events.rs`, `pool/decoders.rs`, `pool/discovery/mod.rs`,
      `pipeline/scanner.rs::topics`) are purpose-distinct (trades+flash+transfer / decoder
      dispatch / factory-created / activity-scan), so a full table merge was rejected; instead
      every scattered *same-valued* const was re-pointed at `chain/events.rs` as the single
      home (stable names preserved via aliases, zero call-site churn):
      `pool/state/apply.rs::SWAP_TOPIC` → alias of `events::V2_SWAP_TOPIC`;
      `mev/detectors/sandwich.rs` local `V2_SWAP_TOPIC` → `use events::V2_SWAP_TOPIC`;
      `pool/state/manager.rs::ERC20_TRANSFER_TOPIC` → alias of `events::TRANSFER_TOPIC`;
      `cache/store/mod.rs::TRANSFER_EVENT_TOPIC` → alias re-export of `events::TRANSFER_TOPIC`.
      The 3 shared values between events.rs and decoders.rs (V3_SWAP, CURVE_TOKEN_EXCHANGE,
      CURVE_V2_TOKEN_EXCHANGE) remain intentionally dual-listed — each table is
      single-sourced within its purpose and the rot vector (whole dead table copies) is gone.
- [x] After all deletions, re-run `rg` for removed names to catch stale doc references
      (`docs/*.md` mention `aggregate.rs` intentionally — leave those). **DONE (2026-09-07):**
      fixed `coingecko_api_key` in `cli/tests/README.md:63` and
      `docs/CLI_TESTS_REVIEW_AND_PLAN.md:413`; removed the `coingecko` node from
      `docs/ARCHITECTURE.md:53` mermaid diagram. `EXPLORER_UNIFIED_PLAN.md` coingecko
      references are intentional planning context (2.1 DECISION) — left. Also executed the
      outstanding 3.34 gap: `TokenCache::save_all_to_sqlite` (token_cache.rs:158) removed.
      Known stale-doc
      spots for deleted names: `coingecko_api_key` in `cli/tests/README.md:63` and
      `docs/CLI_TESTS_REVIEW_AND_PLAN.md:406`; `coingecko · data · error · …` node in the
      `docs/ARCHITECTURE.md:53` mermaid diagram; `replay_each()` doc mention handled by 3.11.
- [x] **(third-pass)** sweep re-export lists that lose members so they stay compilable and
      honest: `mev/mod.rs:5-11` + `mev/detectors/mod.rs:13-14` (3.38/3.39), `types/mod.rs`
      (2.1, 3.15, 3.36), `pool/math/mod.rs` + `pool/mod.rs` (3.13, 3.14, 3.44),
      `replay/mod.rs` (3.12), `config/mod.rs` (4.6), `error/mod.rs` (3.32).
      **DONE (2026-09-07):** all lists pruned as part of their phases and re-verified — every
      re-exported item exists; build green.
- [ ] Optional: reduce over-broad re-export lists (`pool/mod.rs`, `types/mod.rs`,
      `pipeline/mod.rs`) once the surface is final. — **Skipped (optional):** current lists
      are compilable and honest; narrowing is cosmetic and can ride a future refactor.

---

## Verification checklist (after every phase)

```powershell
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -W dead_code -D warnings
cargo test -p mev-scout-core --test config --test sandwich --test liquidation --test arbitrage
cargo test -p mev-scout-cli --test cli_args
# full suite incl. live-RPC tests when RPC is available:
$env:MEV_SCOUT_E2E="1"; cargo test --workspace
```

Suggested commit granularity: one commit per phase, message pattern
`chore(cleanup): phase N — <summary>` (e.g. `phase 1 — drop orphaned mev/gas.rs, dedupe gas consts`).

---

## Review changelog (2026-09) `(review)`

Summary of everything the review added/corrected:

- **Line estimate** bumped from ~800–900 → ~1,050–1,100 (Group D adds ~250).
- **Phase 2.1** — added the `EXPLORER_PLAN.md`/`EXPLORER_EXECUTION_PLAN.md` references that name
  `coingecko.rs` as the explorer pricing source (DECISION context); added `types/mod.rs` re-export
  cleanup; new decision option "keep-as-reserved".
- **Phase 2.3** — added removal of `pub mod pending;` (`store/mod.rs:12`) and the `cache/mod.rs:4`
  re-export.
- **Phase 3** — added Group D (items 3.28–3.37): dead topic consts in `chain/events.rs`; dead
  Curve topics in `discovery/mod.rs`; `RpcClient::reset` + `ProviderState` accessors; the four
  never-constructed error enums + `Error` variants; `SqliteStore::list_manifests`;
  `TokenCache::save_all_to_sqlite`; four dead `PoolManager` builder/getters; `V4HookFlags`;
  `FlashLoanProvider::priority_list`.
- **Phase 5** — added 5.5 (manual_clamp) and 5.6 (backtest.rs lint noise).
- **Phase 6** — corrected: `config_rpc_url()` fallback is duplicated per-file (e2e.rs, backtest.rs)
  *and* in `common/setup.rs`; gating must cover `arbitrage.rs` and `sandwich.rs` too.
- **Phase 7** — scope correction from execution: `-D warnings` as literally written fails on ~70
  pre-existing clippy style lints unrelated to dead code; the guardrail scripts error on
  dead_code only (`-D dead_code`, lib+bins). Topic-table "consolidation" resolved by
  re-pointing scattered same-valued consts at `chain/events.rs` instead of merging the four
  purpose-distinct tables.
- **Phase 7** — noted clippy's blind spot for `pub` items; added topic-table consolidation
  guardrail.

Items verified **NOT dead** (do not delete): `calldata_gas_estimate`; `FlashLoanProvider::gas_overhead()`
(two_hop.rs:162, multi_hop.rs:340); the setup helpers in `core/tests/common/setup.rs` /
`cli/tests/common/mod.rs` (shared across test binaries — per-binary clippy warnings are noise);
`chain/events.rs` decode functions and swap/flash/transfer topics.

## Second-pass changelog (2026-09) `(second-pass)`

Summary of items found in the second audit pass:

- **Line estimate** bumped from ~1,050–1,100 → ~1,230–1,340 (Group E adds ~180).
- **Phase 3** — added Group E (items 3.38–3.44): the entire `PendingPoolEffect` simulation
  subsystem in `mempool.rs` (~150–200 lines, never wired in); `balancer_output_amount` in
  `two_hop.rs`; `AaveReserveCache::is_empty`; `CachedRpcDb::block_number/set_block_number`;
  `PoolInfo::is_concentrated_liquidity`; `LabelDb::merge/is_empty`; `constant_product_input_amount`
  (only reachable from the dead mempool subsystem).

Items verified **NOT dead** (do not delete): `capture_pending_block`, `detect_pending_opportunities`,
`PendingBlockCapture` (mempool.rs live paths); `AaveReserveCache::get/len`; `LabelDb::load/get/len`;
`CachedRpcDb` other methods; all `decode_*` functions in `chain/events.rs`.

## Third-pass additions (2026-09) `(third-pass)`

Findings from re-verifying the plan against the current working tree (no new dead code found —
these are correctness/executor-safety fixes to the plan itself):

- **Ground rules** — added the line-drift warning: line numbers in this plan have shifted
  (e.g. `parse_token_prices` moved `settings.rs:632` → `:683`); locate items by identifier.
- **Phase 2.1** — two missed coupling points for deleting `rpc.coingecko_api_key`:
  the env-expansion block in `expand_secrets` (`settings.rs:292-293`, doc mention `:255`)
  and the unit test `expands_plain_urls_and_coingecko_key_unchanged` (`settings.rs:738-749`),
  which writes/asserts the key and must be rewritten. Table rows refreshed (drifted refs).
  Fixed stale references to the deleted `EXPLORER_PLAN.md`/`EXPLORER_EXECUTION_PLAN.md`
  (consolidated into `EXPLORER_UNIFIED_PLAN.md` in commit `63e9f6f`; its coingecko/aggregate
  references were re-confirmed at lines ~208/444/1186 and ~110/173/1178).
- **Phase 2.2** — same doc-consolidation reference fix.
- **Phase 3.11** — added the stale module-doc mention of `replay_each()` at `replayer.rs:201`.
- **Phase 3.38/3.39** — added the missing re-export cleanup in `mev/detectors/mod.rs:13-14`
  and `mev/mod.rs:5-11` (deleting the items without trimming these lists breaks the build).
- **Phase 4** — refreshed drifted line refs; warned that grepping `token_prices` will hit
  `pipeline/aggregate.rs` (live parameter of the KEEP'd API, not the config knob).
- **Phase 7** — noted there is **no CI** today (no `.github/`), so the clippy guardrail needs
  a workflow created from scratch or a hook/script; added an rg-audit task for `cli/src`
  (never audited by this plan's method); noted `strum` must stay after 2.1; listed the known
  stale-doc spots for deleted names (`cli/tests/README.md:63`, `CLI_TESTS_REVIEW_AND_PLAN.md:406`,
  `ARCHITECTURE.md:53` mermaid node); added a final re-export-list sweep item.
- Items verified **NOT dead / NOT needed** (checked during this pass): `SqliteError` and the
  `Error::{Rpc,Replay,Cache}` variants have zero construction sites outside `error/*` (3.32 is
  safe); `mev-scout.toml` contains none of the config keys slated for removal; no CLI flags or
  `core/tests` references exist for `token_prices`/`price_oracle_mode`/`parquet_dir`.
