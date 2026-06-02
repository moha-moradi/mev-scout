use std::collections::HashMap;

use alloy::primitives::{U256, U512};

use crate::pool::state::UniswapV3PoolState;

const MIN_TICK: i32 = -887272;
const MAX_TICK: i32 = 887272;

static MIN_SQRT_RATIO: std::sync::LazyLock<U256> =
    std::sync::LazyLock::new(|| get_sqrt_ratio_at_tick(MIN_TICK + 1));
static MAX_SQRT_RATIO: std::sync::LazyLock<U256> =
    std::sync::LazyLock::new(|| get_sqrt_ratio_at_tick(MAX_TICK - 1));

fn limbs_to_u512(lo: &[u64; 4]) -> U512 {
    U512::from_limbs([lo[0], lo[1], lo[2], lo[3], 0, 0, 0, 0])
}

fn u512_to_u256_checked(v: &U512) -> Option<U256> {
    let limbs = v.as_limbs();
    if limbs[4] != 0 || limbs[5] != 0 || limbs[6] != 0 || limbs[7] != 0 {
        return None;
    }
    Some(U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}

fn mul_div(a: U256, b: U256, d: U256) -> Option<U256> {
    if d.is_zero() {
        return None;
    }
    let a512 = limbs_to_u512(a.as_limbs());
    let b512 = limbs_to_u512(b.as_limbs());
    let d512 = limbs_to_u512(d.as_limbs());
    let product = a512 * b512;
    let result = product / d512;
    u512_to_u256_checked(&result)
}

fn mul_div_round_up(a: U256, b: U256, d: U256) -> Option<U256> {
    if d.is_zero() {
        return None;
    }
    let a512 = limbs_to_u512(a.as_limbs());
    let b512 = limbs_to_u512(b.as_limbs());
    let d512 = limbs_to_u512(d.as_limbs());
    let product = a512 * b512;
    let quotient = product / d512;
    let remainder = product % d512;
    let result = if remainder.is_zero() {
        quotient
    } else {
        quotient + limbs_to_u512(&[1, 0, 0, 0])
    };
    u512_to_u256_checked(&result)
}

pub fn get_sqrt_ratio_at_tick(tick: i32) -> U256 {
    let abs_tick = tick.unsigned_abs();
    if abs_tick > MAX_TICK as u32 {
        return U256::ZERO;
    }
    let mut ratio: U256 = if (abs_tick & 0x1) != 0 {
        U256::from(0xfffcb933bd6fad37aa2d162d1a594001u128)
    } else {
        U256::from(1u128) << 128
    };
    let one_128 = U256::from(1u128) << 128;
    if (abs_tick & 0x2) != 0 {
        ratio = mul_div(ratio, U256::from(0xfff97272373d413259a46990580e213au128), one_128).unwrap();
    }
    if (abs_tick & 0x4) != 0 {
        ratio = mul_div(ratio, U256::from(0xfff2e50f5f656932ef12357cf3c7fdccu128), one_128).unwrap();
    }
    if (abs_tick & 0x8) != 0 {
        ratio = mul_div(ratio, U256::from(0xffe5caca7e10e4e61c3624eaa0941cd0u128), one_128).unwrap();
    }
    if (abs_tick & 0x10) != 0 {
        ratio = mul_div(ratio, U256::from(0xffcb9843d60f6159c9db58835c926644u128), one_128).unwrap();
    }
    if (abs_tick & 0x20) != 0 {
        ratio = mul_div(ratio, U256::from(0xff973b41fa98c081472e6896dfb254c0u128), one_128).unwrap();
    }
    if (abs_tick & 0x40) != 0 {
        ratio = mul_div(ratio, U256::from(0xff2ea16466c96a3843ec78b326b52861u128), one_128).unwrap();
    }
    if (abs_tick & 0x80) != 0 {
        ratio = mul_div(ratio, U256::from(0xfe5dee046a99a2a811c461f1969c3053u128), one_128).unwrap();
    }
    if (abs_tick & 0x100) != 0 {
        ratio = mul_div(ratio, U256::from(0xfcbe86c7900a88aedcffc83b479aa3a4u128), one_128).unwrap();
    }
    if (abs_tick & 0x200) != 0 {
        ratio = mul_div(ratio, U256::from(0xf987a7253ac413176f2b074cf7815e54u128), one_128).unwrap();
    }
    if (abs_tick & 0x400) != 0 {
        ratio = mul_div(ratio, U256::from(0xf3392b0822b70005940c7a398e4b70f3u128), one_128).unwrap();
    }
    if (abs_tick & 0x800) != 0 {
        ratio = mul_div(ratio, U256::from(0xe7159475a2c29b7443b29c7fa6e889d9u128), one_128).unwrap();
    }
    if (abs_tick & 0x1000) != 0 {
        ratio = mul_div(ratio, U256::from(0xd097f3bdfd2022b8845ad8f792aa5825u128), one_128).unwrap();
    }
    if (abs_tick & 0x2000) != 0 {
        ratio = mul_div(ratio, U256::from(0xa9f746462d870fdf8a65dc1f90e061e5u128), one_128).unwrap();
    }
    if (abs_tick & 0x4000) != 0 {
        ratio = mul_div(ratio, U256::from(0x70d869a156d2a1b890bb3df62baf32f7u128), one_128).unwrap();
    }
    if (abs_tick & 0x8000) != 0 {
        ratio = mul_div(ratio, U256::from(0x31be135f97d08fd981231505542fcfa6u128), one_128).unwrap();
    }
    if (abs_tick & 0x10000) != 0 {
        ratio = mul_div(ratio, U256::from(0x9aa508b5b7a84e1c677de54f3e99bc9u128), one_128).unwrap();
    }
    if (abs_tick & 0x20000) != 0 {
        ratio = mul_div(ratio, U256::from(0x5d6af8dedb81196699c329225ee604u128), one_128).unwrap();
    }
    if (abs_tick & 0x40000) != 0 {
        ratio = mul_div(ratio, U256::from(0x2216e584f5fa1ea926041bedfe98u128), one_128).unwrap();
    }
    if (abs_tick & 0x80000) != 0 {
        ratio = mul_div(ratio, U256::from(0x48a170391f7dc42444e8fa2u128), one_128).unwrap();
    }
    if tick > 0 {
        ratio = U256::MAX / ratio;
    }
    // Convert from Q128 to Q96 (shift right by 32, round up)
    let shifted = ratio >> 32;
    if (ratio & U256::from(0xffffffffu64)).is_zero() {
        shifted
    } else {
        shifted + U256::from(1u64)
    }
}

fn get_amount_0_delta(
    sqrt_ratio_a_x96: U256,
    sqrt_ratio_b_x96: U256,
    liquidity: u128,
    round_up: bool,
) -> Option<U256> {
    let (low, high) = if sqrt_ratio_a_x96 > sqrt_ratio_b_x96 {
        (sqrt_ratio_b_x96, sqrt_ratio_a_x96)
    } else {
        (sqrt_ratio_a_x96, sqrt_ratio_b_x96)
    };
    if low.is_zero() {
        return None;
    }
    let numerator1 = U256::from(liquidity) << 96;
    let numerator2 = high - low;
    let intermediate = mul_div(numerator1, numerator2, high)?;
    if round_up {
        Some((intermediate + low - U256::from(1u64)) / low)
    } else {
        Some(intermediate / low)
    }
}

fn get_amount_1_delta(
    sqrt_ratio_a_x96: U256,
    sqrt_ratio_b_x96: U256,
    liquidity: u128,
    round_up: bool,
) -> Option<U256> {
    let (low, high) = if sqrt_ratio_a_x96 > sqrt_ratio_b_x96 {
        (sqrt_ratio_b_x96, sqrt_ratio_a_x96)
    } else {
        (sqrt_ratio_a_x96, sqrt_ratio_b_x96)
    };
    let numerator = U256::from(liquidity) * (high - low);
    let denominator = U256::from(1u128 << 96);
    if round_up {
        mul_div_round_up(numerator, U256::from(1u64), denominator)
    } else {
        mul_div(numerator, U256::from(1u64), denominator)
    }
}

fn get_next_sqrt_price_from_input(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount_in: U256,
    zero_for_one: bool,
) -> Option<U256> {
    if liquidity == 0 || amount_in.is_zero() {
        return None;
    }
    if zero_for_one {
        let numerator1 = U256::from(liquidity) << 96;
        let numerator2 = amount_in * sqrt_price_x96;
        let denominator = numerator1 + numerator2;
        if denominator <= numerator1 {
            return None;
        }
        mul_div(numerator1, sqrt_price_x96, denominator)
    } else {
        let amount_in_ratio = mul_div(amount_in, U256::from(1u128 << 96), U256::from(liquidity))?;
        if amount_in_ratio.is_zero() {
            return None;
        }
        Some(sqrt_price_x96 + amount_in_ratio)
    }
}

fn compute_swap_step(
    sqrt_ratio_current_x96: U256,
    sqrt_ratio_target_x96: U256,
    liquidity: u128,
    amount_remaining: U256,
    fee_bps: u32,
) -> (U256, U256, U256, U256) {
    let zero_for_one = sqrt_ratio_target_x96 < sqrt_ratio_current_x96;

    let max_in = if zero_for_one {
        get_amount_0_delta(
            sqrt_ratio_target_x96,
            sqrt_ratio_current_x96,
            liquidity,
            true,
        )
    } else {
        get_amount_1_delta(
            sqrt_ratio_current_x96,
            sqrt_ratio_target_x96,
            liquidity,
            true,
        )
    }
    .unwrap_or(U256::ZERO);

    let fee_on_max =
        mul_div_round_up(max_in, U256::from(fee_bps as u64), U256::from(1_000_000u64))
            .unwrap_or(U256::ZERO);
    let total_max_cost = max_in + fee_on_max;

    if amount_remaining >= total_max_cost {
        let amount_out = if zero_for_one {
            get_amount_1_delta(
                sqrt_ratio_target_x96,
                sqrt_ratio_current_x96,
                liquidity,
                false,
            )
        } else {
            get_amount_0_delta(
                sqrt_ratio_current_x96,
                sqrt_ratio_target_x96,
                liquidity,
                false,
            )
        }
        .unwrap_or(U256::ZERO);

        (sqrt_ratio_target_x96, max_in, amount_out, fee_on_max)
    } else {
        let remaining = amount_remaining;
        let fee_amount = mul_div_round_up(
            remaining,
            U256::from(fee_bps as u64),
            U256::from(1_000_000u64),
        )
        .unwrap_or(U256::ZERO);
        let amount_after_fee = remaining - fee_amount.min(remaining);

        let next_sqrt = get_next_sqrt_price_from_input(
            sqrt_ratio_current_x96,
            liquidity,
            amount_after_fee,
            zero_for_one,
        )
        .unwrap_or(sqrt_ratio_current_x96);

        let amount_out = if zero_for_one {
            get_amount_1_delta(next_sqrt, sqrt_ratio_current_x96, liquidity, false)
        } else {
            get_amount_0_delta(sqrt_ratio_current_x96, next_sqrt, liquidity, false)
        }
        .unwrap_or(U256::ZERO);

        (next_sqrt, amount_after_fee, amount_out, fee_amount)
    }
}

fn find_next_initialized_tick(
    ticks: &HashMap<i32, i128>,
    current_tick: i32,
    zero_for_one: bool,
) -> Option<i32> {
    let mut best: Option<i32> = None;
    if zero_for_one {
        for (&t, &liq) in ticks {
            if liq != 0 && t < current_tick {
                match best {
                    None => best = Some(t),
                    Some(b) => {
                        if t > b {
                            best = Some(t);
                        }
                    }
                }
            }
        }
    } else {
        for (&t, &liq) in ticks {
            if liq != 0 && t > current_tick {
                match best {
                    None => best = Some(t),
                    Some(b) => {
                        if t < b {
                            best = Some(t);
                        }
                    }
                }
            }
        }
    }
    best
}

#[allow(dead_code)]
fn get_tick_spacing_from_fee(fee_bps: u32) -> i32 {
    if fee_bps <= 100 {
        1
    } else if fee_bps <= 500 {
        10
    } else if fee_bps <= 3000 {
        60
    } else {
        200
    }
}

#[allow(dead_code)]
fn get_tick_at_sqrt_ratio(sqrt_price_x96: U256) -> i32 {
    if sqrt_price_x96 < *MIN_SQRT_RATIO || sqrt_price_x96 > *MAX_SQRT_RATIO {
        return 0;
    }
    let mut low = MIN_TICK;
    let mut high = MAX_TICK;
    while low <= high {
        let mid = low + (high - low) / 2;
        let mid_ratio = get_sqrt_ratio_at_tick(mid);
        if mid_ratio == sqrt_price_x96 {
            return mid;
        }
        if mid_ratio < sqrt_price_x96 {
            low = mid + 1;
        } else {
            high = mid - 1;
        }
    }
    high
}

pub fn quote_v3_exact_in(
    pool: &UniswapV3PoolState,
    amount_in: u128,
    zero_for_one: bool,
) -> Option<u128> {
    if amount_in == 0 || pool.liquidity == 0 || pool.sqrt_price_x96.is_zero() {
        return None;
    }

    let mut sqrt_price = pool.sqrt_price_x96;
    let mut current_tick = pool.tick;
    let mut liquidity = pool.liquidity;
    let mut amount_remaining = U256::from(amount_in);
    let mut total_amount_out = U256::ZERO;

    while amount_remaining > U256::ZERO {
        let next_tick = find_next_initialized_tick(&pool.ticks, current_tick, zero_for_one);

        let target_sqrt_price = match next_tick {
            Some(t) => {
                let r = get_sqrt_ratio_at_tick(t);
                if zero_for_one {
                    r.max(*MIN_SQRT_RATIO).min(sqrt_price)
                } else {
                    r.min(*MAX_SQRT_RATIO).max(sqrt_price)
                }
            }
            None => {
                if zero_for_one {
                    *MIN_SQRT_RATIO
                } else {
                    *MAX_SQRT_RATIO
                }
            }
        };

        if target_sqrt_price == sqrt_price {
            break;
        }

        let (next_sqrt_price, amount_in_step, amount_out_step, fee_step) = compute_swap_step(
            sqrt_price,
            target_sqrt_price,
            liquidity,
            amount_remaining,
            pool.info.fee,
        );

        total_amount_out += amount_out_step;

        let consumed = amount_in_step + fee_step;
        if consumed >= amount_remaining {
            amount_remaining = U256::ZERO;
        } else {
            amount_remaining -= consumed;
        }

        if next_sqrt_price == target_sqrt_price {
            current_tick = if zero_for_one {
                next_tick.unwrap_or(MIN_TICK)
            } else {
                next_tick.unwrap_or(MAX_TICK)
            };
            if let Some(liq_delta) = pool.ticks.get(&next_tick.unwrap_or(0)) {
                let delta = *liq_delta;
                if zero_for_one {
                    liquidity = if delta > 0 {
                        liquidity.saturating_sub(delta as u128)
                    } else {
                        liquidity.saturating_add((-delta) as u128)
                    };
                } else {
                    liquidity = if delta > 0 {
                        liquidity.saturating_add(delta as u128)
                    } else {
                        liquidity.saturating_sub((-delta) as u128)
                    };
                }
            }
        }

        sqrt_price = next_sqrt_price;
    }

    if total_amount_out.is_zero() {
        return None;
    }
    let limbs = total_amount_out.as_limbs();
    Some(limbs[0] as u128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::dex_type::DexType;
    use crate::pool::state::PoolInfo;

    fn make_pool(
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: u128,
        fee: u32,
        tick_spacing: Option<u32>,
        ticks: HashMap<i32, i128>,
    ) -> UniswapV3PoolState {
        UniswapV3PoolState {
            info: PoolInfo {
                address: alloy::primitives::Address::ZERO,
                pool_type: "uniswap_v3".into(),
                token0: alloy::primitives::Address::ZERO,
                token1: alloy::primitives::Address::ZERO,
                fee,
                name: None,
                dex_type: DexType::UniswapV3,
                tick_spacing,
            },
            sqrt_price_x96,
            tick,
            liquidity,
            ticks,
        }
    }

    #[test]
    fn test_get_sqrt_ratio_at_tick_zero() {
        let ratio = get_sqrt_ratio_at_tick(0);
        assert_eq!(ratio, U256::from(1u128 << 96));
    }

    #[test]
    fn test_get_sqrt_ratio_at_tick_positive() {
        let r0 = get_sqrt_ratio_at_tick(0);
        let r1 = get_sqrt_ratio_at_tick(1);
        assert!(r1 > r0);
    }

    #[test]
    fn test_get_sqrt_ratio_at_tick_negative() {
        let r0 = get_sqrt_ratio_at_tick(0);
        let rn1 = get_sqrt_ratio_at_tick(-1);
        assert!(rn1 < r0);
    }

    #[test]
    fn test_get_sqrt_ratio_reciprocal() {
        let pos = get_sqrt_ratio_at_tick(100);
        let neg = get_sqrt_ratio_at_tick(-100);
        let p_512 = limbs_to_u512(&pos.as_limbs()) * limbs_to_u512(&neg.as_limbs());
        let one = limbs_to_u512(&[1, 0, 0, 0]);
        let two_192 = (one << 96) * (one << 96);
        // Product should be approximately 2^192 (within ±2^128)
        let diff = if p_512 > two_192 { p_512 - two_192 } else { two_192 - p_512 };
        let max_diff = one << 128;
        assert!(diff < max_diff,
            "product/2^192 should be ≈ 1, rounding error too large");
        // Consistent monotonic behavior
        assert!(pos > get_sqrt_ratio_at_tick(99));
        assert!(neg < get_sqrt_ratio_at_tick(-99));
    }

    #[test]
    fn test_quote_no_liquidity() {
        let pool = make_pool(U256::from(1u128 << 96), 0, 0, 3000, Some(60), HashMap::new());
        assert!(quote_v3_exact_in(&pool, 1000, true).is_none());
        assert!(quote_v3_exact_in(&pool, 1000, false).is_none());
    }

    #[test]
    fn test_quote_no_amount() {
        let pool = make_pool(U256::from(1u128 << 96), 0, 1_000_000, 3000, Some(60), HashMap::new());
        assert!(quote_v3_exact_in(&pool, 0, true).is_none());
    }

    #[test]
    fn test_quote_basic_zero_for_one() {
        let sqrt_price = get_sqrt_ratio_at_tick(0);
        let pool = make_pool(sqrt_price, 0, 1_000_000_000_000u128, 3000, Some(60), HashMap::new());
        let out = quote_v3_exact_in(&pool, 100_000, true);
        assert!(out.is_some(), "should get output");
        let out_val = out.unwrap();
        assert!(out_val > 0, "output should be positive");
        assert!(out_val < 100_000, "output should be less than input due to fee/price impact");
    }

    #[test]
    fn test_quote_basic_one_for_zero() {
        let sqrt_price = get_sqrt_ratio_at_tick(0);
        let pool = make_pool(sqrt_price, 0, 1_000_000_000_000u128, 3000, Some(60), HashMap::new());
        let out = quote_v3_exact_in(&pool, 100_000, false);
        assert!(out.is_some(), "should get output");
        assert!(out.unwrap() > 0);
    }

    #[test]
    fn test_quote_small_amount() {
        let sqrt_price = get_sqrt_ratio_at_tick(0);
        let pool = make_pool(sqrt_price, 1200, 1_000_000_000_000u128, 3000, Some(60), HashMap::new());
        // With 0.3% fee, 1 wei input is fully consumed by fee → no output
        // Use enough to exceed the fee rounding threshold
        let out = quote_v3_exact_in(&pool, 10_000, true);
        assert!(out.is_some());
        assert!(out.unwrap() > 0);
        assert!(out.unwrap() < 10_000); // loss due to fee and price impact
    }

    #[test]
    fn test_get_sqrt_ratio_min_max() {
        let min = *MIN_SQRT_RATIO;
        let max = *MAX_SQRT_RATIO;
        assert!(min < max);
        assert!(min > U256::ZERO);
        assert!(max > min);

        // Verify tick -887271 produces the same as MIN_SQRT_RATIO
        let at_min_tick_plus_1 = get_sqrt_ratio_at_tick(MIN_TICK + 1);
        assert_eq!(min, at_min_tick_plus_1);
    }

    #[test]
    fn test_mul_div_basic() {
        assert_eq!(mul_div(U256::from(10u64), U256::from(20u64), U256::from(5u64)), Some(U256::from(40u64)));
        assert_eq!(mul_div(U256::from(10u64), U256::from(20u64), U256::from(0u64)), None);
    }

    #[test]
    fn test_mul_div_round_up() {
        let result = mul_div_round_up(U256::from(10u64), U256::from(3u64), U256::from(4u64));
        assert_eq!(result, Some(U256::from(8u64))); // 30/4 = 7.5 → 8
    }

    #[test]
    fn test_get_amount_0_delta_basic() {
        let sqrt_a = get_sqrt_ratio_at_tick(0);
        let sqrt_b = get_sqrt_ratio_at_tick(10);
        // For 0→10 price increase, amount1 in → amount0 out (oneForZero)
        // amount0 output = get_amount_0_delta(sqrt_a, sqrt_b, L, false)
        let liq = 1_000_000_000_000u128;
        let amount0 = get_amount_0_delta(sqrt_a, sqrt_b, liq, false);
        assert!(amount0.is_some());
        assert!(amount0.unwrap() > U256::ZERO);
    }

    #[test]
    fn test_get_amount_1_delta_basic() {
        let sqrt_a = get_sqrt_ratio_at_tick(0);
        let sqrt_b = get_sqrt_ratio_at_tick(10);
        let liq = 1_000_000_000_000u128;
        let amount1 = get_amount_1_delta(sqrt_a, sqrt_b, liq, false);
        assert!(amount1.is_some());
        assert!(amount1.unwrap() > U256::ZERO);
    }

    #[test]
    fn test_quote_with_initialized_tick_crossing() {
        let sqrt_price = get_sqrt_ratio_at_tick(0);
        let mut ticks = HashMap::new();
        // Add a tick with liquidity at tick -100
        ticks.insert(-100, 1_000_000_000i128);
        let pool = make_pool(sqrt_price, 0, 5_000_000_000_000u128, 500, Some(10), ticks);
        // zero_for_one = true: price goes down, might cross tick -100
        let out = quote_v3_exact_in(&pool, 1_000_000_000_000u128, true);
        assert!(out.is_some());
        let out_val = out.unwrap();
        assert!(out_val > 0);
        assert!(out_val < 1_000_000_000_000u128); // loss due to fee + crossing liquidity change
    }

    #[test]
    fn test_find_next_initialized_tick_down() {
        let mut ticks = HashMap::new();
        ticks.insert(-100, 1000i128);
        ticks.insert(-200, 500i128);
        ticks.insert(-50, 300i128);
        // zero_for_one (down): should find -50 (largest tick < 0, closest to 0)
        let result = find_next_initialized_tick(&ticks, 0, true);
        assert_eq!(result, Some(-50));
    }

    #[test]
    fn test_find_next_initialized_tick_up() {
        let mut ticks = HashMap::new();
        ticks.insert(100, 1000i128);
        ticks.insert(200, 500i128);
        ticks.insert(50, 300i128);
        // oneForZero (up): should find 50 (smallest tick > 0)
        let result = find_next_initialized_tick(&ticks, 0, false);
        assert_eq!(result, Some(50));
    }

    #[test]
    fn test_find_next_initialized_tick_none() {
        let ticks = HashMap::new();
        assert_eq!(find_next_initialized_tick(&ticks, 0, true), None);
        assert_eq!(find_next_initialized_tick(&ticks, 0, false), None);
    }

    #[test]
    fn test_get_tick_spacing_from_fee() {
        assert_eq!(get_tick_spacing_from_fee(100), 1);
        assert_eq!(get_tick_spacing_from_fee(500), 10);
        assert_eq!(get_tick_spacing_from_fee(3000), 60);
        assert_eq!(get_tick_spacing_from_fee(10000), 200);
    }

    #[test]
    fn test_quote_large_amount_still_works() {
        let sqrt_price = get_sqrt_ratio_at_tick(0);
        let pool = make_pool(sqrt_price, 0, u128::MAX, 3000, Some(60), HashMap::new());
        let out = quote_v3_exact_in(&pool, 1_000_000_000_000u128, true);
        assert!(out.is_some());
    }

    #[test]
    fn test_compute_swap_step_full() {
        let sqrt_current = get_sqrt_ratio_at_tick(0);
        let sqrt_target = get_sqrt_ratio_at_tick(-10);
        let liq = 1_000_000_000_000u128;
        let amount = U256::from(u128::MAX);
        let (sqrt_next, amount_in, amount_out, fee) = compute_swap_step(sqrt_current, sqrt_target, liq, amount, 3000);
        assert_eq!(sqrt_next, sqrt_target);
        assert!(amount_in > U256::ZERO);
        assert!(amount_out > U256::ZERO);
        assert!(fee > U256::ZERO);
    }

    #[test]
    fn test_compute_swap_step_partial() {
        let sqrt_current = get_sqrt_ratio_at_tick(0);
        let sqrt_target = get_sqrt_ratio_at_tick(-1000);
        let liq = 1_000_000_000_000u128;
        // Very small amount — won't reach target
        let amount = U256::from(1000u64);
        let (sqrt_next, amount_in, _amount_out, fee) = compute_swap_step(sqrt_current, sqrt_target, liq, amount, 3000);
        assert!(sqrt_next > sqrt_target);
        assert!(amount_in > U256::ZERO);
        assert!(amount_in <= amount);
        assert!(fee > U256::ZERO);
    }
}
