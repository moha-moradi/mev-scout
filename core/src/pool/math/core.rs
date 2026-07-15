//! Uniswap V2/V3 AMM math: constant-product formulas, optimal arbitrage amounts, multi-hop routing,
//! and unified `quote_exact_in` dispatcher for all pool types.

use alloy::primitives::Address;
use crate::pool::state::{PoolState, UniswapV3PoolState};
use super::v3::quote_v3_exact_in;
use super::curve;
use super::balancer;
use super::lb;
use super::pendle;

/// Unified single-pool quoting dispatch.
///
/// Routes to the correct quoting function based on pool type and variant.
/// This is the single entry point for all exact-input quotes across all DEX types.
/// New pool variants only need to be handled in this function.
pub fn quote_exact_in(
    pool: &PoolState,
    token_in: Address,
    token_out: Address,
    amount_in: u128,
) -> Option<u128> {
    if amount_in == 0 {
        return None;
    }
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
        PoolState::UniswapV4(v4) => {
            let v3: UniswapV3PoolState = v4.clone().into();
            let zero_for_one = v4.info.token0 == token_in;
            if !zero_for_one && v4.info.token1 != token_in {
                return None;
            }
            quote_v3_exact_in(&v3, amount_in, zero_for_one)
        }
        PoolState::Curve(curve) => {
            if !curve.token_index.contains_key(&token_in) || !curve.token_index.contains_key(&token_out) {
                return None;
            }
            curve::curve_output_amount(amount_in, curve, token_in, token_out)
        }
        PoolState::Balancer(bal) => {
            if !bal.token_index.contains_key(&token_in) || !bal.token_index.contains_key(&token_out) {
                return None;
            }
            balancer::balancer_quote_exact_in(amount_in, bal, token_in, token_out)
        }
        PoolState::TraderJoeLB(lb) => {
            let (reserve_in, reserve_out) = if lb.info.token0 == token_in {
                (lb.reserve_x, lb.reserve_y)
            } else if lb.info.token1 == token_in {
                (lb.reserve_y, lb.reserve_x)
            } else {
                return None;
            };
            lb::lb_output_amount(amount_in, reserve_in, reserve_out, lb.info.fee)
        }
        PoolState::Dodo(_) | PoolState::Clipper(_) => None,
        PoolState::Pendle(p) => {
            let (total_in, total_out) = if p.info.token0 == token_in {
                (p.total_pt, p.total_sy)
            } else if p.info.token1 == token_in {
                (p.total_sy, p.total_pt)
            } else {
                return None;
            };
            pendle::pendle_output_amount(amount_in, total_in, total_out)
        }
    }
}

/// Compute output amount for a given input amount under constant product.
///
/// Implements the Uniswap V2 AMM formula with fee:
/// `dx * (10000 - fee) * reserve_out / (reserve_in * 10000 + dx * (10000 - fee))`
///
/// Returns `None` if the input is zero, reserves are depleted, or the output rounds to zero.
pub fn constant_product_output_amount(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee: u32,
) -> Option<u128> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return None;
    }
    let fee_factor = 10000u128 - fee as u128;
    let amount_in_with_fee = amount_in.checked_mul(fee_factor)?;
    let numerator = amount_in_with_fee.checked_mul(reserve_out)?;
    let denominator = reserve_in.checked_mul(10000u128)?.checked_add(amount_in_with_fee)?;
    let output = numerator / denominator;
    if output == 0 {
        return None;
    }
    Some(output)
}

/// Compute required input amount for a desired output amount.
///
/// Uses the same constant-product formula as `constant_product_output_amount`
/// but solves for the input. Always rounds up to avoid undershooting.
pub fn constant_product_input_amount(
    amount_out: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee: u32,
) -> Option<u128> {
    if amount_out == 0 || reserve_in == 0 || reserve_out == 0 || amount_out >= reserve_out {
        return None;
    }
    let fee_factor = 10000u128 - fee as u128;
    let numerator = reserve_in.checked_mul(amount_out)?.checked_mul(10000u128)?;
    let denominator = (reserve_out.checked_sub(amount_out)?).checked_mul(fee_factor)?;
    let input = numerator / denominator;
    if input == 0 {
        return None;
    }
    Some(input + 1) // round up
}

/// Result of an optimal two-hop arbitrage calculation.
#[derive(Debug, Clone, Copy)]
pub struct TwoHopArbResult {
    pub input_amount: u128,
    pub intermediate_amount: u128,
    pub output_amount: u128,
    pub profit: u128,
}

/// Find the optimal input amount that maximizes profit for a two-hop arbitrage
/// between two constant-product pools sharing a common token.
///
/// Direction: buy `shared_token` from `pool_a` (spending `token_in`),
/// then sell `shared_token` to `pool_b` (receiving `token_out` back).
///
/// Uses ternary search over the concave profit function.
///
/// Returns `None` if the price gap is too small to cover fees (profit <= 0).
pub fn optimal_two_hop_arb(
    pool_a_reserve_in: u128,
    pool_a_reserve_out: u128,
    pool_a_fee: u32,
    pool_b_reserve_in: u128,
    pool_b_reserve_out: u128,
    pool_b_fee: u32,
) -> Option<TwoHopArbResult> {
    // Maximum input limited by the smaller pool reserve
    let max_input = pool_a_reserve_in.min(pool_b_reserve_out);
    if max_input < 2 {
        return None;
    }

    let mut lo = 0u128;
    let mut hi = max_input;
    let mut best: Option<TwoHopArbResult> = None;

    for _ in 0..80 {
        let m1 = lo + (hi - lo) / 3;
        let m2 = hi - (hi - lo) / 3;

        if m1 == m2 {
            break;
        }

        let p1 = simulate_two_hop(
            m1,
            pool_a_reserve_in, pool_a_reserve_out, pool_a_fee,
            pool_b_reserve_in, pool_b_reserve_out, pool_b_fee,
        );
        let p2 = simulate_two_hop(
            m2,
            pool_a_reserve_in, pool_a_reserve_out, pool_a_fee,
            pool_b_reserve_in, pool_b_reserve_out, pool_b_fee,
        );

        match (p1, p2) {
            (None, None) => break,
            (Some(_), None) => { hi = m2; }
            (None, Some(_)) => { lo = m1; }
            (Some(r1), Some(r2)) => {
                if r1.profit >= r2.profit {
                    hi = m2;
                    best = Some(r1);
                } else {
                    lo = m1;
                    best = Some(r2);
                }
            }
        }
    }

    best
}

fn simulate_two_hop(
    input_amount: u128,
    r_a_in: u128, r_a_out: u128, fee_a: u32,
    r_b_in: u128, r_b_out: u128, fee_b: u32,
) -> Option<TwoHopArbResult> {
    // Swap 1: buy intermediate token from pool A
    let intermediate = constant_product_output_amount(input_amount, r_a_in, r_a_out, fee_a)?;
    // Swap 2: sell intermediate to pool B for token_out
    let output = constant_product_output_amount(intermediate, r_b_in, r_b_out, fee_b)?;
    if output <= input_amount {
        return None;
    }
    Some(TwoHopArbResult {
        input_amount,
        intermediate_amount: intermediate,
        output_amount: output,
        profit: output - input_amount,
    })
}

/// Evaluate profit at a given input. Returns 0 if quote fails or no profit.
fn eval_profit(input: u128, quote_fn: &impl Fn(u128) -> Option<u128>) -> u128 {
    quote_fn(input)
        .filter(|&output| output > input)
        .map(|output| output - input)
        .unwrap_or(0)
}

/// Single golden-section search pass on the profit function in [lo, hi].
///
/// Returns the most profitable input point found, or `None` if none is profitable.
fn golden_section_maximize(
    mut lo: u128,
    mut hi: u128,
    quote_fn: &impl Fn(u128) -> Option<u128>,
    max_iter: usize,
) -> Option<u128> {
    if lo >= hi {
        return None;
    }

    let inv_phi = 0.618033988749895f64;

    let mut x1 = hi - ((hi - lo) as f64 * inv_phi) as u128;
    let mut x2 = lo + ((hi - lo) as f64 * inv_phi) as u128;

    if x1 <= lo { x1 = lo + 1; }
    if x2 >= hi { x2 = hi - 1; }
    if x1 >= x2 {
        let p = eval_profit(lo.max(1), quote_fn);
        return if p > 0 { Some(lo.max(1)) } else { None };
    }

    let mut f1 = eval_profit(x1, quote_fn);
    let mut f2 = eval_profit(x2, quote_fn);

    for _ in 0..max_iter {
        if hi - lo <= 1 {
            break;
        }

        if f1 > f2 {
            hi = x2;
            x2 = x1;
            f2 = f1;
            x1 = hi - ((hi - lo) as f64 * inv_phi) as u128;
            if x1 <= lo { x1 = lo + 1; }
            f1 = eval_profit(x1, quote_fn);
        } else {
            lo = x1;
            x1 = x2;
            f1 = f2;
            x2 = lo + ((hi - lo) as f64 * inv_phi) as u128;
            if x2 >= hi { x2 = hi - 1; }
            f2 = eval_profit(x2, quote_fn);
        }
    }

    if f1 >= f2 && f1 > 0 { Some(x1) }
    else if f2 > 0 { Some(x2) }
    else { None }
}

/// Coarse grid scan followed by golden-section refinement and random restarts.
///
/// Samples `grid_points` evenly-spaced points in [0, max_input], picks the
/// best one, then refines with golden-section search around that region.
/// Finally, runs multiple random-restart golden-section searches to escape
/// local optima in non-convex profit landscapes (V3 step-function liquidity).
///
/// This handles non-convex profit functions (e.g. V3 with tick boundaries)
/// much better than pure ternary/golden-section search.
fn grid_plus_refine(
    max_input: u128,
    quote_fn: &impl Fn(u128) -> Option<u128>,
    grid_points: usize,
) -> Option<(u128, u128)> {
    if max_input == 0 {
        return None;
    }

    let gp = grid_points.max(3);
    let step = max_input / gp as u128;
    let mut best_input = 0u128;
    let mut best_output = 0u128;
    let mut best_profit = 0u128;

    // Phase 1: coarse grid
    for i in 0..=gp {
        let input = (i as u128).saturating_mul(step).min(max_input);
        if input == 0 {
            continue;
        }
        if let Some(output) = quote_fn(input) {
            if output > input {
                let profit = output - input;
                if profit > best_profit {
                    best_profit = profit;
                    best_input = input;
                    best_output = output;
                }
            }
        }
    }

    if best_profit == 0 {
        return None;
    }

    // Phase 2: golden-section refinement around best region
    let radius = (step / 2).max(1);
    let lo = best_input.saturating_sub(radius);
    let hi = (best_input + radius).min(max_input);

    if let Some(refined) = golden_section_maximize(lo, hi, quote_fn, 40) {
        if let Some(output) = quote_fn(refined) {
            if output > refined && output - refined > best_profit {
                best_profit = output - refined;
                best_input = refined;
                best_output = output;
            }
        }
    }

    // Phase 3: random restarts to find additional local optima (H1 fix).
    // V3 step-function liquidity creates multiple local maxima across the
    // input range; a single grid + refine can miss peaks between grid points.
    // Multiple golden-section searches from random start points provide
    // stochastic coverage of the full search space.
    let num_restarts = 5;
    for i in 0..num_restarts {
        let ratio = ((i as f64 + 1.0) * 0.618033988749895).fract();
        let start = ((max_input as f64) * ratio) as u128;
        if start == 0 || start >= max_input {
            continue;
        }
        let r_radius = max_input / 8;
        let r_lo = start.saturating_sub(r_radius).max(1);
        let r_hi = (start + r_radius).min(max_input);
        if r_lo >= r_hi {
            continue;
        }
        if let Some(x) = golden_section_maximize(r_lo, r_hi, quote_fn, 30) {
            if let Some(output) = quote_fn(x) {
                if output > x {
                    let profit = output - x;
                    if profit > best_profit {
                        best_profit = profit;
                        best_input = x;
                        best_output = output;
                    }
                }
            }
        }
    }

    Some((best_input, best_output))
}

/// General N-hop optimizer using grid + golden-section refinement.
///
/// `quote_fn(x)` returns the output amount for input `x` through the entire pool chain.
/// Returns `Some((optimal_input, output_amount))` or `None` if no profitable path found.
///
/// `output_amount` is guaranteed to be strictly greater than `optimal_input` when `Some`.
pub fn optimal_n_hop_generic(
    max_input: u128,
    quote_fn: &impl Fn(u128) -> Option<u128>,
) -> Option<(u128, u128)> {
    grid_plus_refine(max_input, quote_fn, 50)
}

/// Version of `optimal_two_hop_arb` that accepts generic quoting functions.
///
/// `quote_a(x)` returns the amount of bridge token received from pool A when spending `x` of token_in.
/// `quote_b(x)` returns the amount of `token_out` received from pool B when spending `x` of the bridge token.
///
/// Uses grid search + golden-section refinement on the profit function:
/// `profit(x) = quote_b(quote_a(x)) - x`.
/// Returns `None` when no profitable input exists (profit <= 0 for all inputs).
pub fn optimal_two_hop_arb_generic(
    max_input: u128,
    quote_a: &impl Fn(u128) -> Option<u128>,
    quote_b: &impl Fn(u128) -> Option<u128>,
) -> Option<TwoHopArbResult> {
    if max_input == 0 {
        return None;
    }

    let combined = |x: u128| -> Option<u128> {
        let mid = quote_a(x)?;
        quote_b(mid)
    };

    let (input, output) = grid_plus_refine(max_input, &combined, 50)?;
    let intermediate = quote_a(input)?;
    Some(TwoHopArbResult {
        input_amount: input,
        intermediate_amount: intermediate,
        output_amount: output,
        profit: output - input,
    })
}

