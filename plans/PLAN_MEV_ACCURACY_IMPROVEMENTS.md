# MEV Detector Accuracy Improvements — Implementation Plan

## Overview

This plan covers 13 accuracy improvements across 5 MEV detection modules in `core/src/mev/`, plus 3 cross-cutting structural items. The work is organized into 5 milestones that follow dependency chains — each milestone can be tested independently.

## Milestone Structure

| Milestone | Focus | Issues | Effort | Dependencies |
|-----------|-------|--------|--------|--------------|
| M1 | Profit & Gas Plumbing | #1, #2, #11 | Medium | None |
| M2 | JIT Detector Correctness | #3, #8 | Medium | M1 |
| M3 | Sandwich Detector: V3 + Profit | #4, #6 | Large | M1 |
| M4 | Arbitrage Optimizer Fixes | #5, #9, #10 | Medium | None |
| M5 | Structural & Edge Cases | #7, #12, #13 | Small | M1-M4 |

---

## M1: Profit & Gas Plumbing (estimated: 3-5 days)

### Issue #1: JIT/JitArb `expected_profit = 0`

**Problem:** `jit.rs:175` and `jit_arb.rs:183` hardcode `expected_profit: U256::ZERO`.

**Implementation:**

1. **JIT fee estimation** (`jit.rs`):
   - In `ActiveMint`, add field `swap_amounts: Vec<u128>` to track amounts from swaps that touched this mint's range.
   - In `process_tx()`, when processing a V3 swap event (`V3_SWAP_TOPIC`), decode the full event using `decode_v3_swap()` to get post-swap `sqrt_price_x96` and `tick`.
   - For each active mint on that pool, check if the swap occurred within the mint's tick range. This requires knowing the pre-swap tick. Approach:
     - Before processing any logs in the tx, snapshot the current pool state from `PoolManager` (pre-swap `sqrt_price_x96`/`tick`).
     - A swap is within range if `pre_swap_tick < tick_upper && post_swap_tick > tick_lower` for `zero_for_one`, or conversely.
     - If within range, record `swap_amount_in` from the decoded event (the absolute value of amount0 if zero_for_one else amount1).
   - In `build_opp()`, compute profit as:
     ```
     total_swap_volume = sum(swap_amounts)
     fee_rate = pool.fee (from pool.info, e.g., 3000 = 0.3%)
     estimated_fees = total_swap_volume * fee_rate / 1_000_000
     expected_profit = U256::from(estimated_fees)
     ```
   - **Simplification for v1:** Use a fixed proportion of swap volume: `swap_volume * fee / 1_000_000`. This treats the JIT position as capturing all fees from the swap, which is an overestimate when multiple positions exist in the same range. Acceptable as upper-bound estimate.

2. **JitArb profit estimation** (`jit_arb.rs`):
   - In `build_opp()`, the JitArb involves two swaps on different pools by the same sender.
   - Extract `amount_in` from the swap log on pool P (the JIT pool) and `amount_out` from the swap on pool Q (the arbitrage pool).
   - If both amounts are available, compute `profit = amount_out - amount_in` (in shared token terms).
   - Use `pools_share_token()` to determine which token is common and convert to a single numeraire.
   - Set `expected_profit` accordingly. Fall back to `U256::ZERO` if conversion isn't possible.

**Files to modify:** `core/src/mev/jit.rs`, `core/src/mev/jit_arb.rs`

**Tests:**
- `test_jit_fee_estimation`: Create a detector, process a mint + swap at overlapping tick range, verify `expected_profit > 0`.
- `test_jit_fee_no_overlap`: Process a mint and a swap at non-overlapping tick range, verify `expected_profit == 0`.
- `test_jitarb_profit_estimation`: Create pools with known reserves, process mint + arb swap pair, verify profit > 0.

---

### Issue #2: `gas_cost_wei = 0` for JIT, Sandwich, JitArb

**Problem:** `jit.rs:176`, `sandwich.rs:192`, `jit_arb.rs:184` hardcode `gas_cost_wei: 0`.

**Implementation:**

For each detector:

1. **JitDetector:** Pass `base_fee_per_gas` and `gas_config` parameters into `detect()`. Compute:
   ```rust
   let gas_cost_wei = gas_config.compute_gas_cost(Strategy::Jit, base_fee_per_gas, &HashMap::new());
   ```
   Store result in `MevOpportunity.gas_cost_wei`.

2. **SandwichDetector:** Same pattern — pass `base_fee_per_gas` and `gas_config` into `detect()`. Use `Strategy::Sandwich`.

3. **JitArbDetector:** Same pattern — pass `base_fee_per_gas` and `gas_config` into `detect()`. Use `Strategy::JitArb`.

**Caller changes** (`run.rs`):
- `jit_detector.detect(timestamp)` → `jit_detector.detect(timestamp, base_fee_per_gas, self.gas_config)`
- `sandwich_detector.detect(timestamp, &pm)` → `sandwich_detector.detect(timestamp, &pm, base_fee_per_gas, self.gas_config)`
- `jit_arb_detector.detect(timestamp, &pool_manager.borrow())` → `jit_arb_detector.detect(timestamp, &pool_manager.borrow(), base_fee_per_gas, self.gas_config)`

**Files to modify:** `core/src/mev/jit.rs`, `core/src/mev/sandwich.rs`, `core/src/mev/jit_arb.rs`, `core/src/run.rs`

**Tests:**
- Verify `gas_cost_wei > 0` after fix for each detector.
- Test with different `GasConfig` values (different gas models, different priority fees) to ensure it propagates correctly.

---

### Issue #11: Inconsistent profit filtering

**Problem:** TwoHop/MultiHop filter (`profit > 0` at gross level), JIT/Sandwich/JitArb don't.

**Implementation:**
- After M1 fixes, all detectors will have meaningful `expected_profit` and `gas_cost_wei`.
- Add a post-detection filter in all detectors (or in `run.rs` at the collection point) that drops opportunities where `expected_profit < gas_cost_wei`.
- **Preference:** Apply the filter in `run.rs` uniformly rather than in each detector. This keeps detectors as pure detection and lets the caller decide the profitability threshold.

**Files to modify:** `core/src/run.rs` (single filter point after `all_opportunities.extend(...)`)

**Tests:**
- Verify that opportunities with `expected_profit < gas_cost_wei` are excluded from results.

---

## M2: JIT Detector Correctness (estimated: 3-4 days)

### Issue #3: Swap marks ALL active mints on pool as swapped

**Problem:** `jit.rs:116-119` marks every active mint as swapped when ANY swap occurs on the pool.

**Implementation:**

1. **Extract pre-swap state:** In `process_tx()`, before processing logs, snapshot the tick of each pool that has active mints. Since we don't have pre-swap state directly from the V3 swap event (it only contains post-swap tick), we maintain a `pool_state_cache: HashMap<Address, i32>` within `JitDetector` that records the last-known tick for each pool.

2. **Determine tick overlap:** For each V3 swap event, decode the post-swap tick. Check each active mint:
   - `zero_for_one` direction: the price moves down (tick decreases). The swap trades against positions where `tick_lower <= post_swap_tick < tick_upper` (the position was active before and during the swap).
   - For direction-agnostic check: the position is active if `tick_lower <= current_tick < tick_upper` where `current_tick` is the pre-swap tick.
   - Only mark `mint.swapped = true` if the swap's price crossed the mint's range.

3. **Store per-mint swap amounts** (also needed for M1 JIT fee estimation):
   - Track `swap_volume_in_range: u128` on each `ActiveMint`.
   - Accumulate the `amount_in` from swaps that overlap this mint's range.

**Simplification for v1:** If tracking tick ranges adds too much complexity, a simpler (but weaker) heuristic: only mark the most recent mint on each pool as swapped. This assumes JIT positions are deployed just before the swap they intend to capture. The current "mark all" behavior is the worst option.

**Files to modify:** `core/src/mev/jit.rs`

**Tests:**
- `test_multiple_mints_only_relevant_marked`: Create two mints at different tick ranges. Process a swap that only crosses one range. Verify only the correct mint is marked `swapped`.
- `test_swap_out_of_range_no_mark`: Create a mint at tick [-100, 100]. Process a swap at tick 500 (far outside range). Verify mint is NOT marked.

---

### Issue #8: Burn matching doesn't verify sender

**Problem:** `jit.rs:98-109` matches burns to mints by pool + tick range only.

**Implementation:**

1. Add `sender: Option<Address>` to `ActiveMint` (it already exists but `#[allow(dead_code)]` — on line 18).
2. In the burn matching loop (line 99), add a sender check:
   ```rust
   if mint.burned { continue; }
   if let Some(sender) = sender {
       if mint.sender != Some(sender) { continue; }
   }
   ```
   This matches the pattern already used in `JitArbDetector::process_tx()` (line 84).

**Files to modify:** `core/src/mev/jit.rs`

**Tests:**
- `test_burn_different_sender_no_match`: Two mints at same tick range by different senders. Process a burn from sender 2. Verify only sender 2's mint is marked burned.

---

## M3: Sandwich Detector: V3 + Profit Accuracy (estimated: 5-7 days)

### Issue #4: V2-only sandwich detection

**Problem:** `sandwich.rs` only processes `V2_SWAP_TOPIC`.

**Implementation:**

1. **Extend `SwapRecord`** to store DEX type or a direction indicator that works for both V2 and V3:
   ```rust
   struct SwapRecord {
       tx_index: usize,
       sender: Address,
       pool: Address,
       direction: SwapDirection,
       amount_in: u128,
       amount_out: u128,
       dex_type: DexType,  // NEW
   }
   ```

2. **Add V3 swap processing in `process_tx()`**:
   - Check for `V3_SWAP_TOPIC` in addition to `V2_SWAP_TOPIC`.
   - Use `decode_v3_swap()` to extract post-swap state. However, the V3 swap event's `amount0`/`amount1` are `int256` (signed). One is positive (input), one is negative (output). Extract:
     - `amount0 > 0` and `amount1 < 0` → `Token0ForToken1`, `amount_in = amount0`, `amount_out = |amount1|`
     - `amount0 < 0` and `amount1 > 0` → `Token1ForToken0`, `amount_in = amount1`, `amount_out = |amount0|`
   - The V3 swap topics are: `topic[0]` = topic hash, `topic[1]` = sender, `topic[2]` = recipient.
   - The event `data` contains: `int256 amount0` (32 bytes), `int256 amount1` (32 bytes), `uint160 sqrtPriceX96` (32 bytes), `uint128 liquidity` (32 bytes), `int24 tick` (32 bytes) — total 160 bytes.

3. **Extend `detect()`** to work with V3 pools:
   - The 3-tx sliding window logic is DEX-agnostic (works on `SwapRecord` regardless of source).
   - Profit calculation for V3: instead of V2 reserve-based conversion, use the same approach but via V3 pool state from `PoolManager`.
   - For V3 profit in native token: use the V3 pool's current sqrt price to estimate the conversion rate from profit token to the other token (which may be wrapped native).

**Files to modify:** `core/src/mev/sandwich.rs`

**Tests:**
- `test_v3_sandwich_detected`: Process 3 V3 swap records with same sender, proper directions, verify detection.
- `test_v3_sandwich_profit_conversion`: Process a V3 sandwich and verify `expected_profit > 0`.
- Extend existing V2 tests to ensure no regression.

---

### Issue #6: Sandwich profit uses attacked pool's reserves as oracle

**Problem:** `compute_sandwich_profit()` in `sandwich.rs:95-131` uses the attacked pool's own post-attack reserves for price conversion.

**Implementation:**

1. **Snapshot pre-attack reserves:** In `SandwichDetector`, before processing any txs, capture the pool's initial state for the block. Since `process_tx` is called sequentially and `detect()` after each tx, the pool state is already updated. To get pre-frontrun state:
   - Either snapshot at block start (`SandwichDetector::new()` could also accept a `&PoolManager` snapshot)
   - Or compute an estimated pre-attack price by reversing the frontrun swap's impact on reserves.

2. **Simpler approach (recommended for v1):** Use a separate, non-attacked reference pool for the same token pair. Query `PoolManager` for any pool that has the same token pair and is not the attacked pool. Use its reserves as the price oracle.

3. **Fallback:** If no reference pool exists, mark profit as `U256::ZERO` rather than using a distorted value. This is conservative but honest.

**Files to modify:** `core/src/mev/sandwich.rs`

**Tests:**
- `test_sandwich_profit_with_reference_pool`: Create two pools with same pair, different reserves. Verify profit uses reference pool rates.
- `test_sandwich_profit_no_reference`: Single pool, verify profit is 0 (conservative fallback).

---

## M4: Arbitrage Optimizer Fixes (estimated: 4-6 days)

### Issue #5: V3 `pool_max_input` uses raw `liquidity`

**Problem:** `multi_hop.rs:194` uses `v3.liquidity` as `max_input` for the ternary search.

**Implementation:**

1. **Compute max tradeable amount for a V3 pool:**
   - Find the nearest initialized tick in the swap direction using `find_next_initialized_tick()`.
   - If no initialized tick exists, use `MIN_TICK` (for `zero_for_one`) or `MAX_TICK` (for `!zero_for_one`).
   - Compute the sqrt price at that tick: `target_sqrt = get_sqrt_ratio_at_tick(next_tick_or_boundary)`.
   - Compute the max input using `get_amount_0_delta()` or `get_amount_1_delta()` between current sqrt price and target sqrt price:
     ```rust
     let max_in = if zero_for_one {
         get_amount_1_delta(target_sqrt, v3.sqrt_price_x96, v3.liquidity, true)
     } else {
         get_amount_0_delta(v3.sqrt_price_x96, target_sqrt, v3.liquidity, true)
     }.unwrap_or(U256::ZERO);
     ```
   - Add the fee: `max_input_with_fee = max_in * 1_000_000 / (1_000_000 - fee)`.
   - **Edge case:** If `max_input_with_fee == 0`, fall back to `v3.liquidity / 100` as a conservative bound.

2. **New helper function in `v3_quote.rs`:**
   ```rust
   pub fn max_v3_tradeable_amount(pool: &UniswapV3PoolState, zero_for_one: bool) -> u128
   ```

**Files to modify:** `core/src/mev/multi_hop.rs` (update `pool_max_input`), `core/src/pool/v3_quote.rs` (add helper)

**Tests:**
- `test_v3_max_input_equals_liquidity_when_no_ticks`: Pool with empty ticks map. Max input should fall back to `liquidity / 100`.
- `test_v3_max_input_bounded_by_nearest_tick`: Create pool with a tick boundary at -100. Verify max input is finite and correct.
- `test_v3_max_input_with_fee`: Verify fee is included in the computation.

---

### Issue #9: Hardcoded V2 min-reserve filter

**Problem:** `two_hop.rs:118-121` skips pools with reserves < 1000.

**Implementation:**

1. Remove the min-reserve check entirely (lines 118-123).
2. The `optimal_two_hop_arb()` function already handles edge cases via its own math — it will return `None` if reserves are too low for any profitable trade.
3. This is a safe change: removing a filter can only increase (not decrease) the number of opportunities found. The optimizer will naturally reject unprofitable ones.

**Files to modify:** `core/src/mev/two_hop.rs`

**Tests:**
- `test_v2_low_reserve_arbitrage`: Create two pools with reserves of 200 and 300 (below old threshold). If they have a price discrepancy, verify an opportunity is detected.

---

### Issue #10: Missing Curve/Balancer support

**Problem:** `two_hop.rs:150-152` and `multi_hop.rs:218` return `None` for Curve/Balancer.

**Implementation:**

1. **Curve quoting** (`two_hop.rs`, `multi_hop.rs`):
   - Curve StableSwap invariant: `D = n^n * S * prod(x_i) + ...` (simplified: `D` is constant, solve for output token).
   - For Curve pools, use `CurvePoolState.balances` and compute the stable-swap output:
     ```rust
     // Simplified two-token curve swap
     fn curve_quote(pool: &CurvePoolState, token_in: Address, amount_in: u128) -> Option<u128> {
         let idx_in = pool.token_index.get(&token_in)?;
         let idx_out = pool.token_index.iter().find(|(k, _)| *k != &token_in).map(|(_, v)| v)?;
         let balance_in = pool.balances[*idx_in];
         let balance_out = pool.balances[*idx_out];
         let fee = pool.info.fee as u128;
         // Compute D, then solve for output
         // ... (standard StableSwap math)
     }
     ```
   - Full StableSwap math is involved. For v1, implement only the 2-token case.

2. **Balancer quoting**:
   - Weighted pool invariant: `product(balances[i]^weights[i]) = constant`.
   - Compute output given input: adjust the invariant with fee, solve for output balance.
   - For v1, implement the basic weighted pool formula.

3. **Integration:**
   - Add a `Curve` and `Balancer` arm in `quote_single_pool()` in `multi_hop.rs`.
   - Add the same in `quote_path()` in `two_hop.rs`.

**Files to modify:** `core/src/mev/two_hop.rs`, `core/src/mev/multi_hop.rs`, optionally `core/src/pool/math.rs` (add Curve/Balancer math)

**Tests:**
- `test_curve_two_token_quote`: Create a Curve pool with known balances, verify quote matches expected output.
- `test_balancer_quote`: Create a Balancer pool with known weights/balances, verify quote.
- `test_multi_hop_curve_in_path`: Create a path that includes a Curve pool, verify quoting doesn't return `None`.

---

## M5: Structural & Edge Cases (estimated: 2-3 days)

### Issue #7: JitArb proximity window too restrictive

**Problem:** `jit_arb.rs:147` — `max_idx - min_idx > 1`.

**Implementation:**

1. Add a configurable window parameter to `JitArbDetector`:
   ```rust
   pub struct JitArbDetector {
       // ...existing fields...
       proximity_window: usize,  // NEW: max tx index gap
   }
   ```
2. Default to 1 (current behavior) to maintain backward compatibility.
3. Change the check from hardcoded `> 1` to `> self.proximity_window`.
4. In `run.rs`, pass a config value (could come from CLI or default to 3).

**Files to modify:** `core/src/mev/jit_arb.rs`, `core/src/run.rs`

**Tests:**
- `test_jitarb_proximity_window_2`: Create a mint at tx 0, swaps at tx 3 (gap of 3). With window=3, should detect. With window=1, should not.
- `test_jitarb_proximity_window_default`: Verify default behavior is unchanged.

---

### Issue #12: No validation on `MevOpportunity` strategy fields

**Problem:** No constructor validates strategy-appropriate fields.

**Implementation:**

1. Add a constructor to `MevOpportunity`:
   ```rust
   impl MevOpportunity {
       pub fn new(
           strategy: Strategy,
           block_number: u64,
           tx_index: usize,
           // ...common params...
       ) -> Self {
           // Common field initialization
       }
   }
   ```
2. Add strategy-specific builder methods or validation in `new()`:
   ```rust
   pub fn with_jit_fields(mut self, tick_lower: i32, tick_upper: i32, liquidity: u128) -> Self {
       assert!(matches!(self.strategy, Strategy::Jit | Strategy::JitArb));
       self.tick_lower = Some(tick_lower);
       self.tick_upper = Some(tick_upper);
       self.liquidity_amount = Some(liquidity);
       self
   }
   ```
3. Or, simpler: use `debug_assert!` in a post-construction validation method.

**Files to modify:** `core/src/mev/opportunity.rs`

**Tests:**
- `test_jit_opportunity_must_have_tick_fields`: Create with `Strategy::Jit` but no tick fields → panic in debug builds.
- `test_two_hop_opportunity_should_not_have_victim_fields`: Verify validation catches the error.

---

### Issue #13: O(n²) deduplication via `Vec::contains()`

**Problem:** All stateful detectors use `Vec` for `emitted` dedup with linear search.

**Implementation:**

1. Replace `Vec<(Address, usize, bool)>` in `JitDetector.emitted` with `HashSet<(Address, usize, bool)>`.
2. Replace `Vec<(Address, usize, Address)>` in `JitArbDetector.emitted` with `HashSet<(Address, usize, Address)>`.
3. Replace `Vec<(Address, usize)>` in `SandwichDetector.emitted` with `HashSet<(Address, usize)>`.
4. Change `self.emitted.contains(&key)` → `self.emitted.contains(&key)` (same API on HashSet).
5. Change `self.emitted.push(key)` → `self.emitted.insert(key)`.

**Files to modify:** `core/src/mev/jit.rs`, `core/src/mev/jit_arb.rs`, `core/src/mev/sandwich.rs`

**Tests:** All existing tests should pass unchanged (behavior-preserving refactor).

---

## Dependency Graph

```
M1 (Profit & Gas)
├── M2 (JIT Correctness) — needs M1 for gas plumbing
├── M3 (Sandwich V3 + Profit) — needs M1 for gas plumbing
│   └── M5 Item #7 (JitArb proximity) — independent
M4 (Arb Optimizer)
│   ├── #5 V3 max_input — independent
│   ├── #9 V2 min-reserve — independent
│   └── #10 Curve/Balancer — independent
M5 Items #12, #13 — independent, can be done in parallel
```

**Parallelization opportunities:**
- M1, M4, and M5 items #12/#13 can proceed in parallel.
- M2 and M3 can begin once M1's signature changes are stable.

---

## Testing Strategy

| Layer | What | Tooling |
|-------|------|---------|
| Unit tests | Each detector's test module (existing pattern) | `cargo test -p mev-scout-core` |
| Integration tests | End-to-end with synthetic blocks | `core/tests/integration.rs` |
| Regression | Compare output counts before/after on historical data | Manual run with `--range` |

**For each issue, the test criteria are:**
- Before fix: test demonstrates the bug (wrong output, missed opportunity, etc.)
- After fix: test demonstrates correct behavior
- All prior tests still pass (no regressions)

---

## Resource Allocation

| Role | Responsibility |
|------|----------------|
| 1 engineer | M1 (Profit & Gas) — foundational, touches all detectors |
| 1 engineer | M2 + M3 (JIT + Sandwich correctness) — adjacent domains |
| 1 engineer | M4 (Arb optimizer) — math-heavy, independent |
| Code review | All changes, cross-team for `v3_quote.rs` changes |

---

## Timeline

| Week | Milestones |
|------|------------|
| 1 | M1 complete (profit + gas plumbing), M5 #12/#13 complete |
| 2 | M2 complete (JIT correctness), M4 #9 complete (min-reserve) |
| 3 | M3 complete (V3 sandwich + profit oracle) |
| 4 | M4 #5 (V3 max_input) + #10 (Curve/Balancer) complete |
| 5 | Integration testing, regression runs, documentation |

---

## Risk Mitigation

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| V3 fee computation is complex | Medium | Ship simplified v1 (capture all volume × fee rate) and iterate |
| Curve/Balancer math is non-trivial | High | Implement only 2-token Curve and basic weighted Balancer; leave advanced pools for future |
| V3 sandwich detection has edge cases | Medium | Start with strict pattern (consecutive same-pool same-sender), relax later |
| Performance regression from V3 tick iteration | Low | Profile with real data; `find_next_initialized_tick` is O(n_ticks) per quote — acceptable for most pools |
