# Phase 4 Completion Plan — Remaining Items

**Goal:** Bring all incomplete Phase 4 items to completion, prioritizing quick wins first.

**Current status:** 8 items remain + L1 added — 6 not started, 3 partial.

---

## Remaining Work Inventory

| # | Item | Status | Est. Effort | Key Files |
|---|------|--------|-------------|-----------|
| M6 | JitArb profit model | Partial — arb profit + fee revenue are separate; proximity_window default is 1 | 1 day | `jit_arb.rs:55,188-190,348-416` |
| L4 | Token decimals | Not started — hardcoded address list of 7 tokens; falls back to 18 | 0.5 day | `fact_check.rs:105-125` |
| L6 | V2 slot 6 for forks | Not started — storage fallback at `state.rs:793` always uses slot 6 | 1 day | `state.rs:787-795`, `types.rs` |
| L3 | Single USD price | Not started — `aggregate.rs:84` takes one `f64`, applies uniformly | 2 days | `aggregate.rs:84,232,290` |
| L5 | CoinGecko midpoint | Not started — no on-chain oracle fallback | 2-3 days | `coingecko.rs`, `cli.rs`, `config.rs` |
| L1 | Aave liquidation detection | Partial — reactive event capture only; no pre-tx health factor prediction | 1-2 weeks | `liquidation.rs`, `run.rs:308-315` |
| M3 | State re-execution | DONE — added in previous implementation | 0 | `fact_check.rs` |
| L8 | Dynamic pool discovery | Not started — pools discovered upfront only; none mid-range | 3-5 days | `discovery.rs`, `run.rs:85-106` |
| H8 | MEV-Share / mempool | Not started — no Flashbots, bloXroute, pending txs | 3-4 weeks | New modules |
| L2 | Cross-block MEV | Not started — no reorg/time-bandit/multi-block arb | 2-3 weeks | `block_builder.rs`, `run.rs` |

---

## Phase 4a — Quick Wins (estimated: 2-3 days)

### M6 — Merge JitArb fee revenue + arb profit

**Problem:** `estimate_arb_profit` and `estimate_jit_fee_revenue` are called separately then added at line 190. The PLAN asks for a single economic calculation. `proximity_window` defaults to 1 (arbitrary).

**Implementation:**
1. Refactor `detect()` to call a single `compute_jit_arb_profit()` function
2. Combined function: compute arb profit from the two opposing swaps, then add JIT fee revenue from the position's share of swap fee growth
3. Change `proximity_window` default from 1 to 3 (more realistic window for detecting related swaps) and add `--proximity-window` CLI flag
4. The model remains a best-effort estimate — exact simulation would require revm re-execution (M3 pattern)

**Files:** `core/src/mev/jit_arb.rs`, `core/src/config.rs`, `core/src/cli.rs`, `cli/src/main.rs`

---

### L4 — On-chain token decimals lookup

**Problem:** `guess_token_decimals()` uses a hardcoded list of 7 known addresses (USDC/USDT/DAI/WBTC on Polygon+Ethereum) and falls back to 18 for everything else. Display formatting is wrong for any token outside this list.

**Implementation:**
1. Add `fetch_token_decimals(rpc, token, block)` that calls `decimals()` (selector: 0x313ce567) via `eth_call`
2. Use a `HashMap<Address, u8>` cache so each token is queried at most once per run
3. In `guess_token_decimals`, keep the hardcoded fast path; fall through to RPC call for unknown tokens
4. If RPC fails, default to 18 (unchanged behavior)

The function only affects display formatting in fact-check output, so the RPC calls are acceptable.

**Files:** `core/src/fact_check.rs`

---

### L6 — Add fork-specific V2 storage slots

**Problem:** `fetch_v2_reserves_storage()` at `state.rs:793` hardcodes slot 6. Camelot and Velodrome use different storage layouts. When `eth_call getReserves()` fails, the storage fallback returns garbage.

**Implementation:**
1. Infer pool type from the factory address (mappings already exist in `config.rs` and `types.rs`)
2. Define a mapping: factory address -> storage slot (or slot list to try)
3. Default (Uniswap V2, PancakeSwap, QuickSwap): slot 6
4. Camelot: slot 8
5. Velodrome: try slots [6, 12] sequentially
6. Pass the detected slot(s) to `fetch_v2_reserves_storage`
7. Primary `eth_call getReserves()` path stays unchanged (already works for all forks)

**Files:** `core/src/pool/state.rs`, `core/src/types.rs`

---

## Phase 4b — Pricing & Aggregation (estimated: 3-5 days)

### L3 — Per-token USD prices in aggregate

**Problem:** `aggregate()` takes a single `usd_price: f64` and applies it uniformly. JIT fees accrue in token0/token1; different strategies produce profit in different tokens. A Chainlink ETH/USD price applied to USDC profit gives wrong USD values.

**Implementation:**
1. Change `aggregate()` signature to accept `HashMap<Address, f64>` (token -> USD price)
2. For each opportunity, use the `token_out` address to look up the per-token price
3. If the token is not in the map, call `pool_manager.normalize_to_native(profit)` then multiply by the native token USD price
4. Backward compatibility: keep an overload that accepts a single `f64` (applies as native token price)
5. Add `PriceSource` enum: `CoinGecko` | `FromCoinGecko(HashMap)` | `FromCli(HashMap)`
6. Wire through CLI: `--token-price USDC=0.999,USDT=1.001,WETH=1800`

**Files:** `core/src/aggregate.rs`, `core/src/types.rs`, `core/src/cli.rs`, `cli/src/main.rs`

---

### L5 — On-chain price reference (CoinGecko validation)

**Problem:** `coingecko.rs` returns midpoint CEX prices that may diverge from on-chain execution prices. No verification exists.

**Implementation:**
1. Add `onchain_native_price(pool_manager)` that finds the highest-TVL pool for wrapped_native/USDC or wrapped_native/DAI and computes price = reserve_stable / reserve_native
2. Add `PriceOracleMode` enum: `CoinGeckoOnly | OnChain | Hybrid`
3. In `Hybrid` mode: fetch both CoinGecko and on-chain prices, log a warning if they diverge >5%
4. Add `--price-oracle` CLI flag (default: `CoinGeckoOnly` for backward compat)
5. Cache on-chain prices with block number invalidation

**Files:** `core/src/coingecko.rs`, `core/src/config.rs`, `core/src/cli.rs`, `core/src/pool/state.rs`

---

## Phase 4c — Advanced Features (estimated: 5-9 weeks)

### L8 — Dynamic pool discovery during replay

**Problem:** Discovery is an upfront `mev-scout discover` step. Pools created mid-range (e.g., a new Uniswap V3 pool halfway through a 100k-block backtest) are invisible.

**Implementation (two options):**

**Option A (simpler, recommended): Pre-chunk discovery**
1. Before replay, split the block range into chunks (e.g., 10k blocks)
2. For each chunk, scan factory events for `PairCreated`/`PoolCreated` in that range
3. Add any newly discovered pools to `PoolManager` before processing that chunk
4. This reuses existing `discovery.rs` logic without modifying the hot path

**Option B (real-time, higher overhead):**
1. In `run_block()`, after `update_from_logs`, check logs for pool creation events
2. For each detected new pool, call `init_from_rpc` for that single pool
3. Add to `PoolManager` immediately
4. Requires `--live-discover` flag (default: off)

**Recommendation:** Option A (pre-chunk). Simpler, no performance impact on hot path.

**Files:** `core/src/run.rs`, `core/src/pool/discovery.rs`

---

### H8 — MEV-Share / mempool integration (phased)

**Problem:** Only on-chain settled txs are analyzed. Failed bundles, PGA bids, private order flow are invisible.

**Implementation (3 phases):**

**Phase 1 — Pending tx capture (3-5 days)**
1. Add `eth_getBlockByNumber("pending")` call to fetch the pending block
2. Extract pending transactions and merge with settled txs
3. Store in a new `pending_tx` SQLite table (non-critical, can be empty)
4. Add `--capture-pending` flag (default: off)
5. Display pending tx count alongside settled in per-block summaries

**Phase 2 — MEV-Share API (2-3 weeks)**
1. Integrate Flashbots MEV-Share SSE feed: https://docs.flashbots.net/flashbots-mev-share/introduction
2. Fetch failed searcher bundles (the most informative signal for what *could* happen)
3. Parse bundle data into the internal `TxData` format
4. Store in `failed_bundle` table

**Phase 3 — Mempool-aware detection (1-2 weeks)**
1. Run detection against merged pending + failed + settled tx set
2. Emit "mempool-only" opportunities (trades that existed in mempool but never landed)
3. Compare: what arbitrages were available vs what was actually captured

**Files:** New `core/src/mev/mempool.rs`, `core/src/mev/mev_share.rs`; modify `core/src/run.rs`, `core/src/config.rs`, `core/src/cache.rs`

---

### L2 — Cross-block MEV (time-bandit attacks, reorgs)

**Problem:** No modeling of attacks that span multiple blocks — front-running the sequencer, time-bandit attacks, reorg-based arb.

**Implementation:**
1. Extend `BlockBuilder` to accept a sliding window of consecutive blocks (default: 3)
2. Add `detect_cross_block_opportunities(blocks: &[BlockState])`: look for same-pool price gaps that persist or widen across blocks
3. Add `detect_time_bandit()`: flag opportunities where block N's state was more profitable than block N-1's state for the same pool pair, suggesting the sequencer could have captured more
4. Label all cross-block opportunities with a confidence score (0.0-1.0) since detection is inherently speculative

**Files:** `core/src/mev/block_builder.rs`, `core/src/run.rs`

---

## Phase 4f — Liquidation Detection (estimated: 1-2 weeks)

### L1 — Proactive Aave V3 liquidation detection

**Current state:** Reactive event capture exists — `LiquidationDetector` catches on-chain `LiquidationCall` events, decodes collateral/debt/user/amounts, and estimates profit as `collateral - debt`. This is backward-looking only.

**Goal:** Add proactive detection — identify *potential* liquidations before they happen by tracking Aave reserve state and computing health factors.

**Implementation:**
1. Add `AaveReserveData` struct to track: `availableLiquidity`, `totalDebt`, `liquidationThreshold`, `liquidationBonus`, `currentLiquidityRate`, `currentVariableBorrowRate`, `priceInUsd`
2. Add `fetch_aave_reserve_data(rpc, pool, token, block)` — calls `getReserveData()` on the Aave V3 Pool contract via `eth_call`
3. Add `fetch_user_account_data(rpc, pool, user, block)` — calls `getUserAccountData()` for positions that had debt before liquidation
4. Add `compute_health_factor(total_collateral_usd, total_debt_usd, average_liquidation_threshold)` — standard Aave health factor formula: `(totalCollateral * avgLiquidationThreshold) / totalDebt`
5. Pre-tx detection in `run.rs`: before processing each block, iterate known Aave reserves → fetch user positions → compute health factors → flag positions with HF < 1.1 (approaching liquidation)
6. Emit `MeVOpportunity` with `strategy: Strategy::Liquidation` for pre-liquidation predictions, labeled with `raw_profit = Some(liquidation_bonus_estimate)`
7. Keep existing post-hoc `LiquidationCall` event capture as the "executed" ground truth

**Files:** `core/src/mev/liquidation.rs`, `core/src/run.rs`, `core/src/config.rs`, core/src/types.rs

---

## Implementation Roadmap

| Sprint | Items | Effort | Impact |
|--------|-------|--------|--------|
| **4a Quick Wins** | M6, L4, L6 | 2-3 days | Fixes 3 remaining modeling gaps, fork compatibility, correct token display |
| **4b Pricing** | L3, L5 | 3-5 days | USD becomes token-aware, on-chain prices validate CoinGecko |
| **4c Discovery** | L8 | 3-5 days | Catches pools created mid-range for long backtests |
| **4d Mempool (P1)** | H8 Phase 1 | 3-5 days | Pending tx capture — first step toward mempool analysis |
| **4e Advanced** | H8 Phases 2-3, L2 | 5-8 weeks | Full MEV-Share, cross-block MEV |
| **4f Liquidation** | L1 | 1-2 weeks | Proactive Aave liquidation detection — new strategy class |

---

## Key Design Decisions

1. **M6**: Don't attempt full revm re-execution for JitArb. Combined formula with empirical tuning is sufficient.

2. **L6**: The `eth_call getReserves()` path is the primary path and works for all forks. The storage fallback only triggers for non-archive nodes. Fork-aware slots cover the remaining gap.

3. **L4**: RPC calls for unknown token decimals are cached per-address, so each unique token is queried at most once per fact-check run.

4. **L3**: Backward-compatible overload keeps existing callers working. Per-token prices are optional.

5. **L5**: On-chain pricing is less precise than CoinGecko (slippage, liquidity depth). Use as validation, not replacement. Flag >5% divergences.

6. **L8**: Pre-chunk discovery is recommended over real-time. Reuses existing code, no hot-path overhead.

7. **H8**: Phase 1 (pending tx) has no external dependencies and provides immediate value. Leave full MEV-Share for last due to API dependency.

8. **L2**: Cross-block MEV detection is inherently unreliable. Label all outputs with confidence scores.
9. **L1**: Keep reactive `LiquidationCall` event capture as ground truth. Proactive detection is additional (pre-tx predictions). Aave only — no Compound/Morpho.

---

## Risk Assessment

| Item | Risk | Mitigation |
|------|------|------------|
| H8 (MEV-Share) | High — external API, may need Flashbots partnership | Phase 1 has zero external deps |
| L2 (cross-block) | Medium — speculative detection | Confidence score labels |
| L8 (discovery) | Medium — RPC rate limits | Pre-chunk approach avoids hot-path calls |
| L3 (per-token USD) | Low — display only | Backward-compatible default |
| M6, L6, L4, L5 | Low — isolated changes | Existing test coverage |
| L1 (liquidation) | Medium — Aave RPC state queries, new reserve data types | Pre-tx prediction is additive; reactive capture is the fallback |
