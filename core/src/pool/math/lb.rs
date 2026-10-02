//! Trader Joe V2 Liquidity Book (LB) bin math.
//!
//! LB pools use discrete bins with a configurable bin step (basis points).
//! Within the active bin, swaps follow constant-product x * y = k.
//! Cross-bin swaps aggregate liquidity across multiple bins, each at a
//! different price. This module quotes a swap within the active bin.

use super::fee::FeeTier;

/// Quote an output amount for a swap within the active bin.
///
/// Within a single bin, LB is effectively constant-product:
/// `output = amountIn * kept * reserveOut / (reserveIn * feeDen + amountIn * kept)`
/// where `(kept, feeDen)` is the fee tier's kept fraction (e.g. 9970/10000).
///
/// This is conservative: it only considers the active bin's reserves.
/// Cross-bin liquidity provides additional depth, so the actual output
/// may be higher for large swaps.
pub fn lb_output_amount(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee: FeeTier,
) -> Option<u128> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return None;
    }
    let (fee_factor, fee_den) = fee.kept_fraction();
    let amount_in_eff = amount_in.checked_mul(fee_factor)?;
    let numerator = amount_in_eff.checked_mul(reserve_out)?;
    let denominator = reserve_in
        .checked_mul(fee_den)?
        .checked_add(amount_in_eff)?;
    let output = numerator / denominator;
    if output == 0 {
        return None;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_basic() {
        let out = lb_output_amount(1000, 10000, 10000, FeeTier::Bps(30)).unwrap();
        // fee_factor = 9970, eff_in = 9970000
        // num = 9970000 * 10000 = 99700000000
        // den = 10000 * 10000 + 9970000 = 109970000
        // out = 99700000000 / 109970000 ≈ 906
        assert!(out > 900);
        assert!(out < 910);
    }
}
