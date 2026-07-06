# Simplification Plan

Analysis of over-engineering, duplication, and consolidation opportunities in the mev-scout codebase. Estimated savings: **~1,200+ lines (~15-20% of non-comment source)**.

---

## Priority 1 (High Impact, Low Risk)

### 1. Dune SQL Query Variants — Unify 3× Duplication

**Files:** `core/src/dune/queries.rs` (1,122 lines)
**Problem:** Every major query (sandwiches, arbitrages, flash loans, liquidations, failed txs, whale transfers) exists in 3 variants — `BY_RANGE / BY_BLOCK / BY_TIME` — with identical SELECT columns and JOINs, only the WHERE clause differs. ~500 lines of copy-paste.
**Fix:** Single parameterized query with optional WHERE, or a macro to generate variants. Also fix hardcoded `ethereum` -> `{chain}` (lines 677, 794) for multi-chain correctness.
**Effort:** ~1 day

### 2. put_block_data / put_block_data_batch Duplication

**File:** `core/src/cache/store.rs` (1,208 lines)
**Problem:** Two ~120-line methods that are ~80% identical — same INSERTs, same parameter binding, same log-loop. One handles single blocks, the other loops over many.
**Fix:** Delegate `put_block_data` to `put_block_data_batch` with a single-element slice.
**Effort:** ~0.5 day

### 3. Redundant Dual Log Storage (Consistency Bug)

**File:** `core/src/cache/store.rs`
**Problem:** Receipt logs are stored both as serialized BLOB in `receipts.logs` AND as individual rows in the `logs` table. No benefit, consistency risk.
**Fix:** Drop the BLOB column; reconstruct from normalized table on read.
**Effort:** ~0.5 day

### 4. build_and_send_*_tx Boilerplate

**File:** `core/src/mev/execution/live.rs`
**Problem:** Four methods (`build_and_send_arb_tx`, `_sandwich_txs`, `_jit_txs`, `_liquidation_tx`) share ~80% identical 8-step pattern — build calldata, get factory, compute gas, fetch gas prices, get nonce, sign, broadcast. Only calldata builder and safety floor differ.
**Fix:** Extract `build_and_send_tx(build_calldata: impl Fn() -> Result<Bytes>, safety_floor)`.
**Effort:** ~0.5 day

### 5. Dashboard/Summary Formatting Duplication

**File:** `core/src/mev/execution/live.rs`
**Problem:** `print_dashboard` and `print_summary` independently compute `native_whole`/`native_frac` from `U256` and P&L percentage — same arithmetic, same format strings copy-pasted.
**Fix:** Extract `format_native(wei) -> String`, `format_pnl(current, initial) -> String`.
**Effort:** ~0.25 day

### 6. Newton-StableSwap Math Duplicated in curve.rs and balancer.rs

**Files:** `core/src/pool/math/curve.rs`, `core/src/pool/math/balancer.rs`
**Problem:** `newton_stableswap_invariant` and `newton_stableswap_output` (~55 lines each) exist byte-for-byte identical in both files.
**Fix:** Extract to `core/src/pool/math/stable_swap.rs`.
**Effort:** ~0.5 day

### 7. V2 Reserve Extraction Pattern (6× Across Detectors)

**Files:** `two_hop.rs`, `multi_hop.rs`, `sandwich.rs`, `mempool.rs`, `jit_arb.rs`
**Problem:** `if token0 == token_in { (r0, r1) } else { (r1, r0) }` appears 6 times. Already extracted as `v2_reserves()` in `two_hop.rs:536` but private.
**Fix:** Make `pub` or move to `crate::pool::math`.
**Effort:** ~0.25 day

### 8. check_and_update_seen Duplicated Verbatim

**Files:** `core/src/mev/detectors/two_hop.rs:87-108`, `core/src/mev/detectors/multi_hop.rs:68-89`
**Problem:** 22-line dedup method duplicated byte-for-byte.
**Fix:** Extract to a shared `DedupSet` struct or free function.
**Effort:** ~0.25 day

### 9. MevOpportunity Constructor Boilerplate (9× Detectors)

**Files:** All detector files
**Problem:** Every detector fills 22 struct fields inline with 15+ common defaults (`canonical_id: None`, `mempool_only: false`, etc.) — ~20 lines × 9 = ~180 lines of noise.
**Fix:** Migrate detectors to use `MevOpportunity::new()` + builder setters (already exist but unused).
**Effort:** ~0.5 day

---

## Priority 2 (Moderate Impact)

### 10. PGA Misclassified as Detector

**File:** `core/src/mev/detectors/pga.rs`
**Problem:** `pga.rs` is a post-processing profit-adjustment utility, not a detector — no `process_tx`/`detect` interface, no state.
**Fix:** Move to `crate::mev::pga`.

### 11. ABI Decoding Helpers Private to mempool.rs

**File:** `core/src/mev/detectors/mempool.rs:159-180`
**Problem:** `abi_decode_u128`, `abi_decode_address`, `abi_decode_u256` are generic utilities locked in a detector module.
**Fix:** Move to `crate::utils` or `crate::abi`.

### 12. PoolInitResult Tuple Variants — Fragile

**File:** `core/src/pool/state/factory.rs`
**Problem:** `BalancerState(8 positional fields)`, `CurveState(8 positional fields)` — mixing up tuple positions is silent at compile time.
**Fix:** Replace with named structs `BalancerInitData` and `CurveInitData`.

### 13. Inconsistent Selector Definition Styles

**Files:** `factory.rs`, `discovery.rs`
**Problem:** 4 different styles for 4-byte selectors — `const [u8; 4]` at module level, inside functions, `static LazyLock<Bytes>`, `Bytes::from_static`. No rationale.
**Fix:** Consolidate all to `const [u8; 4]` in a single `selectors.rs` module.

### 14. 3-Layer Parameter Passthrough in discovery.rs

**File:** `core/src/pool/discovery.rs`
**Problem:** `discover_pools` → `discover_and_cache` → `discover_pools_with_sources` all pass the same 11 parameters. Adding one means touching all 3 signatures.
**Fix:** Bundle into a `DiscoveryConfig` struct.

### 15. V2 Math Lives in core.rs

**File:** `core/src/pool/math/core.rs`
**Problem:** `core.rs` does 3 things: quote dispatcher, V2 constant-product math, generic arbitrage optimizer. V3/Curve/Balancer each have their own file; V2 doesn't.
**Fix:** Move `constant_product_output_amount` / `constant_product_input_amount` to `v2.rs`.

### 16. Factory Event Handlers Are Data-Driven Candidates

**File:** `core/src/pool/discovery.rs`
**Problem:** 6 factory event handlers (V2, V3, Balancer, Curve, Solidly, Camelot) follow the same filter-fetch-parse-insert pattern, differing only in event signature and byte offsets.
**Fix:** `Vec<FactoryEventHandler>` config + single loop. Could collapse ~200 lines to ~40.

### 17. Run Method is a Monolith

**Files:** `core/src/pipeline/runner.rs:run_block` (215 lines), `core/src/mev/execution/live.rs:run` (230 lines)
**Problem:** Each handles multiple independent concerns — replay vs live mode, settled blocks vs mempool, bankruptcy checks, dashboard output. Hard to test.
**Fix:** Split into focused methods (e.g., `run_replay_mode()`, `process_settled_blocks()`, `scan_mempool()`).

### 18. Curve Token Re-discovery via RPC

**File:** `core/src/pool/state/factory.rs:fetch_curve_state`
**Problem:** Does 2-16 RPC calls per Curve pool to rediscover tokens already obtained in `discovery.rs`. Redundant for 2-token pools.
**Fix:** Pass discovered tokens through `PoolInfo` to short-circuit the RPC loop.

### 19. Unused Gas Models

**File:** `core/src/types/strategy.rs`
**Problem:** `GasModel::P90` is a special case of `GasModel::Distribution(90)` — functionally identical but with its own variant and parsing branch.
**Fix:** Remove `P90`, treat as `Distribution(90)` at the parsing layer.

### 20. TimeBandit Strategy — Never Implemented

**File:** `core/src/types/strategy.rs`
**Problem:** `Strategy::TimeBandit` exists in the enum, Display, FromStr, and `all()` — but there is no `TimeBanditDetector`, no code paths handle it. Pure dead weight.
**Fix:** Remove until actually implemented.

---

## Estimated Effort Summary

| Category | Lines Removed | Effort |
|---|---|---|
| Dune query variants | ~500 | 1 day |
| `store.rs` put_block_data duplication | ~100 | 0.5 day |
| Dual log storage cleanup | ~30 | 0.5 day |
| Newton StableSwap dedup | ~110 | 0.5 day |
| `build_and_send_*_tx` boilerplate | ~80 | 0.5 day |
| Detector pattern consolidation (MevOpportunity, v2_reserves, dedup, PGA, ABI utils) | ~300 | 1.5 days |
| Factory event handlers (data-driven) | ~150 | 1 day |
| PoolInitResult named structs | 0 (safety) | 0.5 day |
| Misc (selectors, dead code, passthrough, V2 module) | ~80 | 1 day |
| **Total** | **~1,200+** | **~6 days** |
