# Arbitrage Detection — Optimization Plan

Tips for improving **speed** and **accuracy** of two-hop / multi-hop arbitrage detection, ordered by value-to-effort.

---

## Speed

### 1. Spot-price pre-filter (highest impact)
**Problem:** `TwoHopArbDetector::detect` (`core/src/mev/detectors/two_hop.rs:50`) runs the full numeric
optimizer (~100+ quote evaluations, each walking V3 ticks) for *every* pool pair, both directions,
every block — even when prices are aligned.

**Fix:** Before optimizing, compare marginal spot prices:
- V2: `reserve_in / reserve_out`
- V3: `sqrtRatioX96²`

Skip the pair when `|pA/pB − 1| < combined_fees + gas_break_even` (keep a safety margin).
Eliminates ~90%+ of work at zero accuracy loss.

### 2. Incremental / dirty-pool scanning
**Problem:** `pipeline/runner.rs` rescans all arbitrage pairs every block.

**Fix:** Track pools touched by the block's swaps (pool/state/apply.rs) in a dirty set;
restrict detection to pairs containing at least one dirty pool. Untouched pool states cannot
produce new opportunities (H2 dedup already relies on this invariant).

### 3. Closed-form optimum for V2↔V2
**Problem:** `optimal_two_hop_arb` (`core/src/pool/math/core.rs:154`) uses 80 ternary-search iterations
with integer rounding error on the u128 domain.

**Fix:** Two constant-product pools have an analytic optimal input formula. Use it for V2↔V2
(exact result, ~100× fewer evaluations); keep the numeric path for mixed pool types.

### 4. Avoid cloning `arbitrage_pairs()`
**Problem:** `PoolManager::arbitrage_pairs` (`core/src/pool/state/manager.rs:217`) clones the whole
`Vec` on every call — invoked twice per block (two-hop + multi-hop seed).

**Fix:** Cache as `Arc<Vec<(Address, Address, Address)>>` and return clones of the Arc.

### 5. Prune multi-hop BFS enumeration
**Problem:** `MultiHopArbDetector::extend_path` (`core/src/mev/detectors/multi_hop.rs:82`) enumerates
all paths up to depth 4 — combinatorial explosion on dense token graphs.

**Fix:** Negative-cycle detection on a `-log(exchange_rate)` graph (Bellman–Ford / SPFA).
Finds all profitable cycles without path enumeration; work bounded by O(V·E).

---

## Accuracy

### 6. Deterministic V3 optimization (replace random restarts)
**Problem:** `grid_plus_refine` (`core/src/pool/math/core.rs:302`) uses coarse grid + golden-section
+ 5 stochastic restarts because V3 profit is non-convex. May still miss peaks.

**Fix:** V3 profit is *piecewise concave* with known breakpoints (tick boundaries /
`max_v3_tradeable_amount` segments). Enumerate segments exactly and ternary-search within each →
guaranteed global optimum, deterministic, fewer quote evaluations.

### 7. Calibrate gas limits from observed swaps
**Problem:** Static constants (e.g. `V3_POOL_GAS = 120_000`, `consts.rs`) ignore tick-crossing counts.
`GasPriceDistribution` (`pipeline/gas.rs`) models gas *price* well, but the *limit* side stays static.

**Fix:** While scanning blocks, average actual `gasUsed` per `(dex_type, hop_count)` bucket
and feed those into `estimate_gas_for_two_hop`.

### 8. Keep golden-section bounds in integer arithmetic
**Problem:** f64 casts in `golden_section_maximize` (`core/src/pool/math/core.rs:253`) lose precision
for inputs above ~2^53 wei.

**Fix:** Maintain `[lo, hi]` interval updates in u128; use f64 only for the φ ratio step sizing,
or switch to integer Fibonacci search.

### 9. Filter transfer-tax / honeypot tokens
**Problem:** Simulation assumes full output received. Tokens with sell taxes produce phantom
opportunities → systematically overstated backtest profit.

**Fix:** Detect buy/sell tax in `cache/token_cache.rs` (simulate small round-trip or check known
tax registries); exclude taxed tokens from candidate generation or flag them with low confidence.

### 10. Competition-realism scoring
**Observation:** Opportunities at `tx_index` assume exclusive insertion rights.

**Fix:** Record how long an opportunity persists across blocks (H2 dedup state can supply this);
use persistence as a competitiveness proxy and rank/filter opportunities accordingly.

---

## Suggested order

1. #1 Spot-price pre-filter — highest speed win, minimal risk
2. #3 Closed-form V2↔V2 optimum — exact + fast
3. #6 Deterministic V3 segmentation — removes stochastic misses
4. #2 Dirty-pool incremental scan — requires runner refactor
5. #7, #9 accuracy hardening
