use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::LazyLock;
use alloy::primitives::{address, Address, U256};
use serde::{Deserialize, Serialize};
use crate::pool::dex_type::DexType;

/// Known fee-on-transfer (FoT) tokens — mainnet addresses.
/// These tokens deduct a fee on every transfer, causing reserve divergences in V2 pools.
static FOT_TOKENS: LazyLock<HashSet<Address>> = LazyLock::new(|| {
    let mut s = HashSet::new();
    // USDT (Ethereum) — fee on transfer for non-exchange addresses
    s.insert(address!("dac17f958d2ee523a2206206994597c13d831ec7"));
    // SafeMoon
    s.insert(address!("80860b64f856deade4d8f1e0103500207c12ff0f"));
    // PAXG (Pax Gold)
    s.insert(address!("45804880de22913dafe09f4980848ecece09f8fc"));
    s
});

/// Known rebase tokens — mainnet addresses.
/// These tokens periodically adjust balances (rebasing), so balanceOf > reserve.
static REBASE_TOKENS: LazyLock<HashSet<Address>> = LazyLock::new(|| {
    let mut s = HashSet::new();
    // AMPL (Ampleforth)
    s.insert(address!("d46ba6d942050d489dbd938a2c909a5d5039a161"));
    // stETH (Lido)
    s.insert(address!("ae7ab96520de3a18e5e111b5eaab095312d7fe84"));
    // reth (Rocket Pool)
    s.insert(address!("ae78736cd615f374d3085123a210448e74fc6393"));
    // cbETH (Coinbase)
    s.insert(address!("be9895146f7af43049ca1c1ae358b0541ea49704"));
    s
});

/// Returns true if the given token is a known fee-on-transfer token.
pub fn is_fee_on_transfer_token(token: &Address) -> bool {
    FOT_TOKENS.contains(token)
}

/// Returns true if the given token is a known rebase token.
pub fn is_rebase_token(token: &Address) -> bool {
    REBASE_TOKENS.contains(token)
}

/// Static pool information loaded from the discovery cache (on-chain or Dune).
///
/// `PoolInfo` is deserialized from the discovery cache (SQLite) after
/// pools are discovered via on-chain eth_getLogs or Dune Analytics.
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
    /// Whether the pool is a stable-swap pool (Solidly/Camelot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_stable: Option<bool>,
    /// Whether either token in the pool is a fee-on-transfer token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_fot: Option<bool>,
    /// Whether either token in the pool is a rebase token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_rebase: Option<bool>,
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
            is_stable: None,
            is_fot: None,
            is_rebase: None,
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
    /// DODO pools (no MEV detection support — passive discovery only)
    Dodo(PoolInfo),
    /// Clipper pools (no MEV detection support — passive discovery only)
    Clipper(PoolInfo),
}

impl PoolState {
    pub fn address(&self) -> Address {
        match self {
            PoolState::UniswapV2(s) => s.info.address,
            PoolState::UniswapV3(s) => s.info.address,
            PoolState::Curve(s) => s.info.address,
            PoolState::Balancer(s) => s.info.address,
            PoolState::Dodo(s) => s.address,
            PoolState::Clipper(s) => s.address,
        }
    }

    pub fn info(&self) -> &PoolInfo {
        match self {
            PoolState::UniswapV2(s) => &s.info,
            PoolState::UniswapV3(s) => &s.info,
            PoolState::Curve(s) => &s.info,
            PoolState::Balancer(s) => &s.info,
            PoolState::Dodo(s) => s,
            PoolState::Clipper(s) => s,
        }
    }

    pub fn info_mut(&mut self) -> &mut PoolInfo {
        match self {
            PoolState::UniswapV2(s) => &mut s.info,
            PoolState::UniswapV3(s) => &mut s.info,
            PoolState::Curve(s) => &mut s.info,
            PoolState::Balancer(s) => &mut s.info,
            PoolState::Dodo(s) => s,
            PoolState::Clipper(s) => s,
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
            PoolState::Dodo(_) => 0,
            PoolState::Clipper(_) => 0,
        }
    }
}

/// Estimate calldata gas cost for a transaction involving `pool_count` pool addresses.
/// Base tx cost: 21,000 gas. Each warm address read in calldata: ~2,800 gas.
/// (H7: Include calldata cost in per-opportunity gas estimation.)
pub fn calldata_gas_estimate(pool_count: usize) -> u64 {
    21_000 + (pool_count as u64) * 2_800
}

