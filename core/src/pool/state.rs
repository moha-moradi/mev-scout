//! Pool state management for the backtest engine.
//!
//! This module maintains runtime state for all tracked liquidity pools and
//! processes on-chain events (Swap, Sync, Mint, Burn) to keep state current
//! during block replay.
//!
//! Key components:
//! - `PoolInfo` — static metadata loaded from registry JSON files
//! - `PoolState` enum — runtime state for V2, V3, Curve, and Balancer pools
//! - `PoolManager` — central registry that initializes pools from on-chain state
//!   and updates reserves from transaction logs
//!
//! Pool initialization uses a two-phase approach:
//! 1. Load `PoolInfo` from JSON registry + discovery cache
//! 2. Fetch live reserve/state data via `eth_call` (with `eth_getStorageAt` fallback)

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::Arc;
use std::sync::LazyLock;

use alloy::primitives::{b256, keccak256, Address, Bytes, B256, U256};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::data::ExecutedLog;
use crate::pool::decoders;
use crate::pool::dex_type::DexType;
use crate::rpc::RpcClient;
use crate::utils::u128_from_be_bytes;

/// Static pool information loaded from the discovery cache or subgraph.
///
/// `PoolInfo` is deserialized from the discovery cache (SQLite) after
/// pools are discovered via subgraph (default) or on-chain eth_getLogs.
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
    pub creation_block: u64,
    /// Balancer V2 pool ID (bytes32), used to query vault for token balances.
    #[serde(default)]
    pub pool_id: Option<[u8; 32]>,
    /// Factory address that created this pool (L6: fork-aware V2 storage slots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factory: Option<Address>,
}

impl Default for PoolInfo {
    fn default() -> Self {
        PoolInfo {
            address: Address::ZERO,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 0,
            name: None,
            dex_type: DexType::UniswapV2,
            tick_spacing: None,
            creation_block: 0,
            pool_id: None,
            factory: None,
        }
    }
}

impl PoolInfo {
    pub fn is_concentrated_liquidity(&self) -> bool {
        self.dex_type.is_concentrated_liquidity()
    }
}

/// Runtime state for a Uniswap V2 constant-product pool.
///
/// Tracks the two reserve balances that define the pool's invariant `x * y = k`.
/// Reserves are updated on every Swap or Sync event during block replay.
#[derive(Debug, Clone)]
pub struct UniswapV2PoolState {
    pub info: PoolInfo,
    pub reserve0: u128,
    pub reserve1: u128,
}

/// Runtime state for a Uniswap V3 concentrated-liquidity pool.
///
/// Tracks sqrt price, current tick, global liquidity, and per-tick liquidity
/// deltas. The `ticks` map is updated on Mint/Burn events and consulted
/// during V3 swap quoting (`v3_quote.rs`).
#[derive(Debug, Clone)]
pub struct UniswapV3PoolState {
    pub info: PoolInfo,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    /// Ticks with position liquidity (tick_idx → net_liquidity_delta from all positions)
    pub ticks: std::collections::BTreeMap<i32, i128>,
    /// Cumulative fee growth per unit of liquidity for token0 (Q128 format).
    /// Updated on each Swap event: feeGrowthGlobal0 += fee_amount0 * 2^128 / liquidity
    pub fee_growth_global_0_x128: U256,
    /// Cumulative fee growth per unit of liquidity for token1 (Q128 format).
    pub fee_growth_global_1_x128: U256,
}

impl UniswapV3PoolState {
    pub fn new(info: PoolInfo) -> Self {
        UniswapV3PoolState {
            info,
            sqrt_price_x96: U256::ZERO,
            tick: 0,
            liquidity: 0,
            ticks: std::collections::BTreeMap::new(),
            fee_growth_global_0_x128: U256::ZERO,
            fee_growth_global_1_x128: U256::ZERO,
        }
    }
}

/// Curve pool variant — determines which quoting formula to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CurvePoolVariant {
    #[default]
    /// Standard StableSwap (Plain or Lending pools).
    Plain,
    /// Metapool — wraps a base pool LP token; needs base-pool nesting for quotes.
    Meta,
    /// CryptoSwap V2 (Tricrypto, Two-Asset, etc.) — uses gamma, price_scale, dynamic fee.
    Crypto,
    /// Catch-all for unsupported variants.
    Other,
}

/// Runtime state for a Curve pool (stable-swap / crypto).
#[derive(Debug, Clone)]
pub struct CurvePoolState {
    pub info: PoolInfo,
    pub balances: Vec<u128>,
    pub token_index: HashMap<Address, usize>,
    /// Amplification coefficient (A). Defaults to 100 if unknown.
    pub a_coeff: u128,
    /// Pool variant — determines which quoting formula to use.
    pub pool_variant: CurvePoolVariant,
    /// Gamma parameter for CryptoSwap V2 pools (price-invariant-convergence).
    pub gamma: Option<u128>,
    /// Price scale for CryptoSwap V2 pools (one per non-first token).
    pub price_scale: Vec<u128>,
    /// Base pool address for metapools (None for plain/crypto pools).
    pub base_pool: Option<Address>,
}

/// Balancer pool variant — determines which quoting formula to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BalancerPoolVariant {
    #[default]
    Weighted,
    Stable,
    /// Composable Stable (same math as Stable, but with BPT in token list and scaling factors).
    ComposableStable,
    /// Catch-all for unsupported variants (Gyro, LBP, Managed, etc.).
    Other,
}

/// Runtime state for a Balancer V2 weighted/stable pool.
#[derive(Debug, Clone)]
pub struct BalancerPoolState {
    pub info: PoolInfo,
    pub balances: Vec<u128>,
    pub token_index: HashMap<Address, usize>,
    pub pool_id: Option<[u8; 32]>,
    /// Weights for each token (same order as balances, basis points = 1e18).
    /// If empty, equal weights are assumed.
    pub weights: Vec<u128>,
    /// Pool variant — determines which quoting formula to use.
    pub pool_variant: BalancerPoolVariant,
    /// Amplification parameter for Stable pools (None for Weighted pools).
    pub amplification: Option<u128>,
    /// Scaling factors for each token (Composable Stable / Boosted pools).
    pub scaling_factors: Vec<u128>,
    /// Index of the pool's own BPT token in the token list (None for non-composable).
    pub bpt_index: Option<usize>,
}

/// Runtime state for any tracked pool.
///
/// Enum variants correspond to supported DEX types.
/// Each variant wraps the type-specific state struct defined above.
#[derive(Debug, Clone)]
pub enum PoolState {
    UniswapV2(UniswapV2PoolState),
    UniswapV3(UniswapV3PoolState),
    Curve(CurvePoolState),
    Balancer(BalancerPoolState),
}

impl PoolState {
    pub fn address(&self) -> Address {
        match self {
            PoolState::UniswapV2(s) => s.info.address,
            PoolState::UniswapV3(s) => s.info.address,
            PoolState::Curve(s) => s.info.address,
            PoolState::Balancer(s) => s.info.address,
        }
    }

    pub fn info(&self) -> &PoolInfo {
        match self {
            PoolState::UniswapV2(s) => &s.info,
            PoolState::UniswapV3(s) => &s.info,
            PoolState::Curve(s) => &s.info,
            PoolState::Balancer(s) => &s.info,
        }
    }

    pub fn info_mut(&mut self) -> &mut PoolInfo {
        match self {
            PoolState::UniswapV2(s) => &mut s.info,
            PoolState::UniswapV3(s) => &mut s.info,
            PoolState::Curve(s) => &mut s.info,
            PoolState::Balancer(s) => &mut s.info,
        }
    }

    pub fn dex_label(&self) -> &'static str {
        self.info().dex_type.label()
    }

    /// Estimated gas cost for a single swap on this pool type.
    /// Empirical benchmarks (H7):
    /// - V2 swap: ~80k (Uniswap V2 swap)
    /// - V3 swap (base, few tick crossings): ~120k average (use `estimate_v3_swap_gas` for per-direction estimate)
    /// - Curve swap: ~100k
    /// - Balancer swap: ~100k
    pub fn gas_estimate(&self) -> u64 {
        match self {
            PoolState::UniswapV2(_) => 80_000,
            PoolState::UniswapV3(_) => 120_000,
            PoolState::Curve(_) => 100_000,
            PoolState::Balancer(_) => 100_000,
        }
    }
}

/// Estimate calldata gas cost for a transaction involving `pool_count` pool addresses.
/// Base tx cost: 21,000 gas. Each warm address read in calldata: ~2,800 gas.
/// (H7: Include calldata cost in per-opportunity gas estimation.)
pub fn calldata_gas_estimate(pool_count: usize) -> u64 {
    21_000 + (pool_count as u64) * 2_800
}

/// Internal helper: result of fetching on-chain state for a pool during init.
enum PoolInitResult {
    V2Reserves(u128, u128),
    V3State(U256, i32, u128, std::collections::BTreeMap<i32, i128>),
    /// (tokens, balances, weights, fee_bps, variant, amplification, scaling_factors, bpt_index)
    BalancerState(Vec<Address>, Vec<u128>, Vec<u128>, u32, BalancerPoolVariant, Option<u128>, Vec<u128>, Option<usize>),
    /// (tokens, balances, a_coeff, fee_bps, variant, gamma, price_scale, base_pool)
    CurveState(Vec<Address>, Vec<u128>, u128, u32, CurvePoolVariant, Option<u128>, Vec<u128>, Option<Address>),
}

/// Event signature for Uniswap V2 Swap event
const SWAP_TOPIC: B256 = b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");
/// Event signature for Uniswap V2 Sync event
const SYNC_TOPIC: B256 = b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");
/// getReserves() selector
const GET_RESERVES_SELECTOR: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];

/// balances(int128) selector for Curve pools
const CURVE_BALANCES_SELECTOR: [u8; 4] = [0x49, 0x7b, 0x66, 0x78];

/// A() selector for Curve pools — amplification coefficient
const CURVE_A_SELECTOR: [u8; 4] = [0x0f, 0x0b, 0x7c, 0x7e];

/// fee() selector for Curve pools — swap fee (parts per 10¹⁰)
const CURVE_FEE_SELECTOR: [u8; 4] = [0xdd, 0xca, 0x3f, 0x43];

/// get_A() selector for Curve CryptoSwap V2 pools
const CURVE_GET_A_SELECTOR: [u8; 4] = [0x4d, 0x30, 0xa4, 0x7f];

/// gamma() selector for Curve CryptoSwap V2 pools
const CURVE_GAMMA_SELECTOR: [u8; 4] = [0x67, 0x1d, 0x47, 0x23];

/// price_scale() selector for Curve CryptoSwap V2 pools (returns all scales as array)
const CURVE_PRICE_SCALE_SELECTOR: [u8; 4] = [0x5e, 0x0d, 0x7a, 0x5a];

/// price_oracle(uint256) selector for Curve CryptoSwap V2 pools
#[allow(dead_code)]
const CURVE_PRICE_ORACLE_SELECTOR: [u8; 4] = [0xaa, 0x1e, 0x29, 0x84];

/// base_pool() selector for Curve Metapools
const CURVE_BASE_POOL_SELECTOR: [u8; 4] = [0x9c, 0xec, 0x6e, 0xae];

/// slot0() selector for Uniswap V3
static V3_SLOT0_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"slot0()");
    Bytes::copy_from_slice(&hash[..4])
});
/// liquidity() selector for Uniswap V3
static V3_LIQUIDITY_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"liquidity()");
    Bytes::copy_from_slice(&hash[..4])
});
/// getPoolTokens(bytes32) selector for Balancer V2 vault
static GET_POOL_TOKENS_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getPoolTokens(bytes32)");
    Bytes::copy_from_slice(&hash[..4])
});

/// getNormalizedWeights() selector for Balancer weighted pools
static GET_NORMALIZED_WEIGHTS_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getNormalizedWeights()");
    Bytes::copy_from_slice(&hash[..4])
});

/// getSwapFeePercentage() selector for Balancer pools
static GET_SWAP_FEE_PERCENTAGE_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getSwapFeePercentage()");
    Bytes::copy_from_slice(&hash[..4])
});

/// getAmplificationParameter() selector for Balancer stable pools
static GET_AMPLIFICATION_PARAMETER_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getAmplificationParameter()");
    Bytes::copy_from_slice(&hash[..4])
});

/// getScalingFactors() selector for Balancer composable/boosted pools
static GET_SCALING_FACTORS_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"getScalingFactors()");
    Bytes::copy_from_slice(&hash[..4])
});

/// tickBitmap(int16) selector for Uniswap V3 tick bitmap queries
static V3_TICK_BITMAP_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"tickBitmap(int16)");
    Bytes::copy_from_slice(&hash[..4])
});

/// ticks(int24) selector for Uniswap V3 per-tick data queries
static V3_TICKS_SELECTOR: LazyLock<Bytes> = LazyLock::new(|| {
    let hash = keccak256(b"ticks(int24)");
    Bytes::copy_from_slice(&hash[..4])
});

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
    pools: HashMap<Address, PoolState>,
    /// token address -> list of pool addresses that trade this token
    token_index: HashMap<Address, Vec<Address>>,
    /// Cached arbitrage pairs (invalidated on add_pool)
    pairs_cache: Mutex<Option<Vec<(Address, Address, Address)>>>,
    /// Address of the wrapped native token (WMATIC/WETH/WBNB) per chain.
    wrapped_native: Option<Address>,
    /// Address of the Balancer V2 vault for flash loans and pool state queries.
    balancer_vault: Option<Address>,
    /// Pre-filter set of known pool addresses for fast log filtering.
    known_set: HashSet<Address>,
    /// Maximum number of pools per token when computing arbitrage pairs.
    max_pairs_per_token: usize,
    /// Per-token overrides for max_pairs_per_token (H3).
    /// Allows configuring different caps for high/medium/low-connectivity tokens.
    /// Key = token address, value = per-token max pairs limit.
    token_max_pairs: HashMap<Address, usize>,
    /// Maximum number of concurrent RPC calls during pool initialization.
    concurrency_limit: u32,
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
        *self.pairs_cache.lock().unwrap() = None;
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
            None => 0,
        }
    }

    /// Returns pairs of pool addresses that share at least one common token.
    /// Each pair is returned once (pool_a < pool_b by address), with the shared token.
    /// Pools are sorted by liquidity estimate (descending) before truncation to
    /// `max_pairs_per_token`, so high-volume pairs are preferred over low-volume ones.
    /// Result is cached and invalidated on add_pool.
    pub fn arbitrage_pairs(&self) -> Vec<(Address, Address, Address)> {
        if let Some(cached) = &*self.pairs_cache.lock().unwrap() {
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

        *self.pairs_cache.lock().unwrap() = Some(pairs.clone());
        pairs
    }

    /// Update a V2 pool's reserves using amounts from a Swap event.
    pub fn apply_v2_swap(
        &mut self,
        address: &Address,
        amount0_in: u128,
        amount1_in: u128,
        amount0_out: u128,
        amount1_out: u128,
    ) {
        if let Some(PoolState::UniswapV2(state)) = self.pools.get_mut(address) {
            state.reserve0 = state.reserve0.wrapping_add(amount0_in).wrapping_sub(amount0_out);
            state.reserve1 = state.reserve1.wrapping_add(amount1_in).wrapping_sub(amount1_out);
        }
    }

    /// Update a V2 pool's reserves from a Sync event (authoritative override).
    pub fn apply_v2_sync(&mut self, address: &Address, reserve0: u128, reserve1: u128) {
        if let Some(PoolState::UniswapV2(state)) = self.pools.get_mut(address) {
            state.reserve0 = reserve0;
            state.reserve1 = reserve1;
        }
    }

    /// Update a V3 pool's state from a Swap event.
    pub fn apply_v3_swap(
        &mut self,
        address: &Address,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: u128,
        amount0: i128,
        amount1: i128,
    ) {
        if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(address) {
            state.sqrt_price_x96 = sqrt_price_x96;
            state.tick = tick;
            state.liquidity = liquidity;

            // Update fee growth global based on swap amounts.
            // The negative amount is the input (trader sends to pool).
            // Fee = input * fee_tier / 1_000_000, added to feeGrowthGlobal.
            let fee_tier = state.info.fee as u128;
            if amount0 < 0 {
                let input = amount0.unsigned_abs();
                let fee = input.saturating_mul(fee_tier) / 1_000_000u128;
                if fee > 0 && liquidity > 0 {
                    let inc = (U256::from(fee) << 128) / U256::from(liquidity);
                    state.fee_growth_global_0_x128 = state.fee_growth_global_0_x128.saturating_add(inc);
                }
            }
            if amount1 < 0 {
                let input = amount1.unsigned_abs();
                let fee = input.saturating_mul(fee_tier) / 1_000_000u128;
                if fee > 0 && liquidity > 0 {
                    let inc = (U256::from(fee) << 128) / U256::from(liquidity);
                    state.fee_growth_global_1_x128 = state.fee_growth_global_1_x128.saturating_add(inc);
                }
            }
        }
    }

    /// Update a V3 pool's tick liquidity from a Mint or Burn event.
    pub fn apply_v3_mint_burn(
        &mut self,
        address: &Address,
        tick_lower: i32,
        tick_upper: i32,
        amount: i128,
    ) {
        if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(address) {
            *state.ticks.entry(tick_lower).or_insert(0) += amount;
            *state.ticks.entry(tick_upper).or_insert(0) -= amount;
            if amount > 0 {
                state.liquidity = state.liquidity.saturating_add(amount as u128);
            } else {
                state.liquidity = state.liquidity.saturating_sub((-amount) as u128);
            }
        }
    }

    /// Count pools that have non-zero reserves (i.e., initialized).
    pub fn initialized_count(&self) -> usize {
        self.pools.values().filter(|p| match p {
            PoolState::UniswapV2(s) => s.reserve0 > 0 && s.reserve1 > 0,
            PoolState::UniswapV3(s) => s.liquidity > 0,
            PoolState::Curve(s) => s.balances.iter().all(|b| *b > 0),
            PoolState::Balancer(s) => s.balances.iter().all(|b| *b > 0),
        })
        .count()
    }

    /// Initialize pool state from on-chain calls at a historical block.
    /// Dispatches V2 / V3 / Curve / Balancer per pool type.
    /// Fetches all pools in parallel, capped by concurrency_limit.
    pub async fn init_from_rpc(&mut self, rpc: &RpcClient, block_num: u64) {
        let pool_addrs: Vec<Address> = self.pools.keys().copied().collect();
        let max_concurrent = self.concurrency_limit as usize;
        let cap = pool_addrs.len().clamp(1, max_concurrent);
        let semaphore = Arc::new(Semaphore::new(cap));

        // Snapshot pool metadata before spawning tasks
        let vault = self.balancer_vault;
        let pool_meta: Vec<(DexType, Option<[u8; 32]>, i32, Option<Address>)> = pool_addrs
            .iter()
            .map(|addr| match self.pools.get(addr) {
                Some(PoolState::UniswapV2(s)) => (DexType::UniswapV2, None, 0, s.info.factory),
                Some(PoolState::UniswapV3(state)) => (
                    DexType::UniswapV3,
                    None,
                    state.info.tick_spacing.unwrap_or(60) as i32,
                    None,
                ),
                Some(PoolState::Curve(_)) => (DexType::Curve, None, 0, None),
                Some(PoolState::Balancer(_)) => (DexType::Balancer, None, 0, None),
                None => (DexType::UniswapV2, None, 0, None),
            })
            .collect();

        let tasks: Vec<_> = pool_addrs
            .iter()
            .zip(pool_meta.iter())
            .map(|(addr, (dt, pool_id, tick_spacing, factory))| {
                let rpc = rpc.clone();
                let sem = Arc::clone(&semaphore);
                let addr = *addr;
                let dt = *dt;
                let pool_id = *pool_id;
                let tick_spacing = *tick_spacing;
                let factory = *factory;
                async move {
                    let _permit = sem.acquire_owned().await.ok();
                    Self::fetch_pool_state(&rpc, addr, dt, pool_id, tick_spacing, vault, factory, block_num).await
                }
            })
            .collect();

        let results = join_all(tasks).await;

        for (addr, result) in pool_addrs.iter().zip(results) {
            match result {
                Some(PoolInitResult::V2Reserves(r0, r1)) => {
                    if let Some(PoolState::UniswapV2(state)) = self.pools.get_mut(addr) {
                        state.reserve0 = r0;
                        state.reserve1 = r1;
                    }
                }
                Some(PoolInitResult::V3State(sqrt, tick, liq, initialized_ticks)) => {
                    if let Some(PoolState::UniswapV3(state)) = self.pools.get_mut(addr) {
                        state.sqrt_price_x96 = sqrt;
                        state.tick = tick;
                        state.liquidity = liq;
                        state.ticks = initialized_ticks;
                        if state.ticks.is_empty() {
                            tracing::warn!("V3 pool {} initialized with empty tick map", addr);
                        } else {
                            tracing::debug!("V3 pool {} initialized with {} ticks", addr, state.ticks.len());
                        }
                        if sqrt.is_zero() {
                            tracing::warn!("V3 pool {} initialized with zero sqrt price", addr);
                        }
                    }
                }
                Some(PoolInitResult::BalancerState(tokens, balances, weights, fee_bps, variant, amplification, scaling_factors, bpt_index)) => {
                    if let Some(PoolState::Balancer(state)) = self.pools.get_mut(addr) {
                        state.balances = balances;
                        state.weights = weights;
                        state.info.fee = fee_bps;
                        state.pool_variant = variant;
                        state.amplification = amplification;
                        state.scaling_factors = scaling_factors;
                        state.bpt_index = bpt_index;
                        if tokens.len() >= 2 {
                            state.info.token0 = tokens[0];
                            state.info.token1 = tokens[1];
                        }
                        state.token_index.clear();
                        for (i, token) in tokens.iter().enumerate() {
                            state.token_index.insert(*token, i);
                        }
                        for token in &tokens {
                            if !token.is_zero() {
                                self.token_index.entry(*token).or_default().push(*addr);
                            }
                        }
                    }
                }
                Some(PoolInitResult::CurveState(tokens, balances, a_coeff, fee_bps, variant, gamma, price_scale, base_pool)) => {
                    if let Some(PoolState::Curve(state)) = self.pools.get_mut(addr) {
                        state.balances = balances;
                        state.a_coeff = a_coeff;
                        state.info.fee = fee_bps;
                        state.pool_variant = variant;
                        state.gamma = gamma;
                        state.price_scale = price_scale;
                        state.base_pool = base_pool;
                        state.token_index.clear();
                        for (i, token) in tokens.iter().enumerate() {
                            state.token_index.insert(*token, i);
                            if i == 0 {
                                state.info.token0 = *token;
                            } else if i == 1 {
                                state.info.token1 = *token;
                            }
                        }
                        for token in &tokens {
                            if !token.is_zero() {
                                self.token_index.entry(*token).or_default().push(*addr);
                            }
                        }
                    }
                }
                None => {
                    tracing::warn!("Failed to fetch state for pool {}", addr);
                }
            }
        }
    }

    /// Filter pools to only those that have non-empty bytecode at the target block.
    /// Uses concurrent `eth_getCode` calls bounded by the concurrency limit.
    /// Pools with empty code (not yet deployed or self-destructed) are excluded.
    pub async fn filter_existing_pools(
        rpc: &RpcClient,
        pools: &[PoolInfo],
        block_num: u64,
        concurrency_limit: usize,
    ) -> Vec<PoolInfo> {
        if pools.is_empty() {
            return Vec::new();
        }
        let cap = pools.len().clamp(1, concurrency_limit);
        let semaphore = Arc::new(Semaphore::new(cap));

        let tasks: Vec<_> = pools
            .iter()
            .map(|info| {
                let rpc = rpc.clone();
                let sem = Arc::clone(&semaphore);
                let addr = info.address;
                let info = info.clone();
                async move {
                    let _permit = sem.acquire_owned().await.ok();
                    match rpc.get_code(addr, block_num).await {
                        Ok(code) if !code.is_empty() => Some(info),
                        _ => None,
                    }
                }
            })
            .collect();

        let results = join_all(tasks).await;
        results.into_iter().flatten().collect()
    }

    /// Fetch the appropriate on-chain state for a pool based on its type.
    async fn fetch_pool_state(
        rpc: &RpcClient,
        pool: Address,
        dt: DexType,
        pool_id: Option<[u8; 32]>,
        tick_spacing: i32,
        vault: Option<Address>,
        factory: Option<Address>,
        block: u64,
    ) -> Option<PoolInitResult> {
        match dt {
            DexType::UniswapV2 => {
                let (r0, r1) = Self::fetch_v2_reserves(rpc, pool, block, factory).await?;
                Some(PoolInitResult::V2Reserves(r0, r1))
            }
            DexType::UniswapV3 => {
                let (sqrt, tick, liq, ticks) = Self::fetch_v3_state(rpc, pool, block, tick_spacing).await?;
                Some(PoolInitResult::V3State(sqrt, tick, liq, ticks))
            }
            DexType::Balancer => {
                let vault = vault?;
                let pool_id = pool_id?;
                Self::fetch_balancer_state(rpc, vault, pool, &pool_id, block).await.ok()
            }
            DexType::Curve => {
                Self::fetch_curve_state(rpc, pool, block).await
            }
        }
    }

    async fn call_once(rpc: &RpcClient, pool: Address, data: Bytes, block: u64) -> Result<Bytes, ()> {
        rpc.call(pool, data, block).await.map_err(|_| ())
    }

    async fn fetch_v2_reserves(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
        factory: Option<Address>,
    ) -> Option<(u128, u128)> {
        let slots: Vec<U256> = crate::types::v2_storage_slots_for_factory(factory)
            .iter()
            .map(|&s| U256::from(s))
            .collect();

        // M9: eth_getStorageAt is primary path (cheaper, works on more nodes).
        // eth_call getReserves() is the fallback.
        if let Some(reserves) = Self::fetch_v2_reserves_storage(rpc, pool, block, &slots).await {
            if Self::validate_v2_reserves(reserves) {
                return Some(reserves);
            }
            tracing::trace!("storage reserves failed validation ({},{}), trying eth_call", reserves.0, reserves.1);
        }

        // Fallback: eth_call getReserves()
        let data = Bytes::copy_from_slice(&GET_RESERVES_SELECTOR);
        if let Ok(result) = Self::call_once(rpc, pool, data, block).await {
            if result.len() >= 64 {
                let r0 = Self::decode_u128_from_abi_word(&result[..32]);
                let r1 = Self::decode_u128_from_abi_word(&result[32..64]);
                let reserves = (r0, r1);
                if Self::validate_v2_reserves(reserves) {
                    return Some(reserves);
                }
                tracing::trace!("eth_call reserves failed validation ({},{}), returning raw", r0, r1);
                return Some(reserves);
            }
        }
        tracing::trace!("eth_call getReserves() failed for {}", pool);
        None
    }

    /// Validate V2 reserves: both > 0 and ratio is within 100x (M9).
    fn validate_v2_reserves(reserves: (u128, u128)) -> bool {
        let (r0, r1) = reserves;
        if r0 == 0 || r1 == 0 {
            return false;
        }
        // Reject extreme ratios (>100:1) which indicate corrupted data
        let (big, small) = if r0 > r1 { (r0, r1) } else { (r1, r0) };
        if small == 0 { return false; }
        let ratio = big / small;
        ratio < 100
    }

    /// Given raw slot 6 (packed uint112 reserve0 | uint112 reserve1 | uint32 blockTimestampLast),
    /// decode reserve0 and reserve1.
    fn decode_v2_reserves_from_storage(raw: U256) -> (u128, u128) {
        let mask: U256 = (U256::from(1u128) << 112) - U256::from(1u128);
        let masked_r0 = raw & mask;
        let masked_r1 = (raw >> U256::from(112u64)) & mask;
        let lo_r0 = masked_r0.as_limbs();
        let lo_r1 = masked_r1.as_limbs();
        let r0 = (lo_r0[0] as u128) | ((lo_r0[1] as u128) << 64);
        let r1 = (lo_r1[0] as u128) | ((lo_r1[1] as u128) << 64);
        (r0, r1)
    }

    /// Decode a u128 from a 32-byte ABI-encoded word (right-aligned uint128/uint112).
    /// Handles values up to 2^128-1 without truncation.
    fn decode_u128_from_abi_word(word: &[u8]) -> u128 {
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&word[16..32]);
        u128::from_be_bytes(buf)
    }

    /// Fallback: fetch V2 reserves via eth_getStorageAt.
    /// Tries the given storage slots and returns the first that decodes to non-zero.
    /// If all slots return zero, falls back to the first slot as last resort.
    async fn fetch_v2_reserves_storage(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
        slots: &[U256],
    ) -> Option<(u128, u128)> {
        for &slot in slots {
            if let Ok(raw) = rpc.get_storage_at(pool, slot, block).await {
                let (r0, r1) = Self::decode_v2_reserves_from_storage(raw);
                if r0 > 0 || r1 > 0 {
                    return Some((r0, r1));
                }
            }
        }
        // Last resort: try the first slot even if zero
        let first = slots.first().copied().unwrap_or(U256::from(6u64));
        let raw = rpc.get_storage_at(pool, first, block).await.ok()?;
        Some(Self::decode_v2_reserves_from_storage(raw))
    }

    /// Fetch V3 pool slot0() + liquidity() + tick data at a historical block.
    /// Also bootstraps the tick map from on-chain via `tickBitmap` and `ticks` calls,
    /// fixing C2: pre-existing LP positions are now visible from the first block.
    async fn fetch_v3_state(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
        tick_spacing: i32,
    ) -> Option<(U256, i32, u128, std::collections::BTreeMap<i32, i128>)> {
        let slot0_result = Self::call_once(rpc, pool, V3_SLOT0_SELECTOR.clone(), block).await;
        let liq_result = Self::call_once(rpc, pool, V3_LIQUIDITY_SELECTOR.clone(), block).await;
        if let (Ok(slot0), Ok(liq)) = (slot0_result.as_ref(), liq_result.as_ref()) {
            if slot0.len() >= 96 && liq.len() >= 32 {
                let mut buf = [0u8; 32];
                buf.copy_from_slice(&slot0[..32]);
                let sqrt_price_x96 = U256::from_be_bytes(buf);
                let mut tick_bytes = [0u8; 4];
                tick_bytes.copy_from_slice(&slot0[60..64]);
                let tick = i32::from_be_bytes(tick_bytes);
                let liquidity = Self::decode_u128_from_abi_word(&liq[..32]);

                // Bootstrap tick data from on-chain tick bitmap + per-tick queries
                // This makes pre-existing LP positions visible from the first block.
                let initialized_ticks = Self::fetch_v3_initialized_ticks(rpc, pool, tick, tick_spacing, block).await;

                return Some((sqrt_price_x96, tick, liquidity, initialized_ticks));
            }
        }
        if slot0_result.is_err() {
            tracing::trace!("eth_call slot0() failed for {}", pool);
        }
        if liq_result.is_err() {
            tracing::trace!("eth_call liquidity() failed for {}", pool);
        }
        if let Ok(slot0) = slot0_result.as_ref() {
            if slot0.len() < 96 {
                tracing::trace!("eth_call slot0() returned short result for {}", pool);
            }
        }
        if let Ok(liq) = liq_result.as_ref() {
            if liq.len() < 32 {
                tracing::trace!("eth_call liquidity() returned short result for {}", pool);
            }
        }
        tracing::trace!("falling back to storage for V3 pool {}", pool);
        let (sqrt, tick, liq) = Self::fetch_v3_state_storage(rpc, pool, block).await?;
        let initialized_ticks = Self::fetch_v3_initialized_ticks(rpc, pool, tick, tick_spacing, block).await;
        Some((sqrt, tick, liq, initialized_ticks))
    }

    /// Fetch initialized tick liquidity nets from a V3 pool contract.
    ///
    /// Queries the tick bitmap for 5 word positions centered around the current
    /// tick (~1280 tick range), then fetches `liquidityNet` for each initialized
    /// tick found (up to 200 ticks). This bootstraps pre-existing LP positions
    /// so that V3 quoting in the first block is accurate (fixes C2).
    async fn fetch_v3_initialized_ticks(
        rpc: &RpcClient,
        pool: Address,
        current_tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> std::collections::BTreeMap<i32, i128> {
        let max_ticks = 200usize;
        let mut ticks = std::collections::BTreeMap::new();

        // Compressed tick: floor division that handles negatives
        let compressed = if current_tick < 0 && current_tick % tick_spacing != 0 {
            current_tick / tick_spacing - 1
        } else {
            current_tick / tick_spacing
        };
        let center_word = compressed >> 8;

        // Scan 5 word positions (≈1280 tick range) centered on current tick
        for word_offset in -2i16..=2i16 {
            if ticks.len() >= max_ticks {
                break;
            }
            let w = center_word.wrapping_add(word_offset as i32);

            // Build calldata for tickBitmap(int16)
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&V3_TICK_BITMAP_SELECTOR);
            let mut arg = [0u8; 32];
            let w_i16 = w as i16;
            let w_be = w_i16.to_be_bytes();
            arg[30..32].copy_from_slice(&w_be);
            if w_i16 < 0 {
                arg[..30].fill(0xFF);
            }
            calldata.extend_from_slice(&arg);

            let bitmap_bytes = match Self::call_once(rpc, pool, Bytes::from(calldata), block).await {
                Ok(b) if b.len() >= 32 => b,
                _ => continue,
            };

            let bitmap = U256::from_be_slice(&bitmap_bytes[..32]);
            if bitmap.is_zero() {
                continue;
            }

            // Iterate over set bits in the 256-bit bitmap
            let mut bits_remaining = bitmap;
            for bit_pos in 0u8..=255u8 {
                if ticks.len() >= max_ticks {
                    break;
                }
                if !bits_remaining.bit(bit_pos as usize) {
                    continue;
                }
                bits_remaining = bits_remaining & !(U256::from(1u128) << bit_pos as usize);

                // Decode tick index: compressed = w * 256 + bit_pos
                let compressed_tick = (w << 8) | (bit_pos as i32);
                // For negative word positions, ensure proper sign extension
                let compressed_tick = if w < 0 && bit_pos == 0 {
                    compressed_tick
                } else {
                    compressed_tick
                };
                let actual_tick = compressed_tick.wrapping_mul(tick_spacing);

                // Fetch liquidityNet for this tick via ticks(int24)
                if let Some(liq_net) = Self::fetch_v3_tick_liquidity_net(rpc, pool, actual_tick, block).await {
                    if liq_net != 0 {
                        ticks.insert(actual_tick, liq_net);
                    }
                }
            }
        }

        if !ticks.is_empty() {
            tracing::debug!(
                "Bootstrapped {} initialized ticks for V3 pool {} at block {}",
                ticks.len(),
                pool,
                block,
            );
        }

        ticks
    }

    /// Fetch liquidityNet for a single tick from a V3 pool via `ticks(int24)`.
    async fn fetch_v3_tick_liquidity_net(
        rpc: &RpcClient,
        pool: Address,
        tick: i32,
        block: u64,
    ) -> Option<i128> {
        // Build calldata for ticks(int24)
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&V3_TICKS_SELECTOR);
        let mut arg = [0u8; 32];
        let tick_bytes = tick.to_be_bytes();
        arg[29..32].copy_from_slice(&tick_bytes[1..4]);
        if tick < 0 {
            arg[..29].fill(0xFF);
        }
        calldata.extend_from_slice(&arg);

        let result = Self::call_once(rpc, pool, Bytes::from(calldata), block).await.ok()?;

        // ABI decode: (uint128 liquidityGross, int128 liquidityNet, ...)
        // Tuple with ABIEncoderV2: 8 fields × 32 bytes = 256 bytes
        if result.len() < 64 {
            return None;
        }

        // Check initialized flag (8th element, last 32-byte slot)
        let init_byte = result[result.len() - 1];
        if init_byte == 0 {
            return None;
        }

        // Decode int128 liquidityNet from bytes 48..64 (second 32-byte slot, lower 16 bytes)
        let mut net_bytes = [0u8; 16];
        net_bytes.copy_from_slice(&result[48..64]);
        let liq_net = i128::from_be_bytes(net_bytes);

        Some(liq_net)
    }

    /// Given raw slot0 + slot1, decode sqrtPriceX96, tick, and liquidity.
    fn decode_v3_state_from_storage(slot0_raw: U256, slot1_raw: U256) -> (U256, i32, u128) {
        let bytes = slot0_raw.to_be_bytes::<32>();
        let sqrt_price_x96 = U256::from_be_bytes({
            let mut buf = [0u8; 32];
            buf[12..32].copy_from_slice(&bytes[12..32]);
            buf
        });
        let mut tick_buf = [0u8; 4];
        tick_buf[1..4].copy_from_slice(&bytes[9..12]);
        if tick_buf[1] & 0x80 != 0 {
            tick_buf[0] = 0xFF;
        }
        let tick = i32::from_be_bytes(tick_buf);

        let bytes = slot1_raw.to_be_bytes::<32>();
        let liquidity = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&bytes[16..32]);
            buf
        });

        (sqrt_price_x96, tick, liquidity)
    }

    /// Fallback: fetch V3 state via eth_getStorageAt.
    async fn fetch_v3_state_storage(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
    ) -> Option<(U256, i32, u128)> {
        let slot0_raw = rpc.get_storage_at(pool, U256::ZERO, block).await.ok()?;
        let slot1_raw = rpc.get_storage_at(pool, U256::from(1), block).await.ok()?;
        Some(Self::decode_v3_state_from_storage(slot0_raw, slot1_raw))
    }

    /// Decode an int128 from raw storage bytes (big-endian, right-aligned).
    #[allow(dead_code)]
    fn decode_i128_from_be_bytes(bytes: &[u8]) -> i128 {
        let mut buf = [0u8; 16];
        let start = bytes.len().saturating_sub(16);
        let copy_len = bytes.len().min(16);
        buf[16 - copy_len..].copy_from_slice(&bytes[start..start + copy_len]);
        i128::from_be_bytes(buf)
    }

    /// Process a list of executed logs from a single transaction, updating pool state
    /// for any Swap or Sync events emitted by tracked pools.
    pub fn update_from_logs(&mut self, logs: &[ExecutedLog]) {
        for log in logs {
            if log.topics.is_empty() {
                continue;
            }
            if !self.known_set.contains(&log.address) {
                continue;
            }
            let topic0 = log.topics[0];

            // V2 Swap
            if topic0 == SWAP_TOPIC {
                self.process_v2_swap_log(log);
                continue;
            }
            // V2 Sync
            if topic0 == SYNC_TOPIC {
                self.process_v2_sync_log(log);
                continue;
            }
            // V3 Swap
            if topic0 == decoders::V3_SWAP_TOPIC {
                self.process_v3_swap_log(log);
                continue;
            }
            // V3 Mint/Burn
            if topic0 == *decoders::V3_MINT_TOPIC || topic0 == decoders::V3_BURN_TOPIC {
                self.process_v3_mint_burn_log(log);
                continue;
            }
            // Curve TokenExchange
            if topic0 == *decoders::CURVE_TOKEN_EXCHANGE_TOPIC
                || topic0 == *decoders::CURVE_V2_TOKEN_EXCHANGE_TOPIC
            {
                self.process_curve_swap_log(log);
                continue;
            }
            // Balancer Swap
            if topic0 == *decoders::BALANCER_SWAP_TOPIC {
                self.process_balancer_swap_log(log);
                continue;
            }
        }
    }

    fn process_v2_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if log.data.len() < 128 {
            return;
        }
        let amt0_in = u128_from_be_bytes(&log.data[..32]);
        let amt1_in = u128_from_be_bytes(&log.data[32..64]);
        let amt0_out = u128_from_be_bytes(&log.data[64..96]);
        let amt1_out = u128_from_be_bytes(&log.data[96..128]);
        self.apply_v2_swap(&log.address, amt0_in, amt1_in, amt0_out, amt1_out);
    }

    fn process_v2_sync_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if log.data.len() < 64 {
            return;
        }
        let r0 = u128_from_be_bytes(&log.data[..32]);
        let r1 = u128_from_be_bytes(&log.data[32..64]);
        self.apply_v2_sync(&log.address, r0, r1);
    }

    fn process_v3_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_v3_swap(log) {
            self.apply_v3_swap(
                &log.address,
                decoded.sqrt_price_x96,
                decoded.tick,
                decoded.liquidity,
                decoded.amount0,
                decoded.amount1,
            );
        }
    }

    fn process_v3_mint_burn_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_v3_mint_burn(log) {
            self.apply_v3_mint_burn(
                &log.address,
                decoded.tick_lower,
                decoded.tick_upper,
                decoded.amount,
            );
        }
    }

    fn process_curve_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_curve_swap(log) {
            if let Some(PoolState::Curve(state)) = self.pools.get_mut(&log.address) {
                let coin_sold = decoded.coin_sold as usize;
                let coin_bought = decoded.coin_bought as usize;
                if coin_sold < state.balances.len() && coin_bought < state.balances.len() {
                    state.balances[coin_sold] =
                        state.balances[coin_sold].saturating_add(decoded.amount_sold);
                    state.balances[coin_bought] =
                        state.balances[coin_bought].saturating_sub(decoded.amount_bought);
                }
            }
        }
    }

    fn process_balancer_swap_log(&mut self, log: &ExecutedLog) {
        if !self.pools.contains_key(&log.address) {
            return;
        }
        if let Some(decoded) = decoders::decode_balancer_swap(log) {
            if let Some(PoolState::Balancer(state)) = self.pools.get_mut(&log.address) {
                let idx_in = state.token_index.get(&decoded.token_in);
                let idx_out = state.token_index.get(&decoded.token_out);
                if let (Some(&i_in), Some(&i_out)) = (idx_in, idx_out) {
                    if i_in < state.balances.len() && i_out < state.balances.len() {
                        state.balances[i_in] =
                            state.balances[i_in].saturating_add(decoded.amount_in);
                        state.balances[i_out] =
                            state.balances[i_out].saturating_sub(decoded.amount_out);
                    }
                }
            }
        }
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

    /// Fetch Balancer V2 pool state (tokens + balances) from the vault,
    /// plus weights and swap fee from the pool contract.
    async fn fetch_balancer_state(
        rpc: &RpcClient,
        vault: Address,
        pool: Address,
        pool_id: &[u8; 32],
        block: u64,
    ) -> anyhow::Result<PoolInitResult> {
        // --- Step 1: getPoolTokens from vault ---
        let data = {
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&GET_POOL_TOKENS_SELECTOR);
            calldata.extend_from_slice(pool_id);
            Bytes::from(calldata)
        };

        let result = rpc.call(vault, data, block).await?;
        let return_data = result.0;
        if return_data.len() < 96 {
            anyhow::bail!("Balancer getPoolTokens returned too short data");
        }

        // ABI decode: (address[], uint256[], uint256)
        let tokens_offset = U256::from_be_slice(&return_data[..32]);
        let balances_offset = U256::from_be_slice(&return_data[32..64]);
        let tokens_len_offset = 32 + tokens_offset.as_limbs()[0] as usize;
        let token_count = U256::from_be_slice(&return_data[tokens_len_offset..tokens_len_offset + 32]);
        let token_count = token_count.as_limbs()[0] as usize;

        let tokens_start = tokens_len_offset + 32;
        let mut tokens = Vec::with_capacity(token_count);
        for i in 0..token_count {
            let off = tokens_start + i * 32;
            let addr = Address::from_slice(&return_data[off + 12..off + 32]);
            tokens.push(addr);
        }

        let balances_start = 64 + balances_offset.as_limbs()[0] as usize + 32;
        let mut balances = Vec::with_capacity(token_count);
        for i in 0..token_count {
            let off = balances_start + i * 32;
            let bal = U256::from_be_slice(&return_data[off..off + 32]);
            balances.push(bal.as_limbs()[0] as u128);
        }

        if balances.len() < 2 {
            anyhow::bail!("Balancer pool has fewer than 2 tokens");
        }

        // --- Step 2: Fetch normalized weights from pool ---
        let weights = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&GET_NORMALIZED_WEIGHTS_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    let w_off = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as usize;
                    let w_count = if w_off + 32 <= result.0.len() {
                        U256::from_be_slice(&result.0[w_off..w_off + 32]).as_limbs()[0] as usize
                    } else {
                        0
                    };
                    let w_start = w_off + 32;
                    let mut w = Vec::with_capacity(w_count);
                    for j in 0..w_count {
                        let off = w_start + j * 32;
                        if off + 32 <= result.0.len() {
                            w.push(U256::from_be_slice(&result.0[off..off + 32]).as_limbs()[0] as u128);
                        }
                    }
                    w
                }
                _ => vec![],
            }
        };

        // --- Step 3: Fetch swap fee percentage from pool ---
        let fee_bps = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&GET_SWAP_FEE_PERCENTAGE_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    let chain_fee = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128;
                    // Balancer returns fee in 1e18 scale; convert to PoolInfo bps (1e6 = 100%)
                    (chain_fee / 1_000_000_000_000) as u32
                }
                _ => 0,
            }
        };

        // --- Step 4: Fetch scaling factors from pool ---
        let scaling_factors = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&GET_SCALING_FACTORS_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 64 => {
                    let off = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as usize;
                    let count = if off + 32 <= result.0.len() {
                        U256::from_be_slice(&result.0[off..off + 32]).as_limbs()[0] as usize
                    } else { 0 };
                    let start = off + 32;
                    let mut sf = Vec::with_capacity(count);
                    for j in 0..count {
                        let pos = start + j * 32;
                        if pos + 32 <= result.0.len() {
                            sf.push(U256::from_be_slice(&result.0[pos..pos + 32]).as_limbs()[0] as u128);
                        }
                    }
                    sf
                }
                _ => vec![],
            }
        };

        // --- Step 5: Determine pool variant, amplification, and BPT index ---
        let (variant, amplification, bpt_index) = if !weights.is_empty() {
            // Has normalized weights → Weighted pool
            (BalancerPoolVariant::Weighted, None, None)
        } else {
            // No weights — try amplification parameter to detect Stable pool
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&GET_AMPLIFICATION_PARAMETER_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 96 => {
                    let value = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128;
                    let precision = U256::from_be_slice(&result.0[64..96]).as_limbs()[0] as u128;
                    let effective_a = if precision > 0 { value / precision } else { value };

                    // Detect ComposableStable: has scaling factors and one token matches pool address
                    let bpt_idx = if scaling_factors.len() == token_count {
                        tokens.iter().position(|t| *t == pool)
                    } else {
                        None
                    };

                    let variant = if bpt_idx.is_some() {
                        BalancerPoolVariant::ComposableStable
                    } else {
                        BalancerPoolVariant::Stable
                    };

                    (variant, Some(effective_a), bpt_idx)
                }
                _ => (BalancerPoolVariant::Other, None, None),
            }
        };

        Ok(PoolInitResult::BalancerState(tokens, balances, weights, fee_bps, variant, amplification, scaling_factors, bpt_index))
    }

    /// Re-fetch a pool's on-chain state at a given block number (M3 fact-check support).
    ///
    /// Returns a fresh `PoolState` with data read directly from the chain via `eth_call`,
    /// bypassing the in-memory state. This is used by the EVM-based fact-check to verify
    /// opportunities against the actual on-chain state rather than the potentially-diverged
    /// cached state.
    pub async fn refetch_pool_state(
        &self,
        rpc: &RpcClient,
        addr: &Address,
        block: u64,
    ) -> Option<PoolState> {
        let pool = self.pools.get(addr)?;
        match pool {
            PoolState::UniswapV2(v2) => {
                let (r0, r1) = Self::fetch_v2_reserves(rpc, *addr, block, v2.info.factory).await?;
                Some(PoolState::UniswapV2(UniswapV2PoolState {
                    info: v2.info.clone(),
                    reserve0: r0,
                    reserve1: r1,
                }))
            }
            PoolState::UniswapV3(v3) => {
                let spacing = v3.info.tick_spacing.unwrap_or(60) as i32;
                let (sqrt, tick, liq, ticks) =
                    Self::fetch_v3_state(rpc, *addr, block, spacing).await?;
                Some(PoolState::UniswapV3(UniswapV3PoolState {
                    info: v3.info.clone(),
                    sqrt_price_x96: sqrt,
                    tick,
                    liquidity: liq,
                    ticks,
                    fee_growth_global_0_x128: U256::ZERO,
                    fee_growth_global_1_x128: U256::ZERO,
                }))
            }
            PoolState::Curve(curve) => {
                let result = Self::fetch_curve_state(rpc, *addr, block).await?;
                match result {
                    PoolInitResult::CurveState(tokens, balances, a_coeff, fee_bps, variant, gamma, price_scale, base_pool) => {
                        let token_index: HashMap<Address, usize> = tokens
                            .iter()
                            .enumerate()
                            .map(|(i, t)| (*t, i))
                            .collect();
                        let mut info = curve.info.clone();
                        info.fee = fee_bps;
                        Some(PoolState::Curve(CurvePoolState {
                            info,
                            balances,
                            token_index,
                            a_coeff,
                            pool_variant: variant,
                            gamma,
                            price_scale,
                            base_pool,
                        }))
                    }
                    _ => None,
                }
            }
            PoolState::Balancer(bal) => {
                let vault = self.balancer_vault?;
                let pool_id = bal.pool_id?;
                let result = Self::fetch_balancer_state(rpc, vault, *addr, &pool_id, block).await.ok()?;
                match result {
                    PoolInitResult::BalancerState(tokens, balances, weights, fee_bps, variant, amplification, scaling_factors, bpt_index) => {
                        let token_index: HashMap<Address, usize> = tokens
                            .iter()
                            .enumerate()
                            .map(|(i, t)| (*t, i))
                            .collect();
                        let mut info = bal.info.clone();
                        info.fee = fee_bps;
                        Some(PoolState::Balancer(BalancerPoolState {
                            info,
                            balances,
                            token_index,
                            pool_id: Some(pool_id),
                            weights,
                            pool_variant: variant,
                            amplification,
                            scaling_factors,
                            bpt_index,
                        }))
                    }
                    _ => None,
                }
            }
        }
    }

    /// Fetch Curve pool state by calling `coins(int128)` and `balances(int128)` for each token index.
    /// Tries up to 16 token indices, stopping when a call returns zero address or fails.
    /// Detects pool variant (Plain/Meta/Crypto) and fetches variant-specific state.
    async fn fetch_curve_state(
        rpc: &RpcClient,
        pool: Address,
        block: u64,
    ) -> Option<PoolInitResult> {
        static CURVE_COINS_SELECTOR: [u8; 4] = [0xc6, 0x61, 0x1f, 0x94]; // coins(int128)
        static CURVE_COINS_U256_SELECTOR: [u8; 4] = [0x19, 0x6c, 0xac, 0x5f]; // coins(uint256) — used by some forks
        let mut tokens = Vec::new();
        let mut balances = Vec::new();
        let max_tokens = 16u8;

        for i in 0u8..max_tokens {
            let token = {
                let mut calldata = Vec::with_capacity(36);
                calldata.extend_from_slice(&CURVE_COINS_SELECTOR);
                let mut arg = [0u8; 32];
                arg[31] = i;
                calldata.extend_from_slice(&arg);
                match rpc.call(pool, Bytes::from(calldata), block).await {
                    Ok(result) if result.0.len() >= 32 => {
                        let addr = Address::from_slice(&result.0[12..32]);
                        if addr.is_zero() { break; }
                        addr
                    }
                    _ => {
                        let mut calldata2 = Vec::with_capacity(36);
                        calldata2.extend_from_slice(&CURVE_COINS_U256_SELECTOR);
                        let mut arg2 = [0u8; 32];
                        arg2[31] = i;
                        calldata2.extend_from_slice(&arg2);
                        match rpc.call(pool, Bytes::from(calldata2), block).await {
                            Ok(result) if result.0.len() >= 32 => {
                                let addr = Address::from_slice(&result.0[12..32]);
                                if addr.is_zero() { break; }
                                addr
                            }
                            _ => break,
                        }
                    }
                }
            };

            let balance = {
                let mut calldata = Vec::with_capacity(36);
                calldata.extend_from_slice(&CURVE_BALANCES_SELECTOR);
                let mut arg = [0u8; 32];
                arg[31] = i;
                calldata.extend_from_slice(&arg);
                match rpc.call(pool, Bytes::from(calldata), block).await {
                    Ok(result) if result.0.len() >= 32 => {
                        U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128
                    }
                    _ => break,
                }
            };

            tokens.push(token);
            balances.push(balance);
        }

        if tokens.len() < 2 {
            return None;
        }

        // Fetch amplification coefficient A() / get_A()
        let a_coeff = {
            // Try get_A() first (CryptoSwap V2), fallback to A() (StableSwap V1)
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&CURVE_GET_A_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128
                }
                _ => {
                    // Fallback to A()
                    let mut calldata2 = Vec::with_capacity(4);
                    calldata2.extend_from_slice(&CURVE_A_SELECTOR);
                    match rpc.call(pool, Bytes::from(calldata2), block).await {
                        Ok(result) if result.0.len() >= 32 => {
                            U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128
                        }
                        _ => 100,
                    }
                }
            }
        };

        // Fetch swap fee fee()
        let fee_bps = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&CURVE_FEE_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    let chain_fee = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128;
                    (chain_fee / 10_000) as u32
                }
                _ => 0,
            }
        };

        // --- Variant detection ---
        // Try gamma() — only CryptoSwap V2 pools have this
        let is_crypto = {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&CURVE_GAMMA_SELECTOR);
            matches!(rpc.call(pool, Bytes::from(calldata), block).await, Ok(r) if r.0.len() >= 32 && !r.0[..32].iter().all(|&b| b == 0))
        };

        // Try base_pool() — only Metapools have this
        let base_pool = if !is_crypto {
            let mut calldata = Vec::with_capacity(4);
            calldata.extend_from_slice(&CURVE_BASE_POOL_SELECTOR);
            match rpc.call(pool, Bytes::from(calldata), block).await {
                Ok(result) if result.0.len() >= 32 => {
                    let addr = Address::from_slice(&result.0[12..32]);
                    if !addr.is_zero() { Some(addr) } else { None }
                }
                _ => None,
            }
        } else {
            None
        };

        // Fetch variant-specific fields
        let (variant, gamma, price_scale) = if is_crypto {
            // Gamma
            let gamma_val = {
                let mut calldata = Vec::with_capacity(4);
                calldata.extend_from_slice(&CURVE_GAMMA_SELECTOR);
                match rpc.call(pool, Bytes::from(calldata), block).await {
                    Ok(result) if result.0.len() >= 32 => {
                        Some(U256::from_be_slice(&result.0[..32]).as_limbs()[0] as u128)
                    }
                    _ => None,
                }
            };

            // Price scale — returns a dynamic array of N-1 values
            let price_scales = {
                let mut calldata = Vec::with_capacity(4);
                calldata.extend_from_slice(&CURVE_PRICE_SCALE_SELECTOR);
                match rpc.call(pool, Bytes::from(calldata), block).await {
                    Ok(result) if result.0.len() >= 64 => {
                        let off = U256::from_be_slice(&result.0[..32]).as_limbs()[0] as usize;
                        let count = if off + 32 <= result.0.len() {
                            U256::from_be_slice(&result.0[off..off + 32]).as_limbs()[0] as usize
                        } else { 0 };
                        let start = off + 32;
                        let mut scales = Vec::with_capacity(count);
                        for j in 0..count {
                            let pos = start + j * 32;
                            if pos + 32 <= result.0.len() {
                                scales.push(U256::from_be_slice(&result.0[pos..pos + 32]).as_limbs()[0] as u128);
                            }
                        }
                        scales
                    }
                    _ => vec![],
                }
            };

            (CurvePoolVariant::Crypto, gamma_val, price_scales)
        } else if base_pool.is_some() {
            (CurvePoolVariant::Meta, None, vec![])
        } else {
            (CurvePoolVariant::Plain, None, vec![])
        };

        Some(PoolInitResult::CurveState(tokens, balances, a_coeff, fee_bps, variant, gamma, price_scale, base_pool))
    }
}

impl Clone for PoolManager {
    fn clone(&self) -> Self {
        let cache = self.pairs_cache.lock().unwrap().clone();
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Address, B256, U256};
    use crate::data::ExecutedLog;
    use crate::pool::decoders::V3_SWAP_TOPIC;

    fn wmatic() -> Address {
        address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270")
    }
    fn usdc() -> Address {
        address!("2791bca1f2de4661ed88a30c99a7a9449aa84174")
    }
    fn usdt() -> Address {
        address!("c2132d05d31c914a87c6611c10748aeb04b58e8f")
    }

    fn make_v2_pool(addr: Address, t0: Address, t1: Address, r0: u128, r1: u128) -> PoolState {
        PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: addr,
                token0: t0,
                token1: t1,
                fee: 30,
                name: None,
                dex_type: DexType::UniswapV2,
                tick_spacing: None,
                creation_block: 0,
                pool_id: None,
                factory: None,
            },
            reserve0: r0,
            reserve1: r1,
        })
    }

    fn make_v3_pool(addr: Address, t0: Address, t1: Address, sqrt: U256, tick: i32, liq: u128) -> PoolState {
        PoolState::UniswapV3(UniswapV3PoolState {
            info: PoolInfo {
                address: addr,
                token0: t0,
                token1: t1,
                fee: 30,
                name: None,
                dex_type: DexType::UniswapV3,
                tick_spacing: Some(60),
                creation_block: 0,
                pool_id: None,
                factory: None,
            },
            sqrt_price_x96: sqrt,
            tick,
            liquidity: liq,
            ticks: std::collections::BTreeMap::new(),
            fee_growth_global_0_x128: U256::ZERO,
            fee_growth_global_1_x128: U256::ZERO,
        })
    }

    fn encode_u256(value: u128) -> [u8; 32] {
        let mut buf = [0u8; 32];
        let be = value.to_be_bytes();
        buf[16..32].copy_from_slice(&be);
        buf
    }

    fn encode_i32_right(value: i32) -> [u8; 32] {
        let mut buf = [0u8; 32];
        let be = value.to_be_bytes();
        buf[28..32].copy_from_slice(&be);
        buf
    }

    // ---- PoolManager creation & management ----

    #[test]
    fn test_pm_new_and_empty() {
        let pm = PoolManager::new();
        assert_eq!(pm.pool_count(), 0);
        assert!(pm.pool_addresses().is_empty());
        assert!(pm.token_addresses().is_empty());
    }

    #[test]
    fn test_pm_add_and_get_pool() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        assert_eq!(pm.pool_count(), 1);
        let p = pm.get(&addr);
        assert!(p.is_some());
        assert_eq!(p.unwrap().address(), addr);
    }

    #[test]
    fn test_pm_get_mut_updates_reserves() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        if let Some(PoolState::UniswapV2(s)) = pm.get_mut(&addr) {
            s.reserve0 = 999;
        }
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 999);
        } else {
            panic!("expected V2 pool");
        }
    }

    #[test]
    fn test_pm_pool_addresses() {
        let mut pm = PoolManager::new();
        let a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        pm.add_pool(make_v2_pool(a, usdc(), wmatic(), 1, 1));
        pm.add_pool(make_v2_pool(b, usdt(), wmatic(), 1, 1));
        let addrs = pm.pool_addresses();
        assert_eq!(addrs.len(), 2);
        assert!(addrs.contains(&a));
        assert!(addrs.contains(&b));
    }

    #[test]
    fn test_pm_token_addresses_and_pools_for_token() {
        let mut pm = PoolManager::new();
        let p1 = address!("1111111111111111111111111111111111111111");
        let p2 = address!("2222222222222222222222222222222222222222");
        pm.add_pool(make_v2_pool(p1, usdc(), wmatic(), 1, 1));
        pm.add_pool(make_v2_pool(p2, usdt(), wmatic(), 1, 1));
        let tokens = pm.token_addresses();
        assert_eq!(tokens.len(), 3);
        assert!(tokens.contains(&wmatic()));
        let wmatic_pools = pm.pools_for_token(&wmatic()).unwrap();
        assert_eq!(wmatic_pools.len(), 2);
    }

    #[test]
    fn test_pm_find_pair_pool() {
        let mut pm = PoolManager::new();
        let p1 = address!("1111111111111111111111111111111111111111");
        pm.add_pool(make_v2_pool(p1, usdc(), wmatic(), 1, 1));
        assert_eq!(pm.find_pair_pool(&usdc(), &wmatic()), Some(p1));
        assert!(pm.find_pair_pool(&usdc(), &usdt()).is_none());
    }

    #[test]
    fn test_pm_arbitrage_pairs_caching() {
        let mut pm = PoolManager::new();
        let a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        pm.add_pool(make_v2_pool(a, usdc(), wmatic(), 1, 1));
        pm.add_pool(make_v2_pool(b, usdt(), wmatic(), 1, 1));
        let pairs = pm.arbitrage_pairs();
        assert_eq!(pairs.len(), 1);
        let pairs2 = pm.arbitrage_pairs();
        assert_eq!(pairs, pairs2);
        let c = address!("cccccccccccccccccccccccccccccccccccccccc");
        pm.add_pool(make_v2_pool(c, usdc(), usdt(), 1, 1));
        assert_eq!(pm.arbitrage_pairs().len(), 3);
    }

    // ---- V2 state updates ----

    #[test]
    fn test_pm_apply_v2_swap() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        pm.apply_v2_swap(&addr, 100, 0, 0, 50);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1100);
            assert_eq!(s.reserve1, 1950);
        }
    }

    #[test]
    fn test_pm_apply_v2_sync() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        pm.apply_v2_sync(&addr, 999, 1999);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 999);
            assert_eq!(s.reserve1, 1999);
        }
    }

    #[test]
    fn test_pm_apply_v2_swap_unknown_address_noop() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let unknown = address!("ffffffffffffffffffffffffffffffffffffffff");
        pm.apply_v2_swap(&unknown, 100, 0, 0, 50);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1000);
        }
    }

    // ---- V3 state updates ----

    #[test]
    fn test_pm_apply_v3_swap() {
        let mut pm = PoolManager::new();
        let addr = address!("3333333333333333333333333333333333333333");
        pm.add_pool(make_v3_pool(addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1_000_000));
        pm.apply_v3_swap(&addr, U256::from(2u128 << 96), 10, 999_000, 1000, -1000);
        if let Some(PoolState::UniswapV3(s)) = pm.get(&addr) {
            assert_eq!(s.sqrt_price_x96, U256::from(2u128 << 96));
            assert_eq!(s.tick, 10);
            assert_eq!(s.liquidity, 999_000);
        }
    }

    #[test]
    fn test_pm_apply_v3_mint_burn_add() {
        let mut pm = PoolManager::new();
        let addr = address!("3333333333333333333333333333333333333333");
        pm.add_pool(make_v3_pool(addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1_000_000));
        pm.apply_v3_mint_burn(&addr, -100, 100, 500_000);
        if let Some(PoolState::UniswapV3(s)) = pm.get(&addr) {
            assert_eq!(s.liquidity, 1_500_000);
            assert_eq!(*s.ticks.get(&-100).unwrap(), 500_000);
            assert_eq!(*s.ticks.get(&100).unwrap(), -500_000);
        }
    }

    #[test]
    fn test_pm_apply_v3_mint_burn_remove() {
        let mut pm = PoolManager::new();
        let addr = address!("3333333333333333333333333333333333333333");
        pm.add_pool(make_v3_pool(addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1_000_000));
        pm.apply_v3_mint_burn(&addr, -100, 100, -200_000);
        if let Some(PoolState::UniswapV3(s)) = pm.get(&addr) {
            assert_eq!(s.liquidity, 800_000);
            assert_eq!(*s.ticks.get(&-100).unwrap(), -200_000);
            assert_eq!(*s.ticks.get(&100).unwrap(), 200_000);
        }
    }

    // ---- initialized_count ----

    #[test]
    fn test_pm_initialized_count() {
        let mut pm = PoolManager::new();
        let a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let c = address!("cccccccccccccccccccccccccccccccccccccccc");
        pm.add_pool(make_v2_pool(a, usdc(), wmatic(), 1000, 2000));
        pm.add_pool(make_v2_pool(b, usdt(), wmatic(), 0, 0));
        pm.add_pool(make_v3_pool(c, wmatic(), usdt(), U256::ZERO, 0, 0));
        assert_eq!(pm.initialized_count(), 1);
    }

    // ---- update_from_logs ----

    fn make_v2_swap_log(pool: Address, amt0_in: u128, amt1_in: u128, amt0_out: u128, amt1_out: u128) -> ExecutedLog {
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&encode_u256(amt0_in));
        data.extend_from_slice(&encode_u256(amt1_in));
        data.extend_from_slice(&encode_u256(amt0_out));
        data.extend_from_slice(&encode_u256(amt1_out));
        ExecutedLog {
            address: pool,
            topics: vec![SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data: data.into(),
        }
    }

    fn make_v2_sync_log(pool: Address, r0: u128, r1: u128) -> ExecutedLog {
        let mut data = Vec::with_capacity(64);
        data.extend_from_slice(&encode_u256(r0));
        data.extend_from_slice(&encode_u256(r1));
        ExecutedLog {
            address: pool,
            topics: vec![SYNC_TOPIC],
            data: data.into(),
        }
    }

    fn make_v3_swap_log(pool: Address, sqrt: U256, liq: u128, tick: i32) -> ExecutedLog {
        let mut data = Vec::with_capacity(160);
        // amount0 and amount1 come before sqrt/liq/tick in the V3 Swap event data
        data.extend_from_slice(&[0u8; 32]); // amount0
        data.extend_from_slice(&[0u8; 32]); // amount1
        let mut b = [0u8; 32];
        b.copy_from_slice(&sqrt.to_be_bytes::<32>());
        data.extend_from_slice(&b);
        data.extend_from_slice(&encode_u256(liq));
        data.extend_from_slice(&encode_i32_right(tick));
        ExecutedLog {
            address: pool,
            topics: vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data: data.into(),
        }
    }

    #[test]
    fn test_pm_update_from_logs_v2_swap() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let log = make_v2_swap_log(addr, 100, 0, 0, 50);
        pm.update_from_logs(&[log]);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1100);
            assert_eq!(s.reserve1, 1950);
        }
    }

    #[test]
    fn test_pm_update_from_logs_v2_sync() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let log = make_v2_sync_log(addr, 555, 666);
        pm.update_from_logs(&[log]);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 555);
            assert_eq!(s.reserve1, 666);
        }
    }

    #[test]
    fn test_pm_update_from_logs_v3_swap() {
        let mut pm = PoolManager::new();
        let addr = address!("3333333333333333333333333333333333333333");
        pm.add_pool(make_v3_pool(addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1_000_000));
        let new_sqrt = U256::from(15u128 << 96) / U256::from(10);
        let log = make_v3_swap_log(addr, new_sqrt, 999_000, 5);
        pm.update_from_logs(&[log]);
        if let Some(PoolState::UniswapV3(s)) = pm.get(&addr) {
            assert_eq!(s.sqrt_price_x96, new_sqrt);
            assert_eq!(s.liquidity, 999_000);
            assert_eq!(s.tick, 5);
        }
    }

    #[test]
    fn test_pm_update_from_logs_ignores_untracked_pool() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let unknown = address!("ffffffffffffffffffffffffffffffffffffffff");
        pm.update_from_logs(&[make_v2_swap_log(unknown, 100, 0, 0, 50)]);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1000);
        }
    }

    #[test]
    fn test_pm_update_from_logs_empty_topics_skipped() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        let log = ExecutedLog {
            address: addr,
            topics: vec![],
            data: Default::default(),
        };
        pm.update_from_logs(&[log]);
        if let Some(PoolState::UniswapV2(s)) = pm.get(&addr) {
            assert_eq!(s.reserve0, 1000);
        }
    }

    // ---- PoolState helpers ----

    #[test]
    fn test_pool_info_is_concentrated_liquidity() {
        let a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let v2 = make_v2_pool(a, usdc(), wmatic(), 1, 1);
        assert!(!v2.info().is_concentrated_liquidity());
        let v3_addr = address!("3333333333333333333333333333333333333333");
        let v3 = make_v3_pool(v3_addr, wmatic(), usdc(), U256::from(1u128 << 96), 0, 1);
        assert!(v3.info().is_concentrated_liquidity());
    }

    // ---- PoolState::info / info_mut ----

    #[test]
    fn test_pool_state_info_mut() {
        let mut pm = PoolManager::new();
        let addr = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        pm.add_pool(make_v2_pool(addr, usdc(), wmatic(), 1000, 2000));
        pm.get_mut(&addr).unwrap().info_mut().fee = 100;
        assert_eq!(pm.get(&addr).unwrap().info().fee, 100);
    }

    // ---- decode_v2_reserves_from_storage ----

    fn make_v2_storage_raw(reserve0: u128, reserve1: u128, block_ts: u32) -> U256 {
        let mut bytes = [0u8; 32];
        bytes[0..4].copy_from_slice(&block_ts.to_be_bytes());
        let r1_be = reserve1.to_be_bytes();
        bytes[4..18].copy_from_slice(&r1_be[2..]);
        let r0_be = reserve0.to_be_bytes();
        bytes[18..32].copy_from_slice(&r0_be[2..]);
        U256::from_be_bytes(bytes)
    }

    #[test]
    fn test_decode_v2_small_reserves() {
        let raw = make_v2_storage_raw(1, 2, 0);
        let (r0, r1) = PoolManager::decode_v2_reserves_from_storage(raw);
        assert_eq!(r0, 1);
        assert_eq!(r1, 2);
    }

    #[test]
    fn test_decode_v2_max_reserves() {
        let max = (1u128 << 112) - 1;
        let raw = make_v2_storage_raw(max, max, 0);
        let (r0, r1) = PoolManager::decode_v2_reserves_from_storage(raw);
        assert_eq!(r0, max);
        assert_eq!(r1, max);
    }

    #[test]
    fn test_decode_v2_zero_reserves() {
        let raw = make_v2_storage_raw(0, 0, 0);
        let (r0, r1) = PoolManager::decode_v2_reserves_from_storage(raw);
        assert_eq!(r0, 0);
        assert_eq!(r1, 0);
    }

    // ---- decode_v3_state_from_storage ----

    fn make_v3_slot0_raw(sqrt_price_x96: U256, tick: i32) -> U256 {
        let mut bytes = [0u8; 32];
        let sqrt_be = sqrt_price_x96.to_be_bytes::<32>();
        bytes[12..32].copy_from_slice(&sqrt_be[12..32]);
        let tick_u24 = (tick as u32) & 0xFFFFFF;
        let tick_be = tick_u24.to_be_bytes();
        bytes[9] = tick_be[1];
        bytes[10] = tick_be[2];
        bytes[11] = tick_be[3];
        U256::from_be_bytes(bytes)
    }

    #[test]
    fn test_decode_v3_minimal() {
        let slot0 = make_v3_slot0_raw(U256::from(1u128), 0);
        let slot1 = U256::ZERO;
        let (sqrt, tick, liq) = PoolManager::decode_v3_state_from_storage(slot0, slot1);
        assert_eq!(sqrt, U256::from(1u128));
        assert_eq!(tick, 0);
        assert_eq!(liq, 0);
    }

    #[test]
    fn test_decode_v3_negative_tick() {
        let slot0 = make_v3_slot0_raw(U256::from(1u128), -10);
        let slot1 = U256::ZERO;
        let (_sqrt, tick, _liq) = PoolManager::decode_v3_state_from_storage(slot0, slot1);
        assert_eq!(tick, -10);
    }

    #[test]
    fn test_decode_v3_typical() {
        let sqrt = U256::from(1234567890u128) << 64;
        let slot0 = make_v3_slot0_raw(sqrt, 50000);
        let slot1 = U256::from(1_000_000_000u128);
        let (sqrt_out, tick_out, liq_out) = PoolManager::decode_v3_state_from_storage(slot0, slot1);
        assert_eq!(sqrt_out, sqrt);
        assert_eq!(tick_out, 50000);
        assert_eq!(liq_out, 1_000_000_000);
    }

    #[test]
    fn test_decode_v3_tick_min_int24() {
        // int24 minimum: -8,388,608 (0x800000 as 24-bit two's complement)
        let slot0 = make_v3_slot0_raw(U256::from(1u128), -8388608);
        let (_sqrt, tick, _liq) = PoolManager::decode_v3_state_from_storage(slot0, U256::ZERO);
        assert_eq!(tick, -8388608);
    }

    #[test]
    fn test_decode_v3_tick_max_int24() {
        // int24 maximum: 8,388,607 (0x7FFFFF)
        let slot0 = make_v3_slot0_raw(U256::from(1u128), 8388607);
        let (_sqrt, tick, _liq) = PoolManager::decode_v3_state_from_storage(slot0, U256::ZERO);
        assert_eq!(tick, 8388607);
    }

    // ---- PoolManager helper methods ----

    #[test]
    fn test_is_wrapped_native() {
        let wmatic = address!("0x0d500b1d8e8ef31e21c99d1db9a6444d3adf1270");
        let pm = PoolManager::new().with_wrapped_native(wmatic);
        assert!(pm.is_wrapped_native(&wmatic));
        assert!(!pm.is_wrapped_native(&Address::ZERO));
    }

    #[test]
    fn test_get_v2_state_returns_none_for_missing() {
        let pm = PoolManager::new();
        assert!(pm.get_v2_state(&Address::ZERO).is_none());
    }

    #[test]
    fn test_with_wrapped_native_sets_field() {
        let weth = address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let pm = PoolManager::new().with_wrapped_native(weth);
        assert_eq!(pm.wrapped_native, Some(weth));
    }
}
