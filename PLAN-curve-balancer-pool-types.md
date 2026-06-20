# Curve & Balancer Pool-Type Coverage Plan

**Current rating: 3/10 for Curve, 3/10 for Balancer** — The plumbing (discovery, event decode, state struct, quoting dispatch) exists for one variant of each DEX, but both variants are quoted with wrong parameters that are never fetched on-chain, and the second-most-common variant of each is quoted with the *wrong formula entirely*.

This plan covers: (a) what pool types are currently supported, (b) the accuracy gaps in already-tracked pools, (c) which new pool types to add and in what order, and (d) a concrete implementation roadmap.

---

## Table of Contents

1. [Current Support Matrix](#1-current-support-matrix)
2. [Accuracy Gaps in Already-Tracked Pools (Tier 1)](#2-accuracy-gaps-in-already-tracked-pools-tier-1)
3. [New Pool Types to Add (Tier 2 & 3)](#3-new-pool-types-to-add-tier-2--3)
4. [Recommendation Summary](#4-recommendation-summary)
5. [Implementation Roadmap](#5-implementation-roadmap)
6. [Out of Scope](#6-out-of-scope)
7. [Appendix — On-Chain Read Selectors](#7-appendix--on-chain-read-selectors)

---

## 1. Current Support Matrix

### Curve

| User-listed pool type | Discovery | State fetch | Quoting math | Status |
|-----------------------|:---------:|:-----------:|:------------:|:------:|
| Stableswap — Plain (3pool etc.)        | ✅ `PoolAdded` | ⚠️ coins+balances only | ⚠️ StableSwap solver, **A hardcoded=100, fee=0** | **Broken params** |
| Stableswap — Lending (aUSDC etc.)      | ✅ (same)     | ⚠️ | ⚠️ | No underlying/wrapped distinction |
| Stableswap — Metapool                  | ✅ (same)     | ⚠️ | ❌ No base-pool nesting | **Wrong** |
| Cryptoswap — Plain (CRV/ETH)           | ✅ (same)     | ⚠️ | ❌ Quoted as StableSwap | **Wrong formula** |
| Cryptoswap — Tricrypto (WBTC/WETH/USDT)| ✅ (same)     | ⚠️ | ❌ Quoted as StableSwap | **Wrong formula** |
| Cryptoswap — Two-Asset (LSD/Forex)     | ✅ (same)     | ⚠️ | ❌ Quoted as StableSwap | **Wrong formula** |

Findings from code:
- `curve_output_amount` (`core/src/mev/two_hop.rs:427`) implements a **correct** generalized StableSwap Newton solver — the accuracy plan's C1 ("uses x*y=k") was already fixed in the uncommitted working-tree changes.
- `CurvePoolState.a_coeff` (`core/src/pool/state.rs:130`) is `pub` but **never populated**; every site that constructs a `CurvePoolState` hardcodes `a_coeff: 100` (`state.rs:1429`, `run.rs:460`, `two_hop.rs:867`, `integration.rs:30`).
- `CurvePoolState.info.fee` defaults to 0 and `fetch_curve_state` (`state.rs:1463`) does **not** call `fee()` — every tracked Curve pool quotes with a 0% fee.
- `fetch_curve_state` calls `coins(int128)` / `coins(uint256)` and `balances(int128)` for up to 16 tokens. It does **not** read `A`, `gamma`, `price_scale`, `price_oracle`, or `fee`.
- Event decode handles both `TokenExchange(address,int128,uint256,int128,uint256)` and the V2 `TokenExchange(...,uint128)` variant identically (`decoders.rs:159`), reading only the first 128 bytes.

### Balancer

| User-listed pool type | Discovery | State fetch | Quoting math | Status |
|-----------------------|:---------:|:-----------:|:------------:|:------:|
| Weighted (2–8 tokens)              | ✅ type 0 | ⚠️ `getPoolTokens` only | ⚠️ weighted product, **weights empty→equal, fee=0** | **Broken params** |
| Stable                             | ✅ type 1 | ⚠️ | ❌ Quoted with weighted formula | **Wrong formula** |
| Composable Stable                  | ✅ type 1 | ⚠️ | ❌ No BPT/nesting | **Wrong** |
| Boosted (nested Linear)            | ✅ (as weighted/stable) | ⚠️ | ❌ No LinearPool nesting | **Wrong** |
| Gyroscope 2-CLP / E-CLP            | ❌ filtered out (type>1) | ❌ | ❌ | **Not supported** |
| AutoRange                          | ❌ | ❌ | ❌ | **Not supported** |
| LBP (time-varying weights)         | ✅ (as weighted) | ⚠️ | ❌ Static weights | **Wrong** |
| Managed                            | ❌ | ❌ | ❌ | **Not supported** |

Findings from code:
- `BalancerPoolState.weights` (`state.rs:142`) is `pub` but **never populated**; `balancer_weights()` (`two_hop.rs:623`) falls back to equal weights (1e18) whenever the vector is empty or mismatches `balances.len()` — i.e. always, in production.
- `BalancerPoolState.info.fee` defaults to 0 and `fetch_balancer_state` (`state.rs:1330`) does **not** call `getSwapFeePercentage()` — every tracked Balancer pool quotes with a 0% fee.
- `discover_balancer_pools` (`discovery.rs:188`) reads `specialization` from `topic[3]` but only uses `poolType` (weighted/stable) implicitly; the pool *variant* (Composable, Boosted, Gyro, LBP, Managed) is **not** determined — all type 0/1 pools land in the same `BalancerPoolState` and get the weighted-product formula.
- `balancer_output_amount` (`two_hop.rs:572`) is a correct weighted-product implementation (uses f64 exponentiation) but is applied to Stable pools too, which is mathematically wrong.

---

## 2. Accuracy Gaps in Already-Tracked Pools (Tier 1)

These do not add new pool types. They fix the numbers on pools the engine already tracks. **Highest ROI: small surface, large correctness gain.** They should land first.

### T1.1 — Fetch Curve `A` and `fee` at init

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/pool/state.rs` (`CurvePoolState`, `fetch_curve_state`), `core/src/run.rs:460` |
| **Problem** | `a_coeff` hardcoded 100; `fee` 0. StableSwap output is extremely sensitive to `A` (e.g. 3pool uses A=2000, not 100). With A wrong by 20×, output near-peg is ~ok but diverges sharply off-peg. With fee=0, profit is systematically overstated. |
| **Fix** | In `fetch_curve_state`, after reading coins/balances, call `A()` (selector from Appendix) and store into a new field; call `fee()` and store into `info.fee`. For crypto pools, the amplification lives in `get_A`/`gamma`/`price_scale` — gate this on the detected variant (see T2.1). |
| **State change** | `CurvePoolState { a_coeff: u128 }` → keep, but populate. No schema change needed. |
| **Tests** | Unit test: `fetch_curve_state` returns `a_coeff != 100` for the known 3pool address on a fixed block (use a recorded RPC response fixture, or skip via `#[ignore]` for live). Regression: a Curve↔V2 two-hop quote changes when A/fee are wired. |

### T1.2 — Fetch Balancer `getNormalizedWeights()` and `getSwapFeePercentage()`

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/pool/state.rs` (`BalancerPoolState`, `fetch_balancer_state`), `core/src/run.rs:470` |
| **Problem** | `weights` always empty → equal-weight assumption → wrong output for any pool that isn't 50/50 (e.g. 80/20 WBTC/WETH). `fee` 0 → profit overstated. |
| **Fix** | In `fetch_balancer_state`, after `getPoolTokens`, decode the pool ID's first byte to get the specialization/pool type, then: for weighted pools call `getNormalizedWeights()` and `getSwapFeePercentage()` on the pool (not the vault) and populate `weights` and `info.fee`. For stable pools, call only `getSwapFeePercentage()` and route to T2.2 math. |
| **State change** | No schema change; populate existing `weights: Vec<u128>` (denominated 1e18). |
| **Tests** | Fixture test that an 80/20 weighted pool returns weights `[8e17, 2e17]`. Quote regression test: `balancer_output_amount` output changes once weights are non-equal. |

### T1.3 — Route Balancer Stable pools to a Stable invariant

| Aspect | Detail |
|--------|--------|
| **Files** | `core/src/mev/two_hop.rs` (`quote_path` Balancer arms, `balancer_output_amount`), `core/src/pool/state.rs` (`BalancerPoolState`) |
| **Problem** | Pools discovered as Balancer `poolType=1` (stable) are quoted with the weighted-product formula. The stable invariant `A·Σx + D = A·D + D³/(Πx)` (for the 2-token case; generalized for N) is never used. |
| **Fix** | Add a `pool_variant` (enum) field to `BalancerPoolState` (or infer from `weights.is_empty()` + a new `amplification: Option<u128>`). Add `balancer_stable_output_amount()` mirroring `curve_output_amount`'s Newton solver (Balancer StableMath is a near-identical StableSwap). Dispatch in the Balancer arms of `quote_path`, `two_hop_profit_at`, `multi_hop::quote_single_pool`, `jit_arb::convert_to_shared_token`, `sandwich`, and `fact_check`. |
| **State change** | Add `amplification: Option<u128>` (or `pool_variant: BalancerPoolVariant`) to `BalancerPoolState`. Fetch `getAmplificationParameter()` for stable pools. |

### T1.4 — Curve fee is dynamic for CryptoSwap; handle per-swap

Curve V2 pools have a fee that scales with the price deviation from the price scale (`fee_gamma * dx * D` region). For Tier 1 we treat it as the static `fee()` value read at init; full dynamic fee is part of T2.1. Document this approximation in the math function's doc comment.

---

## 3. New Pool Types to Add (Tier 2 & 3)

### Tier 2 — Real volume, worth the math

#### T2.1 — Curve CryptoSwap (V2) math

| Aspect | Detail |
|--------|--------|
| **Pool types** | Cryptoswap Plain, Tricrypto (WBTC/WETH/USDT), Two-Asset (LSD, Forex) |
| **Why** | These are the highest-volume volatile Curve pools. Currently all quoted with the StableSwap formula → output error of 10–50%+ off-peg. |
| **Math** | CryptoSwap invariant `K₀ = Πxᵢ · N^N / D^N`, with `gamma` (price-invariant-convergence) and `price_scale`/`price_oracle` per non-first token. Newton iteration over `D`, then over `y` (output balance), then apply dynamic fee (`out_fee = fee + price_deviation_term`). This is substantially more complex than StableSwap; recommend porting from the official `curve-crypto-contract` Solidity `get_y` / `get_dx` (verified reference implementations exist). |
| **State needed** | Per pool: `A`, `gamma`, `price_scale` (Vec, len = N-1), `fee` (base), `D` (optional, recompute). |
| **Detection** | Heuristic in `fetch_curve_state`: if `price_scale()` exists (call succeeds) → CryptoSwap; if `price_oracle(uint256)` exists → CryptoSwap; else if `A()` exists → StableSwap. Cache the detected variant on the state. |
| **Files** | New `core/src/pool/curve_math.rs` (both stable + crypto solvers, extracted from `two_hop.rs`), `state.rs` (`CurvePoolState` gains `variant`, `gamma`, `price_scale`), `two_hop.rs` dispatch. |
| **Tests** | Reproduce 3–5 known on-chain swaps (tx hash → expected output) from mainnet Tricrypto-2/3 and a stable/metapool. |

#### T2.2 — Curve Metapool support

| Aspect | Detail |
|--------|--------|
| **Pool types** | Stableswap Metapools (e.g. `am3CRV` pairing a coin with the 3pool LP token) |
| **Why** | Common for long-tail stablecoin onboarding; currently quoted as a plain 2-token StableSwap pool against the **LP token** rather than routing through the base pool. |
| **Math** | No new formula — a metapool swap `coin → base_pool_LP` is one StableSwap step; `coin → underlying_base_coin` is a two-step quote (metapool step + base-pool step). |
| **State needed** | `base_pool: Option<Address>` link on `CurvePoolState`; resolve via registry `get_base_pool()` or `base_pool()` view. Base pool must already be tracked. |
| **Quoting** | When `token_out` is a base-pool underlying coin, chain: metapool quote (`token_in → LP`) then base-pool quote (`LP → token_out`). Update `arb_tokens` / multi-hop so the metapool is connected to all N base coins, not just the LP. |
| **Files** | `state.rs` (fetch + link), `two_hop.rs` / `multi_hop.rs` (chained quote). |

#### T2.3 — Balancer Composable Stable + Boosted (Linear) nesting

| Aspect | Detail |
|--------|--------|
| **Pool types** | Composable Stable Pools, Boosted Pools (which nest Linear Pools like Aave aTokens) |
| **Why** | These hold a large share of Balancer stablecoin TVL. The BPT (pool's own token) is one of the registered tokens; swaps through boosted pools route via Linear pools internally. |
| **Math** | No new invariant for composable stable (reuse T1.3 StableMath). Linear pools are trivial `rate·x` linear transforms; need per-token `rate` (from `getRate()` or `scalingFactors()`). |
| **State needed** | `scaling_factors: Vec<u128>` (or rates) on `BalancerPoolState`; the BPT token index (skip it in balance math like Curve's LP token). |
| **Files** | `state.rs` (`getScalingFactors` call), `two_hop.rs` (apply scaling before/after weighted/stable formula). |

### Tier 3 — Niche, defer until Tier 1 & 2 land

#### T3.1 — Gyroscope 2-CLP (Quadratic)

| Aspect | Detail |
|--------|--------|
| **Why** | Concentrated-liquidity AMM with a different invariant (`√price` bounded in a range). Lower TVL than the above but is a genuinely different shape and matters where it is the dominant pool for a pair. |
| **Math** | Port the Gyroscope 2-CLP `get_dy` (quadratic invariant with price bounds). E-CLP (elliptic) is significantly harder — defer. |
| **Detection** | Gyroscope pools are deployed behind the Balancer vault as a separate pool type; discovery needs the Gyroscope factory or a `GyroPoolType` selector. |

#### T3.2 — Balancer LBP (time-varying weights)

| Aspect | Detail |
|--------|--------|
| **Why** | Fair-launch token sales; weights move on a schedule. Low sustained arb volume. |
| **Math** | Reuse weighted formula but compute weights from `getGradualWeightUpdateParams()` schedule at the block timestamp. |
| **Verdict** | Low priority; only implement if a tracked chain has a live LBP that shows up in detection runs. |

---

## 4. Recommendation Summary

**Implement, in this order:**

1. **T1.1** Fetch Curve `A` + `fee` — fixes every already-tracked Curve pool. *~1 day.*
2. **T1.2** Fetch Balancer weights + `fee` — fixes every already-tracked Balancer weighted pool. *~1 day.*
3. **T1.3** Balancer Stable pool StableMath + variant dispatch — fixes already-discovered type-1 pools. *~2 days.*
4. **T2.1** Curve CryptoSwap math — unlocks Tricrypto and all volatile Curve pools (the biggest new pool class). *~4–5 days (math is non-trivial; budget for reference-port validation).*
5. **T2.3** Balancer Composable/Boosted scaling + Linear nesting — captures the bulk of Balancer stable TVL. *~2–3 days.*
6. **T2.2** Curve Metapool base-pool routing — long-tail stablecoin coverage. *~2 days.*

**Recommend NOT implementing (see §6):** Balancer Managed pools, AutoRange, Gyroscope E-CLP, Curve lending-pool underlying token handling beyond the approximation.

---

## 5. Implementation Roadmap

### Shared refactors (do once, before T2 work)

- **R1. Extract Curve math** into `core/src/pool/curve_math.rs` (`stableswap_output`, `cryptoswap_output`, invariant solvers). Move the existing Newton solvers out of `two_hop.rs`. `curve_output_amount` becomes a thin dispatcher on `CurvePoolState.variant`.
- **R2. Extract Balancer math** into `core/src/pool/balancer_math.rs` (`weighted_output`, `stable_output`). Add `BalancerPoolVariant` enum: `Weighted`, `Stable`, `ComposableStable`, `Gyroscope`, `Other`.
- **R3. Add `PoolVariant` to state structs** so downstream code dispatches on variant rather than guessing from `weights.is_empty()`.
- **R4. Generalize quoting dispatch.** Every site that matches on `PoolState::Curve` / `PoolState::Balancer` must call the new dispatcher. Audit: `two_hop.rs`, `multi_hop.rs`, `jit_arb.rs`, `sandwich.rs`, `fact_check.rs`, `state.rs` (`normalize_to_native`). Add a single `quote_exact_in(pool, token_in, token_out, amount_in) -> Option<u128>` free function in `pool/math.rs` and route everything through it, so new variants only need one implementation site.

### Phase A — Tier 1 (accuracy fixes)

1. T1.1: add `A()` + `fee()` calls in `fetch_curve_state`; thread into `a_coeff` and `info.fee`.
2. T1.2: decode pool-id specialization byte in `fetch_balancer_state`; add `getNormalizedWeights()` + `getSwapFeePercentage()` for weighted pools.
3. T1.3: add `BalancerPoolVariant`; implement `balancer_stable_output_amount`; fetch `getAmplificationParameter()` for stable pools; update dispatch.
4. Regression tests: run the backtest over a known block range on Polygon and diff the Curve/Balancer opportunity set before/after; expect profit magnitudes to shift (generally downward) and some borderline opportunities to flip.

### Phase B — Tier 2 (new coverage)

5. R1 + R2 + R3 + R4 shared refactors.
6. T2.1: CryptoSwap detection + math + state fields (`gamma`, `price_scale`, base `fee`). Validate against ≥3 recorded Tricrypto swaps.
7. T2.3: Balancer `getScalingFactors()` + BPT-index skip + composable stable nesting.
8. T2.2: Curve metapool `base_pool` link + chained quote + multi-hop token-graph expansion.

### Phase C — Tier 3 (only if driven by real data)

9. T3.1 Gyroscope 2-CLP (skip E-CLP).
10. T3.2 Balancer LBP time-varying weights (only if a live LBP appears in a target block range).

### Validation strategy (all phases)

- **Golden swaps**: record `(pool, block, token_in, token_out, amount_in, expected_out)` tuples from etherscan/curve UI for ≥2 pools per variant. Add as `#[ignore]` integration tests that hit a real RPC, plus offline fixture tests.
- **Fact-check parity**: the existing `refetch_pool_state` + EVM fact-check path (`fact_check.rs`) must agree with the new quoting within tolerance. If fact-check diverges, the math is wrong.
- **Profit sanity**: after each phase, confirm `expected_profit > 0` opportunities drop or stay flat (adding correct fees/params can only reduce nominal profit; a *rise* signals a bug).

---

## 6. Out of Scope (recommend NOT implementing)

- **Balancer Managed pools** — governed, rare, circuit breakers / dynamic fees make them un-attributable for backtesting. Filter them out in discovery (pool type > 1 already filtered; extend the filter once `BalancerPoolVariant` exists).
- **Balancer AutoRange** — very low deployment count, dynamic range recomputation is per-trade.
- **Gyroscope E-CLP (Elliptic)** — elliptic invariant is substantially harder than 2-CLP and very low TVL.
- **Curve Lending pools (crvUSD / lend)** — would require modeling the underlying vs wrapped token and interest accrual. For backtest profit estimation the current "quote against the wrapped aToken-like balance" is an acceptable approximation; note it in docs.
- **CryptoSwap dynamic fee exactness** — Tier 1 uses the static `fee()`; the full price-deviation-scaled fee is only added as part of T2.1's full port.

---

## 7. Appendix — On-Chain Read Selectors

Selectors are `keccak256(signature)[0..4]`. Verify each at implementation time (some pools use `uint256`-indexed variants on forks).

**Curve (StableSwap V1)**
- `A()` → amplification coefficient
- `fee()` → swap fee (parts per 10¹⁰)
- `balances(int128)` / `balances(uint256)` — already used
- `coins(int128)` / `coins(uint256)` — already used
- `base_pool()` / `get_base_pool()` (metapool, T2.2)

**Curve (CryptoSwap V2)**
- `get_A()` / `A()` → amplification
- `gamma()` → convergence parameter
- `price_scale()` / `price_scale(uint256)` → price scales
- `price_oracle(uint256)` → EMA price oracle
- `fee()` → base fee
- `get_dx(int128,int128,uint256)` / `get_dy(int128,int128,uint256)` → reference quote views (use as cross-check, not as the simulation primitive, since we need arbitrary-amount quotes during the optimizer)

**Balancer V2 (vault + pool)**
- Vault `getPoolTokens(bytes32)` — already used
- Vault `getPool(bytes32)` → (address pool, specialization)
- Pool `getPoolId()` → bytes32 (first byte = pool type tag for some factories)
- Pool `getNormalizedWeights()` → uint256[] (weighted only)
- Pool `getSwapFeePercentage()` → uint256
- Pool `getScalingFactors()` → uint256[] (composable / boosted)
- Pool `getAmplificationParameter()` → (uint256 value, bool isUpdating, uint256 precision) (stable)
- Pool `getGradualWeightUpdateParams()` → (startTime, endTime, endWeights) (LBP, T3.2)

**Gyroscope (T3.1)**
- Pool `GyroPoolType()` / `getDerivedPriceRange()` / `get_dy` — port from `gyroscope-contracts`.
