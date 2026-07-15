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

/// Flags describing a Uniswap V4 hook contract's capabilities.
///
/// Hook capabilities are encoded in the last 3 bytes of the hook address:
/// - Bit 0 (byte 2, bit 7): beforeInitialize / afterInitialize
/// - Bit 1 (byte 2, bit 6): beforeSwap / afterSwap
/// - Bit 2 (byte 2, bit 5): beforeAddLiquidity / afterAddLiquidity
/// - Bit 3 (byte 2, bit 4): beforeRemoveLiquidity / afterRemoveLiquidity
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct V4HookFlags {
    pub before_initialize: bool,
    pub after_initialize: bool,
    pub before_swap: bool,
    pub after_swap: bool,
    pub before_add_liquidity: bool,
    pub after_add_liquidity: bool,
    pub before_remove_liquidity: bool,
    pub after_remove_liquidity: bool,
}

impl V4HookFlags {
    /// Classify a V4 hook address into capability flags.
    ///
    /// The hook address encodes its callbacks in the last 3 bytes.
    /// Byte 2 encodes before/after hooks, byte 1 encodes lock/dynamic fee flags.
    pub fn classify(hook_address: &Address) -> Self {
        let bytes = hook_address.as_slice();
        // The hook address is 20 bytes; the capability bits are in bytes 0 and 1
        // of the 3-byte hook suffix (bytes 17, 18, 19 of the 20-byte address).
        // However, the convention in Uniswap V4 is:
        // - The pool key's hook address has its last 2 bytes encode the flags.
        //
        // Based on Uniswap V4 spec (hooks.sol):
        // position 19 (last byte):  bits [7..4] = before flags, [3..0] = unused
        // position 18 (2nd to last): bits [7..4] = after flags, [3..0] = unused
        //
        // Simplified: the 2 least significant bytes of the address encode:
        //   byte 18: bit 7 = afterSwapEnabled, bit 6 = afterAddLiq, ...
        //   byte 19: bit 7 = beforeSwapEnabled, bit 6 = beforeAddLiq, ...
        //
        // For MEV detection, the key flags are:
        // - afterSwap: the hook modifies state/amounts AFTER the swap (may change output)
        // - beforeSwap: the hook modifies state/amounts BEFORE the swap
        // - beforeAddLiquidity/afterAddLiquidity: may affect pool depth
        let b18 = bytes[18];
        let b19 = bytes[19];

        V4HookFlags {
            before_initialize: b19 & 0x80 != 0,
            after_initialize: b18 & 0x80 != 0,
            before_swap: b19 & 0x40 != 0,
            after_swap: b18 & 0x40 != 0,
            before_add_liquidity: b19 & 0x20 != 0,
            after_add_liquidity: b18 & 0x20 != 0,
            before_remove_liquidity: b19 & 0x10 != 0,
            after_remove_liquidity: b18 & 0x10 != 0,
        }
    }

    /// Returns true if this hook has any swap-modifying capability.
    /// MEV strategies should be cautious with these hooks as they may
    /// alter swap outcomes.
    pub fn modifies_swap(&self) -> bool {
        self.before_swap || self.after_swap
    }

    /// Returns true if this hook modifies liquidity operations.
    pub fn modifies_liquidity(&self) -> bool {
        self.before_add_liquidity || self.after_add_liquidity
            || self.before_remove_liquidity || self.after_remove_liquidity
    }
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
    /// Full token list for multi-token pools (Curve 3+, Balancer 2-8, Pendle).
    /// `token0`/`token1` remain as primary pair for display; this provides the full set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlying_tokens: Option<Vec<Address>>,
    /// Balancer pool type from PoolRegistered event (0=Weighted, 1=Weighted2Tokens, 3=ComposableStable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balancer_pool_type: Option<u8>,
    /// Uniswap V4 hook contract address (derived from pool key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_address: Option<Address>,
    /// Trader Joe LB bin step in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_step: Option<u32>,
    /// Pendle Finance market maturity timestamp (unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maturity_timestamp: Option<u64>,
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
            underlying_tokens: None,
            balancer_pool_type: None,
            hook_address: None,
            bin_step: None,
            maturity_timestamp: None,
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

/// Runtime state for a Uniswap V4 concentrated-liquidity pool.
///
/// Same fields as V3; V4 pools are identified by a `bytes32 poolKey` and
/// may have an associated hook contract.  The quoting logic is identical
/// to V3 (sqrt-price + ticks + liquidity).
#[derive(Debug, Clone)]
pub struct UniswapV4PoolState {
    pub info: PoolInfo,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub ticks: std::collections::BTreeMap<i32, i128>,
    pub fee_growth_global_0_x128: U256,
    pub fee_growth_global_1_x128: U256,
}

impl UniswapV4PoolState {
    pub fn new(info: PoolInfo) -> Self {
        UniswapV4PoolState {
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

impl From<UniswapV4PoolState> for UniswapV3PoolState {
    fn from(v4: UniswapV4PoolState) -> Self {
        UniswapV3PoolState {
            info: v4.info,
            sqrt_price_x96: v4.sqrt_price_x96,
            tick: v4.tick,
            liquidity: v4.liquidity,
            ticks: v4.ticks,
            fee_growth_global_0_x128: v4.fee_growth_global_0_x128,
            fee_growth_global_1_x128: v4.fee_growth_global_1_x128,
        }
    }
}

/// Runtime state for a Trader Joe V2 LB (Liquidity Book) pool.
///
/// LB pools use discrete bins with a configurable bin step.
/// Each bin holds a single asset (tokenX or tokenY), and the active bin
/// holds both. State is initialized via `getActiveId()` and `getBin(activeId)`.
#[derive(Debug, Clone)]
pub struct TraderJoeLBPoolState {
    pub info: PoolInfo,
    /// Current active bin ID.
    pub active_id: u32,
    /// Bin step in basis points.
    pub bin_step: u32,
    /// Reserve of tokenX in the active bin.
    pub reserve_x: u128,
    /// Reserve of tokenY in the active bin.
    pub reserve_y: u128,
}

impl TraderJoeLBPoolState {
    pub fn new(info: PoolInfo, active_id: u32, bin_step: u32) -> Self {
        TraderJoeLBPoolState {
            info,
            active_id,
            bin_step,
            reserve_x: 0,
            reserve_y: 0,
        }
    }
}

/// Runtime state for a Pendle Finance AMM market.
///
/// Pendle uses a modified logistic UAMM for PT/SY yield trading.
/// State is initialized via `readState(address)` on the market contract,
/// which returns totalPt, totalSy, totalLp, reserve, effectiveFeeRate.
#[derive(Debug, Clone)]
pub struct PendlePoolState {
    pub info: PoolInfo,
    /// Address of the PT (Principal Token) for this market.
    pub pt_address: Address,
    /// Address of the SY (Standardized Yield) token for this market.
    pub sy_address: Address,
    /// Total PT tokens in the AMM reserve.
    pub total_pt: u128,
    /// Total SY tokens in the AMM reserve.
    pub total_sy: u128,
}

impl PendlePoolState {
    pub fn new(info: PoolInfo) -> Self {
        PendlePoolState {
            info,
            pt_address: Address::ZERO,
            sy_address: Address::ZERO,
            total_pt: 0,
            total_sy: 0,
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
    /// Rate provider addresses for each token (same order as balances).
    /// Used by strategy 7.7 (Balancer rate provider staleness).
    pub rate_providers: Vec<Option<Address>>,
}

/// Runtime state for any tracked pool.
///
/// Enum variants correspond to supported DEX types.
/// Each variant wraps the type-specific state struct defined above.
#[derive(Debug, Clone)]
pub enum PoolState {
    UniswapV2(UniswapV2PoolState),
    UniswapV3(UniswapV3PoolState),
    UniswapV4(UniswapV4PoolState),
    Curve(CurvePoolState),
    Balancer(BalancerPoolState),
    /// DODO pools (no MEV detection support — passive discovery only)
    Dodo(PoolInfo),
    /// Clipper pools (no MEV detection support — passive discovery only)
    Clipper(PoolInfo),
    /// Trader Joe V2 LB (Liquidity Book) pools
    TraderJoeLB(TraderJoeLBPoolState),
    /// Pendle Finance AMM markets (PT/SY yield trading)
    Pendle(PendlePoolState),
}

impl PoolState {
    pub fn address(&self) -> Address {
        match self {
            PoolState::UniswapV2(s) => s.info.address,
            PoolState::UniswapV3(s) => s.info.address,
            PoolState::UniswapV4(s) => s.info.address,
            PoolState::Curve(s) => s.info.address,
            PoolState::Balancer(s) => s.info.address,
            PoolState::Dodo(s) => s.address,
            PoolState::Clipper(s) => s.address,
            PoolState::TraderJoeLB(s) => s.info.address,
            PoolState::Pendle(s) => s.info.address,
        }
    }

    pub fn info(&self) -> &PoolInfo {
        match self {
            PoolState::UniswapV2(s) => &s.info,
            PoolState::UniswapV3(s) => &s.info,
            PoolState::UniswapV4(s) => &s.info,
            PoolState::Curve(s) => &s.info,
            PoolState::Balancer(s) => &s.info,
            PoolState::Dodo(s) => s,
            PoolState::Clipper(s) => s,
            PoolState::TraderJoeLB(s) => &s.info,
            PoolState::Pendle(s) => &s.info,
        }
    }

    pub fn info_mut(&mut self) -> &mut PoolInfo {
        match self {
            PoolState::UniswapV2(s) => &mut s.info,
            PoolState::UniswapV3(s) => &mut s.info,
            PoolState::UniswapV4(s) => &mut s.info,
            PoolState::Curve(s) => &mut s.info,
            PoolState::Balancer(s) => &mut s.info,
            PoolState::Dodo(s) => s,
            PoolState::Clipper(s) => s,
            PoolState::TraderJoeLB(s) => &mut s.info,
            PoolState::Pendle(s) => &mut s.info,
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
            PoolState::UniswapV4(_) => 120_000,
            PoolState::Curve(_) => 100_000,
            PoolState::Balancer(_) => 100_000,
            PoolState::Dodo(_) => 0,
            PoolState::Clipper(_) => 0,
            PoolState::TraderJoeLB(_) => 100_000,
            PoolState::Pendle(_) => 100_000,
        }
    }
}

/// Estimate calldata gas cost for a transaction involving `pool_count` pool addresses.
/// Base tx cost: 21,000 gas. Each warm address read in calldata: ~2,800 gas.
/// (H7: Include calldata cost in per-opportunity gas estimation.)
pub fn calldata_gas_estimate(pool_count: usize) -> u64 {
    21_000 + (pool_count as u64) * 2_800
}

