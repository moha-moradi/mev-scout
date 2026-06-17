# MEV Scout Codebase Review

## 1. Backtest Accuracy Rating: **7/10**

### Strengths (why 7, not lower)
- **Real EVM execution via revm** — faithful state replay, not just log analysis
- **Live pool state tracking** — reserves, V3 ticks/liquidity updated per-tx from Swap/Sync/Mint/Burn events
- **5 strategy detectors** with optimal input discovery (ternary search over concave profit functions)
- **Gas modeling** — base fee, priority fee, EIP-1559 effective gas price
- **Multi-DEX support** — V2, V3, Curve, Balancer pool types
- **Receipt verification** — compares revm output vs cached receipts
- **Fact-checking pass** after run

### Limitations (why not 10/10)
1. **Curve & Balancer use simplified constant-product math** (`core/src/mev/two_hop.rs:250-285`) instead of actual StableSwap/Balancer weighted formulas. Material error for non-1:1 pools.
2. **Sandwich detection is narrow** — only catches consecutive same-pool swaps by same sender. Misses cross-pool sandwiches, coordinated addresses, multi-victim patterns.
3. **JIT fee estimation is approximate** — single-step `swap_volume x fee x (pos_liq / total_liq)`. Real fees accrue continuously per-tick.
4. **No mempool data** — only on-chain settled txs. Cannot simulate failed bundles, PGA bids, or private order flow.
5. **No slippage/price impact on profit estimates** — assumes optimal input executes atomically with no competition.
6. **Gas model uses receipt effective gas price** — does not reflect what a bot pays in a priority gas auction.
7. **Pool init depends on RPC historical state** — pruned nodes yield inaccurate initial reserves.
8. **No liquidation detection** — Aave/Compound/Morpho positions not tracked.
9. **No cross-block MEV** (time-bandit, multi-block arbitrage).
10. **CoinGecko USD price is midpoint, not executable price** — display-only but users may misinterpret.

---

## 2. Speed & Optimization Improvements

### High Impact (estimated 2-10x speedup)

**1. Parallel block processing** (`core/src/run.rs:321-344`)
- Currently sequential: `for block_num in start..=end { run_block(block_num) }`
- Blocks are independent since pool state carries forward — use `rayon` or `tokio::task::spawn_blocking`
- **Prerequisite**: Replace single `Arc<Mutex<Connection>>` (cache.rs) with a connection pool (`r2d2` + `r2d2_sqlite`): one writer + N readers in WAL mode
- **Estimated**: 4-10x speedup on multi-core

**2. Arbitrage path pruning** (`core/src/mev/multi_hop.rs:48-90`)
- BFS depth-4 seeded from all arbitrage pairs. With `max_pairs_per_token=50`, paths explode combinatorially
- **Fix 1**: Early termination — stop extending paths when cumulative estimated profit < gas cost
- **Fix 2**: Path deduplication — canonical hash for same pool sets
- **Fix 3**: Reduce default `max_pairs_per_token` from 50 to 20

**3. Ternary search optimization** (`core/src/pool/math.rs:88-124, 173-206`)
- Fixed 80 iterations per path. V3 paths call `quote_v3_exact_in` per iteration (up to 256 tick-crossing steps)
- **Fix 1**: Adaptive convergence — stop when `hi - lo < 2` or interval is negligible relative to reserves
- **Fix 2**: Cache quote results for (pool, input_amount) within a block's detection pass
- **Fix 3**: Golden-section search over ternary (fewer iterations, same precision)

### Medium Impact

**4. Prefetch account/storage state** (`core/src/replay.rs` CachedRpcDb)
- Three-tier DB does individual RPC calls per slot/account. On cold cache, this devastates blocks with many DEX txs
- **Fix**: Before replay, batch-load all storage slots for known pool addresses via `eth_getProof` (one RPC call with multiple storage keys)

**5. Reduce allocations in hot path** (`core/src/run.rs:185-298`)
- Pre-allocate `all_opportunities` with capacity hint
- Cache `pool_to_dex` HashMap in `aggregate.rs` instead of rebuilding per run
- Pool Vec allocations across per-tx callbacks

**6. SQLite query optimization** (`core/src/cache.rs`)
- Cache frequently accessed data (pool info, token maps) in memory rather than re-querying
- `PRAGMA cache_size = -64000` for larger working set
- Batch INSERTs during fetch phase in transactions

**7. Pool init RPC batching** (`core/src/pool/state.rs:461-553`)
- N concurrent `eth_call`s for N pools. With 1000+ pools this is heavy
- **Fix**: Use `eth_call` with state override to batch multiple calls in one request
- **Fix**: For V2 pools, primary path should be `eth_getStorageAt` (cheaper) not fallback

### Low Impact

**8. JSON streaming** — Write results incrementally instead of building full Vec in memory

**9. Comfy-table laziness** — Only format results when terminal output is requested

**10. Parquet direct write** — Skip SQLite round-trip, write from in-memory buffers

---

## 3. Tips for Increasing Accuracy

### Critical (closest to real MEV bot)

**1. Replace simplified AMM math with real formulas**
- Curve uses StableSwap: `x*y*(x+y) + D^3 = D^3*A*(x+y)` — not constant-product
- Balancer uses weighted product: `product w_i^{p_i} = const` with per-pool weights
- Implement real formulas in `two_hop.rs` quote functions. Until this is done, all Curve/Balancer arb profits are materially wrong.

**2. Simulate bundle execution, not just per-tx detection**
- Real MEV: frontrun + swap + backrun as one atomic bundle. Pipeline detects patterns post-hoc.
- Add bundle constructor + EVM re-execution to verify exact profit and atomicity.

**3. Mempool-aware replay mode**
- The single biggest gap: you cannot see what *could have* executed, only what *was* executed.
- Short-term: Parse Flashbots MEV-Share / bloXroute data for failed searcher txs.
- Medium-term: Capture pending tx pool and replay bundles against historical fork state.

**4. Priority gas auction (PGA) modeling**
- Receipt gas price != what a winning bot pays. Add PGA simulator that estimates gas price needed to outbid competing txs in the same block for each opportunity.

### High-value

**5. Cross-pool sandwich detection**
- Real sandwiches use different pools (buy on V2, victim on V3, sell on V2). Current detector requires same-pool.
- Generalize: track all swaps by sender across all pools, detect triangular sandwich patterns.

**6. JIT fee accrual simulation**
- Track `feeGrowthGlobal0X128` / `feeGrowthGlobal1X128` per V3 pool. Compute exact accrued fees for JIT position across its lifetime in the block, not single-step approximation.

**7. Liquidation detection (Aave, Compound, Morpho)**
- Major MEV category absent from engine. Track oracle prices, health factors, liquidation thresholds.

**8. Slippage-adjusted profit estimates**
- Compute profit range for input amount +/- 1-2%. Ternary search gives a single optimal point; real execution has variance.

**9. Token decimal awareness**
- `fact_check.rs:136` hardcodes 18 decimals. Look up actual token decimals (from contract or hardcoded map). Affects display, not detection logic.

**10. Dynamic pool discovery during replay**
- Pools created during the backtest range are missed. Track V2/V3 factory events during replay to dynamically register new pools.

**11. Opportunity deduplication**
- Same base arbitrage can be detected across multiple tx indices. Standardized opportunity ID (normalized by net profit per capital unit) would help.

**12. Fact-check with state re-execution**
- Current fact-check is structural. Re-play detected opportunity against forked state to produce a real profit/loss number.
