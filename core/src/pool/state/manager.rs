use std::cmp;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use alloy::primitives::{Address, U256};
use crate::pool::state::pool_types::{PoolState, UniswapV2PoolState, UniswapV3PoolState};

/// Manages runtime pool state for all tracked pools during block replay.
///
/// Responsibilities:
/// - Stores and updates `PoolState` for every pool in the registry
/// - Maintains a `token_index` for fast token→pool lookups (used by arb detectors)
/// - Caches computed arbitrage pairs (invalidated on `add_pool`)
/// - Dispatches on-chain event logs to the appropriate state update method
///
/// `PoolManager` is the single source of truth for pool state during a run.
#[derive(Debug)]
pub struct PoolManager {
    pub(crate) pools: HashMap<Address, PoolState>,
    /// token address -> list of pool addresses that trade this token
    pub(crate) token_index: HashMap<Address, Vec<Address>>,
    /// Cached arbitrage pairs (invalidated on add_pool)
    pub(crate) pairs_cache: Mutex<Option<Vec<(Address, Address, Address)>>>,
    /// Address of the wrapped native token (WMATIC/WETH/WBNB) per chain.
    pub(crate) wrapped_native: Option<Address>,
    /// Address of the Balancer V2 vault for flash loans and pool state queries.
    pub(crate) balancer_vault: Option<Address>,
    /// Pre-filter set of known pool addresses for fast log filtering.
    pub(crate) known_set: HashSet<Address>,
    /// Maximum number of pools per token when computing arbitrage pairs.
    pub(crate) max_pairs_per_token: usize,
    /// Per-token overrides for max_pairs_per_token (H3).
    /// Allows configuring different caps for high/medium/low-connectivity tokens.
    /// Key = token address, value = per-token max pairs limit.
    pub(crate) token_max_pairs: HashMap<Address, usize>,
    /// Maximum number of concurrent RPC calls during pool initialization.
    pub(crate) concurrency_limit: u32,
}

impl PoolManager {
    /// Create an empty pool manager with no pools loaded.
    ///
    /// Pools must be added via `add_pool()` and initialized via `init_from_rpc()`
    /// before use in detection.
    pub fn new() -> Self {
        PoolManager {
            pools: HashMap::new(),
            token_index: HashMap::new(),
            pairs_cache: Mutex::new(None),
            wrapped_native: None,
            balancer_vault: None,
            known_set: HashSet::new(),
            max_pairs_per_token: 50,
            token_max_pairs: HashMap::new(),
            concurrency_limit: 1,
        }
    }

    /// Create a pool manager pre-allocated for the given number of pools.
    pub fn with_capacity(capacity: usize) -> Self {
        PoolManager {
            pools: HashMap::with_capacity(capacity),
            token_index: HashMap::with_capacity(capacity),
            pairs_cache: Mutex::new(None),
            wrapped_native: None,
            balancer_vault: None,
            known_set: HashSet::with_capacity(capacity),
            max_pairs_per_token: 50,
            token_max_pairs: HashMap::new(),
            concurrency_limit: 1,
        }
    }

    /// Set the maximum number of pool pairs per token for arbitrage pair computation.
    pub fn set_max_pairs_per_token(&mut self, max: usize) {
        self.max_pairs_per_token = max;
    }

    /// Set per-token max pairs limit (H3). Tokens without an explicit override
    /// use the global `max_pairs_per_token`. Set 0 for no limit.
    pub fn set_token_max_pairs(&mut self, token: Address, max: usize) {
        self.token_max_pairs.insert(token, max);
    }

    /// Get the effective max_pairs for a given token, accounting for per-token overrides.
    fn effective_max_pairs(&self, token: &Address) -> usize {
        self.token_max_pairs.get(token).copied().unwrap_or(self.max_pairs_per_token)
    }

    /// Set the maximum number of concurrent RPC calls during pool initialization.
    /// Lower values (1-3) are safer for public RPCs with rate limits.
    pub fn set_concurrency_limit(&mut self, limit: u32) {
        self.concurrency_limit = limit.max(1);
    }

    /// Add a pool and update the token index.
    ///
    /// Invalidates the cached arbitrage pairs (recomputed on next `arbitrage_pairs()` call).
    /// Skips ZERO addresses in token index to avoid polluting pair computation.
    pub fn add_pool(&mut self, state: PoolState) {
        let addr = state.address();
        let info = state.info().clone();
        self.known_set.insert(addr);
        self.pools.insert(addr, state);
        if !info.token0.is_zero() {
            self.token_index
                .entry(info.token0)
                .or_default()
                .push(addr);
        }
        if !info.token1.is_zero() {
            self.token_index
                .entry(info.token1)
                .or_default()
                .push(addr);
        }
        *self.pairs_cache.lock().expect("pairs_cache mutex poisoned") = None;
    }

    /// Look up a pool by address.
    pub fn get(&self, address: &Address) -> Option<&PoolState> {
        self.pools.get(address)
    }

    /// Mutable lookup — used to update reserves after events.
    pub fn get_mut(&mut self, address: &Address) -> Option<&mut PoolState> {
        self.pools.get_mut(address)
    }

    /// Iterate over all tracked pools.
    pub fn all_pools(&self) -> impl Iterator<Item = &PoolState> {
        self.pools.values()
    }

    /// Number of pools currently tracked.
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// All pool addresses (used for transaction filtering during replay).
    pub fn pool_addresses(&self) -> Vec<Address> {
        self.pools.keys().copied().collect()
    }

    /// Returns all unique token addresses tracked by the pool manager.
    ///
    /// Used by the transaction filter in `replay.rs` to determine whether
    /// a transaction touches any tracked token (and thus needs full EVM replay).
    pub fn token_addresses(&self) -> Vec<Address> {
        self.token_index.keys().copied().collect()
    }

    /// Returns all pool addresses that trade the given token.
    pub fn pools_for_token(&self, token: &Address) -> Option<&[Address]> {
        self.token_index.get(token).map(|v| v.as_slice())
    }

    /// Find a pool that trades both `token_a` and `token_b`.
    ///
    /// Typically used to find a WMATIC pair for USD pricing fallback.
    /// Returns the first match found in the token index.
    pub fn find_pair_pool(&self, token_a: &Address, token_b: &Address) -> Option<Address> {
        let pools_a = self.token_index.get(token_a)?;
        let pools_b = self.token_index.get(token_b)?;
        // Find the first address common to both sets
        // Use the smaller set for iteration
        let (smaller, larger) = if pools_a.len() < pools_b.len() {
            (pools_a, pools_b)
        } else {
            (pools_b, pools_a)
        };
        smaller.iter().find(|addr| larger.contains(addr)).copied()
    }

    /// Estimate a pool's total liquidity/TVL for sorting purposes.
    /// Higher values mean more meaningful arbitrage candidates.
    pub fn pool_liquidity_estimate(&self, addr: &Address) -> u128 {
        match self.pools.get(addr) {
            Some(PoolState::UniswapV2(v2)) => {
                // Use the smaller reserve as a conservative liquidity bound
                v2.reserve0.min(v2.reserve1)
            }
            Some(PoolState::UniswapV3(v3)) => v3.liquidity,
            Some(PoolState::Curve(c)) => c.balances.iter().sum(),
            Some(PoolState::Balancer(b)) => b.balances.iter().sum(),
            Some(PoolState::Dodo(_)) | Some(PoolState::Clipper(_)) => 0,
            None => 0,
        }
    }

    /// Returns pairs of pool addresses that share at least one common token.
    /// Each pair is returned once (pool_a < pool_b by address), with the shared token.
    /// Pools are sorted by liquidity estimate (descending) before truncation to
    /// `max_pairs_per_token`, so high-volume pairs are preferred over low-volume ones.
    /// Result is cached and invalidated on add_pool.
    pub fn arbitrage_pairs(&self) -> Vec<(Address, Address, Address)> {
        if let Some(cached) = &*self.pairs_cache.lock().expect("pairs_cache mutex poisoned") {
            return cached.clone();
        }
        let mut pairs = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (_token, pool_addrs) in &self.token_index {
            let mut sorted: Vec<Address> = pool_addrs.clone();
            // Sort by estimated liquidity descending so the most meaningful pairs come first
            sorted.sort_by(|a, b| {
                let la = self.pool_liquidity_estimate(a);
                let lb = self.pool_liquidity_estimate(b);
                lb.cmp(&la)
            });
            // Use per-token-tier max_pairs if configured, else global default (H3)
            let token_limit = self.effective_max_pairs(_token);
            let limit = if token_limit == 0 { sorted.len() } else { token_limit.min(sorted.len()) };
            for i in 0..limit {
                for j in (i + 1)..limit {
                    let a = sorted[i];
                    let b = sorted[j];
                    let key = if a < b { (a, b) } else { (b, a) };
                    if seen.insert(key) {
                        pairs.push((key.0, key.1, *_token));
                    }
                }
            }
        }

        *self.pairs_cache.lock().expect("pairs_cache mutex poisoned") = Some(pairs.clone());
        pairs
    }

    /// Count pools that have non-zero reserves (i.e., initialized).
    pub fn initialized_count(&self) -> usize {
        self.pools.values().filter(|p| match p {
            PoolState::UniswapV2(s) => s.reserve0 > 0 && s.reserve1 > 0,
            PoolState::UniswapV3(s) => s.liquidity > 0,
            PoolState::Curve(s) => s.balances.iter().all(|b| *b > 0),
            PoolState::Balancer(s) => s.balances.iter().all(|b| *b > 0),
            PoolState::Dodo(_) | PoolState::Clipper(_) => false,
        })
        .count()
    }

    /// Check if the given address is the wrapped native token (e.g., WMATIC, WETH).
    pub fn is_wrapped_native(&self, token: &Address) -> bool {
        self.wrapped_native.as_ref() == Some(token)
    }

    /// Get the wrapped native token address (e.g., WMATIC, WETH), if set.
    pub fn wrapped_native(&self) -> Option<Address> {
        self.wrapped_native
    }

    /// Convert an amount from the given token to its wrapped native equivalent,
    /// using a V2/V3/Curve/Balancer pool that pairs the token with wrapped native,
    /// or a 2-hop path through an intermediate token.
    ///
    /// For the direct path: uses the pool's quoting function (V2 spot rate, V3 exact-in,
    /// Curve StableSwap, Balancer weighted product).
    ///
    /// When no direct pool exists, searches for a 2-hop path:
    ///   token -> intermediate -> native
    /// using V2 pools (fast, closed-form). Returns None if no path is found.
    pub fn normalize_to_native(&self, token: Address, amount: u128) -> Option<u128> {
        let native = self.wrapped_native()?;
        if token == native {
            return Some(amount);
        }
        // 1. Try direct pair via unified dispatcher
        if let Some(pool_addr) = self.find_pair_pool(&token, &native) {
            if let Some(pool) = self.get(&pool_addr) {
                let result = crate::pool::math::quote_exact_in(pool, token, native, amount);
                if result.is_some() {
                    return result;
                }
            }
        }

        // 2. C5 fallback: try a 2-hop path through an intermediate token
        //    token -> intermediate -> native
        self.normalize_to_native_multi_hop(token, amount, native)
    }

    /// 2-hop normalization fallback: token -> intermediate -> native.
    /// Iterates through pools trading `token` to find an intermediate token
    /// that itself has a pool pairing with `native`.
    fn normalize_to_native_multi_hop(&self, token: Address, amount: u128, native: Address) -> Option<u128> {
        let token_pools = self.pools_for_token(&token)?;
        // Limit search to avoid excessive iteration
        for &pool_addr in token_pools.iter().take(10) {
            let pool = self.get(&pool_addr)?;
            let intermediate = if pool.info().token0 == token {
                pool.info().token1
            } else {
                pool.info().token0
            };
            if intermediate.is_zero() || intermediate == native {
                continue;
            }
            // Check if intermediate trades with native
            if self.find_pair_pool(&intermediate, &native).is_none() {
                continue;
            }
            // Step 1: token -> intermediate via unified dispatcher
            let mid_amount = match pool {
                PoolState::UniswapV2(v2) => {
                    let (reserve_token, reserve_intermediate) = if v2.info.token0 == token {
                        (v2.reserve0, v2.reserve1)
                    } else {
                        (v2.reserve1, v2.reserve0)
                    };
                    if reserve_token == 0 { continue; }
                    amount.saturating_mul(reserve_intermediate).saturating_div(reserve_token)
                }
                other => crate::pool::math::quote_exact_in(other, token, intermediate, amount)?,
            };
            if mid_amount == 0 {
                continue;
            }
            // Step 2: intermediate -> native
            let native_amount = self.normalize_to_native(intermediate, mid_amount)?;
            return Some(native_amount);
        }
        None
    }

    /// Get V2 pool state by address (returns None if not a V2 pool or not found).
    pub fn get_v2_state(&self, address: &Address) -> Option<&UniswapV2PoolState> {
        match self.pools.get(address) {
            Some(PoolState::UniswapV2(state)) => Some(state),
            _ => None,
        }
    }

    /// Get V3 pool state by address (returns None if not a V3 pool or not found).
    pub fn get_v3_state(&self, address: &Address) -> Option<&UniswapV3PoolState> {
        match self.pools.get(address) {
            Some(PoolState::UniswapV3(state)) => Some(state),
            _ => None,
        }
    }

    /// Derive native token USD price from the highest-TVL pool that pairs
    /// wrapped native with a stablecoin (USDC, USDT, DAI).
    /// Returns `None` if no suitable pool is found.
    ///
    /// Price = reserve_stable / reserve_native, adjusted for token decimals.
    /// Used as an on-chain oracle fallback (L5).
    pub fn onchain_native_price(&self, stable_tokens: &[Address]) -> Option<f64> {
        let native = self.wrapped_native()?;
        let mut best_price: Option<f64> = None;
        let mut best_tvl: u128 = 0;
        for &stable in stable_tokens {
            let pool_addr = self.find_pair_pool(&native, &stable)?;
            let pool = self.get(&pool_addr)?;
            let (reserve_native, reserve_stable) = match pool {
                PoolState::UniswapV2(v2) => {
                    if v2.info.token0 == native {
                        (v2.reserve0, v2.reserve1)
                    } else {
                        (v2.reserve1, v2.reserve0)
                    }
                }
                PoolState::UniswapV3(v3) => {
                    // V3 native/stable pools aren't typically used for price
                    // estimation; skip V3 and prefer V2/Curve/Balancer
                    if v3.liquidity == 0 { continue; }
                    let tvl = v3.liquidity;
                    // Use sqrt price for direction: if native is token0,
                    // price = (sqrtPriceX96 / 2^96)^2 token1 per token0
                    let price = if v3.info.token0 == native {
                        let sqrt = v3.sqrt_price_x96;
                        if sqrt.is_zero() { continue; }
                        let p_u256: U256 = sqrt.saturating_mul(sqrt) >> 192;
                        let p = p_u256.to::<u128>();
                        if p == 0 { continue; }
                        p
                    } else {
                        let sqrt = v3.sqrt_price_x96;
                        if sqrt.is_zero() { continue; }
                        let one: U256 = U256::from(1u128) << 192;
                        let inv: U256 = one / sqrt;
                        let p_u256: U256 = inv.saturating_mul(inv) >> 192;
                        let p = p_u256.to::<u128>();
                        if p == 0 { continue; }
                        p
                    };
                    // Use (reserve_native, reserve_stable * price) as TVL proxy
                    // Higher TVL means more reliable price
                    (tvl, tvl.saturating_mul(price))
                }
                PoolState::Curve(curve) => {
                    let idx_native = curve.token_index.get(&native)?;
                    let idx_stable = curve.token_index.get(&stable)?;
                    let bal_native = curve.balances.get(*idx_native)?;
                    let bal_stable = curve.balances.get(*idx_stable)?;
                    (*bal_native, *bal_stable)
                }
                PoolState::Balancer(bal) => {
                    let idx_native = bal.token_index.get(&native)?;
                    let idx_stable = bal.token_index.get(&stable)?;
                    let bal_native = bal.balances.get(*idx_native)?;
                    let bal_stable = bal.balances.get(*idx_stable)?;
                    (*bal_native, *bal_stable)
                }
                PoolState::Dodo(_) | PoolState::Clipper(_) => continue,
            };
            let tvl = reserve_native.saturating_mul(reserve_stable).max(1);
            if tvl > best_tvl {
                best_tvl = tvl;
                if reserve_native > 0 && reserve_stable > 0 {
                    best_price = Some(reserve_stable as f64 / reserve_native as f64);
                }
            }
        }
        best_price
    }

    /// Set the wrapped native token address.
    pub fn with_wrapped_native(mut self, addr: Address) -> Self {
        self.wrapped_native = Some(addr);
        self
    }

    /// Set the Balancer V2 vault address for flash loans and pool state queries.
    pub fn with_balancer_vault(mut self, addr: Address) -> Self {
        self.balancer_vault = Some(addr);
        self
    }
}

impl Clone for PoolManager {
    fn clone(&self) -> Self {
        let cache = self.pairs_cache.lock().expect("pairs_cache mutex poisoned").clone();
        PoolManager {
            pools: self.pools.clone(),
            token_index: self.token_index.clone(),
            pairs_cache: Mutex::new(cache),
            wrapped_native: self.wrapped_native,
            balancer_vault: self.balancer_vault,
            known_set: self.known_set.clone(),
            max_pairs_per_token: self.max_pairs_per_token,
            token_max_pairs: self.token_max_pairs.clone(),
            concurrency_limit: self.concurrency_limit,
        }
    }
}

impl Default for PoolManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Check whether a dedup entry has changed sufficiently to re-emit an opportunity.
/// Returns true if the entry is new or pool reserves changed by >0.1%.
pub fn check_dedup_key(
    seen: &mut HashMap<(Address, Address, Address, Address), (u128, u128)>,
    key: &(Address, Address, Address, Address),
    pm: &PoolManager,
    pool_a: Address,
    pool_b: Address,
) -> bool {
    let la = pm.pool_liquidity_estimate(&pool_a);
    let lb = pm.pool_liquidity_estimate(&pool_b);
    let new_snapshot = (la, lb);

    if let Some(&(prev_la, prev_lb)) = seen.get(key) {
        let threshold_a = cmp::max(prev_la / 1000, 1);
        let threshold_b = cmp::max(prev_lb / 1000, 1);
        if la.abs_diff(prev_la) <= threshold_a && lb.abs_diff(prev_lb) <= threshold_b {
            return false;
        }
    }

    seen.insert(*key, new_snapshot);
    true
}
