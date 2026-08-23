//! Uniswap V2/V3 AMM math: constant-product formulas, optimal arbitrage amounts, multi-hop routing,
//! and unified `quote_exact_in` dispatcher for all pool types.

use super::consts::{BPS_DENOMINATOR, GOLDEN_SECTION_REFINE_ITERATIONS, N_HOP_GRID_POINTS};
use crate::pool::state::PoolState;
use alloy::primitives::{Address, U256, U512};

/// Rational approximation of the golden-ratio conjugate φ⁻¹ = 0.6180339887…
/// used for golden-section step sizing. Kept as an exact rational so interval
/// updates stay in integer arithmetic (no f64 precision loss above 2^53 wei).
const GOLDEN_STEP_NUM: u128 = 6_180_339_887;
const GOLDEN_STEP_DEN: u128 = 10_000_000_000;

/// Golden-section step size ⌊δ·φ⁻¹⌋ computed in exact integer arithmetic.
fn golden_step(delta: u128) -> u128 {
    (U256::from(delta) * U256::from(GOLDEN_STEP_NUM) / U256::from(GOLDEN_STEP_DEN))
        .try_into()
        .unwrap_or(delta / 2)
}

/// Integer square root (floor) of a 512-bit value via Newton iteration.
/// Converges monotonically from above; exact for all inputs.
fn isqrt_u512(n: U512) -> U512 {
    if n.is_zero() {
        return U512::ZERO;
    }
    let mut bits = 0usize;
    let mut v = n;
    while !v.is_zero() {
        v >>= 1usize;
        bits += 1;
    }
    let mut x = U512::from(1u8) << ((bits + 1) / 2);
    loop {
        let y = (x + n / x) >> 1usize;
        if y >= x {
            return x;
        }
        x = y;
    }
}
use super::balancer;
use super::curve;
use super::lb;
use super::pendle;
use super::v3::quote_v3_exact_in;

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
            let zero_for_one = v4.info.token0 == token_in;
            if !zero_for_one && v4.info.token1 != token_in {
                return None;
            }
            quote_v3_exact_in(v4, amount_in, zero_for_one)
        }
        PoolState::Curve(curve) => {
            if !curve.token_index.contains_key(&token_in)
                || !curve.token_index.contains_key(&token_out)
            {
                return None;
            }
            curve::curve_output_amount(amount_in, curve, token_in, token_out)
        }
        PoolState::Balancer(bal) => {
            if !bal.token_index.contains_key(&token_in) || !bal.token_index.contains_key(&token_out)
            {
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
/// `dx * (BPS_DENOMINATOR - fee) * reserve_out / (reserve_in * BPS_DENOMINATOR + dx * (BPS_DENOMINATOR - fee))`
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
    let fee_factor = BPS_DENOMINATOR - fee as u128;
    let amount_in_with_fee = amount_in.checked_mul(fee_factor)?;
    let numerator = amount_in_with_fee.checked_mul(reserve_out)?;
    let denominator = reserve_in
        .checked_mul(BPS_DENOMINATOR)?
        .checked_add(amount_in_with_fee)?;
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
    let fee_factor = BPS_DENOMINATOR - fee as u128;
    let numerator = reserve_in
        .checked_mul(amount_out)?
        .checked_mul(BPS_DENOMINATOR)?;
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
/// Uses the analytic closed-form optimum for composite constant-product swaps:
/// the composite output is `C·x / (D0 + D1·x)`, whose profit derivative has a
/// single root at `x* = (√(C·D0) − D0) / D1`. The root is computed with exact
/// integer square roots (no f64 precision loss), then the exact integer
/// simulator picks the best candidate in a small window around `x*`.
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
    let max_input = pool_a_reserve_in.min(pool_b_reserve_out);
    if max_input < 2 || pool_a_reserve_out == 0 || pool_b_reserve_in == 0 {
        return None;
    }

    let ga = U512::from(BPS_DENOMINATOR.saturating_sub(pool_a_fee as u128));
    let gb = U512::from(BPS_DENOMINATOR.saturating_sub(pool_b_fee as u128));
    let ra_o = U512::from(pool_a_reserve_out);
    let rb_o = U512::from(pool_b_reserve_out);
    let ra_i = U512::from(pool_a_reserve_in);
    let rb_i = U512::from(pool_b_reserve_in);
    let bps = U512::from(BPS_DENOMINATOR);

    // Composite output: y(x) = C·x / (D0 + D1·x) where
    //   C  = ga·gb·Ra_o·Rb_o
    //   D0 = Ra_i·Rb_i·BPS²
    //   D1 = Rb_i·BPS·ga + ga·gb·Ra_o
    // Profit P(x) = y(x) − x peaks at x* = (√(C·D0) − D0) / D1.
    // √(C·D0) factorizes into BPS · √(ga·gb) · √(Ra_o·Rb_o) · √(Ra_i·Rb_i)
    // so each square root fits in 256 bits and stays exact to ±1 unit.
    let d0 = ra_i * rb_i * bps * bps;
    let d1 = rb_i * bps * ga + ga * gb * ra_o;
    if d1.is_zero() {
        return None;
    }
    let root = bps * isqrt_u512(ga * gb) * isqrt_u512(ra_o * rb_o) * isqrt_u512(ra_i * rb_i);
    if root <= d0 {
        return None; // marginal rate cannot cover fees — no profitable trade
    }

    let x_star = (root - d0) / d1;
    let x_star = x_star.min(U512::from(max_input));

    // Evaluate the exact integer simulator around the real-valued optimum;
    // profit is concave so the discrete maximum is within ±2 of x*.
    let base = x_star.try_into().unwrap_or(max_input).min(max_input);
    let spread = (base / (1u128 << 30)).max(2).min(max_input);
    let mut best: Option<TwoHopArbResult> = None;
    for cand in [
        base.saturating_sub(spread),
        base.saturating_sub(1),
        base,
        base.saturating_add(1),
        base.saturating_add(spread),
        max_input,
    ] {
        if cand == 0 || cand > max_input {
            continue;
        }
        if let Some(r) = simulate_two_hop(
            cand,
            pool_a_reserve_in,
            pool_a_reserve_out,
            pool_a_fee,
            pool_b_reserve_in,
            pool_b_reserve_out,
            pool_b_fee,
        ) {
            match &best {
                Some(b) if b.profit >= r.profit => {}
                _ => best = Some(r),
            }
        }
    }

    best
}

fn simulate_two_hop(
    input_amount: u128,
    r_a_in: u128,
    r_a_out: u128,
    fee_a: u32,
    r_b_in: u128,
    r_b_out: u128,
    fee_b: u32,
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

    let mut x1 = hi - golden_step(hi - lo);
    let mut x2 = lo + golden_step(hi - lo);

    if x1 <= lo {
        x1 = lo + 1;
    }
    if x2 >= hi {
        x2 = hi - 1;
    }
    if x1 >= x2 {
        let p = eval_profit(lo.max(1), quote_fn);
        return (p > 0).then(|| lo.max(1));
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
            x1 = hi - golden_step(hi - lo);
            if x1 <= lo {
                x1 = lo + 1;
            }
            f1 = eval_profit(x1, quote_fn);
        } else {
            lo = x1;
            x1 = x2;
            f1 = f2;
            x2 = lo + golden_step(hi - lo);
            if x2 >= hi {
                x2 = hi - 1;
            }
            f2 = eval_profit(x2, quote_fn);
        }
    }

    if f1 >= f2 && f1 > 0 {
        Some(x1)
    } else if f2 > 0 {
        Some(x2)
    } else {
        None
    }
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

    if let Some(refined) =
        golden_section_maximize(lo, hi, quote_fn, GOLDEN_SECTION_REFINE_ITERATIONS)
    {
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
    let num_restarts = 5u32;
    for i in 0..num_restarts {
        let ratio = ((i as f64 + 1.0) * 0.618033988749895).fract();
        let start = ((max_input as f64) * ratio) as u128;
        if start == 0 || start >= max_input {
            continue;
        }
        let r_radius = max_input / 8u128;
        let r_lo = start.saturating_sub(r_radius).max(1);
        let r_hi = (start + r_radius).min(max_input);
        if r_lo >= r_hi {
            continue;
        }
        if let Some(x) =
            golden_section_maximize(r_lo, r_hi, quote_fn, GOLDEN_SECTION_REFINE_ITERATIONS)
        {
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
    grid_plus_refine(max_input, quote_fn, N_HOP_GRID_POINTS)
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

/// Deterministic segment-wise maximization.
///
/// `breakpoints` are input amounts at which a quote function changes its
/// concave piece (e.g. V3 tick-band crossings). They partition [0, max_input]
/// into brackets within which the composite profit is concave, so a
/// golden-section search per bracket is guaranteed to find each bracket's
/// optimum — making the overall result the global optimum across all brackets,
/// deterministically (no grid sampling or random restarts).
///
/// Returns `Some((optimal_input, output_amount))` for the most profitable
/// bracket, or `None` if no profitable input exists anywhere.
pub fn optimal_on_segments(
    max_input: u128,
    breakpoints: &[u128],
    quote_fn: &impl Fn(u128) -> Option<u128>,
) -> Option<(u128, u128)> {
    if max_input == 0 {
        return None;
    }

    let mut cuts: Vec<u128> = breakpoints
        .iter()
        .copied()
        .filter(|&b| b > 0 && b < max_input)
        .collect();
    cuts.sort_unstable();
    cuts.dedup();

    let mut best: Option<(u128, u128)> = None;
    let mut consider = |x: u128| {
        if let Some(output) = quote_fn(x) {
            if output > x {
                match best {
                    Some((_, bo)) if bo >= output => {}
                    _ => best = Some((x, output)),
                }
            }
        }
    };

    let mut lo = 1u128;
    for &cut in cuts.iter().chain(std::iter::once(&max_input)) {
        let hi = cut.min(max_input);
        // Bracket endpoints can themselves be optimal (e.g. exactly at a band edge)
        consider(lo);
        if hi > lo {
            consider(hi);
            if let Some(x) =
                golden_section_maximize(lo, hi, quote_fn, GOLDEN_SECTION_REFINE_ITERATIONS)
            {
                consider(x);
            }
        }
        lo = hi;
    }

    best
}

/// Two-hop optimizer over precomputed liquidity-band breakpoints (V3-aware).
///
/// Equivalent to `optimal_two_hop_arb_generic` but deterministic: instead of
/// grid + random restarts, brackets the input domain at V3 tick crossings and
/// golden-section-searches each bracket where profit is provably concave.
pub fn optimal_two_hop_arb_segmented(
    max_input: u128,
    breakpoints: &[u128],
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

    let (input, output) = optimal_on_segments(max_input, breakpoints, &combined)?;
    let intermediate = quote_a(input)?;
    Some(TwoHopArbResult {
        input_amount: input,
        intermediate_amount: intermediate,
        output_amount: output,
        profit: output - input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force reference: dense scan of the exact integer simulator.
    fn brute_force_best(
        pool_a_reserve_in: u128,
        pool_a_reserve_out: u128,
        pool_a_fee: u32,
        pool_b_reserve_in: u128,
        pool_b_reserve_out: u128,
        pool_b_fee: u32,
    ) -> Option<TwoHopArbResult> {
        let max_input = pool_a_reserve_in.min(pool_b_reserve_out);
        let step = (max_input / 20_000).max(1);
        let mut best: Option<TwoHopArbResult> = None;
        let mut x = step;
        while x <= max_input {
            if let Some(r) = simulate_two_hop(
                x,
                pool_a_reserve_in,
                pool_a_reserve_out,
                pool_a_fee,
                pool_b_reserve_in,
                pool_b_reserve_out,
                pool_b_fee,
            ) {
                match &best {
                    Some(b) if b.profit >= r.profit => {}
                    _ => best = Some(r),
                }
            }
            x += step;
        }
        best
    }

    #[test]
    fn closed_form_matches_brute_force() {
        let cases = [
            // (ra_i, ra_o, fee_a, rb_i, rb_o, fee_b) — imbalanced stables-style
            (
                1_000_000u128,
                2_000_000u128,
                30u32,
                2_000_000u128,
                500_000u128,
                30u32,
            ),
            (1_000_000, 3_000_000, 30, 1_000_000, 1_000_000, 30),
            (10_000_000, 10_050_000, 25, 8_000_000, 8_100_000, 25),
            (
                999_999_999,
                1_000_000_001,
                30,
                1_000_000_001,
                999_999_999,
                30,
            ),
            (
                50_000_000_000_000,
                120_000_000_000_000,
                997,
                80_000_000_000_000,
                30_000_000_000_000,
                996,
            ),
            // zero-fee pools
            (1_000_000, 2_000_000, 0, 2_000_000, 1_100_000, 0),
            // asymmetric fees
            (5_000_000, 9_000_000, 10, 9_000_000, 4_000_000, 3000),
        ];
        for &(ra_i, ra_o, fa, rb_i, rb_o, fb) in &cases {
            let fast = optimal_two_hop_arb(ra_i, ra_o, fa, rb_i, rb_o, fb);
            let slow = brute_force_best(ra_i, ra_o, fa, rb_i, rb_o, fb);
            assert_eq!(
                fast.is_some(),
                slow.is_some(),
                "profitability disagreement for case ({},{},{},{},{},{})",
                ra_i,
                ra_o,
                fa,
                rb_i,
                rb_o,
                fb,
            );
            if let (Some(f), Some(s)) = (fast, slow) {
                assert!(
                    f.profit >= s.profit * 99 / 100,
                    "closed-form profit {} below brute-force {} for ({},{},{},{},{},{})",
                    f.profit,
                    s.profit,
                    ra_i,
                    ra_o,
                    fa,
                    rb_i,
                    rb_o,
                    fb,
                );
            }
        }
    }

    #[test]
    fn closed_form_no_profit_when_prices_aligned() {
        assert!(optimal_two_hop_arb(1_000_000, 1_000_000, 30, 1_000_000, 1_000_000, 30).is_none());
    }

    #[test]
    fn isqrt_matches_reference() {
        assert_eq!(isqrt_u512(U512::ZERO), U512::ZERO);
        assert_eq!(isqrt_u512(U512::from(1u8)), U512::from(1u8));
        assert_eq!(isqrt_u512(U512::from(3u8)), U512::from(1u8));
        assert_eq!(isqrt_u512(U512::from(4u8)), U512::from(2u8));
        assert_eq!(isqrt_u512(U512::from(15u8)), U512::from(3u8));
        assert_eq!(isqrt_u512(U512::from(16u8)), U512::from(4u8));
        let big = U512::from(u128::MAX);
        assert_eq!(isqrt_u512(big), U512::from(18_446_744_073_709_551_615u64));
    }

    #[test]
    fn golden_step_scales_exactly() {
        assert_eq!(golden_step(0), 0);
        assert_eq!(golden_step(10), 6);
        assert_eq!(golden_step(10_000_000_000), 6_180_339_887);
        assert_eq!(
            golden_step(u128::MAX),
            (U256::from(u128::MAX) * U256::from(GOLDEN_STEP_NUM) / U256::from(GOLDEN_STEP_DEN))
                .try_into()
                .unwrap()
        );
    }
}
