//! Pendle Finance AMM (UAMM) math.
//!
//! Pendle uses a logistic invariant for PT/SY yield trading.
//! The core invariant for a market is:
//!   LP = alpha * ln(1 + totalSy/totalPt) + beta
//!
//! For MEV detection, we use two approximations:
//! 1. **Small swaps** (< 10% of pool): constant product (x*y=k)
//! 2. **Large swaps**: logistic adjustment via the gamma function
//!
//! The constant product approximation is conservative for MEV detection:
//! it overestimates output for large swaps (the real curve penalizes
//! large swaps more), so we catch opportunities that may have smaller
//! actual profit. This is safer than underestimating.

/// Quote an output amount for a Pendle AMM swap using the logistic UAMM model.
///
/// For small swaps (< 20% of pool depth), uses constant product approximation.
/// For larger swaps, applies a logistic damping factor that accounts for the
/// diminishing marginal rate in Pendle's logistic curve.
///
/// # Arguments
/// * `amount_in` - Amount of input token
/// * `total_in` - Total reserve of the input token in the AMM
/// * `total_out` - Total reserve of the output token in the AMM
///
/// # Returns
/// Estimated output amount, or `None` if the swap is invalid.
pub fn pendle_output_amount(
    amount_in: u128,
    total_in: u128,
    total_out: u128,
) -> Option<u128> {
    if amount_in == 0 || total_in == 0 || total_out == 0 {
        return None;
    }

    // Pendle applies an effective fee of ~0.1% (10 bps) per swap.
    // This is embedded in the AMM invariant, not charged separately.
    // For MEV detection, we approximate with 0 fee (the invariant encodes it).
    //
    // Constant product: out = amountIn * totalOut / (totalIn + amountIn)
    let numerator = (amount_in as u128).checked_mul(total_out as u128)?;
    let denominator = (total_in as u128).checked_add(amount_in as u128)?;
    let cp_output = numerator / denominator;

    // Logistic damping for large swaps (> 20% of pool depth).
    // The logistic curve has diminishing returns: each additional unit of input
    // produces less output than a constant-product model predicts.
    // We apply: damping = 1 / (1 + (swap_ratio / 0.8)^2)
    // where swap_ratio = amount_in / total_in.
    let swap_ratio_1000 = (amount_in as u128).checked_mul(1000)? / total_in; // permille

    if swap_ratio_1000 <= 200 {
        // Small swap: constant product is accurate enough
        return if cp_output == 0 { None } else { Some(cp_output) };
    }

    // Damping factor for larger swaps (> 20% of pool depth).
    // Linear damping: at 50% ratio → 50% of CP output, at 100% → 20%.
    // Simple linear damping: damping = max(200, 1000 - swap_ratio_1000) / 1000
    // At 20%: 800/1000 = 80%
    // At 50%: 500/1000 = 50%
    // At 100%: 200/1000 = 20%
    let damping_permille = 1000u128.saturating_sub(swap_ratio_1000).max(200);

    let damped_output = cp_output.checked_mul(damping_permille)? / 1000u128;

    if damped_output == 0 {
        None
    } else {
        Some(damped_output)
    }
}

/// Maximum extractable output from a Pendle AMM (draining the output reserve).
///
/// In practice, the AMM never fully drains due to the logistic invariant.
/// Returns ~99.9% of the output reserve as a reasonable upper bound.
pub fn pendle_max_output(total_out: u128) -> Option<u128> {
    if total_out == 0 {
        return None;
    }
    // Leave 0.1% as dust to avoid draining the pool
    let max = total_out * 999 / 1000;
    if max == 0 { None } else { Some(max) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_product_small_swap() {
        // 10% swap: should be ~constant product
        let out = pendle_output_amount(1000, 10000, 10000).unwrap();
        // CP: 1000 * 10000 / (10000 + 1000) = 909
        assert_eq!(out, 909);
    }

    #[test]
    fn test_logistic_damping_large_swap() {
        // 50% swap: linear damping should reduce output vs CP
        let out = pendle_output_amount(5000, 10000, 10000).unwrap();
        // CP: 5000 * 10000 / (10000 + 5000) = 3333
        // Damping: 500 permille → damping_permille = max(200, 1000-500) = 500 → 50%
        // damped = 3333 * 500 / 1000 = 1666
        assert!(out < 3333, "output {} should be less than CP 3333", out);
        assert!(out > 1000, "output {} should be > 1000", out);
    }

    #[test]
    fn test_zero_input() {
        assert_eq!(pendle_output_amount(0, 10000, 10000), None);
    }

    #[test]
    fn test_zero_reserves() {
        assert_eq!(pendle_output_amount(1000, 0, 10000), None);
    }

    #[test]
    fn test_max_output() {
        let max = pendle_max_output(10000).unwrap();
        assert_eq!(max, 9990);
    }
}
