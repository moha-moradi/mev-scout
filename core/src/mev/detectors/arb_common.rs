//! Shared arbitrage-detection helpers used by both front-ends: the analytical
//! [`super::two_hop`] detector and the numeric [`super::multi_hop`] detector.
//!
//! Unifies the duplicated opportunity builder, profit normalization (C5),
//! slippage evaluation, dominant-DEX gas blend (H7), per-block dedup gate and
//! monotone-quote inversion — so a profit-normalization bug stays in one place
//! instead of two.

use std::collections::HashMap;

use alloy::primitives::{Address, U256, U512};

use crate::dex_type::DexType;
use crate::pool::state::{check_dedup_key, PoolManager, PoolState};
use crate::types::gas::GasCalibrationSnapshot;
use crate::types::{MevOpportunity, Strategy};

/// Percentage multipliers for the ±1%/±2% slippage evaluation.
const PCT_P1: u128 = 101;
const PCT_M1: u128 = 99;
const PCT_P2: u128 = 102;
const PCT_M2: u128 = 98;

/// Profits at ±1%/±2% slippage around the optimal input.
pub(super) struct SlippageProfits {
    pub(super) p1: Option<U256>,
    pub(super) m1: Option<U256>,
    pub(super) p2: Option<U256>,
    pub(super) m2: Option<U256>,
}

/// Evaluates the profit function at ±1%/±2% of `input`. Any single evaluation
/// returning `None` is reported as `None` (unprofitable/unknowable at that size).
pub(super) fn slippage_profits(
    input: u128,
    eval: impl Fn(u128) -> Option<U256>,
) -> SlippageProfits {
    if input == 0 {
        return SlippageProfits {
            p1: None,
            m1: None,
            p2: None,
            m2: None,
        };
    }
    SlippageProfits {
        p1: eval(input.saturating_mul(PCT_P1) / 100),
        m1: eval(input.saturating_mul(PCT_M1) / 100),
        p2: eval(input.saturating_mul(PCT_P2) / 100),
        m2: eval(input.saturating_mul(PCT_M2) / 100),
    }
}

/// Normalize an arbitrage profit to wrapped native when `token_in != token_out`,
/// falling back to `output_native - input_native` when direct normalization is
/// unavailable (C5). `token_in == token_out` is returned as-is (no raw variant).
pub(super) fn normalize_profit(
    pm: &PoolManager,
    token_in: Address,
    token_out: Address,
    profit: u128,
    input_amount: u128,
) -> (U256, Option<U256>) {
    if token_in == token_out {
        (U256::from(profit), None)
    } else {
        let raw = U256::from(profit);
        let native_profit = normalize_profit_native(pm, token_in, token_out, profit, input_amount)
            .unwrap_or_else(|| U256::from(profit));
        (U256::from(native_profit), Some(raw))
    }
}

/// The same C5 normalization, but `None` instead of a raw-amount fallback when
/// neither direct nor double-hop native pricing is available. Slippage probes
/// use this so an unpriceable point becomes an absent datapoint rather than a
/// phantom same-token number — `ref_input` is the optimal input whose native
/// value anchors the fallback difference.
pub(super) fn normalize_profit_native(
    pm: &PoolManager,
    token_in: Address,
    token_out: Address,
    profit: u128,
    ref_input: u128,
) -> Option<U256> {
    if token_in == token_out {
        return Some(U256::from(profit));
    }
    pm.normalize_to_native(token_out, profit)
        .or_else(|| {
            let total_output = ref_input.saturating_add(profit);
            let native_in = pm.normalize_to_native(token_in, ref_input)?;
            let native_out = pm.normalize_to_native(token_out, total_output)?;
            native_out.checked_sub(native_in)
        })
        .map(U256::from)
}

/// Fee-on-transfer filter: quotes assume the full output is received, but
/// sell-tax tokens take a cut on transfer — producing phantom opportunities.
pub(super) fn is_fot_pair(pm: &PoolManager, token_in: Address, token_out: Address) -> bool {
    pm.is_taxed_token(&token_in) || pm.is_taxed_token(&token_out)
}

/// Per-block dedup gate: returns `true` when the opportunity should be emitted.
/// The same `(pool_a, pool_b, token_in, token_out)` key is emitted at most once
/// per block unless pool reserves shift by >0.1% (H2), which clears the gate.
pub(super) fn dedup_arb(
    seen: &mut HashMap<(Address, Address, Address, Address), (u128, u128)>,
    pm: &PoolManager,
    opp: &MevOpportunity,
) -> bool {
    let key = (opp.pool_a, opp.pool_b, opp.token_in, opp.token_out);
    check_dedup_key(seen, &key, pm, opp.pool_a, opp.pool_b)
}

/// Most frequent DEX type among the participating pools (tie → UniswapV2).
pub(super) fn dominant_dex_type(counts: &HashMap<DexType, usize>) -> DexType {
    counts
        .iter()
        .max_by_key(|(_, &c)| c)
        .map(|(&d, _)| d)
        .unwrap_or(DexType::UniswapV2)
}

/// Blend a structural gas estimate with the calibrated observation for the
/// dominant DEX shape when enough samples are available (H7).
pub(super) fn blend_gas_limit(
    calibration: &GasCalibrationSnapshot,
    dex_counts: &HashMap<DexType, usize>,
    hop_count: usize,
    analytic: u64,
) -> u64 {
    calibration.blended_gas_limit(dominant_dex_type(dex_counts), hop_count, analytic)
}

/// Shared arbitrary-arbitrage opportunity fields; the front-ends supply only
/// their differentiating values (strategy, address endpoints, path).
pub(super) struct ArbOpportunityInput {
    pub(super) strategy: Strategy,
    pub(super) block_number: u64,
    pub(super) tx_index: usize,
    pub(super) timestamp: u64,
    pub(super) pool_a: Address,
    pub(super) pool_b: Address,
    pub(super) token_in: Address,
    pub(super) token_out: Address,
    pub(super) input_amount: u128,
    pub(super) expected_profit: U256,
    pub(super) raw_profit: Option<U256>,
    pub(super) slippage: SlippageProfits,
    pub(super) gas_cost_wei: u128,
    pub(super) path: Option<Vec<Address>>,
}

/// Build a fully-populated `MevOpportunity` from the shared arb fields.
pub(super) fn build_arb_opportunity(input: ArbOpportunityInput) -> MevOpportunity {
    MevOpportunity {
        canonical_id: None,
        block_number: input.block_number,
        tx_index: input.tx_index,
        strategy: input.strategy,
        pool_a: input.pool_a,
        pool_b: input.pool_b,
        token_in: input.token_in,
        token_out: input.token_out,
        input_amount: U256::from(input.input_amount),
        expected_profit: input.expected_profit,
        raw_profit: input.raw_profit,
        profit_slippage_p1: input.slippage.p1,
        profit_slippage_m1: input.slippage.m1,
        profit_slippage_p2: input.slippage.p2,
        profit_slippage_m2: input.slippage.m2,
        gas_cost_wei: input.gas_cost_wei,
        timestamp: input.timestamp,
        path: input.path,
        tick_lower: None,
        tick_upper: None,
        liquidity_amount: None,
        victim_tx_index: None,
        backrun_tx_index: None,
        mempool_only: false,
        confidence: None,
        sender: None,
        tx_hash: None,
        detection_path: Some(super::REPLAY_PATH.to_string()),
    }
}

/// Smallest x in [1, max_input] whose monotone-increasing quote reaches `target`.
pub(super) fn invert_monotone_quote(
    quote: &impl Fn(u128) -> Option<u128>,
    target: u128,
    max_input: u128,
) -> Option<u128> {
    if target == 0 {
        return None;
    }
    if quote(max_input)? < target {
        return None;
    }
    let mut lo = 1u128;
    let mut hi = max_input;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if quote(mid).unwrap_or(0) >= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(lo)
}

// ----------------------------------------------------------------------
// Spot-price prefilter (shared safety net)
// ----------------------------------------------------------------------

/// Safety margin applied on top of the combined-fee break-even in the spot
/// pre-filter: +2 bps. Absorbs integer truncation so only provably aligned
/// prices are skipped.
const PREFILTER_SAFETY_NUM: u64 = 100_002;
const PREFILTER_SAFETY_DEN: u64 = 100_000;

/// Spot-price pre-filter: returns `false` only when the marginal cycle rate is
/// *provably* below the fee break-even (plus safety margin), meaning no input
/// size can be profitable and the numeric optimizer can be skipped entirely.
///
/// Compares the marginal spot price of `shared_token` quoted in each pool's
/// other token: V2-style reserve ratios and V3 `sqrtRatioX96²`, in exact
/// integer arithmetic. Returns `true` when either price is unavailable.
///
/// Primary user is the analytical [`super::two_hop`] front-end; the numeric
/// [`super::multi_hop`] front-end also gates 2-pool cycles through it.
pub(super) fn passes_spot_prefilter(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
) -> bool {
    let (Some((cost_n, cost_d)), Some((yield_n, yield_d))) = (
        marginal_price_fraction(pool_a, shared_token),
        marginal_price_fraction(pool_b, shared_token),
    ) else {
        return true;
    };
    let (Some((fa_n, fa_d)), Some((fb_n, fb_d))) = (fee_fraction(pool_a), fee_fraction(pool_b))
    else {
        return true;
    };

    // Gross cycle rate = (yield_n·cost_d) / (yield_d·cost_n), scaled by the
    // fee factors ((fa_d−fa_n)/fa_d)·((fb_d−fb_n)/fb_d). Require it to beat
    // the safety ratio SAFETY_NUM/SAFETY_DEN:
    //   yield_n·cost_d·(fa_d−fa_n)·(fb_d−fb_n)·SAFETY_DEN
    //     > yield_d·cost_n·fa_d·fb_d·SAFETY_NUM
    let lhs = U512::from(yield_n)
        * U512::from(cost_d)
        * U512::from(fa_d - fa_n.min(fa_d))
        * U512::from(fb_d - fb_n.min(fb_d))
        * U512::from(PREFILTER_SAFETY_DEN);
    let rhs = U512::from(yield_d)
        * U512::from(cost_n)
        * U512::from(fa_d)
        * U512::from(fb_d)
        * U512::from(PREFILTER_SAFETY_NUM);
    lhs > rhs
}

/// Marginal spot price of `shared_token` expressed in the pool's other token,
/// as an exact fraction `(numerator, denominator)`:
/// - V2-family: reserve ratio of the other token over the shared-token reserve
/// - V3/V4: `sqrtPriceX96² / 2^192` (token1 per token0), pre-shifted by 2^64
///   on both sides to keep cross-multiplications compact
///
/// Returns `None` for pool types without a local closed-form spot price; the
/// caller then skips filtering for that pair.
fn marginal_price_fraction(pool: &PoolState, shared_token: Address) -> Option<(U256, U256)> {
    match pool {
        PoolState::UniswapV2(v2) => {
            if v2.reserve0 == 0 || v2.reserve1 == 0 {
                return None;
            }
            if v2.info.token0 == shared_token {
                Some((U256::from(v2.reserve1), U256::from(v2.reserve0)))
            } else if v2.info.token1 == shared_token {
                Some((U256::from(v2.reserve0), U256::from(v2.reserve1)))
            } else {
                None
            }
        }
        PoolState::TraderJoeLB(lb) => {
            if lb.reserve_x == 0 || lb.reserve_y == 0 {
                return None;
            }
            if lb.info.token0 == shared_token {
                Some((U256::from(lb.reserve_y), U256::from(lb.reserve_x)))
            } else if lb.info.token1 == shared_token {
                Some((U256::from(lb.reserve_x), U256::from(lb.reserve_y)))
            } else {
                None
            }
        }
        PoolState::Pendle(p) => {
            if p.total_pt == 0 || p.total_sy == 0 {
                return None;
            }
            if p.info.token0 == shared_token {
                Some((U256::from(p.total_sy), U256::from(p.total_pt)))
            } else if p.info.token1 == shared_token {
                Some((U256::from(p.total_pt), U256::from(p.total_sy)))
            } else {
                None
            }
        }
        PoolState::UniswapV3(_) | PoolState::UniswapV4(_) => {
            let sqrt_price_x96 = v3_style_sqrt_price_x96(pool)?;
            let (token0, token1) = pool.token_pair();
            // Guard against absurd prices (≥ 2^128 would overflow the compact
            // fraction below); skip filtering instead.
            if sqrt_price_x96.is_zero() || sqrt_price_x96 >= (U256::from(1u8) << 128usize) {
                return None;
            }
            // p01 = sqrtP²/2^192, rescaled: numerator = sqrtP² >> 128 (< 2^128),
            // denominator = 2^64 — relative truncation error ≤ 2^-64, ample for a filter.
            let s = U512::from(sqrt_price_x96);
            let sq = ((s * s) >> 128usize).to::<u128>();
            if sq == 0 {
                return None;
            }
            if token0 == shared_token {
                Some((U256::from(sq), U256::from(1u128 << 64)))
            } else if token1 == shared_token {
                Some((U256::from(1u128 << 64), U256::from(sq)))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `sqrtPriceX96` for concentrated-liquidity variants that share the V3 field
/// layout; `None` for every other pool type.
fn v3_style_sqrt_price_x96(pool: &PoolState) -> Option<U256> {
    match pool {
        PoolState::UniswapV3(p) => Some(p.sqrt_price_x96),
        PoolState::UniswapV4(p) => Some(p.sqrt_price_x96),
        PoolState::PancakeInfinity(p) => Some(p.sqrt_price_x96),
        _ => None,
    }
}

/// Pool fee as an exact fraction, unit resolved by the canonical
/// [`FeeTier`] map (see [`PoolInfo::fee_tier`]). Pendle is simulated fee-free
/// upstream. Returns `None` for pool types without a quoted fee.
fn fee_fraction(pool: &PoolState) -> Option<(u64, u64)> {
    match pool {
        PoolState::UniswapV2(_)
        | PoolState::TraderJoeLB(_)
        | PoolState::UniswapV3(_)
        | PoolState::UniswapV4(_)
        | PoolState::Pendle(_) => {
            let (num, den) = pool.info().fee_tier().fraction();
            Some((num as u64, den as u64))
        }
        _ => None,
    }
}
