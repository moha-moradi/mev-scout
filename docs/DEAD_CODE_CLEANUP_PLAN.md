# Dead Code Cleanup Plan

**Goal:** Remove confirmed dead code (~800–900 lines), deduplicate gas constants, shrink the
public API surface, and add guardrails so dead code cannot silently accumulate again.

**Method behind this plan:** Every item below was verified by cross-referencing the definition
against *all* call sites in the workspace (core lib, CLI bin, `core/tests`, `cli/tests`,
`core/examples`). "No callers" means: no `rg` match for the identifier outside its own
definition, its re-export lines, and its own in-file unit tests.

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
verify `Config` doesn't set it; if it doesn't, removal is non-breaking for old TOML files).
Also note a latent bug if wiring up later: `resolve_onchain_price()` hardcodes Ethereum
mainnet stable-token addresses regardless of the `chain` parameter.

**Alternative (wire up):** only worth it if USD pricing becomes a near-term feature; otherwise
delete — git history keeps it.

- [ ] Decision recorded: delete / wire up
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

- **Delete:** remove `pending.rs`, remove the `CREATE TABLE IF NOT EXISTS pending_txs (...)`
  block from `mod.rs`. Existing DBs keep the orphan table harmlessly (no migration needed).
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

- [ ] Group A: 3.1–3.9 removed, build green
- [ ] Group B: 3.10–3.14 removed, build green
- [ ] Group C: 3.15–3.27 removed, build green

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

---

## Phase 6 — Test gating consistency (live-RPC core tests)

`cli/tests` is well-gated (`MEV_SCOUT_E2E=1` + `rpc_ready()` probe), but core integration
tests silently fall back to the repo's `mev-scout.toml` RPCs and do live network I/O by
default. Make them consistent:

- [ ] `core/tests/e2e.rs`, `core/tests/backtest.rs`, `core/tests/replay.rs` — require
      `RPC_URL` (or a `MEV_SCOUT_E2E=1` gate) instead of falling back to `config_rpc_url()`;
      remove/repurpose `config_rpc_url()` in `core/tests/common/setup.rs:27` if unused after.
- [ ] Document the gate at the top of each test file like `cli/tests/cli_e2e.rs` does.

---

## Phase 7 — Guardrails so this doesn't come back

- [ ] Add a clippy job (script, CI, or pre-push hook) with:
      `cargo clippy --workspace --all-targets -- -W dead_code -D warnings`
- [ ] Add `cargo machete` (or `cargo +nightly udeps`) to check unused deps
      (suspects to verify: `alloy` feature `signers` in core, `futures` breadth, `url`).
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
