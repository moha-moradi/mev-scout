//! Trader Joe V2 Liquidity Book (LB) bin math.
//!
//! LB pools use discrete bins with a configurable bin step (basis points).
//! The price of bin `i` relative to bin 0 is:
//!   price(i) = ((10000 + binStep) / 10000) ^ i
//!
//! Within the active bin, swaps follow constant-product x * y = k.
//! Cross-bin swaps aggregate liquidity across multiple bins, each at a
//! different price. This module provides:
//!   - `lb_get_price_from_id`: compute the price for a given bin ID
//!   - `lb_output_amount`: quote a swap within the active bin
//!   - `lb_max_output`: maximum output draining the active bin

use alloy::primitives::U256;

/// Compute the price of bin `active_id` relative to bin 0.
///
/// Uses integer math: `price = ((10000 + binStep) / 10000) ^ active_id`
/// Returned as a Q64.64 fixed-point number (1.0 = 2^64).
///
/// Uses Q64.64 internally to avoid U256 overflow during squaring.
/// For `active_id` values up to ~100,000, this provides sufficient precision.
pub fn lb_get_price_from_id(active_id: u32, bin_step: u32) -> u128 {
    if active_id == 0 {
        return 1u128 << 64;
    }
    // Q64.64: 1.0 = 2^64
    let base_num = 10000u128 + bin_step as u128;
    let scale = 10000u128;
    let mut result: u128 = 1u128 << 64;
    // base in Q64.64
    let mut base: u128 = ((base_num as u128) << 64) / scale;
    let mut exp = active_id;
    while exp > 0 {
        if exp & 1 == 1 {
            let product: U256 = U256::from(result) * U256::from(base);
            let shifted: U256 = product >> 64;
            result = shifted.to::<u128>();
        }
        let sq: U256 = U256::from(base) * U256::from(base);
        let shifted_sq: U256 = sq >> 64;
        base = shifted_sq.to::<u128>();
        exp >>= 1;
    }
    result
}

/// Quote an output amount for a swap within the active bin.
///
/// Within a single bin, LB is effectively constant-product:
/// `output = amountIn * (10000 - fee) * reserveOut / (reserveIn * 10000 + amountIn * (10000 - fee))`
///
/// This is conservative: it only considers the active bin's reserves.
/// Cross-bin liquidity provides additional depth, so the actual output
/// may be higher for large swaps.
pub fn lb_output_amount(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee: u32,
) -> Option<u128> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return None;
    }
    let fee_factor = 10000u128 - fee as u128;
    let amount_in_eff = amount_in.checked_mul(fee_factor)?;
    let numerator = amount_in_eff.checked_mul(reserve_out)?;
    let denominator = reserve_in.checked_mul(10000u128)?.checked_add(amount_in_eff)?;
    let output = numerator / denominator;
    if output == 0 {
        return None;
    }
    Some(output)
}

/// Maximum output draining the active bin (swap entire reserve).
///
/// Returns `reserve_out * (10000 - fee) / 10000` — the maximum extractable
/// output after fee deduction.
pub fn lb_max_output(reserve_out: u128, fee: u32) -> Option<u128> {
    if reserve_out == 0 {
        return None;
    }
    let fee_factor = 10000u128 - fee as u128;
    let output = (reserve_out as u128 * fee_factor) / 10000u128;
    if output == 0 {
        return None;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_bin0() {
        let price = lb_get_price_from_id(0, 10);
        assert_eq!(price, 1u128 << 64);
    }

    #[test]
    fn test_price_bin1_1bp() {
        let price = lb_get_price_from_id(1, 1);
        // 10001/10000 = 1.0001 in Q64
        let expected = (10001u128 << 64) / 10000u128;
        assert!((price.abs_diff(expected)) < 1u128 << 30);
    }

    #[test]
    fn test_output_basic() {
        let out = lb_output_amount(1000, 10000, 10000, 30).unwrap();
        // fee_factor = 9970, eff_in = 9970000
        // num = 9970000 * 10000 = 99700000000
        // den = 10000 * 10000 + 9970000 = 109970000
        // out = 99700000000 / 109970000 ≈ 906
        assert!(out > 900);
        assert!(out < 910);
    }

    #[test]
    fn test_max_output() {
        let out = lb_max_output(10000, 30).unwrap();
        assert_eq!(out, 9970);
    }
}
