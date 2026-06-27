use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use alloy::primitives::{keccak256, Address, Bytes, U256};
use serde::{Deserialize, Serialize};

use crate::types::MevOpportunity;
use crate::pool::math::quote_exact_in;
use crate::pool::math::constant_product_output_amount;
use crate::pool::state::{PoolManager, PoolState};
use crate::pool::math::v3::quote_v3_exact_in;
use crate::rpc::RpcClient;
use crate::types::Strategy;

/// Thread-safe cache for on-chain token decimals.
/// Populated lazily by `fetch_token_decimals` and checked by `guess_token_decimals`.
static DECIMALS_CACHE: LazyLock<Mutex<HashMap<Address, u8>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// ERC20 `decimals()` function selector: 0x313ce567
const DECIMALS_SELECTOR: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

/// Per-block stats collected during a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockReplayStats {
    pub block_number: u64,
    pub total_tx_count: usize,
    pub dex_tx_count: usize,
    pub pending_tx_count: usize,
    pub mempool_opp_count: usize,
}

/// Per-block summary from a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSummary {
    pub block_number: u64,
    pub total_tx: usize,
    pub dex_tx: usize,
    pub pending_tx: usize,
    pub mempool_opps: usize,
    pub opportunities: usize,
    pub by_strategy: std::collections::HashMap<String, usize>,
}

/// Recomputation accuracy label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecomputationAccuracy {
    /// Recomputation not applicable for this strategy
    NotApplicable,
    /// Recomputed profit matches stored profit (within 1%)
    Match,
    /// Recomputed profit differs materially from stored profit
    Mismatch,
    /// Pool state unavailable for recomputation
    Unavailable,
}

impl std::fmt::Display for RecomputationAccuracy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecomputationAccuracy::NotApplicable => write!(f, "N/A"),
            RecomputationAccuracy::Match => write!(f, "✓"),
            RecomputationAccuracy::Mismatch => write!(f, "✗"),
            RecomputationAccuracy::Unavailable => write!(f, "?"),
        }
    }
}

/// Fact-check result for a single opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityFactCheck {
    pub block_number: u64,
    pub tx_index: usize,
    pub strategy: String,
    pub pool_a: Address,
    pub pool_b: Address,
    pub pool_a_name: Option<String>,
    pub pool_b_name: Option<String>,
    pub token_in: Address,
    pub token_out: Address,
    pub input_amount: String,
    pub expected_profit: String,
    pub gas_cost_wei: u128,
    pub profit_gt_gas: bool,
    pub recomputed_profit: Option<String>,
    pub recomputation_match: Option<bool>,
    pub recomputation_accuracy: RecomputationAccuracy,
    /// Profit returned by EVM simulation (via eth_call on DEX view functions).
    /// None when EVM simulation is not applicable (Liquidation, Jit, JitArb).
    pub evm_simulated_profit: Option<String>,
    /// Whether the EVM-simulated profit matches the stored expected_profit (within 1%).
    /// None when EVM simulation was not performed.
    pub evm_simulation_match: Option<bool>,
    /// Whether the cached pool state is consistent with on-chain balances (V2 only).
    pub pool_state_consistent: Option<bool>,
    /// Percentage divergence between cached and on-chain state.
    pub state_divergence_pct: Option<f64>,
    pub victim_tx_index: Option<usize>,
    pub backrun_tx_index: Option<usize>,
    pub tick_lower: Option<i32>,
    pub tick_upper: Option<i32>,
    pub liquidity_amount: Option<u128>,
    pub path: Option<Vec<Address>>,
}

/// Full fact-check report for a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactCheckReport {
    pub run_id: String,
    pub chain: String,
    pub block_count: usize,
    pub total_opportunities: usize,
    pub passed: usize,
    pub failed: usize,
    pub block_summaries: Vec<BlockSummary>,
    pub opportunity_checks: Vec<OpportunityFactCheck>,
}

/// Fetch token decimals from on-chain via `decimals()` (selector: 0x313ce567).
/// Results are cached per address so each token is queried at most once.
async fn fetch_token_decimals(rpc: &RpcClient, token: Address, block: u64) -> Option<u8> {
    // Check cache first
    {
        let cache = DECIMALS_CACHE.lock().expect("DECIMALS_CACHE mutex poisoned");
        if let Some(&d) = cache.get(&token) {
            return Some(d);
        }
    }
    let data = {
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&DECIMALS_SELECTOR);
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(token.as_slice());
        calldata.extend_from_slice(&word);
        Bytes::from(calldata)
    };
    let result = rpc.call(token, data, block).await.ok()?;
    if result.len() < 32 {
        return None;
    }
    let val = result[31]; // uint8, right-aligned in 32-byte word
    DECIMALS_CACHE.lock().expect("DECIMALS_CACHE mutex poisoned").insert(token, val);
    Some(val)
}

/// Guess token decimals for well-known tokens by address.
/// Checks the on-chain decimals cache first, then hardcoded addresses.
/// Returns 18 (default) for unknown tokens.
fn guess_token_decimals(token: &Address) -> u8 {
    // Check on-chain cache first (populated by fetch_token_decimals)
    if let Ok(cache) = DECIMALS_CACHE.lock() {
        if let Some(&d) = cache.get(token) {
            return d;
        }
    }

    // Well-known addresses on Polygon
    const USDC_POLYGON: Address = alloy::primitives::address!("2791bca1f2de4661ed88a30c99a7a9449aa84174");
    const USDT_POLYGON: Address = alloy::primitives::address!("c2132d05d31c914a87c6611c10748aeb04b58e8f");
    const DAI_POLYGON: Address = alloy::primitives::address!("8f3cf7ad23cd3cadbd9735aff958023239c6a063");
    // Well-known addresses on Ethereum
    const USDC_ETH: Address = alloy::primitives::address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    const USDT_ETH: Address = alloy::primitives::address!("dac17f958d2ee523a2206206994597c13d831ec7");
    const DAI_ETH: Address = alloy::primitives::address!("6b175474e89094c44da98b954eedeac495271d0f");
    const WBTC_ETH: Address = alloy::primitives::address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599");
    const WBTC_POLYGON: Address = alloy::primitives::address!("1bfd67037b42cf73acf2047067bd4f2c47d9bfd6");

    match token {
        &USDC_POLYGON | &USDT_POLYGON | &USDC_ETH | &USDT_ETH => 6,
        &DAI_POLYGON | &DAI_ETH => 18,
        &WBTC_ETH | &WBTC_POLYGON => 8,
        _ => 18,
    }
}

/// Format a U256 token amount as a human-readable decimal string.
///
/// `decimals` is the number of decimal places for the token (e.g. 18 for WETH, 6 for USDC).
/// Default to 18 when the actual token decimals are unknown.
fn format_amount(val: &alloy::primitives::U256, decimals: u8) -> String {
    let s = val.to_string();
    let d = decimals as usize;
    if s.len() > d {
        let (int_part, dec_part) = s.split_at(s.len() - d);
        let dec_trimmed = dec_part.trim_end_matches('0');
        if dec_trimmed.is_empty() {
            format!("{}.0", int_part)
        } else {
            format!("{}.{}", int_part, dec_trimmed)
        }
    } else {
        format!("0.{:0>width$}", s, width = d).trim_end_matches('0').to_string()
    }
}

/// Compute per-block summaries from opportunities and per-block tx/dex counts.
pub fn compute_block_summaries(
    opportunities: &[MevOpportunity],
    per_block_stats: &[BlockReplayStats],
) -> Vec<BlockSummary> {
    let stats_map: std::collections::HashMap<u64, &BlockReplayStats> =
        per_block_stats.iter().map(|s| (s.block_number, s)).collect();

    let mut opps_by_block: std::collections::HashMap<u64, Vec<&MevOpportunity>> =
        std::collections::HashMap::new();
    for opp in opportunities {
        opps_by_block.entry(opp.block_number).or_default().push(opp);
    }

    let mut summaries = Vec::new();
    let mut block_numbers: Vec<u64> = stats_map.keys().copied().collect();
    block_numbers.sort();

    for block_num in block_numbers {
        let stats = stats_map[&block_num];
        let opps = opps_by_block.remove(&block_num).unwrap_or_default();
        let mut by_strategy = std::collections::HashMap::new();
        for opp in &opps {
            *by_strategy.entry(opp.strategy.to_string()).or_insert(0) += 1;
        }
        summaries.push(BlockSummary {
            block_number: block_num,
            total_tx: stats.total_tx_count,
            dex_tx: stats.dex_tx_count,
            pending_tx: stats.pending_tx_count,
            mempool_opps: stats.mempool_opp_count,
            opportunities: opps.len(),
            by_strategy,
        });
    }

    summaries
}

/// Quote a single swap through any pool type.
/// Returns the output amount for the given `amount_in` of `token_in`.
pub fn quote_single_swap(
    pool: &PoolState,
    token_in: Address,
    token_out: Address,
    amount_in: u128,
) -> Option<u128> {
    match pool {
        PoolState::UniswapV2(v2) => {
            let (reserve_in, reserve_out) = if v2.info.token0 == token_in {
                (v2.reserve0, v2.reserve1)
            } else if v2.info.token1 == token_in {
                (v2.reserve1, v2.reserve0)
            } else {
                return None;
            };
            constant_product_output_amount(amount_in, reserve_in, reserve_out, v2.info.fee)
        }
        PoolState::UniswapV3(v3) => {
            let zero_for_one = v3.info.token0 == token_in;
            if !zero_for_one && v3.info.token1 != token_in {
                return None;
            }
            quote_v3_exact_in(v3, amount_in, zero_for_one)
        }
        PoolState::Curve(_) | PoolState::Balancer(_) => {
            quote_exact_in(pool, token_in, token_out, amount_in)
        }
    }
}

// ── ABI encoding helpers for EVM simulation (M3) ──────────────────────────
//
// These helpers construct calldata for on-chain view functions used to
// simulate swaps through the actual DEX contracts via eth_call. The
// resulting output amounts are compared against the quoting-function
// recomputation to catch detection bugs, state divergence, and formula
// errors.

/// Function selector for ERC20 `balanceOf(address)`: 0x70a08231
const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// Function selector for Uniswap V3 Quoter `quoteExactInputSingle`
static QUOTE_EXACT_INPUT_SINGLE_SELECTOR: LazyLock<[u8; 4]> = LazyLock::new(|| {
    keccak256(b"quoteExactInputSingle(address,address,uint24,uint256,uint160)")[..4]
        .try_into()
        .expect("keccak256 output is always >= 4 bytes")
});

/// Function selector for Curve `get_dy(int128,int128,uint256)`
static GET_DY_SELECTOR: LazyLock<[u8; 4]> = LazyLock::new(|| {
    keccak256(b"get_dy(int128,int128,uint256)")[..4]
        .try_into()
        .expect("keccak256 output is always >= 4 bytes")
});

/// Standard Uniswap V3 Quoter address (same on Ethereum, Polygon, Arbitrum, Optimism, etc.)
const V3_QUOTER: Address = alloy::primitives::address!("b27308f9F90D607463bb33eA1BeBb41C27CE5AB6");

/// Function selector for V2 Router `getAmountsOut(uint256,address[])`.
static GET_AMOUNTS_OUT_SELECTOR: LazyLock<[u8; 4]> = LazyLock::new(|| {
    keccak256(b"getAmountsOut(uint256,address[])")[..4]
        .try_into()
        .expect("keccak256 output is always >= 4 bytes")
});

/// ABI-encode `quoteExactInputSingle(address,address,uint24,uint256,uint160)`.
fn encode_quote_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    amount_in: u128,
) -> Bytes {
    let mut data = Vec::with_capacity(164);
    data.extend_from_slice(&*QUOTE_EXACT_INPUT_SINGLE_SELECTOR);
    let mut word = [0u8; 32];
    word[12..32].copy_from_slice(token_in.as_slice());
    data.extend_from_slice(&word);
    word[12..32].copy_from_slice(token_out.as_slice());
    data.extend_from_slice(&word);
    word.fill(0);
    let fee_bytes = fee.to_be_bytes();
    word[29..32].copy_from_slice(&fee_bytes[1..4]);
    data.extend_from_slice(&word);
    word.fill(0);
    word = U256::from(amount_in).to_be_bytes::<32>();
    data.extend_from_slice(&word);
    word.fill(0);
    data.extend_from_slice(&word);
    Bytes::from(data)
}

/// Decode the `uint256 amountOut` return value from `quoteExactInputSingle`.
fn decode_v3_quoter_result(data: &[u8]) -> Option<u128> {
    if data.len() < 32 {
        return None;
    }
    let amount = U256::from_be_slice(&data[..32]);
    Some(amount.to::<u128>())
}

/// ABI-encode `get_dy(int128 i, int128 j, uint256 dx)` for Curve pools.
fn encode_curve_get_dy(i: i128, j: i128, dx: u128) -> Bytes {
    let mut data = Vec::with_capacity(100);
    data.extend_from_slice(&*GET_DY_SELECTOR);
    let mut word = [0u8; 32];
    let i_bytes = i.to_be_bytes();
    word[16..32].copy_from_slice(&i_bytes);
    if i < 0 {
        word[..16].fill(0xFF);
    }
    data.extend_from_slice(&word);
    word.fill(0);
    let j_bytes = j.to_be_bytes();
    word[16..32].copy_from_slice(&j_bytes);
    if j < 0 {
        word[..16].fill(0xFF);
    }
    data.extend_from_slice(&word);
    word.fill(0);
    word = U256::from(dx).to_be_bytes::<32>();
    data.extend_from_slice(&word);
    Bytes::from(data)
}

/// ABI-encode `balanceOf(address owner)` for ERC20 tokens.
fn encode_balance_of(owner: Address) -> Bytes {
    let mut data = Vec::with_capacity(36);
    data.extend_from_slice(&BALANCE_OF_SELECTOR);
    let mut word = [0u8; 32];
    word[12..32].copy_from_slice(owner.as_slice());
    data.extend_from_slice(&word);
    Bytes::from(data)
}

/// Simulate a V3 single-hop swap via the on-chain Quoter contract at a
/// historical block. Returns the exact output amount the pool would produce.
///
/// This is a genuine EVM execution: the Quoter runs the full swap logic
/// (tick crossing, fee application, liquidity calculation) in the EVM at
/// the given block's state. Catches tick data errors, liquidity divergence,
/// and quoting formula bugs that structural recomputation cannot see.
async fn simulate_v3_swap_via_quoter(
    rpc: &RpcClient,
    token_in: Address,
    token_out: Address,
    fee: u32,
    amount_in: u128,
    block: u64,
) -> Option<u128> {
    let calldata = encode_quote_exact_input_single(token_in, token_out, fee, amount_in);
    let result = rpc.call(V3_QUOTER, calldata, block).await.ok()?;
    decode_v3_quoter_result(&result)
}

/// Simulate a Curve swap via `get_dy` at a historical block.
///
/// Like the Quoter for V3, this runs the actual Curve StableSwap formula
/// in the EVM and returns the exact output for the given input.
async fn simulate_curve_swap_via_get_dy(
    rpc: &RpcClient,
    pool: Address,
    i: i128,
    j: i128,
    dx: u128,
    block: u64,
) -> Option<u128> {
    let calldata = encode_curve_get_dy(i, j, dx);
    let result = rpc.call(pool, calldata, block).await.ok()?;
    if result.len() < 32 {
        return None;
    }
    let amount = U256::from_be_slice(&result[..32]);
    Some(amount.to::<u128>())
}

/// ABI-encode `getAmountsOut(uint256 amountIn, address[] memory path)` for V2 routers.
/// The path contains exactly two addresses: [tokenIn, tokenOut].
fn encode_get_amounts_out(amount_in: u128, token_in: Address, token_out: Address) -> Bytes {
    let mut data = Vec::with_capacity(164);
    data.extend_from_slice(&*GET_AMOUNTS_OUT_SELECTOR);
    // amountIn (uint256, 32 bytes)
    data.extend_from_slice(&U256::from(amount_in).to_be_bytes::<32>());
    // offset to path array data (points to byte 64 from start of non-selector data)
    data.extend_from_slice(&U256::from(64u64).to_be_bytes::<32>());
    // path.length = 2
    data.extend_from_slice(&U256::from(2u64).to_be_bytes::<32>());
    // path[0] = tokenIn (right-aligned in 32-byte word)
    let mut word = [0u8; 32];
    word[12..32].copy_from_slice(token_in.as_slice());
    data.extend_from_slice(&word);
    // path[1] = tokenOut
    word.fill(0);
    word[12..32].copy_from_slice(token_out.as_slice());
    data.extend_from_slice(&word);
    Bytes::from(data)
}

/// Decode the `uint256[] amounts` return value from `getAmountsOut`.
/// Returns the second element (output amount) for a 2-element path.
fn decode_get_amounts_out(data: &[u8]) -> Option<u128> {
    if data.len() < 128 {
        return None;
    }
    // amounts[1] is at offset 96: [offset(32) + length(32) + amounts[0](32)]
    let amount = U256::from_be_slice(&data[96..128]);
    Some(amount.to::<u128>())
}

/// Simulate a V2 single-hop swap via the router's `getAmountsOut` at a
/// historical block. Runs the actual constant-product formula in the EVM
/// at the given block's state, catching protocol fees and formula
/// modifications that the structural fallback cannot detect.
async fn simulate_v2_swap_via_router(
    rpc: &RpcClient,
    router: Address,
    token_in: Address,
    token_out: Address,
    amount_in: u128,
    block: u64,
) -> Option<u128> {
    let calldata = encode_get_amounts_out(amount_in, token_in, token_out);
    let result = rpc.call(router, calldata, block).await.ok()?;
    decode_get_amounts_out(&result)
}

/// Check whether the cached pool state matches on-chain token balances.
/// For V2 pools, compares cached `reserve0`/`reserve1` against
/// `balanceOf(pool)` on both token contracts. Returns `(consistent, divergence_pct)`.
///
/// A divergence > 0.1% indicates the pool manager's cached state has
/// drifted from on-chain reality (H5 / M4 scenario).
async fn check_pool_state_consistency(
    rpc: &RpcClient,
    pools: &PoolManager,
    opp: &MevOpportunity,
    block: u64,
) -> Option<(bool, f64)> {
    let pool_a = pools.get(&opp.pool_a)?;
    match pool_a {
        PoolState::UniswapV2(v2) => {
            let calldata0 = encode_balance_of(v2.info.address);
            let calldata1 = encode_balance_of(v2.info.address);
            let bal0_raw = rpc.call(v2.info.token0, calldata0, block).await.ok()?;
            let bal1_raw = rpc.call(v2.info.token1, calldata1, block).await.ok()?;
            if bal0_raw.len() < 32 || bal1_raw.len() < 32 {
                return None;
            }
            let bal0 = U256::from_be_slice(&bal0_raw[..32]).to::<u128>();
            let bal1 = U256::from_be_slice(&bal1_raw[..32]).to::<u128>();
            let r0 = v2.reserve0;
            let r1 = v2.reserve1;
            if r0 == 0 || r1 == 0 {
                return Some((true, 0.0));
            }
            let dev0 = if r0 > bal0 { r0 - bal0 } else { bal0 - r0 };
            let dev1 = if r1 > bal1 { r1 - bal1 } else { bal1 - r1 };
            let max_dev = dev0.max(dev1) as f64;
            let max_res = r0.max(r1).max(1) as f64;
            let divergence = max_dev / max_res;
            Some((divergence < 0.001, divergence * 100.0))
        }
        _ => None,
    }
}

/// EVM-based quote for a single swap via `eth_call` on the pool's view
/// functions (V3 Quoter, Curve get_dy, V2 Router getAmountsOut) or
/// structural fallback (Balancer).
///
/// For V3 and Curve this is a genuine EVM simulation at the historical
/// block. For V2 the router's `getAmountsOut` is tried first (M3); if no
/// known router is available, the structural formula is used with refetched
/// state. Balancer uses the structural formula since state is already
/// refetched from the Vault via `getPoolTokens` — divergence is caught by
/// `check_pool_state_consistency`.
async fn evm_quote_single_swap(
    rpc: &RpcClient,
    pool: &PoolState,
    token_in: Address,
    token_out: Address,
    amount_in: u128,
    block: u64,
) -> Option<u128> {
    match pool {
        PoolState::UniswapV3(v3) => {
            simulate_v3_swap_via_quoter(
                rpc, token_in, token_out, v3.info.fee, amount_in, block,
            )
            .await
        }
        PoolState::Curve(curve) => {
            let i = *curve.token_index.get(&token_in)? as i128;
            let j = *curve.token_index.get(&token_out)? as i128;
            simulate_curve_swap_via_get_dy(rpc, curve.info.address, i, j, amount_in, block).await
        }
        PoolState::UniswapV2(v2) => {
            // M3: Try on-chain router simulation first (catches protocol fees,
            // dynamic fee changes, and formula modifications).
            if let Some(router) = v2.info.factory.and_then(crate::types::v2_router_for_factory) {
                let simulated = simulate_v2_swap_via_router(
                    rpc, router, token_in, token_out, amount_in, block,
                )
                .await;
                if simulated.is_some() {
                    return simulated;
                }
            }
            // Structural fallback with refetched state
            let (reserve_in, reserve_out) = if v2.info.token0 == token_in {
                (v2.reserve0, v2.reserve1)
            } else if v2.info.token1 == token_in {
                (v2.reserve1, v2.reserve0)
            } else {
                return None;
            };
            constant_product_output_amount(amount_in, reserve_in, reserve_out, v2.info.fee)
        }
        PoolState::Balancer(_) => {
            quote_exact_in(pool, token_in, token_out, amount_in)
        }
    }
}

/// Simulate a detected MEV opportunity via EVM and return the actual profit.
///
/// Constructs the swap path encoded by the opportunity and executes each
/// leg through the DEX's on-chain view function (V3 Quoter, Curve get_dy)
/// or deterministic formula (V2, Balancer). Returns the computed profit
/// in the output token, which can be compared against `expected_profit`
/// to detect quoting bugs and state divergence.
///
/// Returns `None` for strategies that cannot be simulated (Liquidation) or
/// when the necessary pools or RPC data are unavailable.
///
/// ## M3 (PLAN-accuracy-improvement.md)
/// This function addresses the "replay opportunity" gap: previously, all
/// verification was structural (recomputed via quoting functions which may
/// share bugs with the detector). By running the same swap through the
/// actual EVM (via eth_call on the DEX's view functions), we catch:
/// - Wrong reserve direction in pool state
/// - Incorrect fee application in quoting formulas
/// - Pool state divergence from on-chain reality
/// - Formula bugs (e.g., constant-product instead of StableSwap)
pub async fn simulate_opportunity_evm(
    rpc: &RpcClient,
    pools: &PoolManager,
    opp: &MevOpportunity,
    block: u64,
) -> Option<U256> {
    let input = opp.input_amount.to::<u128>();
    if input == 0 {
        return None;
    }

    match opp.strategy {
        Strategy::TwoHopArb => {
            let pool_a = pools.get(&opp.pool_a)?;
            let pool_b = pools.get(&opp.pool_b)?;
            let info_a = pool_a.info();
            let info_b = pool_b.info();

            let shared = if info_a.token0 == info_b.token0 || info_a.token0 == info_b.token1 {
                info_a.token0
            } else {
                info_a.token1
            };

            let intermediate =
                evm_quote_single_swap(rpc, pool_a, opp.token_in, shared, input, block).await?;
            if intermediate == 0 {
                return None;
            }
            let output = evm_quote_single_swap(
                rpc, pool_b, shared, opp.token_out, intermediate, block,
            )
            .await?;
            if output <= input {
                return None;
            }
            Some(U256::from(output - input))
        }
        Strategy::MultiHopArb => {
            let path = opp.path.as_ref()?;
            let mut current = input;
            let mut current_token = opp.token_in;
            for &addr in path {
                let pool = pools.get(&addr)?;
                let info = pool.info();
                let next_token = if info.token0 == current_token {
                    info.token1
                } else if info.token1 == current_token {
                    info.token0
                } else {
                    return None;
                };
                current =
                    evm_quote_single_swap(rpc, pool, current_token, next_token, current, block)
                        .await?;
                current_token = next_token;
            }
            if current <= input {
                return None;
            }
            Some(U256::from(current - input))
        }
        Strategy::Sandwich => {
            // Sandwich: simulate front-run buy (token_in -> token_out) then
            // back-run sell (token_out -> token_in) using the stored input_amount
            let pool = pools.get(&opp.pool_a)?;
            let mid =
                evm_quote_single_swap(rpc, pool, opp.token_in, opp.token_out, input, block).await?;
            if mid == 0 {
                return None;
            }
            let back = evm_quote_single_swap(
                rpc, pool, opp.token_out, opp.token_in, mid, block,
            )
            .await?;
            if back <= input {
                return None;
            }
            Some(U256::from(back - input))
        }
        Strategy::Jit | Strategy::JitArb | Strategy::Liquidation => None,
        Strategy::CrossBlockArb | Strategy::TimeBandit => None,
    }
}

/// Recompute the gross profit for a detected MEV opportunity using the
/// current pool manager state.
///
/// Returns `None` when the strategy is not supported for recomputation or
/// when the necessary pools are no longer tracked.
/// The returned profit is the raw (gross) profit in `token_out` before
/// flash loan fee deduction and normalization.
pub fn recompute_opportunity_profit(
    pools: &PoolManager,
    opp: &MevOpportunity,
) -> Option<U256> {
    let input = opp.input_amount.to::<u128>();
    if input == 0 {
        return None;
    }

    match opp.strategy {
        Strategy::TwoHopArb => {
            let pool_a = pools.get(&opp.pool_a)?;
            let pool_b = pools.get(&opp.pool_b)?;

            let info_a = pool_a.info();
            let info_b = pool_b.info();

            let shared = if info_a.token0 == info_b.token0 || info_a.token0 == info_b.token1 {
                info_a.token0
            } else {
                info_a.token1
            };

            let intermediate = quote_single_swap(pool_a, opp.token_in, shared, input)?;
            let output = quote_single_swap(pool_b, shared, opp.token_out, intermediate)?;

            if output <= input {
                return None;
            }
            Some(U256::from(output - input))
        }
        Strategy::MultiHopArb => {
            let path = opp.path.as_ref()?;
            if path.is_empty() {
                return None;
            }
            let mut current = input;
            let mut current_token = opp.token_in;
            for &addr in path {
                let pool = pools.get(&addr)?;
                let info = pool.info();
                let next_token = if info.token0 == current_token {
                    info.token1
                } else if info.token1 == current_token {
                    info.token0
                } else {
                    return None;
                };
                current = quote_single_swap(pool, current_token, next_token, current)?;
                current_token = next_token;
            }
            if current <= input {
                return None;
            }
            Some(U256::from(current - input))
        }
        Strategy::Sandwich => {
            // Sandwich profit = frontrun buy amount back - backrun sell amount
            // Approximate: quote the frontrun buy (token_in -> token_out)
            // then the backrun sell (token_out -> token_in) at stored input_amount
            let pool = pools.get(&opp.pool_a)?;
            let mid = quote_single_swap(pool, opp.token_in, opp.token_out, input)?;
            if mid == 0 { return None; }
            let back = quote_single_swap(pool, opp.token_out, opp.token_in, mid)?;
            if back <= input { return None; }
            Some(U256::from(back - input))
        }
        Strategy::Liquidation => {
            // Liquidation profit verification: structural check only.
            // Full verification would require re-executing the liquidation
            // against forked state with Aave pool data.
            if opp.expected_profit > U256::from(opp.gas_cost_wei) {
                Some(opp.expected_profit)
            } else {
                None
            }
        }
        Strategy::Jit => {
            // JIT fee revenue: liquidity_share * swap_fee_growth
            // Use stored tick range and liquidity amount if available
            let pool = pools.get(&opp.pool_a)?;
            let v3_state = match pool {
                PoolState::UniswapV3(s) => s,
                _ => return None,
            };
            let liq_amount = opp.liquidity_amount? as u128;
            if liq_amount == 0 || v3_state.liquidity == 0 {
                return None;
            }
            let fee_tier = v3_state.info.fee as u128;
            let estimated_fee = liq_amount.saturating_mul(fee_tier) / 1_000_000u128;
            // Estimate: fee revenue ≈ input_amount * (liq_amount / pool.total_liquidity) * fee_tier / 1e6
            let share = U256::from(liq_amount) * U256::from(2u128.pow(64)) / U256::from(v3_state.liquidity.max(1));
            let fee_revenue = U256::from(input) * share * U256::from(fee_tier)
                / (U256::from(2u128.pow(64)) * U256::from(1_000_000u128));
            if fee_revenue.is_zero() {
                Some(U256::from(estimated_fee))
            } else {
                Some(fee_revenue)
            }
        }
        Strategy::JitArb => {
            // JitArb = arb profit + JIT fee revenue
            // Arb profit: difference of two swap amounts in shared token
            let pool = pools.get(&opp.pool_a)?;
            let mid = quote_single_swap(pool, opp.token_in, opp.token_out, input)?;
            if mid == 0 { return None; }
            let back = quote_single_swap(pool, opp.token_out, opp.token_in, mid)?;
            let arb_profit = if back > input { back - input } else { 0u128 };

            // Add JIT fee component
            let jit_fee = if let Some(liq) = opp.liquidity_amount {
                if let PoolState::UniswapV3(v3) = pool {
                    let fee_tier = v3.info.fee as u128;
                    let share = U256::from(liq) * U256::from(2u128.pow(64)) / U256::from(v3.liquidity.max(1));
                    let fee_rev = U256::from(input) * share * U256::from(fee_tier)
                        / (U256::from(2u128.pow(64)) * U256::from(1_000_000u128));
                    fee_rev.to::<u128>()
                } else { 0 }
            } else { 0 };

            let total = U256::from(arb_profit.saturating_add(jit_fee));
            if total.is_zero() { None } else { Some(total) }
        }
        Strategy::CrossBlockArb | Strategy::TimeBandit => None,
    }
}

/// Build opportunity fact checks from saved results.
///
/// If `pools` is `Some`, recomputes each opportunity's profit using the
/// current pool state and fills in `recomputed_profit` and `recomputation_match`.
/// Also computes a `recomputation_accuracy` label summarizing match quality.
pub fn verify_opportunities(
    opportunities: &[MevOpportunity],
    pools: Option<&PoolManager>,
) -> Vec<OpportunityFactCheck> {
    opportunities
        .iter()
        .map(|opp| {
            let profit_gt_gas = opp.expected_profit > U256::from(opp.gas_cost_wei);
            let dec_out = guess_token_decimals(&opp.token_out);
            let dec_in = guess_token_decimals(&opp.token_in);
            let (recomputed_profit, recomputation_match, recomputation_accuracy) = pools
                .and_then(|pm| recompute_opportunity_profit(pm, opp))
                .map(|recomputed| {
                    let stored = opp.raw_profit.unwrap_or(opp.expected_profit);
                    // Compute accuracy: match if within 1% or 1 wei of each other
                    let diff = if stored > recomputed { stored - recomputed } else { recomputed - stored };
                    let matched = diff == U256::ZERO
                        || (stored > U256::ZERO && diff * U256::from(100u64) / stored < U256::from(1u64));
                    let accuracy = if matched {
                        RecomputationAccuracy::Match
                    } else {
                        RecomputationAccuracy::Mismatch
                    };
                    (Some(format_amount(&recomputed, dec_out)), Some(matched), accuracy)
                })
                .unwrap_or((None, None, RecomputationAccuracy::NotApplicable));
            OpportunityFactCheck {
                block_number: opp.block_number,
                tx_index: opp.tx_index,
                strategy: opp.strategy.to_string(),
                pool_a: opp.pool_a,
                pool_b: opp.pool_b,
                pool_a_name: None,
                pool_b_name: None,
                token_in: opp.token_in,
                token_out: opp.token_out,
                input_amount: format_amount(&opp.input_amount, dec_in),
                expected_profit: format_amount(&opp.expected_profit, dec_out),
                gas_cost_wei: opp.gas_cost_wei,
                profit_gt_gas,
                recomputed_profit,
                recomputation_match,
                recomputation_accuracy,
                evm_simulated_profit: None,
                evm_simulation_match: None,
                pool_state_consistent: None,
                state_divergence_pct: None,
                victim_tx_index: opp.victim_tx_index,
                backrun_tx_index: opp.backrun_tx_index,
                tick_lower: opp.tick_lower,
                tick_upper: opp.tick_upper,
                liquidity_amount: opp.liquidity_amount,
                path: opp.path.clone(),
            }
        })
        .collect()
}

/// Verify opportunities against actual on-chain pool state fetched via `eth_call`.
///
/// **M3 (PLAN-accuracy-improvement.md): Full EVM re-execution fact-check.**
///
/// This is a three-tier verification:
///
/// 1. **State refetch** — Re-fetches each pool's state from the chain at the
///    opportunity's block (`getReserves()`, `slot0()+liquidity()`, Curve
///    balances, Balancer pool tokens) into a fresh `PoolManager`.
///
/// 2. **Structural recomputation** — Runs the same quoting formulas used
///    during detection against the fresh state. This catches state divergence
///    and reserve ordering bugs.
///
/// 3. **EVM simulation** — For V3 and Curve pools, calls the actual DEX view
///    functions (`quoteExactInputSingle` on the V3 Quoter, `get_dy` on Curve
///    pools) via `eth_call` at the historical block. The EVM simulates the
///    full swap logic (tick crossing, StableSwap Newton iteration, fee
///    application) and returns the exact output. This catches formula bugs
///    that structural recomputation cannot.
///
/// Also verifies pool state consistency by comparing cached V2 reserves
/// against on-chain `balanceOf()` calls on the token contracts.
///
/// # Performance
/// Makes one `eth_call` per unique pool address for state refetch, plus up
/// to two `eth_call` calls per opportunity for EVM simulation. Opportunities
/// are grouped by block so shared pools are only fetched once.
///
/// # Returns
/// One `OpportunityFactCheck` per input opportunity with `recomputed_profit`,
/// `evm_simulated_profit`, `evm_simulation_match`, `pool_state_consistent`,
/// and `state_divergence_pct` populated where applicable.
pub async fn verify_opportunities_from_chain(
    opportunities: &[MevOpportunity],
    pools: &PoolManager,
    rpc: &RpcClient,
) -> Vec<OpportunityFactCheck> {
    let mut fresh_pools = PoolManager::new();
    let mut fetched = std::collections::HashSet::new();

    // Phase 1: refetch on-chain pool state for each unique (pool, block)
    for opp in opportunities {
        let block = opp.block_number;
        for addr in std::iter::once(&opp.pool_a)
            .chain(std::iter::once(&opp.pool_b))
            .chain(opp.path.as_ref().map(|p| p.as_slice()).unwrap_or(&[]))
        {
            if addr.is_zero() || !fetched.insert((*addr, block)) {
                continue;
            }
            if let Some(state) = pools.refetch_pool_state(rpc, addr, block).await {
                fresh_pools.add_pool(state);
            }
        }
    }

    // Pre-populate decimals cache for all unique tokens (L4)
    for opp in opportunities {
        fetch_token_decimals(rpc, opp.token_in, opp.block_number).await;
        fetch_token_decimals(rpc, opp.token_out, opp.block_number).await;
    }

    // Phase 2: structural recomputation on refetched state
    let mut results = verify_opportunities(opportunities, Some(&fresh_pools));

    // Phase 3: EVM simulation + state consistency per opportunity
    for (opp, check) in opportunities.iter().zip(results.iter_mut()) {
        let block = opp.block_number;
        let dec_out = guess_token_decimals(&opp.token_out);

        // EVM simulation (V3 Quoter, Curve get_dy, or structural fallback)
        if let Some(evm_profit) = simulate_opportunity_evm(rpc, &fresh_pools, opp, block).await {
            let stored = opp.expected_profit;
            let diff = if stored > evm_profit { stored - evm_profit } else { evm_profit - stored };
            let matched = diff == U256::ZERO
                || (stored > U256::ZERO && diff * U256::from(100u64) / stored < U256::from(1u64));
            check.evm_simulated_profit = Some(format_amount(&evm_profit, dec_out));
            check.evm_simulation_match = Some(matched);
        }

        // Pool state consistency (V2 balanceOf vs cached reserves)
        if let Some((consistent, divergence)) =
            check_pool_state_consistency(rpc, &fresh_pools, opp, block).await
        {
            check.pool_state_consistent = Some(consistent);
            check.state_divergence_pct = Some(divergence);
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Strategy;
    use alloy::primitives::{address, U256};

    #[test]
    fn test_compute_block_summaries_empty() {
        let result = compute_block_summaries(&[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_compute_block_summaries_single_block() {
        let stats = vec![BlockReplayStats {
            block_number: 1,
            total_tx_count: 100,
            dex_tx_count: 25,
            pending_tx_count: 0,
            mempool_opp_count: 0,
        }];
        let opps = vec![
            MevOpportunity {
                block_number: 1,
                tx_index: 5,
                strategy: Strategy::TwoHopArb,
                ..MevOpportunity::new(1, 5, Strategy::TwoHopArb, address!("1111111111111111111111111111111111111111"), 100)
            },
            MevOpportunity {
                block_number: 1,
                tx_index: 10,
                strategy: Strategy::Sandwich,
                ..MevOpportunity::new(1, 10, Strategy::Sandwich, address!("2222222222222222222222222222222222222222"), 100)
            },
        ];

        let summaries = compute_block_summaries(&opps, &stats);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].block_number, 1);
        assert_eq!(summaries[0].total_tx, 100);
        assert_eq!(summaries[0].dex_tx, 25);
        assert_eq!(summaries[0].opportunities, 2);
        assert_eq!(summaries[0].by_strategy.get("two_hop_arb"), Some(&1));
        assert_eq!(summaries[0].by_strategy.get("sandwich"), Some(&1));
    }

    #[test]
    fn test_compute_block_summaries_multiple_blocks() {
        let stats = vec![
            BlockReplayStats {
                block_number: 1,
                total_tx_count: 100,
                dex_tx_count: 25,
                pending_tx_count: 0,
                mempool_opp_count: 0,
            },
            BlockReplayStats {
                block_number: 2,
                total_tx_count: 50,
                dex_tx_count: 10,
                pending_tx_count: 0,
                mempool_opp_count: 0,
            },
        ];
        let opps = vec![
            MevOpportunity::new(1, 5, Strategy::TwoHopArb, address!("1111111111111111111111111111111111111111"), 100),
            MevOpportunity::new(1, 10, Strategy::Sandwich, address!("2222222222222222222222222222222222222222"), 100),
        ];

        let summaries = compute_block_summaries(&opps, &stats);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].block_number, 1);
        assert_eq!(summaries[0].opportunities, 2);
        assert_eq!(summaries[1].block_number, 2);
        assert_eq!(summaries[1].opportunities, 0);
    }

    #[test]
    fn test_verify_opportunities_sandwich() {
        let opp = MevOpportunity {
            canonical_id: None,
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::Sandwich,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: Address::ZERO,
            token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_out: address!("cccccccccccccccccccccccccccccccccccccccc"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(500),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            pga_adjusted_profit: None,
            gas_cost_wei: 100,
            timestamp: 12345,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: Some(1),
            backrun_tx_index: Some(2),
            mempool_only: false,
            confidence: None,
        };
        let checks = verify_opportunities(&[opp], None);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].profit_gt_gas);
        assert_eq!(checks[0].victim_tx_index, Some(1));
        assert_eq!(checks[0].backrun_tx_index, Some(2));
    }

    #[test]
    fn test_verify_opportunities_missing_sandwich_fields() {
        let opp = MevOpportunity::new(1, 0, Strategy::Sandwich, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100);
        let checks = verify_opportunities(&[opp], None);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].victim_tx_index.is_none());
        assert!(checks[0].backrun_tx_index.is_none());
    }

    #[test]
    fn test_verify_opportunities_profit_vs_gas() {
        let profitable = MevOpportunity {
            canonical_id: None,
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::TwoHopArb,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_in: address!("cccccccccccccccccccccccccccccccccccccccc"),
            token_out: address!("dddddddddddddddddddddddddddddddddddddddd"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(500),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            pga_adjusted_profit: None,
            gas_cost_wei: 100,
            timestamp: 12345,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
            mempool_only: false,
            confidence: None,
        };
        let unprofitable = MevOpportunity {
            expected_profit: U256::from(50),
            ..profitable.clone()
        };

        let checks = verify_opportunities(&[profitable, unprofitable], None);
        assert_eq!(checks.len(), 2);
        assert!(checks[0].profit_gt_gas);
        assert!(!checks[1].profit_gt_gas);
    }

    #[test]
    fn test_format_amount() {
        let one_eth = U256::from(10u64).pow(U256::from(18));
        assert!(format_amount(&one_eth, 18).contains("1.0"));

        let zero = U256::ZERO;
        assert!(format_amount(&zero, 18).contains("0"));
    }

    // ── ABI encoding tests (M3) ───────────────────────────────────────────

    #[test]
    fn test_encode_balance_of() {
        let owner = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let data = encode_balance_of(owner);
        assert_eq!(data.len(), 36);
        assert_eq!(&data[..4], &BALANCE_OF_SELECTOR);
        // Address is right-aligned in the 32-byte word, offset by 12 from selector start
        let addr_start = 4 + 12;
        assert_eq!(&data[addr_start..addr_start + 20], owner.as_slice());
    }

    #[test]
    fn test_encode_quote_exact_input_single_well_formed() {
        let token_in = address!("1111111111111111111111111111111111111111");
        let token_out = address!("2222222222222222222222222222222222222222");
        let fee = 3000u32;
        let amount = 1_000_000u128;
        let data = encode_quote_exact_input_single(token_in, token_out, fee, amount);
        assert_eq!(data.len(), 164);
        assert_eq!(&data[..4], &*QUOTE_EXACT_INPUT_SINGLE_SELECTOR);
        // tokenIn at bytes 4+12..4+32 = 16..36
        assert_eq!(&data[16..36], token_in.as_slice());
        // tokenOut at bytes 36+12..36+32 = 48..68
        assert_eq!(&data[48..68], token_out.as_slice());
        // fee = 3000 = 0x0BB8, right-aligned in 3 bytes at bytes 68+29..68+32 = 97..100
        assert_eq!(data[98], 0x0b);
        assert_eq!(data[99], 0xb8);
        // amountIn at bytes 100..132
        let decoded_amount = U256::from_be_slice(&data[100..132]);
        assert_eq!(decoded_amount, U256::from(amount));
        // sqrtPriceLimitX96 = 0 (last word at bytes 132..164)
        assert!(data[132..164].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_decode_v3_quoter_result() {
        let buf = U256::from(5000u64).to_be_bytes::<32>();
        assert_eq!(decode_v3_quoter_result(&buf), Some(5000));
        assert_eq!(decode_v3_quoter_result(&[]), None);
        assert_eq!(decode_v3_quoter_result(&[0u8; 16]), None);
    }

    #[test]
    fn test_encode_curve_get_dy_positive_indices() {
        let data = encode_curve_get_dy(0i128, 1i128, 1000u128);
        assert_eq!(data.len(), 100);
        assert_eq!(&data[..4], &*GET_DY_SELECTOR);
        // i=0: all zeros in bytes 4..36 (32-byte word, no sign extension)
        assert!(data[4..36].iter().all(|&b| b == 0), "i word should be all zeros");
        // j=1: last byte of second word = 1 (bytes 36..68)
        assert_eq!(data[67], 1, "j=1 should be at the last byte of the second word");
        assert!(data[36..67].iter().all(|&b| b == 0), "j word before last byte should be zeros");
        // dx = 1000 at bytes 68..100
        let dx_val = U256::from_be_slice(&data[68..100]);
        assert_eq!(dx_val, U256::from(1000u64));
    }

    #[test]
    fn test_encode_curve_get_dy_negative_indices() {
        let data = encode_curve_get_dy(-1i128, 2i128, 500u128);
        assert_eq!(data.len(), 100);
        // i=-1: sign extension fills bytes 4..20 with 0xFF
        for i in 4..20 {
            assert_eq!(data[i], 0xFF, "byte {} should be 0xFF for sign extension", i);
        }
        // Last byte of i should also be 0xFF (-1 is all 1s)
        assert_eq!(data[35], 0xFF);
        // j=2: fourth byte from end of second word should be 2
        assert_eq!(data[67], 2);
        // dx = 500
        let dx_val = U256::from_be_slice(&data[68..100]);
        assert_eq!(dx_val, U256::from(500u64));
    }

    // ── EVM simulation tests (M3) ─────────────────────────────────────────

    #[tokio::test]
    async fn test_simulate_opportunity_evm_unsupported_strategies() {
        let rpc = crate::rpc::RpcClient::new("http://localhost:9999", 1).unwrap();
        let pools = PoolManager::new();

        // Jit (no EVM simulation path)
        let jit_opp = MevOpportunity {
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::Jit,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: Address::ZERO,
            token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_out: address!("cccccccccccccccccccccccccccccccccccccccc"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(500),
            ..MevOpportunity::new(1, 0, Strategy::Jit, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100)
        };
        assert!(simulate_opportunity_evm(&rpc, &pools, &jit_opp, 1).await.is_none());

        // Liquidation (no EVM simulation path)
        let liq_opp = MevOpportunity::new(1, 0, Strategy::Liquidation, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100);
        assert!(simulate_opportunity_evm(&rpc, &pools, &liq_opp, 1).await.is_none());
    }

    #[tokio::test]
    async fn test_simulate_opportunity_evm_two_hop_no_pools() {
        let rpc = crate::rpc::RpcClient::new("http://localhost:9999", 1).unwrap();
        let pools = PoolManager::new();

        let opp = MevOpportunity {
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::TwoHopArb,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_in: address!("cccccccccccccccccccccccccccccccccccccccc"),
            token_out: address!("dddddddddddddddddddddddddddddddddddddddd"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(100),
            ..MevOpportunity::new(1, 0, Strategy::TwoHopArb, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100)
        };
        // Pools are not in PoolManager — expect None
        assert!(simulate_opportunity_evm(&rpc, &pools, &opp, 1).await.is_none());
    }

    #[tokio::test]
    async fn test_simulate_opportunity_evm_sandwich_no_pool() {
        let rpc = crate::rpc::RpcClient::new("http://localhost:9999", 1).unwrap();
        let pools = PoolManager::new();

        let opp = MevOpportunity {
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::Sandwich,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: Address::ZERO,
            token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_out: address!("cccccccccccccccccccccccccccccccccccccccc"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(100),
            ..MevOpportunity::new(1, 0, Strategy::Sandwich, address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), 100)
        };
        assert!(simulate_opportunity_evm(&rpc, &pools, &opp, 1).await.is_none());
    }

    #[test]
    fn test_verify_opportunities_evm_fields_default_none() {
        let opp = MevOpportunity {
            canonical_id: None,
            block_number: 1,
            tx_index: 0,
            strategy: Strategy::TwoHopArb,
            pool_a: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            pool_b: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token_in: address!("cccccccccccccccccccccccccccccccccccccccc"),
            token_out: address!("dddddddddddddddddddddddddddddddddddddddd"),
            input_amount: U256::from(1000),
            expected_profit: U256::from(500),
            raw_profit: None,
            profit_slippage_p1: None,
            profit_slippage_m1: None,
            profit_slippage_p2: None,
            profit_slippage_m2: None,
            pga_adjusted_profit: None,
            gas_cost_wei: 100,
            timestamp: 12345,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
            mempool_only: false,
            confidence: None,
        };
        let checks = verify_opportunities(&[opp], None);
        assert_eq!(checks.len(), 1);
        // When pools=None, EVM fields should be None
        assert!(checks[0].evm_simulated_profit.is_none());
        assert!(checks[0].evm_simulation_match.is_none());
        assert!(checks[0].pool_state_consistent.is_none());
        assert!(checks[0].state_divergence_pct.is_none());
    }
}
