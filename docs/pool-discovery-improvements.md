# Pool Discovery Module — Improvement Plan

> Focus: Make pool discovery robust on **public RPC endpoints** with aggressive rate limits, while cleaning up code quality and fixing correctness issues.

---

## Context

The pool discovery subsystem scans Ethereum event logs to find active DEX pools
across Uniswap V2/V3, Curve, Balancer, Dodo, Clipper, Solidly, and Camelot
protocols. It currently works but has reliability problems on public/free-tier
RPCs (drpc, CloudFlare, Ankr, etc.) that impose strict `eth_getLogs` block-range
limits and low RPS ceilings.

---

## Phase 1 — Public RPC Survival (P0) ✅ COMPLETE

### 1.1 Concurrency-limited metadata fetch ✅

`join_all(fetch_tasks)` replaced with `stream::iter(...).buffer_unordered(config.rpc_concurrency)`.
Default concurrency: 64. Configurable via `--rpc-concurrency` CLI flag and
`DiscoveryConfig::rpc_concurrency`.

### 1.2 Configurable batch size for `eth_getLogs` ✅

Default `batch_size` changed from `10` to `2000` (safe for all public RPCs).
Validation warning added when `batch_size > 5000`.

### 1.3 Retry with exponential backoff on RPC failures ✅

Added `get_logs_with_retry()` helper with 3 attempts and 1s/2s/4s backoff.
Applied to: fast-path DEX scan, fallback full-topic scan, and all factory
creation event scans (via `scan_factory_creation_events` helper).
All factory scans now log errors via `tracing::warn` instead of silently
swallowing failures.

### 1.4 Provider cooldown awareness in discovery ✅

Added `has_healthy_providers()` to `RpcClient`. The discovery batch loop
checks before each iteration and sleeps 5s when all providers are in cooldown.

---

## Phase 2 — Code Quality (P1) ✅ COMPLETE

### 2.1 Extract event classification helper ✅

Extracted `classify_dex_event(log: &Log) -> Option<(DexType, Option<[u8;32]>, Option<(Address, Address)>)>`.
Both the fast-path and fallback scans now call this single function.

### 2.2 Extract factory scan helper ✅

Extracted `scan_factory_creation_events(rpc, factories, topic, from, to, decode)`.
Each factory type provides its own decode closure. Reduced ~250 lines to ~80 lines.

### 2.3 Introduce `DiscoveryConfig` struct ✅

`discover_pools`, `discover_and_cache`, and `discover_pools_with_sources` now
take `&DiscoveryConfig<'a>` instead of 12 individual parameters.
Dead `_v2_factory_fees` parameter removed from both public functions.

### 2.4 Deduplicate Phase 3 lookup (O(n²) → O(n)) ✅

Added `HashSet<Address>` for O(1) membership checks during Phase 3 assembly.

### 2.5 Preserve `creation_block` for event-discovered pools ✅

`pool_hits` now tracks `(DexType, Option<[u8;32]>, Option<(Address, Address)>, u64)`
where the last field is the earliest block number. Phase 3 sets
`creation_block` from this value instead of hardcoded `0`.

---

## Phase 3 — Correctness Fixes (P1-P2) ✅ COMPLETE

### 3.1 Solidly/Camelot fee handling ✅

Added `DexType::Solidly` (discriminant 6) and `DexType::Camelot` (discriminant 7)
with `#[repr(i64)]`. Updated all downstream match arms: `label()`, factory
initialization, pool manager, Dune classification, cache serialization, tests.
Solidly default fee: 30 bps. Camelot default fee: 0 (unknown per-pair).

### 3.2 Balancer pool_type filter ✅

Changed filter from `pool_type > 1` to `pool_type == 2 || pool_type > 3`.
ComposableStable (type 3) is now included. Only type 2+ (deprecated) and
unknown types are skipped.

### 3.3 Clipper token documentation ✅

Added doc comment noting that Clipper pools are multi-asset and the
extracted tokens are the swapped pair (tokenIn/tokenOut), not the full pool
token set.

### 3.4 `tick_spacing_from_fee` completion ✅

Added missing fee tiers: 200→4, 400→4, 2500→50.

---

## Phase 4 — CLI Improvements (P2) ✅ COMPLETE

### 4.1 JSON output flag ✅

Added `--json` flag to `DiscoverArgs`. When set, serializes `Vec<DiscoveredPool>`
to stdout via `serde_json::to_string_pretty`.

### 4.2 Cache instance reuse ✅

`SqliteStore` is now opened once at the start of `cmd_discover` and passed to
both `discover_and_cache` and Dune result caching.

### 4.3 Progress bar for Dune phase ✅

Added indeterminate spinner with phase messages (`querying V2 pools...`,
`querying V3 pools...`, `querying active pools...`).

---

## Phase 5 — Feature Additions (P3) ✅ COMPLETE

### 5.1 Incremental delta-scanning ✅

Added `--incremental` flag. When set:
1. Queries the cache for the latest `creation_block` via
   `cache.max_creation_block()`.
2. Uses `max_block + 1` as `from_block` instead of user-specified range.
3. If cache is up-to-date (`new_from > to`), exits early.
4. Merges new discoveries with existing cached pools.

### 5.2 Pool health check post-discovery ✅

Added `--health-check` flag (default: true). After discovery:
- **V2 / Solidly / Camelot:** Calls `getReserves()` via RPC. Removes pools
  where both reserves are zero.
- **V3:** Calls `slot0()` via RPC. Removes pools where `sqrtPriceX96 == 0`.
- **Curve / Balancer / Dodo / Clipper:** Skipped (no simple on-chain check).

Uses `buffer_unordered(rpc_concurrency)` for bounded concurrency.

### 5.3 `--min-pools` early exit ✅

Added `--min-pools N` flag (default: 0 = disabled). After Dune discovery:
- If `dune_pools.len() >= min_pools`, skips the on-chain scan.
- Only applies when `--source all` or `--source dune` is used.

---

## Configuration

CLI flags (all with sane defaults):
```
--batch-size 2000        eth_getLogs block range per call
--rpc-concurrency 64     Max concurrent RPC metadata fetches
--json                   JSON output mode
--incremental            Resume from cached max block
--health-check true      Post-discovery drain check
--min-pools 0            Skip on-chain when Dune has enough
```

---

## Files Modified

| File | Changes | Phase |
|------|---------|-------|
| `core/src/pool/discovery.rs` | Retry, concurrency, helpers, config struct, dedup, creation_block, health_check_pools, Clipper doc | 1-3, 5.2 |
| `core/src/pool/dex_type.rs` | Solidly/Camelot variants with `#[repr(i64)]` | 3.1 |
| `core/src/pool/mod.rs` | Re-export new types | 3.1 |
| `core/src/dune/pool_discovery.rs` | tick_spacing completion, Dune Solidly/Camelot classification | 3.4 |
| `core/src/rpc/client.rs` | `has_healthy_providers()` | 1.4 |
| `core/src/cache/store.rs` | Solidly/Camelot in dex_type_from_i64, `max_creation_block()`, `count_discovered_pools()` | 3.1, 5.1 |
| `core/src/pool/state/factory.rs` | Solidly/Camelot pool initialization | 3.1 |
| `core/src/pipeline/runner.rs` | Solidly/Camelot in `add_pool_to_manager` | 3.1 |
| `cli/src/cli.rs` | `--json`, `--rpc-concurrency`, `--incremental`, `--health-check`, `--min-pools`, batch_size default 2000 | 4.1, 5 |
| `cli/src/commands/discover.rs` | JSON output, cache reuse, Dune spinner, incremental, health check, min-pools, validation | 4, 5 |
| `core/tests/e2e.rs` | Updated for DiscoveryConfig signature | 3.1 |
| `core/tests/common/setup.rs` | Solidly/Camelot match arms | 3.1 |

---

## Verification Results

- `cargo check`: passes clean (only pre-existing dead_code warnings)
- `cargo test`: 38/39 pass; 1 pre-existing failure (`test_runner_cross_block_detection`)
- All e2e tests pass including `test_e2e_pool_discovery` (live RPC, ~110s)

---

## Success Criteria

- [x] Retry logic recovers from transient 429/connection errors
- [x] No silent batch skips on RPC failure
- [x] `discover_pools` parameter count reduced to ≤4 (config struct + rpc + block range + callback)
- [x] All factory scan errors logged with `tracing::warn`
- [x] `Solidly`/`Camelot` pools correctly typed
- [x] Phase 3 dedup is O(n) not O(n²)
- [x] `--json` flag produces valid JSON output
- [x] `--rpc-concurrency` flag controls metadata fetch parallelism
- [x] `--incremental` resumes from cached max block
- [x] `--health-check` filters drained/paused pools via RPC
- [x] `--min-pools` skips on-chain when Dune threshold met
- [x] Clipper token limitation documented
