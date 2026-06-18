# MEV Scout Accuracy Improvement Plan

**Current rating: 5/10** — Core revm execution and pool state tracking are sound, but 6 critical systematic distortions affect a large fraction of output.

---

## Table of Contents

1. [Critical Items — Materially Wrong Results](#1-critical-items--materially-wrong-results)
2. [High Items — Systematic Accuracy Gaps](#2-high-items--systematic-accuracy-gaps)
3. [Medium Items — Notable but Conditional Impact](#3-medium-items--notable-but-conditional-impact)
4. [Low Items — Edge Cases and Minor Issues](#4-low-items--edge-cases-and-minor-issues)
5. [Implementation Roadmap](#5-implementation-roadmap)

---

## 1. Critical Items — Materially Wrong Results

### C1. Curve & Balancer use constant-product, not real formulas

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/two_hop.rs:250-285`, `core/src/mev/multi_hop.rs:225-242` |
| **Problem** | `curve_output_amount` and `balancer_output_amount` use `x*y=k` (Uniswap V2 constant-product). Curve uses **StableSwap**: `A⋅nⁿ⋅Σxᵢ + D = A⋅D⋅nⁿ + Dⁿ⁺¹/(nⁿ⋅Πxᵢ)`. Balancer uses **weighted product**: `Π(balanceᵢ/weightᵢ)^weightᵢ = const`. Error grows as pools deviate from 1:1 or equal-weight assumptions. |
| **Impact** | Every Curve and Balancer arbitrage profit number is materially wrong. For stablecoin pools near peg this is ~1-5% error; for non-1:1 pools (e.g., stETH/ETH, Tricrypto) error can be 10-50%+. |
| **Fix** | Implement StableSwap solver (Newton's method for D, then compute output with fee) and Balancer weighted product formula with per-pool weights. |

### C2. V3 pools initialized with empty tick map

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/pool/state.rs:109-113`, `core/src/pool/v3_quote.rs:446-480` |
| **Problem** | `UniswapV3PoolState::new()` initializes `ticks: BTreeMap::new()` — empty. `init_from_rpc` fetches `slot0()` + `liquidity()` but never fetches tick data. All pre-existing LP positions are invisible for the first block(s). V3 quoting falls back to synthetic tick_spacing boundaries, capping swaps at the nearest boundary. A pool at tick=0 with spacing=60 stops after 60 ticks, not the hundreds of ticks of real liquidity. |
| **Impact** | All V3 multi-tick quotes in the first N blocks are inaccurate — both false negatives (missed opportunities requiring depth) and wrong profit sizing. |
| **Fix** | Add `eth_call` to fetch initialized tick bitmap + liquidity nets from the V3 pool contract at init time. Use `max_v3_tradeable_amount` (already exists at `v3_quote.rs:522-568`) for ternary search bounds instead of raw liquidity. |

### C3. Multi-token pools (>2 tokens) unsupported

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/two_hop.rs:289-324` |
| **Problem** | `curve_reserves` and `balancer_reserves` find "the other token" by exclusion (`find(|(k, _)| **k != shared_token)`), assuming exactly 2 tokens. Curve 3pool (DAI/USDC/USDT), Tricrypto, and Balancer pools with 3-8 tokens are incorrectly handled. The reserve extraction picks the first non-shared token, ignoring additional tokens entirely. |
| **Impact** | Two-hop arb detection for multi-token pools produces wrong reserve ordering, wrong direction, or None — silently missing or misreporting opportunities. |
| **Fix** | Generalize to N-token pools: for two-hop arb, the path involves only 2 of N tokens. Correctly identify which 2 tokens the arb flow uses, get their correct reserves, and keep a third "unused" token's reserves constant (as real StableSwap does for multi-asset swaps). |

### C4. No flash loan fees in profit calculation

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/two_hop.rs:77`, `core/src/mev/multi_hop.rs:171`, `core/src/types.rs:158-163` |
| **Problem** | All arbitrage profit estimates subtract gas but **not** flash loan fees. Aave charges 0.09%, Uniswap V2/V3 charges 0.05-0.30%. The `FlashLoanProvider` enum exists but is never used in profit computation. For tight arb spreads (common for stablecoin pairs), fees can consume 10-50%+ of nominal profit. |
| **Impact** | All arb profits are inflated by the unsubtracted flash loan fee. Users selecting a specific provider get no cost differentiation in results. |
| **Fix** | Add per-provider fee rate lookup (Aave=0.09%, Balancer=0%, Uniswap=0.05-0.30%), multiply by `input_amount` (flash loan principal), and subtract from `expected_profit` in `GasConfig` or a new `FlashLoanConfig`. |

### C5. Profit denominated in arbitrary token, not native currency

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/pool/math.rs:138-143`, `core/src/mev/multi_hop.rs:180`, `core/src/mev/two_hop.rs:86`, `core/src/run.rs:301` |
| **Problem** | `profit = output_amount - input_amount` where `input` and `output` are **different tokens** (e.g., input=USDC, output=USDT). The subtraction assumes 1:1 value ratio. Gas cost is always in native token wei. The filter `expected_profit > U256::from(gas_cost_wei)` at `run.rs:301` compares e.g. 100 USDC profit vs 50 MATIC gas — dimensionally inconsistent. Every opportunity with `token_out != wrapped_native` has an incorrect absolute profit number. |
| **Impact** | Opportunities may be incorrectly kept or filtered. JIT fees accruing in token0/token1 have no conversion. Aggregate USD numbers compound the error. |
| **Fix** | Add a price oracle (use pool reserves as reference) to convert all profit to wrapped native token before comparison. Store both raw and normalized profit in `MevOpportunity`. |

### C6. Detection runs post-tx, not pre-tx

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/run.rs:218-220` |
| **Problem** | `pm.update_from_logs(&tx.logs)` runs **before** all 5 detectors fire. All strategies see post-swap reserves. An arb opportunity that existed *before* the swap but was consumed *by* the swap is invisible. Only residual post-tx opportunities are detected — those the market didn't bother taking. |
| **Impact** | The engine systematically misses the most obvious, highest-profit opportunities. Detected opportunities are systematically the *least* profitable ones at each block position. |
| **Fix** | Run detection **before** applying the current tx's log updates. Keep a pre-tx snapshot of pool state, detect against it, then apply logs after detection. This requires either cloning pool state per tx (expensive) or refactoring to run detection between blocks of log processing. |

---

## 2. High Items — Systematic Accuracy Gaps

### H1. Ternary search assumes concave profit function (V3 is step-function)

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/pool/math.rs:88-124, 173-206` |
| **Problem** | Ternary search assumes profit is a concave function of input amount. V3 concentrated liquidity creates step-function liquidity at tick boundaries, producing profit functions with **multiple local maxima**. Ternary search converges to whichever local optimum it first finds, not the global optimum. For multi-hop V3 paths with several tick crossings, this can miss profitable opportunities entirely. |
| **Impact** | V3 arbitrage optimal input amounts are unreliable. Some profitable paths may be missed, others reported with suboptimal input. |
| **Fix** | Replace ternary search with golden-section search + multiple random restarts, or a grid search over log-spaced input amounts followed by local refinement. For V2-only paths (smooth concave), ternary is fine — branch by pool type. |

### H2. Same opportunity re-emitted across multiple tx indices

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/run.rs:228-254, 301` |
| **Problem** | Detectors fire after EVERY transaction. If a price gap persists for 5+ txs, the same arbitrage is emitted 5+ times. The profit filter at `run.rs:301` only checks `profit > gas`, not deduplication. Each emission has a different `tx_index` so they appear as distinct opportunities. |
| **Impact** | Inflates opportunity count by 2-10x in aggregate. Wastes storage and downstream processing. Misleads users about opportunity frequency. |
| **Fix** | Add a per-block deduplication cache keyed by `(pool_a, pool_b, token_in, token_out)` after the first detection. Only re-detect when pool reserves change sufficiently (>0.1% price movement) for an existing pair. |

### H3. Arbitrage pairs sorted by address, not liquidity

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/pool/state.rs:372-384` |
| **Problem** | `max_pairs_per_token=50` (`state.rs:235`) truncates pool addresses sorted by address, not by liquidity. High-volume pairs (e.g., WMATIC/USDC on QuickSwap with $50M TVL) may be excluded while a tiny 2-tick V3 pool with $1K TVL is included. The cap is applied uniformly per token, so highly connected tokens like WMATIC lose the most. |
| **Impact** | The most profitable arbitrage paths may be missing from the candidate set. Low-liquidity pairs waste compute time with near-zero profit potential. |
| **Fix** | Sort pools by TVL/liquidity before truncation. Make `max_pairs_per_token` configurable per token tier (high/medium/low connectivity). Or replace with a minimum-liquidity filter. |

### H4. V3 `pool_max_input` uses liquidity units, not token amounts

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/multi_hop.rs:192-204`, `core/src/mev/two_hop.rs:123` |
| **Problem** | `multi_hop.rs:192-204` calls `max_v3_tradeable_amount` (correct, returns token amounts). But `two_hop.rs:123` uses `cmp::max(a.liquidity, b.liquidity)` — raw V3 liquidity parameter in sqrt-token units (~`√k` with Q64 scaling). For a pool with $10M TVL, liquidity ≈ 1e18, max_input = 1e18, but actual max swap is ~1e7 (USDC). The ternary search evaluates inputs from 0 to 1e18, spending most iterations on infeasibly large values. |
| **Impact** | V3+V3 two-hop arb is slow (wasted iterations) and the search may converge to a suboptimal input because the region containing the true optimum is a tiny fraction of the search space. |
| **Fix** | Use `max_v3_tradeable_amount` for V3 pools in both `two_hop.rs` and `multi_hop.rs`. |

### H5. Pool state diverges on failed blocks

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/run.rs:321-343` |
| **Problem** | `run_range:328` catches errors with `match self.run_block(block_num)`. On failure (e.g., RPC timeout, missing block), it logs and continues. But pool state from the last successful block is NOT rolled back and may diverge from chain reality. The next block's detection runs against a state that doesn't reflect on-chain history. |
| **Impact** | A single failed block corrupts all subsequent blocks' detection. Reserves drift from reality, producing false positives/negatives for the rest of the range. |
| **Fix** | On block failure, reload pool state from RPC at the failed block's parent before continuing. Or checkpoint + rollback pool state on error. |

### H6. Multi-hop odd-length paths compare different tokens

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/multi_hop.rs:165-167, 180` |
| **Problem** | For 3-pool paths (e.g., USDC→WMATIC→USDT), `token_in=USDC` and `token_out=USDT`. `output_amount - input_amount` compares two different stablecoins as if 1:1. For the test at line 165 (`output_amount <= input_amount`), a path that produces 100 USDT from 99 USDC would show profit=1 — correct in spirit, but the guard also incorrectly rejects paths where 100 USDT from 100 USDC (no profit) would pass if USDT were worth 0.99 USDC. |
| **Impact** | Small stablecoin price deviations (0.01-1%) can flip the profit test, causing false negatives or false positives. |
| **Fix** | Only emit multi-hop opportunities where `token_in == token_out` (cyclic paths). For non-cyclic paths, convert output to input token using a reference price before comparison. |

### H7. Gas costs are strategy-fixed, not per-opportunity

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/types.rs:358-363` |
| **Problem** | Hardcoded defaults: TwoHopArb=150k, MultiHopArb=300k, Jit=300k, JitArb=350k, Sandwich=200k. Real gas varies by 2-5x: V2 arb ≈80k gas; V3 arb crossing 5 ticks ≈250k; JIT mint+swap+burn ≈500k (mint=150k, swap=200k, burn=150k). |
| **Impact** | Net profit estimates are consistently wrong by the gas error margin. Complex V3 arb may appear profitable at 150k gas but actually cost 400k+. |
| **Fix** | Compute gas per opportunity: estimate V3 tick crossings needed, count pool hops, sum estimated calldata cost. Use empirical gas benchmarks calibrated per pool type. |

### H8. No mempool data

| Aspect | Detail |
|--------|--------|
| **Problem** | Only on-chain settled txs are analyzed. Cannot simulate failed bundles, PGA bids, or private order flow (Flashbots, bloXroute, MEV-Share). Misses the majority of real-world MEV competition dynamics — the most informative signal for what *could* happen vs what *did* happen. |
| **Impact** | Fundamental blind spot. All backtest results are conditional on "assuming we only see settled blocks," which excludes the most interesting MEV scenarios. |
| **Fix** | Integrate MEV-Share data feed for failed searcher txs. Add pending tx pool capture via `eth_getBlockByNumber "pending"`. |

### H9. No slippage/price impact on profit estimates

| Aspect | Detail |
|--------|--------|
| **Problem** | Optimal input from ternary search is a single point. Real execution has variance from other txs landing in the same block (frontrunning, same-block competition). No range or confidence interval is provided. |
| **Impact** | Users see a single profit number with no indication of variance. An opportunity with 0.1 ETH profit at the optimal input might have 0.05-0.15 ETH range at +/-1% input; knowing this bounds the estimate's reliability. |
| **Fix** | Evaluate profit at input ±1%, ±2% and report min/max alongside optimal. Add a "slippage sensitivity" metric. |

### H10. Gas model = receipt effective gas price

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/types.rs:376-384` |
| **Problem** | `GasModel::HistoricalExact` uses `base_fee + priority_fee`. But a winning bot in a PGA pays more than receipt gas price — they must outbid competing txs. The P90 model (`base_fee * 150%`) is a crude proxy. |
| **Impact** | Gas costs are systematically underestimated for contested blocks. Profit estimates are too optimistic for high-competition opportunities. |
| **Fix** | Model gas price distribution from recent blocks' effective gas prices. Estimate percentile needed to win inclusion for each opportunity type. Account for EIP-1559 base fee dynamics (base fee adjusts between blocks based on gas usage). |

---

## 3. Medium Items — Notable but Conditional Impact

### M1. JIT fee estimation doubly approximate

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/jit.rs:225-241` |
| **Problem** | Single-step `swap_volume × fee × (pos_liq / total_liq)` without tracking `feeGrowthGlobal0X128`/`feeGrowthGlobal1X128`. Real fees accrue continuously per-tick based on actual tick-crossing sequence. The simplified formula can over/underestimate by 2-10x for brief in-range positions where only a fraction of swap volume trades against the position's specific tick range. |
| **Fix** | Track `feeGrowthGlobal0X128` and `feeGrowthGlobal1X128` per V3 pool. Compute exact fee accrual for a position across its lifetime in the block using tick-relative fee growth deltas. |

### M2. V3 synthetic tick boundaries overly conservative

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/pool/v3_quote.rs:653-655` |
| **Problem** | When `has_real_tick == false`, swap stops at the nearest tick_spacing boundary. On mainnet, V3 pools have dense liquidity across hundreds or thousands of ticks. For a 0.3% pool (spacing=60) at tick=0, the max swap without tick data is 60 ticks — real pools often have 1000+ ticks of continuous liquidity. |
| **Impact** | Any V3 swap crossing more than one tick_spacing interval is truncated in the first block. Systematically underestimates V3 depth and arb profits. |
| **Fix** | Primary: fix C2 (bootstrapping tick data). Secondary: when tick data is unavailable, don't cap at spacing boundary — allow unbounded quoting but issue a warning that accuracy is reduced. |

### M3. No state-reexecution fact-check

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/fact_check.rs:120-151` |
| **Problem** | `verify_opportunities` only does structural checks (profit > gas, field presence). Does not re-execute the detected opportunity against forked EVM state. Bugs in detection math (wrong reserve direction, incorrect fee application) pass fact-check undetected. |
| **Impact** | Detection bugs are invisible until someone manually verifies results. False positives go unreported. |
| **Fix** | Add a "replay opportunity" method: construct a tx that executes the detected swap(s) against the forked state at the detected block position, run through revm, and report actual profit vs expected profit. |

### M4. In-memory RPC cache has no block-level invalidation

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/replay.rs:95-100` |
| **Problem** | `CachedRpcDb` caches account/storage data across blocks. When `set_block_number(n)` is called, cached entries from a different block may be served as stale. If slot values change between blocks, the wrong state is used during EVM execution. |
| **Impact** | EVM execution can produce incorrect results (wrong balance, wrong storage reads) when cache serves stale data. This is silent — no error is raised. |
| **Fix** | Invalidate cache entries on `set_block_number`. Use block number as part of the cache key. Or implement block-scoped cache regions. |

### M5. Sandwich profit spot-rate fallback for unsupported DEX types

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/sandwich.rs:179-192` |
| **Problem** | When converting sandwich profit to native token, if `find_pair_pool` returns a non-V2/V3 pool, falls back to `profit_raw * reserve_buy / reserve_sell` with **no fee** and **no price impact**. For thin pools, this overestimates profit by fee amount + price impact. |
| **Fix** | Use the pool's own quoting function (e.g., `curve_output_amount`, `balancer_output_amount`) for the conversion instead of raw spot rate. |

### M6. JitArb profit model structurally naive

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/jit_arb.rs:296-304` |
| **Problem** | `estimate_arb_profit` = absolute difference of two swap amounts in shared token. Does not model fee revenue from JIT position (that's in the separate `JitDetector`). Does not account for swap direction or whether the two swaps are in opposite directions. The `proximity_window` (default 1, meaning same or adjacent tx) is arbitrary. |
| **Fix** | Merge JIT fee revenue with arb profit in a single economic calculation. Model direction: one swap buys the pool token, the other sells it. Remove or empirically tune proximity window. |

### M7. No competition / searcher density model

| Aspect | Detail |
|--------|--------|
| **Problem** | Every opportunity is assumed exclusively capturable. In reality, obvious arbitrage (stablecoin de-pegs, CEX-DEX arb) is contested by dozens of searchers. PGA dynamics mean only the fastest/highest-gas searcher wins, and their profit = their bid minus next-highest bid — not the full arb spread. |
| **Impact** | All profit estimates are upper bounds. For highly visible opportunities, actual profit may be 10-90% lower. |
| **Fix** | Add a competition model: estimate number of competing searchers based on opportunity visibility (pool TVL, token volatility, time since last similar opportunity). Subtract an estimated "winning bid premium" from profit. |

### M8. No block-building constraints

| Aspect | Detail |
|--------|--------|
| **Problem** | Real MEV submission requires building a full block (MEV-Boost) or finding a builder to include your bundle. Gas limits constrain how many ops fit per block. Multiple opportunities in the same block may conflict (same pool, overlapping tx indices). |
| **Impact** | Backtest may report 3 profitable opportunities in one block when only 1 would fit given gas limits and ordering constraints. |
| **Fix** | Add a block builder model: sort opportunities by profit/gas, pack them into a block respecting gas limit, reject conflicting ops. |

### M9. Pool init depends on RPC historical state

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/pool/state.rs:461-553` |
| **Problem** | Pool reserves are fetched via `eth_call` at the first block of the range. If the archive node is pruned, or the pool didn't exist yet, `eth_call` returns garbage or fails. |
| **Impact** | Wrong initial reserves cascade through all subsequent detection. |
| **Fix** | Use `eth_getStorageAt` as primary path (cheaper, available on more nodes). Add validation heuristics (reserves > 0, reasonable ratio). Fall back to loading from a previous cached state. |

---

## 4. Low Items — Edge Cases and Minor Issues

| # | Item | Impact | Files |
|---|------|--------|-------|
| L1 | **No liquidation detection** (Aave/Compound/Morpho) — major MEV category entirely absent | Missing whole strategy class | (missing module) |
| L2 | **No cross-block MEV** — time-bandit attacks, multi-block arb, reorgs not modeled | Missing advanced strategies | (architectural gap) |
| L3 | **Single USD price applied to all profit** — JIT fees in non-native tokens mispriced | Display error for per-strategy USD | `aggregate.rs:83, 285` |
| L4 | **Token decimals hardcoded to 18** — USDC (6), USDT (6), WBTC (8) display wrong | Display formatting only | `fact_check.rs:136` |
| L5 | **CoinGecko midpoint price** — not executable; may not reflect on-chain price | Display-only, users may misinterpret | `coingecko.rs` |
| L6 | **V2 reserve storage slot 6 fails for non-standard forks** — Camelot, Velodrome use different layouts | Wrong reserves for some pools | `state.rs:672-679` |
| L7 | **Curve pool init limited to 4 tokens** — 4+ token pools get truncated state | Incomplete state for some Curve pools | `state.rs:991` |
| L8 | **No dynamic pool discovery during replay** — pools created after start block missed | Incomplete pool coverage for long ranges | `discovery.rs` |
| L9 | **No opportunity deduplication** — no canonical ID; aggregate counts inflated | Misleading summary metrics | `aggregate.rs` |

---

## 5. Implementation Roadmap

### Phase 1 — Quick Wins (estimated: 2-4 days)

| Priority | Item | Effort | Impact |
|----------|------|--------|--------|
| 1 | **H3** — Sort pools by liquidity not address | 0.5 day | Prevents missing top arb pairs |
| 2 | **H4** — Use `max_v3_tradeable_amount` for V3 bounds | 0.5 day | Fixes wrong ternary search range |
| 3 | **H2** — Deduplicate per-(pool_a,pool_b,token_in,token_out) per block | 1 day | 2-10x count inflation fix |
| 4 | **H7** — Estimate gas per-opportunity from pool types + hops | 1 day | Fixes 2-5x gas cost error |
| 5 | **H6** — Only emit multi-hop where token_in == token_out | 0.5 day | Eliminates false comparisons |

### Phase 2 — Core Architecture (estimated: 1-2 weeks)

| Priority | Item | Effort | Impact |
|----------|------|--------|--------|
| 6 | **C6** — Pre-tx detection (snapshot state → detect → apply logs) | 3 days | Eliminates systematic missed-opportunity bias |
| 7 | **C5** — Normalize profit to native token via reference price | 2 days | Fixes dimensionally inconsistent profit/gas comparison |
| 8 | **C1** — Real StableSwap + Balancer weighted formulas | 3 days | Fixes all Curve/Balancer profits |
| 9 | **C2** — Bootstrap V3 tick data from RPC at init | 2 days | Fixes first-block V3 underestimation |
| 10 | **C4** — Subtract flash loan fees per provider | 1 day | Fixes 5-20% profit inflation |

### Phase 3 — Advanced (estimated: 2-4 weeks)

| Priority | Item | Effort | Impact |
|----------|------|--------|--------|
| 11 | **M3** — Fact-check with state re-execution | 3 days | Catches detection bugs automatically |
| 12 | **H1** — Multi-start global optimization for V3 paths | 2 days | Finds missed V3 arb opportunities |
| 13 | **C3** — N-token pool support for Curve/Balancer | 3 days | Fixes multi-token pool detection |
| 14 | **M1** — Real JIT fee accrual via feeGrowthGlobal tracking | 3 days | 2-10x JIT profit error → <1% |
| 15 | **M4** — Block-aware RPC cache invalidation | 1 day | Eliminates stale-state execution bugs |

### Phase 4 — Stretch Goals (estimated: 1-2 months)

| Priority | Item | Effort | Impact |
|----------|------|--------|--------|
| 16 | **H8** — MEV-Share / mempool integration | 4 weeks | Opens failed-tx and pending-tx analysis |
| 17 | **H10** — PGA simulator with gas-price distribution model | 2 weeks | Realistic gas cost under competition |
| 18 | **H9** — Slippage-adjusted profit ranges | 3 days | Adds confidence intervals to estimates |
| 19 | **M7** — Competition/searcher density model | 2 weeks | Upper-bound → expected profit |
| 20 | **M8** — Block builder + gas-packing optimizer | 2 weeks | Realistic submission constraints |
| 21 | **M5-M6** — Improve sandwich profit conversion + JitArb model | 3 days | Better profit estimates for these strategies |
| 22 | **L1-L9** — Long tail of minor fixes | 1 week | Polishes remaining inaccuracies |

---

**Key insight:** The first 10 items in Phases 1-2 address all 6 critical issues and 5 out of 10 high issues. Completing Phase 1+2 moves the accuracy rating from **5/10 → ~8/10**.
