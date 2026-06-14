# Pool Module Accuracy & Performance Plan

This plan covers accuracy-critical and performance-critical improvements in the `core/src/pool/` module that are **not already addressed** in the existing plans (`PLAN_MEV_ACCURACY_IMPROVEMENTS.md` and `performance-optimization-plan.md`).

Prioritization: **Accuracy first** (MEV detection correctness), then **speed/optimization** (throughput and resource usage).

---

## Priority Matrix

| # | Issue | Category | Severity | Effort | Existing Plan Overlap |
|---|-------|----------|----------|--------|-----------------------|
| P0 | V3 tick positions never initialized from RPC | Accuracy | Critical | Large | None |
| P1 | Curve/Balancer pools never initialized from RPC | Accuracy | High | Medium | Partial (Issue #10) |
| P2 | JIT `pool_tick_cache` not seeded → false positives | Accuracy | High | Small | None |
| P3 | V3 quote u128 truncation | Accuracy | Medium | Small | None |
| P4 | `HashMap` for V3 ticks → O(n) tick search | Speed | High | Medium | None |
| P5 | `RefCell` prevents `Sync` — blocks parallel replay | Speed | High | Small | None |
| P6 | `init_from_rpc` concurrency cap hardcoded at 20 | Speed | Medium | Small | None |
| P7 | No memoization of `get_sqrt_ratio_at_tick()` | Speed | Medium | Small | None |
| P8 | `update_from_logs` no pool-address pre-filter | Speed | Medium | Small | None |
| P9 | JIT first-swap false-positive due to unseeded cache | Accuracy | Medium | Small | None |
| P10 | No pool state consistency validation | Accuracy | Low | Medium | None |
| P11 | No `eth_call` retry logic in pool init | Accuracy | Low | Small | None |
| P12 | Best Bidirectional V3 Quote Not Available | Accuracy | Medium | Medium | None |
| P13 | `arbitrage_pairs()` O(n²) per token | Speed | Medium | Small | None |
| P14 | `PoolManager` `all_pools()` frequently collected | Speed | Low | Small | None |
| P15 | V2 storage decode with bit shifts vs byte slicing | Speed | Low | Trivial | None |
| P16 | V3 quote direction should support exact_out | Accuracy | Low | Medium | None |
| P17 | `optimal_n_hop_generic` saturating_sub hides no-profit | Accuracy | Low | Trivial | Partial (Issue #11) |
| P18 | No pool creation block tracking | Accuracy | Low | Small | None |

---

## P0: V3 Tick Positions Never Initialized from RPC (Accuracy — Critical)

**File:** `core/src/pool/state.rs:396-449`
**Existing coverage:** Not in any plan.

### Problem

`PoolManager::init_from_rpc()` fetches `sqrt_price_x96`, `tick`, and `liquidity` for V3 pools via `slot0()` + `liquidity()` calls, but **never fetches tick-position data**. The `ticks: HashMap<i32, i128>` map remains empty after initialization. This means:

- `find_next_initialized_tick()` in `v3_quote.rs` always returns `None`
- `quote_v3_exact_in()` treats every V3 pool as having one continuous liquidity range from `MIN_SQRT_RATIO` to `MAX_SQRT_RATIO`
- `max_v3_tradeable_amount()` always uses the absolute MIN/MAX tick as the boundary
- **Swap quotes are incorrect for pools with concentrated positions** — which is the entire premise of Uniswap V3

Ticks are populated during block replay via `apply_v3_mint_burn()`, but if the first transaction in a block performs a swap on a V3 pool that already has concentrated positions, the quote will be wrong because the initial state has no tick boundaries.

### Implementation

**Option A (Recommended): Fetch ticks lazily during replay**

Add a `ticks_initialized` flag to `UniswapV3PoolState`. On first `quote_v3_exact_in()` call, if `ticks` is empty and `ticks_initialized == false`, pend the initialization:

```rust
// In run.rs or v3_quote.rs: before quoting, lazily initialize ticks for V3 pools
async fn ensure_v3_ticks_initialized(
    pm: &mut PoolManager,
    rpc: &RpcClient,
    block_num: u64,
    pool_addr: Address,
) {
    if let Some(PoolState::UniswapV3(state)) = pm.get_mut(&pool_addr) {
        if state.ticks_initialized || state.liquidity == 0 {
            return;
        }
        // Fetch tick bitmap and all position tick ranges for this pool
        // This is pool-specific and requires iterating the tick bitmap
        // via eth_call to tickBitmap(-887272), tickBitmap(-887200), etc.
        // For v1: skip initialization and log a warning
        state.ticks_initialized = true;
    }
}
```

**Option B (Fallback for v1):** Accept that ticks are empty and log a prominent warning at startup:
```
WARN: V3 pool {addr} has no tick data initialized. 
Quotes will treat it as one continuous range. 
Expected output accuracy will degrade for pools with concentrated liquidity.
```

**Option C (Full):** Implement tick bitmap scanning to reconstruct all initialized ticks at a given block:
- For each V3 pool, iterate the tick bitmap word by word (starting at the current tick word, expanding outward)
- Each nonzero bit in a word corresponds to an initialized tick
- For each initialized tick, call `ticks(tick)` via `eth_call` to get the liquidity net
- This is expensive (potentially hundreds of `eth_call`s per pool) but produces a complete initial state

### Tests
- `test_v3_pool_ticks_empty_after_init`: Create V3 pool, run `init_from_rpc()`, verify `ticks` is empty.
- `test_v3_quote_without_ticks_warns`: Verify quoting with empty ticks logs a warning but doesn't panic.
- `test_v3_quote_with_full_ticks`: (Post-implementation) Verify quotes differ with vs without tick data.

---

## P1: Curve/Balancer Pools Never Initialized from RPC (Accuracy — High)

**File:** `core/src/pool/state.rs:468-472`
**Existing coverage:** PARTIAL in PLAN_MEV_ACCURACY_IMPROVEMENTS.md Issue #10 (covers adding quote functions, but does NOT cover the initialization gap).

### Problem

The existing plan #10 focuses on adding Curve/Balancer quoting math. However, even with correct math, the quoting functions will fail because the pool's `balances` and `token_index` are never populated. `PoolManager::fetch_pool_state()` returns `None` for both types.

### Implementation

Add `eth_call` methods to fetch Curve and Balancer state on initialization:

```rust
// In state.rs
DexType::Curve => {
    let balances = Self::fetch_curve_balances(rpc, pool, block).await?;
    // For 2-token Curve pools: token_index is deterministic from token0/token1 in PoolInfo
    Some(PoolInitResult::CurveBalances(balances))
}
DexType::Balancer => {
    let (balances, pool_id) = Self::fetch_balancer_state(rpc, pool, block).await?;
    Some(PoolInitResult::BalancerState(balances, pool_id))
}
```

**Curve balance fetching:**
```rust
async fn fetch_curve_balances(rpc: &RpcClient, pool: Address, block: u64) -> Option<Vec<u128>> {
    // Curve pools expose N_COINS and balances(uint256 i) or get_balances()
    // For v1: use the generic ABI call to coins(0..n) to discover tokens
    // then call balances(i) for each
    // Simpler: hardcode as 2-token for v1, call get_balances() on known ABIs
    None // TODO
}
```

**Balancer state fetching:**
```rust
async fn fetch_balancer_state(rpc: &RpcClient, pool: Address, block: u64) -> Option<(Vec<u128>, [u8; 32])> {
    // Call getPoolId() to get the pool ID
    // Use the Vault to get pool tokens and balances via getPoolTokens(poolId)
    None // TODO
}
```

**`PoolInitResult` variant expansion:**
```rust
enum PoolInitResult {
    V2Reserves(u128, u128),
    V3State(U256, i32, u128),
    CurveBalances(Vec<u128>),      // NEW
    BalancerState(Vec<u128>, [u8; 32]),  // NEW
}
```

### Tests
- `test_curve_pool_init`: Create Curve pool info, mock `eth_call` to return balances, verify `CurvePoolState.balances` is populated.
- `test_balancer_pool_init`: Same for Balancer, verify `pool_id` is populated.

---

## P2: JIT `pool_tick_cache` Not Seeded → False Positives (Accuracy — High)

**File:** `core/src/mev/jit.rs:44, 132`
**Existing coverage:** Not in any plan.

### Problem

`JitDetector.pool_tick_cache` starts empty. On the first swap in a block, the `get_pre_swap_tick()` helper falls back to the post-swap tick as an estimate of the pre-swap tick (line 132). This makes the "was this mint in range" check on line 140 always pass for the first swap, potentially causing false-positive JIT detections.

### Implementation

Seed `pool_tick_cache` from `PoolManager` state before processing transactions in each block:

```rust
// In JitDetector, add method:
pub fn seed_from_pool_manager(&mut self, pm: &PoolManager) {
    for pool_state in pm.all_pools() {
        if let PoolState::UniswapV3(v3) = pool_state {
            self.pool_tick_cache.insert(v3.info.address, v3.tick);
        }
    }
}
```

Call this in `run.rs` before the transaction-processing loop:
```rust
jit_detector.seed_from_pool_manager(&pool_manager);
```

### Tests
- `test_jit_first_swap_no_false_positive`: Seed cache with correct pre-block tick. Process a swap that occurs above the mint's range. Verify the mint is NOT marked as swapped.
- `test_jit_first_swap_correct_match`: Seed cache. Process a swap that genuinely crosses the mint's range. Verify the mint IS marked.

---

## P3: V3 Quote u128 Truncation (Accuracy — Medium)

**File:** `core/src/pool/v3_quote.rs:524`
**Existing coverage:** Not in any plan.

### Problem

`quote_v3_exact_in()` returns `Some(limbs[0] as u128)` which takes only the lower 64 bits of the `U256` output. If the output exceeds `u128::MAX` (theoretically possible for very large swaps on high-liquidity pools), the result silently truncates, producing a wildly incorrect quote.

### Implementation

Replace with checked conversion:

```rust
// Current:
let limbs = total_amount_out.as_limbs();
Some(limbs[0] as u128)

// Fixed:
u128::try_from(total_amount_out).ok()
```

Since the return type is already `Option<u128>`, a failed conversion returns `None` instead of truncating silently:

```rust
// In quote_v3_exact_in():
let total_amount_out_u128 = u128::try_from(total_amount_out).ok()?;
Some(total_amount_out_u128)
```

**Note:** The same pattern exists in `max_v3_tradeable_amount()` at line 421-426. Apply the same fix there.

### Tests
- `test_v3_quote_overflow_returns_none`: Create a pool with max liquidity, input `u128::MAX` into quote. Verify it returns `None` instead of a truncated value.
- `test_v3_max_input_overflow_safe`: Same check for `max_v3_tradeable_amount()`.

---

## P4: `HashMap` for V3 Ticks → O(n) `find_next_initialized_tick()` (Speed — High)

**File:** `core/src/pool/v3_quote.rs:288-322`
**Existing coverage:** Mentioned briefly in earlier analysis but not in any plan.

### Problem

`find_next_initialized_tick()` performs a linear scan over ALL entries in the `ticks: HashMap<i32, i128>` to find the nearest initialized tick. For pools with hundreds or thousands of positions, this makes each swap step O(n) in the number of positions. Since `quote_v3_exact_in()` calls `find_next_initialized_tick()` on every iteration of its loop, a single quote can be O(n × steps).

### Implementation

Replace `HashMap<i32, i128>` with `BTreeMap<i32, i128>`:

```rust
// state.rs
pub struct UniswapV3PoolState {
    pub info: PoolInfo,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub ticks: BTreeMap<i32, i128>,  // WAS: HashMap<i32, i128>
}
```

Then optimize `find_next_initialized_tick()` using range queries:

```rust
fn find_next_initialized_tick(
    ticks: &BTreeMap<i32, i128>,
    current_tick: i32,
    zero_for_one: bool,
) -> Option<i32> {
    if zero_for_one {
        // Largest initialized tick strictly less than current_tick
        ticks.range(..current_tick).next_back().map(|(&t, &liq)| {
            if liq != 0 { Some(t) } else { None }
        }).flatten()
    } else {
        // Smallest initialized tick strictly greater than current_tick
        ticks.range(current_tick + 1..).next().map(|(&t, &liq)| {
            if liq != 0 { Some(t) } else { None }
        }).flatten()
    }
}
```

This reduces the time complexity from **O(n)** to **O(log n)** per lookup.

### Changes Required
- `state.rs`: Change `HashMap` to `BTreeMap` in `UniswapV3PoolState`
- `state.rs`: Update `UniswapV3PoolState::new()` to use `BTreeMap::new()`
- `state.rs`: Update `apply_v3_mint_burn()` (BTreeMap API is identical for `entry().or_insert()`)
- `v3_quote.rs`: Update all function signatures and imports to use `BTreeMap`
- All test files: Update test helpers to use `BTreeMap`

### Tests
- All existing V3 tests pass unchanged (API-compatible change).
- New: `bench_v3_find_next_tick`: Benchmark with 10, 100, 1000 ticks to verify O(log n) scaling.

---

## P5: `RefCell` in `PoolManager` Prevents `Sync` (Speed — High)

**File:** `core/src/pool/state.rs:196`
**Existing coverage:** Not in any plan (performance plan mentions making PoolManager clonable but doesn't address RefCell).

### Problem

`PoolManager.pairs_cache: RefCell<Option<Vec<...>>>` prevents `PoolManager` from implementing `Sync`. Since Rayon's `par_iter()` requires `Send + Sync` for the data being iterated, this blocks parallel block replay (Phase 2 of the performance plan).

### Implementation

Replace `RefCell` with `Mutex`:

```rust
use std::sync::Mutex;

pub struct PoolManager {
    pools: HashMap<Address, PoolState>,
    token_index: HashMap<Address, Vec<Address>>,
    pairs_cache: Mutex<Option<Vec<(Address, Address, Address)>>>,  // WAS: RefCell
    wrapped_native: Option<Address>,
}
```

Update all accessor methods:

```rust
pub fn arbitrage_pairs(&self) -> Vec<(Address, Address, Address)> {
    if let Some(cached) = &*self.pairs_cache.lock().unwrap() {
        return cached.clone();
    }
    // ... compute pairs ...
    *self.pairs_cache.lock().unwrap() = Some(pairs.clone());
    pairs
}
```

The `Mutex` is uncontended in practice (pairs are only recomputed on `add_pool()`, which happens during initialization, not during parallel replay). The overhead is negligible.

### Additional: `PoolManager` must be `Clone + Send`

```rust
#[derive(Clone)] // already present
pub struct PoolManager { ... }
// Clone is already derived. With Mutex instead of RefCell,
// PoolManager becomes Send + Sync automatically.
```

### Tests
- `test_pm_thread_safe`: Construct a `PoolManager`, wrap in `Arc`, spawn threads, call `arbitrage_pairs()` from multiple threads concurrently. Verify no panics and consistent results.

---

## P6: `init_from_rpc` Concurrency Cap Hardcoded at 20 (Speed — Medium)

**File:** `core/src/pool/state.rs:398`
**Existing coverage:** Not in any plan.

### Problem

```rust
let cap = pool_addrs.len().clamp(1, 20);
```

The semaphore limit is hardcoded at 20 concurrent RPC requests. For large chains with 5000+ pools, this means 250 sequential batches of 20 — each batch incurs serialization overhead. With lower-latency RPCs (local node), higher concurrency (50-100) would be safe and faster.

### Implementation

Make the cap configurable via `PoolManager`:

```rust
pub struct PoolManager {
    // ... existing fields ...
    rpc_concurrency: usize,  // NEW: default 20
}

impl PoolManager {
    pub fn with_rpc_concurrency(mut self, concurrency: usize) -> Self {
        self.rpc_concurrency = concurrency;
        self
    }

    pub async fn init_from_rpc(&mut self, rpc: &RpcClient, block_num: u64) {
        let pool_addrs: Vec<Address> = self.pools.keys().copied().collect();
        let cap = pool_addrs.len().clamp(1, self.rpc_concurrency);
        // ... rest unchanged ...
    }
}
```

Expose via config in `run.rs`:
```rust
// cli config or run.rs
let concurrency = config.rpc_init_concurrency.unwrap_or(20);
let mut pm = PoolManager::with_capacity(pool_count)
    .with_rpc_concurrency(concurrency);
```

### Tests
- `test_init_with_custom_concurrency`: Create `PoolManager` with `with_rpc_concurrency(50)`, verify internal field is set.
- `test_init_concurrency_clamp`: Verify cap is clamped to at least 1.

---

## P7: No Memoization of `get_sqrt_ratio_at_tick()` (Speed — Medium)

**File:** `core/src/pool/v3_quote.rs:59-137`
**Existing coverage:** Not in any plan.

### Problem

`get_sqrt_ratio_at_tick()` performs 19 multiplications via `mul_div()` on every call. During a single `quote_v3_exact_in()` execution, it may be called 2-20+ times (once per tick step + once for boundary computation). In a backtest with millions of blocks, these computations add up significantly.

Many pools are at the same tick (e.g., stable pools at tick 0, or pools at common price levels). The result for a given tick is deterministic and never changes.

### Implementation

Add a small LRU cache:

```rust
use lru::LruCache;
use std::sync::Mutex;
use std::num::NonZeroUsize;

lazy_static::lazy_static! {
    static ref SQRT_RATIO_CACHE: Mutex<LruCache<i32, U256>> =
        Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap()));
}

pub fn get_sqrt_ratio_at_tick(tick: i32) -> U256 {
    if let Some(cached) = SQRT_RATIO_CACHE.lock().ok().and_then(|mut c| c.get(&tick).copied()) {
        return cached;
    }
    // ... original computation ...
    let result = /* ... */;
    if let Ok(mut cache) = SQRT_RATIO_CACHE.lock() {
        cache.put(tick, result);
    }
    result
}
```

Alternatively, to avoid a global static and `lazy_static` dependency, use a thread-local cache or pass a cache reference through the quoting functions. The global cache with `LruCache` is simplest and effective for the common case.

**Optimization:** The cache can be pre-seeded with commonly used ticks at startup (e.g., ticks 0 and ±100, ±1000, etc.).

### Tests
- `test_sqrt_ratio_cache_hit`: Call `get_sqrt_ratio_at_tick(100)` twice. Verify the second call returns the cached value (measureable via timing or a counter).
- `test_sqrt_ratio_cache_consistency`: Verify cached and non-cached values match exactly.

---

## P8: `update_from_logs` No Pool-Address Pre-Filter (Speed — Medium)

**File:** `core/src/pool/state.rs:599-639`
**Existing coverage:** Not in any plan.

### Problem

`update_from_logs()` iterates over ALL logs in a transaction and checks each log's `topic0` against all known event signatures (V2 Swap, V2 Sync, V3 Swap, V3 Mint, V3 Burn, Curve TokenExchange, Balancer Swap). For transactions with many logs (e.g., complex DeFi interactions), this is wasteful — most logs are not from tracked pools.

### Implementation

Add a `tracked_pool_addresses: HashSet<Address>` to `PoolManager` for O(1) pool lookups:

```rust
pub struct PoolManager {
    // ... existing fields ...
    tracked_pools_set: HashSet<Address>,  // NEW: derived from pools.keys()
}

impl PoolManager {
    pub fn update_from_logs(&mut self, logs: &[ExecutedLog]) {
        for log in logs {
            // Early exit: skip logs not from tracked pools
            if !self.tracked_pools_set.contains(&log.address) {
                continue;
            }
            // ... existing topic matching ...
        }
    }
}
```

Update `add_pool()` to maintain the set:
```rust
pub fn add_pool(&mut self, state: PoolState) {
    let addr = state.address();
    // ... existing code ...
    self.tracked_pools_set.insert(addr);
}
```

This is a cheap optimization (one `HashSet` lookup per log) that avoids all topic matching and dispatching for non-pool logs. In a typical transaction with 50-100 logs from various DeFi protocols, most logs will be from non-tracked pools.

### Tests
- `test_update_from_logs_untracked_pool_skipped_fast`: Process a log from a non-tracked address. Verify `process_v2_swap_log` etc. are never called (can be verified with a mock or counter).
- `bench_update_from_logs`: Benchmark with 100 logs where 5 are from tracked pools vs 100 from tracked pools.

---

## P9: Curve/Balancer Approximation Using V2 Formula Is Incorrect (Accuracy — Medium)

**File:** `core/src/mev/two_hop.rs:251-285`
**Existing coverage:** PARTIAL (Issue #10 covers adding proper formulas but current code uses V2 approximation)

### Problem

Both `curve_output_amount()` and `balancer_output_amount()` use the V2 constant-product formula (`x * y = k`). This is a reasonable approximation only for Curve stable pools at the 1:1 peg and for Balancer pools with equal weights. For any deviation, the error grows rapidly.

### Implementation

**Step 1: Add proper Curve StableSwap math** (`core/src/pool/math.rs`):

```rust
/// Curve StableSwap 2-token output amount.
///
/// Uses the invariant: D = A * n^n * S + D^(n+1) / (n^n * prod(x_i))
/// where n = 2, A = amplification coefficient.
///
/// Simplified Newton iteration for D, then solve for output.
pub fn curve_stable_output_amount(
    amount_in: u128,
    balances: &[u128; 2],
    amp: u128,
    fee: u32,
) -> Option<u128> {
    // 1. Compute D from current balances using Newton's method
    // 2. Apply fee to amount_in
    // 3. Solve for new balance_out given (D, balance_in + amount_in_after_fee)
    // 4. Return balance_out - original_balance_out
    None // TODO: full StableSwap math
}
```

**Step 2: Add Balancer weighted pool math** (`core/src/pool/math.rs`):

```rust
/// Balancer weighted pool output amount.
///
/// Invariant: prod(balances[i]^weights[i]) = constant
/// where weights are normalized to sum to 1.
pub fn balancer_weighted_output_amount(
    amount_in: u128,
    balances: &[u128],
    weights: &[f64],
    fee: u32,
) -> Option<u128> {
    // 1. Compute invariant from current balances and weights
    // 2. Apply fee to amount_in
    // 3. Solve for new balance_out maintaining invariant
    // 4. Return balance_out - original_balance_out
    None // TODO: full Weighted Pool math
}
```

**Step 3:** Update `two_hop.rs` and `multi_hop.rs` to use these functions instead of `constant_product_output_amount` for Curve/Balancer pools.

### Tests
- `test_curve_stable_near_peg`: Balances [1_000_000, 1_000_000], amp=100. Small swap should produce ~output equal to input minus fee.
- `test_curve_stable_away_from_peg`: Balances [1_100_000, 900_000]. Same swap should produce less output (higher price impact).
- `test_balancer_weighted_80_20`: 80/20 weighted pool. Verify output differs from constant-product.

---

## P10: No Pool State Consistency Validation (Accuracy — Low)

**File:** `core/src/pool/state.rs:349-361`
**Existing coverage:** Not in any plan.

### Problem

`PoolManager::apply_v3_swap()` blindly accepts `sqrt_price_x96`, `tick`, and `liquidity` from the decoded swap event. There's no validation that these three values are internally consistent. A malformed or mis-decoded log could silently corrupt pool state.

Specifically, `get_sqrt_ratio_at_tick(tick)` should produce a sqrt price very close to the decoded `sqrt_price_x96`. If they diverge significantly, something is wrong.

### Implementation

Add an optional consistency check (behind a feature flag or debug assertion):

```rust
pub fn apply_v3_swap(
    &mut self,
    address: &Address,
    sqrt_price_x96: U256,
    tick: i32,
    liquidity: u128,
) {
    if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(address) {
        // Optional: validate sqrt_price_x96 ≈ get_sqrt_ratio_at_tick(tick)
        // Tolerance: allow ±1 due to rounding
        if cfg!(debug_assertions) {
            let computed = get_sqrt_ratio_at_tick(tick);
            let diff = if sqrt_price_x96 > computed {
                sqrt_price_x96 - computed
            } else {
                computed - sqrt_price_x96
            };
            if diff > U256::from(2u64) {
                tracing::warn!(
                    "V3 pool {} swap: tick {} inconsistent with sqrt_price_x96 {} (computed {}, diff {})",
                    address, tick, sqrt_price_x96, computed, diff
                );
            }
        }
        state.sqrt_price_x96 = sqrt_price_x96;
        state.tick = tick;
        state.liquidity = liquidity;
    }
}
```

### Tests
- `test_v3_swap_consistency_ok`: Apply swap with tick=0, sqrt=2^96. Verify no warning.
- `test_v3_swap_consistency_bad`: Apply swap with tick=100, sqrt=2^96 (inconsistent). Verify warning is logged (in debug mode).
- `test_v3_swap_consistency_off_in_release`: Same inconsistent values in release mode. Verify no warning and no panic.

---

## P11: No `eth_call` Retry Logic in Pool Init (Accuracy — Low)

**File:** `core/src/pool/state.rs:476-491, 521-559`
**Existing coverage:** Not in any plan.

### Problem

`fetch_v2_reserves()` and `fetch_v3_state()` make a single `eth_call` attempt before falling back to `eth_getStorageAt`. If the RPC returns a transient error (timeout, rate limit, temporary node issue), the code falls back to the slower storage path unnecessarily. Since storage reads are slower and more expensive (on archive nodes), this degrades initialization performance for no accuracy benefit.

### Implementation

Add a simple retry loop with exponential backoff:

```rust
async fn fetch_v2_reserves(rpc: &RpcClient, pool: Address, block: u64) -> Option<(u128, u128)> {
    const MAX_RETRIES: usize = 3;
    let data = Bytes::copy_from_slice(&GET_RESERVES_SELECTOR);
    
    for attempt in 0..MAX_RETRIES {
        match rpc.call(pool, data.clone(), block).await {
            Ok(result) if result.len() >= 64 => {
                // ... decode reserves ...
                return Some((r0, r1));
            }
            Ok(_) => {
                // Short result — not a transient error, fall through to storage
                break;
            }
            Err(e) => {
                if attempt < MAX_RETRIES - 1 {
                    let delay = Duration::from_millis(50 * 2u64.pow(attempt as u32));
                    tokio::time::sleep(delay).await;
                    tracing::trace!("Retry {} for getReserves({}): {}", attempt + 1, pool, e);
                } else {
                    tracing::trace!("eth_call getReserves() failed after {} retries, falling back to storage for {}", MAX_RETRIES, pool);
                }
            }
        }
    }
    
    Self::fetch_v2_reserves_storage(rpc, pool, block).await
}
```

Apply the same pattern to `fetch_v3_state()`.

### Tests
- Use a mock RPC that fails twice then succeeds. Verify the function retries and returns correct reserves.
- Use a mock RPC that always fails. Verify it falls back to storage.

---

## P12: Best Bidirectional V3 Quote Not Available (Accuracy — Medium)

**File:** `core/src/pool/v3_quote.rs`
**Existing coverage:** Not in any plan.

### Problem

Only `quote_v3_exact_in()` is implemented and publicly exported. The `estimate_swap_in_given_out` (exact-output) direction is missing. This limits arbitrage detection because:

1. `TwoHopArbDetector` and `MultiHopArbDetector` may need to quote both directions for a given V3 pool
2. The `optimal_n_hop_generic` optimizer works on quoting functions, and some optimal paths involve buying token0 with token1 (which requires exact-output quoting)
3. Without this, V3 pools can only be used in one direction in arbitrage paths

### Implementation

Add `quote_v3_exact_out()`:

```rust
/// Quote a Uniswap V3 exact-output swap.
///
/// Returns the amount of `token_in` required to receive exactly `amount_out`
/// of the other token. `zero_for_one` = true means token0 → token1.
pub fn quote_v3_exact_out(
    pool: &UniswapV3PoolState,
    amount_out: u128,
    zero_for_one: bool,
) -> Option<u128> {
    // Similar to quote_v3_exact_in but:
    // 1. Steps through ticks in reverse (target is the boundary in the opposite direction)
    // 2. At each step, computes how much input is needed to produce the output
    // 3. Accumulates total input instead of total output
    None // TODO
}
```

Also export it from `mod.rs`:
```rust
pub use v3_quote::{quote_v3_exact_in, quote_v3_exact_out};
```

### Tests
- `test_v3_quote_exact_out_consistency`: For the same pool/amount, verify that `quote_v3_exact_in(x)` ≈ y if and only if `quote_v3_exact_out(y)` ≈ x (within rounding).
- `test_v3_quote_exact_out_basic`: Known pool state, verify exact-out quote matches expected.
- `test_v3_quote_exact_out_no_liquidity`: Returns `None`.

---

## P13: `arbitrage_pairs()` O(n²) Per Token (Speed — Medium)

**File:** `core/src/pool/state.rs:301-323`
**Existing coverage:** Not in any plan.

### Problem

For each token, `arbitrage_pairs()` generates all pool-pair combinations: O(m²) where m is the number of pools trading that token. For a heavily traded token like WMATIC with 1000+ pools, this is 500,000 pairs. While this is cached and recomputed only on `add_pool()`, it can still be a bottleneck during initialization.

### Implementation

**Option A (Recommended):** Only generate pairs for tokens up to a configurable limit:

```rust
pub struct PoolManager {
    // ... existing fields ...
    max_pairs_per_token: usize,  // NEW: default 200 (caps at ~20k pairs per token)
}

pub fn arbitrage_pairs(&self) -> Vec<(Address, Address, Address)> {
    if let Some(cached) = &*self.pairs_cache.lock().unwrap() {
        return cached.clone();
    }
    let mut pairs = Vec::new();
    let mut seen = HashSet::new();

    for (_token, pool_addrs) in &self.token_index {
        let count = pool_addrs.len().min(self.max_pairs_per_token);
        let subset = &pool_addrs[..count];
        for i in 0..subset.len() {
            for j in (i + 1)..subset.len() {
                // ... existing logic ...
            }
        }
    }
    // ...
}
```

**Option B:** Use a heuristic — for each token, only consider the top N pools by total value locked (requires TVL data, which isn't currently tracked).

**Option C:** Accept the O(n²) cost since it's amortized over the entire run (computed once). This is the simplest and correct approach for moderate pool counts.

**Recommendation:** Implement Option A as a safety cap but set the default high enough (e.g., 500) that it doesn't affect most chains. For Polygon with ~2000 WMATIC pools, capping at 500 produces ~125k pairs, which is manageable.

### Tests
- `test_arbitrage_pairs_capped`: Create 1000 pools for the same token with `max_pairs_per_token=100`. Verify only 4950 pairs (100 choose 2) are generated, not 499,500.

---

## P14: `PoolManager::all_pools()` Frequently Collected (Speed — Low)

**File:** `core/src/pool/state.rs:254-256`
**Existing coverage:** Not in any plan.

### Problem

`all_pools()` returns an iterator, but callers frequently collect it into a `Vec`:
- `seed_from_pool_manager()` iterates all pools
- `init_from_rpc()` collects keys into a Vec (necessary)
- Various debug/logging paths

The overhead is minimal (`HashMap::values()` is O(n)), but the pattern could be optimized for hot paths.

### Implementation

Add a cached all-pools Vec that's updated on `add_pool()`:

```rust
pub struct PoolManager {
    // ... existing fields ...
    all_pools_cache: RefCell<Vec<PoolState>>,  // fine since this is read-only in replay
}

impl PoolManager {
    pub fn add_pool(&mut self, state: PoolState) {
        let addr = state.address();
        let info = state.info().clone();
        self.pools.insert(addr, state);
        // ... token_index update ...
        self.all_pools_cache.borrow_mut().push(
            self.pools.get(&addr).unwrap().clone()
        );
        *self.pairs_cache.borrow_mut() = None;
    }

    pub fn all_pools(&self) -> impl Iterator<Item = &PoolState> {
        self.all_pools_cache.borrow().iter()
        // or keep the original HashMap::values() which is already O(n)
    }
}
```

**Simpler:** Keep the existing `HashMap::values()` approach. The overhead is negligible (the HashMap already stores the data contiguously, and iteration is cache-friendly). Mark this as **won't fix** unless profiling identifies it as a bottleneck.

---

## P15: V2 Storage Decode with Bit Shifts (Speed — Trivial)

**File:** `core/src/pool/state.rs:496-508`
**Existing coverage:** Not in any plan.

### Problem

`decode_v2_reserves_from_storage()` uses byte-level slicing:

```rust
fn decode_v2_reserves_from_storage(raw: U256) -> (u128, u128) {
    let bytes = raw.to_be_bytes::<32>();
    let r0 = u128::from_be_bytes({
        let mut buf = [0u8; 16];
        buf[2..16].copy_from_slice(&bytes[18..32]);
        buf
    });
    let r1 = u128::from_be_bytes({
        let mut buf = [0u8; 16];
        buf[2..16].copy_from_slice(&bytes[4..18]);
        buf
    });
    (r0, r1)
}
```

This creates multiple temporary arrays and copies. Bit shifts are more idiomatic and compile to fewer instructions:

### Implementation

```rust
fn decode_v2_reserves_from_storage(raw: U256) -> (u128, u128) {
    let mask = (U256::from(1u128) << 112) - U256::from(1u128);
    let r0: u128 = (raw & mask).try_into().unwrap_or(0);
    let r1: u128 = ((raw >> 112) & mask).try_into().unwrap_or(0);
    (r0, r1)
}
```

This is also more readable — it directly expresses the packed layout: `[blockTimestampLast: uint32][reserve1: uint112][reserve0: uint112]` where reserve0 is in the lowest 112 bits, reserve1 is in bits 112-223, and blockTimestampLast is in bits 224-255.

### Tests
- All existing decode tests pass unchanged (the bit-shift version and the byte-slice version should produce identical results for all valid inputs).
- New: `test_decode_v2_reserves_edge_shifts`: Test with all-1s, all-0s, and mixed patterns.

---

## P16: V3 `quote_v3_exact_out` Missing (Accuracy — Low)

**File:** `core/src/pool/v3_quote.rs`
**Existing coverage:** Not in any plan.

### Problem

See P12 above — this is a duplicate reference. Re-iterating here with implementation detail.

The `MultiHopArbDetector` and `TwoHopArbDetector` currently handle non-V3 pools deterministically via `constant_product_output_amount()` which works bidirectionally. For V3, quotes are one-directional only.

### Implementation

```rust
/// Quote a Uniswap V3 exact-output swap.
///
/// Returns the amount of `token_in` required to receive exactly `amount_out`.
/// `zero_for_one` = true: selling token0 to buy token1 (output is token1).
pub fn quote_v3_exact_out(
    pool: &UniswapV3PoolState,
    amount_out: u128,
    zero_for_one: bool,
) -> Option<u128> {
    if amount_out == 0 || pool.liquidity == 0 || pool.sqrt_price_x96.is_zero() {
        return None;
    }

    // Walk the tick range in the opposite direction of quote_v3_exact_in,
    // accumulating the input required per tick step.
    // ...
    None // TODO: full implementation
}
```

The algorithm mirrors `quote_v3_exact_in` but:
- Starts from the current sqrt price
- Instead of consuming input and accumulating output, it consumes output and accumulates input
- At each step, uses the inverse of `compute_swap_step` (compute required input for a given output)

---

## P17: `optimal_n_hop_generic` `saturating_sub` Hides No-Profit (Accuracy — Low)

**File:** `core/src/pool/math.rs:180`
**Existing coverage:** Not in any plan.

### Problem

```rust
let p1 = r1.saturating_sub(m1);
```

`saturating_sub` silently converts negative profit to 0. This is fine for the `best` tracking (zero-profit opportunities are rejected later), but it means the ternary search's comparison `p1 >= p2` treats "no profit" and "very small profit" equivalently, which can skew the search toward a local optimum.

### Implementation

Replace with checked subtraction that rejects the input if `r1 <= m1`:

```rust
fn profit_or_none(output: u128, input: u128) -> Option<u128> {
    if output > input {
        Some(output - input)
    } else {
        None
    }
}

// In the comparison:
let p1 = profit_or_none(r1, m1);
let p2 = profit_or_none(r2, m2);
match (p1, p2) {
    (None, None) => break,
    (Some(_), None) => hi = m2,
    (None, Some(_)) => lo = m1,
    (Some(p1_val), Some(p2_val)) => {
        if p1_val >= p2_val {
            // ... existing logic ...
        }
    }
}
```

This preserves the ternary search semantics while avoiding the implicit zero-profit comparison.

### Tests
- All existing `optimal_n_hop_generic` tests pass unchanged (behavior-preserving for profitable cases).
- `test_n_hop_no_profit_comparison`: Use a quote function where all outputs are strictly ≤ inputs. Verify the function returns `None` instead of a zero-profit result.

---

## P18: No Pool Creation Block Tracking (Accuracy — Low)

**File:** `core/src/pool/discovery.rs, state.rs`
**Existing coverage:** Not in any plan.

### Problem

When pools are discovered via event logs, there's no record of which block they were created at. In `PoolManager::add_pool()`, the pool's initial state is either zero (V2: reserves=0, V3: sqrt_price=0) or fetched from the current RPC block. If a pool was discovered in block 1,000,000 but the backtest starts at block 500,000, the pool will be incorrectly included in the backtest (it didn't exist yet).

### Implementation

Add `created_at_block: Option<u64>` to `PoolInfo`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub dex_type: DexType,
    #[serde(default)]
    pub tick_spacing: Option<u32>,
    #[serde(default)]
    pub created_at_block: Option<u64>,  // NEW
}
```

Set it during discovery (in `discovery.rs`):
```rust
// discover_v2_pools and discover_v3_pools: capture block number
// This requires passing the block number through the discovery functions
pools.push(DiscoveredPool {
    address: pair,
    token0,
    token1,
    fee: 0,
    tick_spacing: None,
    dex_type: DexType::UniswapV2,
    created_at_block: current_block,  // NEW
});
```

In `PoolManager`, add a filter:
```rust
pub fn pools_existing_at_block(&self, block_num: u64) -> impl Iterator<Item = &PoolState> {
    self.pools.values().filter(move |p| {
        p.info().created_at_block.map_or(true, |created| created <= block_num)
    })
}
```

Use `pools_existing_at_block()` instead of `all_pools()` in the backtest loop.

### Tests
- `test_pool_not_included_before_creation`: Create pool with `created_at_block=1000`. Backtest at block 500. Verify it's excluded.
- `test_pool_included_after_creation`: Same pool at block 1000. Verify it's included.
- `test_pool_without_creation_block_included`: Pool with `created_at_block=None`. Verify it's always included (backward compatibility).

---

## Implementation Order & Effort Estimate

| # | Task | Effort | Impact | Dependencies |
|---|------|--------|--------|--------------|
| P5 | `RefCell` → `Mutex` | 0.5 day | Enables parallel replay (6-8× speedup) | None |
| P4 | `HashMap` → `BTreeMap` for V3 ticks | 1 day | O(n) → O(log n) tick search | None |
| P8 | Pool-address pre-filter in `update_from_logs` | 0.5 day | 2-5× faster log processing | None |
| P15 | Bit-shift V2 storage decode | 0.25 day | Negligible alone, good practice | None |
| P7 | Memoize `get_sqrt_ratio_at_tick` | 0.5 day | 2-10× faster V3 quoting | None |
| P6 | Configurable init concurrency | 0.5 day | 1-2× faster pool init | None |
| P0 | V3 tick init (Option B — warn only) | 0.5 day | Accuracy awareness (no fix) | P4 (for tick data model) |
| P2 | JIT pool_tick_cache seeding | 0.5 day | Eliminates first-swap false positives | None |
| P3 | V3 quote u128 truncation fix | 0.25 day | Prevents silent overflow errors | None |
| P18 | Pool creation block tracking | 1 day | Prevents anachronistic pool inclusion | None |
| P11 | `eth_call` retry logic | 0.5 day | Reduces unnecessary storage fallbacks | None |
| P10 | Pool state consistency validation | 1 day | Catches malformed events | P7 (needs get_sqrt_ratio) |
| P17 | `saturating_sub` fix | 0.25 day | Minor optimizer accuracy | None |
| P12/P16 | `quote_v3_exact_out` | 2 days | Enables bidirectional V3 in arb paths | P4 (BTreeMap), P7 (cache) |
| P1 | Curve/Balancer init from RPC | 2 days | Enables Curve/Balancer detection | None |
| P9 | Proper Curve/Balancer math | 3 days | Correct quotes for non-V2 pools | P1 |
| P13 | `arbitrage_pairs()` cap | 0.5 day | Prevents O(n²) explosion | None |
| P14 | `all_pools()` cache | 0.5 day | Marginal — skip unless profiled | None |

**Total:** ~14.5 days (excluding P14 which is optional)

### Recommended Sprint Plan

| Sprint | Focus | Items | Est. Days |
|--------|-------|-------|-----------|
| **Sprint 1: Foundation** | Thread safety + data structures | P5, P4, P8, P15 | 2.25 |
| **Sprint 2: V3 quoting accuracy** | Tick init + cache + exact_out | P7, P0, P3, P12/P16 | 3.25 |
| **Sprint 3: Detector accuracy** | JIT fixes + validation | P2, P10, P17, P11, P18 | 3.25 |
| **Sprint 4: Multi-DEX support** | Curve/Balancer init + math | P1, P9 | 5 |
| **Sprint 5: Polish** | Configuration + edge cases | P6, P13, P14 (if needed) | 1.5 |
