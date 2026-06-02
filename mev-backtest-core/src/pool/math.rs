// Uniswap V2 constant-product AMM math.
// All reverse calculations use integer arithmetic with fee denominator 10000.

/// Compute output amount for a given input amount under constant product.
/// amount_in: input token amount (wei)
/// reserve_in: pool reserve of input token
/// reserve_out: pool reserve of output token
/// fee: fee in basis points (e.g. 30 = 0.3%)
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

/// Find the optimal input amount for a two-hop arbitrage using generic quoting functions.
///
/// `quote_a(x)`: returns the amount of bridge token received from pool A when spending `x` of token_in.
/// `quote_b(x)`: returns the amount of token_out received from pool B when spending `x` of the bridge token.
///
/// Uses ternary search over the profit function: `profit(x) = quote_b(quote_a(x)) - x`.
pub fn optimal_two_hop_arb_generic(
    max_input: u128,
    quote_a: &impl Fn(u128) -> Option<u128>,
    quote_b: &impl Fn(u128) -> Option<u128>,
) -> Option<TwoHopArbResult> {
    if max_input == 0 {
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

        let p1 = simulate_two_hop_generic(m1, quote_a, quote_b);
        let p2 = simulate_two_hop_generic(m2, quote_a, quote_b);

        match (p1, p2) {
            (None, None) => break,
            (Some(_), None) => hi = m2,
            (None, Some(_)) => lo = m1,
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

fn simulate_two_hop_generic(
    input_amount: u128,
    quote_a: &impl Fn(u128) -> Option<u128>,
    quote_b: &impl Fn(u128) -> Option<u128>,
) -> Option<TwoHopArbResult> {
    let intermediate = quote_a(input_amount)?;
    let output = quote_b(intermediate)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_amount_basic() {
        let out = constant_product_output_amount(1000, 1_000_000, 1_000_000, 30).unwrap();
        assert!(out > 0);
        assert!(out < 1000); // fee reduces output
    }

    #[test]
    fn test_output_amount_zero_input() {
        assert!(constant_product_output_amount(0, 1000, 1000, 30).is_none());
    }

    #[test]
    fn test_output_amount_zero_reserve() {
        assert!(constant_product_output_amount(100, 0, 1000, 30).is_none());
    }

    #[test]
    fn test_input_amount_rounds_up() {
        let out = constant_product_output_amount(1000, 1_000_000, 1_000_000, 30).unwrap();
        let back = constant_product_input_amount(out, 1_000_000, 1_000_000, 30).unwrap();
        assert!(back >= 1000);
    }

    #[test]
    fn test_input_amount_rejects_full_reserve() {
        assert!(constant_product_input_amount(1_000_000, 1_000_000, 1_000_000, 30).is_none());
    }

    #[test]
    fn test_optimal_two_hop_arb_no_profit() {
        // Identical pools with no price difference = no arbitrage
        let result = optimal_two_hop_arb(1_000_000, 1_000_000, 30, 1_000_000, 1_000_000, 30);
        assert!(result.is_none() || result.unwrap().profit == 0);
    }

    #[test]
    fn test_optimal_two_hop_arb_profitable() {
        // Two pools trading the same pair (USDC/WMATIC) at different prices.
        // Pool A: USDC=1_000_000, WMATIC=2_000_000 (cheap WMATIC: 0.5 USDC per WMATIC)
        // Pool B: USDC=2_000_000, WMATIC=1_000_000 (dear WMATIC: 2 USDC per WMATIC)
        // Strategy: buy cheap WMATIC from A (spend USDC), sell WMATIC to B (get USDC back)
        let result = optimal_two_hop_arb(
            1_000_000, // pool A USDC reserve (what we spend)
            2_000_000, // pool A WMATIC reserve (what we get, shared token)
            30,
            1_000_000, // pool B WMATIC reserve (what we sell, shared token)
            2_000_000, // pool B USDC reserve (what we get back)
            30,
        );
        assert!(result.is_some());
        assert!(result.unwrap().profit > 0);
    }

    #[test]
    fn test_optimal_two_hop_arb_low_liquidity() {
        let result = optimal_two_hop_arb(100, 200, 30, 200, 100, 30);
        assert!(result.is_none() || result.unwrap().profit > 0);
    }

    #[test]
    fn test_optimal_two_hop_arb_generic_no_profit() {
        // Identical pools (no price diff)
        let quote_a = |x: u128| constant_product_output_amount(x, 1_000_000, 1_000_000, 30);
        let quote_b = |x: u128| constant_product_output_amount(x, 1_000_000, 1_000_000, 30);
        let result = optimal_two_hop_arb_generic(1_000_000, &quote_a, &quote_b);
        assert!(result.is_none() || result.unwrap().profit == 0);
    }

    #[test]
    fn test_optimal_two_hop_arb_generic_profitable() {
        // Pool A sells cheap WMATIC (0.5 USDC per WMATIC)
        let quote_a = |x: u128| constant_product_output_amount(x, 1_000_000, 2_000_000, 30);
        // Pool B buys WMATIC at premium (2 USDC per WMATIC)
        let quote_b = |x: u128| constant_product_output_amount(x, 1_000_000, 2_000_000, 30);
        let result = optimal_two_hop_arb_generic(1_000_000, &quote_a, &quote_b);
        assert!(result.is_some());
        assert!(result.unwrap().profit > 0);
    }

    #[test]
    fn test_optimal_two_hop_arb_generic_zero_max_input() {
        let quote_a = |_: u128| Some(0u128);
        let quote_b = |_: u128| Some(0u128);
        assert!(optimal_two_hop_arb_generic(0, &quote_a, &quote_b).is_none());
    }
}
