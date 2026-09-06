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

- [ ] Baseline build green
- [ ] Baseline tests green
- [ ] `clippy-baseline.txt` saved

---

## Phase 1 — Orphaned file & gas constant deduplication (zero risk)

### 1.1 Delete the orphaned module `core/src/mev/gas.rs` entirely

Evidence: `core/src/mev/mod.rs` never declares `mod gas;` — this file is **not even compiled**.
It contains `FLASH_LOAN_OVERHEAD_GAS`, `estimate_base_gas()`, `estimate_multi_swap_gas()`, and
duplicate copies of `BASE_TX_GAS`, `DEFAULT_POOL_GAS`, `JIT_OVERHEAD`, `LIQUIDATION_GAS_LIMIT`
(the canonical copies live in `core/src/pool/math/consts.rs:12-17` and are what the codebase
actually uses).

- [ ] Delete `core/src/mev/gas.rs`

> **(review)** File is only ~20 lines. Note: it imports `crate::pool::state::calldata_gas_estimate`
> — that function is **used** (detectors/jit.rs:290, jit_arb.rs:231, liquidation.rs:473,
> multi_hop.rs:968, two_hop.rs:902, sandwich.rs:467) and must NOT be removed; it is unrelated to
> this orphaned file.

### 1.2 Deduplicate `LIQUIDATION_GAS_LIMIT` in `core/src/mev/detectors/liquidation.rs:36`

The file re-declares a private `const LIQUIDATION_GAS_LIMIT: u64 = 180_000;` even though
`crate::pool::math::consts::LIQUIDATION_GAS_LIMIT` is identical and already used everywhere else.

- [ ] Replace the local const with `use crate::pool::math::consts::LIQUIDATION_GAS_LIMIT;`

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
| `rpc.coingecko_api_key` field + `merge_cli` entry | `core/src/config/settings.rs:34-36,127,463,605` | remove |
| `backtest.price_oracle_mode` field + default + `merge_cli` entry | `core/src/config/settings.rs:72-74,151,480,618` | remove |

⚠️ Removing config fields changes the TOML schema for users who have them set (serde with
`#[serde(default)]` tolerates *unknown* fields only if `deny_unknown_fields` is NOT set —
verified: no `deny_unknown_fields` anywhere in `core/src/config`, so removal is non-breaking
for old TOML files).
Also note a latent bug if wiring up later: `resolve_onchain_price()` hardcodes Ethereum
mainnet stable-token addresses regardless of the `chain` parameter (coingecko.rs:191-195).

**Alternative (wire up):** only worth it if USD pricing becomes a near-term feature; otherwise
delete — git history keeps it.

⚠️ **DECISION context (review):** `docs/EXPLORER_PLAN.md` (line 169) *and*
`docs/EXPLORER_EXECUTION_PLAN.md` (lines 75, 268) both name `core/src/coingecko.rs` as the
explorer's USD-pricing source. This is the same "reserved for explorer" treatment the plan
grants `pipeline/aggregate.rs` (Phase 2.2). If the explorer command is moving forward, mark
coingecko.rs `KEEP` (reserved) instead of deleting — otherwise Phase 2.2 and this deletion
are inconsistent. If deleting, everything inside coingecko.rs goes with it (including
`PriceEntry`, `coingecko_asset_id`, `coingecko_platform`, `get_or_fetch`), and remove the
`PriceOracleMode`/`PriceSource`/`ExecutorType` entries from the `types/mod.rs:9-10`
re-export list.

- [ ] Decision recorded: delete / wire up / keep-as-reserved
- [ ] Executed per decision

### 2.2 `core/src/pipeline/aggregate.rs` (~360 lines) — KEEP for now (explorer plan)

Evidence: `aggregate()`, `aggregate_with_prices()`, `AggregationResult`, `DexMeta`,
`DexMetrics`, `StrategyMetrics`, `SummaryMetrics` are only re-exported in
`core/src/pipeline/mod.rs:6-9`; no caller exists today. **However** `docs/EXPLORER_PLAN.md`
names `pipeline/aggregate.rs` as the aggregation layer for the planned `explorer` command.

Action: **keep**, but mark as intentionally-unused so it doesn't trip the Phase 7 guardrail:

- [ ] Add `#![allow(dead_code)]` with a comment `// Reserved for explorer command (docs/EXPLORER_PLAN.md)` at the top of `aggregate.rs` (or to `pipeline/mod.rs` re-exports), OR decide to drop the explorer plan and delete the module.

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

- [ ] Decision recorded: delete / keep
- [ ] Executed per decision

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
| 3.11 | `BlockReplayer::replay_each()` | `core/src/replay/replayer.rs:601` | runner uses `replay_each_filtered` |
| 3.12 | `StateSnapshot` struct + `new/db/db_mut/fork` | `core/src/replay/replayer.rs:743-767` | also remove from re-export in `core/src/replay/mod.rs:5` and the module doc mention (replayer.rs:11) |
| 3.13 | `quote_v3_exact_out()` | `core/src/pool/math/v3.rs:756` | helpers `compute_swap_step_exact_out` / `get_swap_target_for_tick` / `find_next_initialized_tick` are **also used by `quote_v3_exact_in` — do not delete them**; also remove from re-exports in `pool/math/mod.rs:31` and `pool/mod.rs:15` |
| 3.14 | `optimal_n_hop_generic()` | `core/src/pool/math/core.rs:386` | remove re-exports in `pool/math/mod.rs:22` and `pool/mod.rs:13` |
| 3.15 | `v2_router_for_factory()` | `core/src/types/chain.rs:258` | doc claims "Used by M3" — no such caller; remove re-export in `types/mod.rs:5` |
| 3.16 | `ChainName::public_rpc_urls()` | `core/src/types/chain.rs:146` | `public_rpc_url()` + `public_rpc_endpoints()` are the used ones |
| 3.17 | `RangeResolver::rpc_client()` | `core/src/resolver.rs:44` | |
| 3.18 | `Fetcher::rpc_client()` | `core/src/fetch/fetcher.rs:123` | `cache_store()` has a test caller — keep |
| 3.19 | `SqliteStore::get_logs_for_block()` | `core/src/cache/store/blocks.rs:315` | |
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
| 3.31 | `ProviderState::{mark_failed, rate_limiter, url, reset}` | `core/src/rpc/middleware.rs:200,128,160,206` | the only caller chain is itself (`RpcClient::reset` → `ProviderState::reset`); the live paths use `record_rate_limited`/`set_rate_limiter`/`label` |
| 3.32 | `CacheError`, `SqliteError`, `RpcError`, `ReplayError` enums + `Error::{Rpc, Replay, Cache}` variants + `#[from]` impls | `core/src/error/{cache,rpc,replay}.rs`; `error/mod.rs:6-9,20-24` | never constructed — no `?`/`return` path builds them. Delete all three files (keep `error/config.rs`, used by validation.rs), drop the `pub use` lines and the three `Error` variants. Safe: deleting variants also removes the `From` impls, which nothing relies on |
| 3.33 | `SqliteStore::list_manifests()` | `core/src/cache/store/manifests.rs:36` | `put_manifest`/`get_manifest` are used by cli `run.rs`/`fetch.rs` |
| 3.34 | `TokenCache::save_all_to_sqlite()` | `core/src/cache/token_cache.rs:172` | cli uses `save_batch`/`load`/`merge` instead |
| 3.35 | `PoolManager::{with_capacity, set_token_max_pairs, with_use_latest, get_v2_state}` | `core/src/pool/state/manager.rs:118,142,167,810` | live knobs: `new`, `set_max_pairs_per_token`, `set_use_latest`, `get_v3_state` (jit.rs) |
| 3.36 | `V4HookFlags` + `classify()`/`modifies_swap()`/`modifies_liquidity()` | `core/src/pool/state/pool_types.rs:79-143` | also remove `V4HookFlags` from the re-export at `state/mod.rs:10` |
| 3.37 | `FlashLoanProvider::priority_list()` | `core/src/types/strategy.rs:57` | ⚠️ **`gas_overhead()` is NOT dead — keep it** (used by `two_hop.rs:162`, `multi_hop.rs:340`) |

### Group E — additional dead code found in second pass `(second-pass)`

> Most of these are methods/helpers that became orphaned once the mempool subsystem stopped
> being wired in, plus a few overlooked getters.

| # | Item | Location | Notes |
|---|---|---|---|
| 3.38 | `PendingPoolEffect` struct + `simulate_pending_tx_pool_impact()` + `estimate_pending_tx_pool_impact()` + ~15 private helpers (V2ExactInParams, V3Hop, parse_v2_swap_*, estimate_v2_hop_*, v2_swap_effects, estimate_v2_exact_in/out, parse_v3_path, parse_v3_exact_input, estimate_v3_exact_in/out) | `core/src/mev/detectors/mempool.rs:100–546` | ~150–200 lines. The *live* mempool fns (`capture_pending_block` L31, `detect_pending_opportunities` L56, `PendingBlockCapture`) are **used** — keep them. Only the pending-pool-effect simulation subsystem is dead |
| 3.39 | `balancer_output_amount()` | `core/src/mev/detectors/two_hop.rs:639` | zero callers; two_hop uses `curve_output_amount` + `balancer_quote_exact_in` internally |
| 3.40 | `AaveReserveCache::is_empty()` | `core/src/mev/detectors/liquidation.rs:96` | `get()` and `len()` are used — keep those |
| 3.41 | `CachedRpcDb::block_number()` / `set_block_number()` | `core/src/replay/replayer.rs:126,130` | no callers |
| 3.42 | `PoolInfo::is_concentrated_liquidity()` | `core/src/pool/state/pool_types.rs:245` | `DexType`'s own method is used instead |
| 3.43 | `LabelDb::merge()` / `is_empty()` | `core/src/chain/labels.rs:51,63` | `load()`/`get()`/`len()` are used — keep those |
| 3.44 | `constant_product_input_amount()` | `core/src/pool/math/core.rs:157` | only reachable from the dead mempool effect subsystem (3.38); also remove from re-export in `pool/math/mod.rs` and `pool/mod.rs` |

- [ ] Group E: 3.38–3.44 removed, build green

- [ ] Group A: 3.1–3.9 removed, build green
- [ ] Group B: 3.10–3.14 removed, build green
- [ ] Group C: 3.15–3.27 removed, build green
- [ ] Group D: 3.28–3.37 removed, build green

---

## Phase 4 — Config surface cleanup

| # | Item | Location | Action |
|---|---|---|---|
| 4.1 | `Config::parse_token_prices()` | `core/src/config/settings.rs:632` | delete — no callers ⇒ the whole `backtest.token_prices` knob is dead plumbing (field at :77, default :152, override :481, merge entry :619) |
| 4.2 | `backtest.token_prices` field chain | settings.rs (see 4.1) | remove field, default, `BacktestOverrides` entry, `merge_cli` entry |
| 4.3 | `user_rpc_urls()` | `core/src/config/settings.rs:359` | delete — no callers |
| 4.4 | `ConfigBuilder::with_days/with_blocks/with_block/with_from_block/with_to_block/with_rpc/with_gas/with_backtest` | `core/src/config/settings.rs:555-569` | delete unused builder methods; keep `with_chain`, `with_output`, `build()` (test callers) |
| 4.5 | `output.parquet_dir` | `core/src/config/settings.rs:97-99,165,425,491,627` | DECISION: no parquet functionality exists at all — delete field + plan_summary mention, or keep as reserved |
| 4.6 | `config/mod.rs` re-exports of `validate_rpc_url`, `validate_rpc_urls` | `core/src/config/mod.rs:6` | remove from re-export list (functions stay — used internally by validation.rs) |
| 4.7 | `backtest.price_oracle_mode` | settings.rs | only if Phase 2.1 decided **delete** (already covered there) |

⚠️ After 4.1/4.2/4.7 double-check `core/tests/config.rs` and `cli/tests/*` for references to
removed config keys (grep `token_prices|price_oracle_mode|parquet_dir`).

- [ ] Executed, build + tests green

---

## Phase 5 — Small quality fixes

- [ ] **5.1** `core/examples/dump_logs.rs:7` — hardcoded `r"D:\gitlab.dte.repo\...\cache\polygon-mev-scout.sqlite"`.
      Read the DB path from `env::args()` (2nd arg) like `trace_tx.rs`/`diag_gas.rs` do.
- [ ] **5.2** `core/src/fetch/fetcher.rs:99` — `Fetcher::new` sets `batch_rpc: true` while the CLI
      default is opt-in (`--batch-rpc`). Change the default to `false` for consistency.
- [ ] **5.3** `cli/src/display.rs:52-53` — `let info = Some(ps.info()); if let Some(info) = info`
      redundant Option wrap; use `ps.info()` directly.
- [ ] **5.4** `core/src/pool/math/consts.rs:6` — confirm `SQRT_RATIO_CACHE_CAPACITY` and other
      consts are all still referenced after Phases 1–4 (clippy will tell).
- [ ] **5.5** `(review)` `core/src/fetch/fetcher.rs:163,473` — `parallelism.min(30).max(1)` →
      `self.parallelism.clamp(1, 30)` (two identical spots). Nice-to-have; not dead code.
- [ ] **5.6** `(review)` `core/tests/backtest.rs:198,359` — `let mut fetcher` doesn't need `mut`;
      `core/tests/backtest.rs:290` — `stats` is unused (`_stats`). Harmless lint noise, fold
      into the Phase 6 rewrite of that file.

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

- [ ] `core/tests/e2e.rs`, `core/tests/backtest.rs`, `core/tests/replay.rs` — require
      `RPC_URL` (or a `MEV_SCOUT_E2E=1` gate) instead of falling back to `config_rpc_url()`;
      apply to `arbitrage.rs:408` and `sandwich.rs:211` too.
- [ ] Remove/repurpose `config_rpc_url()` / `env_rpc_url()` / `rpc_url()` in
      `core/tests/common/setup.rs:19-51` after the gating lands.
- [ ] Document the gate at the top of each test file like `cli/tests/cli_e2e.rs` does.

---

## Phase 7 — Guardrails so this doesn't come back

- [ ] Add a clippy job (script, CI, or pre-push hook) with:
      `cargo clippy --workspace --all-targets -- -W dead_code -D warnings`
      ⚠️ **(review)** this only catches dead *private* items — `pub` items in a lib crate
      never warn. The rg-based audit in Phase 7's next bullet is the real guardrail for
      the public API.
- [ ] Add `cargo machete` (or `cargo +nightly udeps`) to check unused deps
      (suspects to verify: `alloy` feature `signers` in core, `futures` breadth, `url`).
- [ ] **Consolidate event-topic tables** `(review)` — there are now **four** near-identical
      topic-hash tables: `core/src/chain/events.rs`, `core/src/pool/decoders.rs`,
      `core/src/pool/discovery/mod.rs`, `core/src/pipeline/scanner.rs::topics`. After
      deleting the dead copies (3.28/3.29), pick one home (suggest `pool/decoders.rs` or
      `pipeline/scanner.rs::topics`) and re-export from there — this exact duplication is
      how `chain/events.rs` rotted. Re-run `rg` for the removed names afterwards.
- [ ] After all deletions, re-run `rg` for removed names to catch stale doc references
      (`docs/*.md` mention `aggregate.rs` intentionally — leave those).
- [ ] Optional: reduce over-broad re-export lists (`pool/mod.rs`, `types/mod.rs`,
      `pipeline/mod.rs`) once the surface is final.

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
