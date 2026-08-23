//! Uniswap V3 exact-input quoting using the geometric tick-to-sqrt-price formula.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use alloy::primitives::{U256, U512};

use super::consts::{LIQUIDITY_FRACTION_DENOM, PPM_DENOMINATOR, SQRT_RATIO_CACHE_CAPACITY};
use crate::pool::state::UniswapV3PoolState;

const MIN_TICK: i32 = -887272;
const MAX_TICK: i32 = 887272;

static MIN_SQRT_RATIO: std::sync::LazyLock<U256> =
    std::sync::LazyLock::new(|| get_sqrt_ratio_at_tick(MIN_TICK + 1));
static MAX_SQRT_RATIO: std::sync::LazyLock<U256> =
    std::sync::LazyLock::new(|| get_sqrt_ratio_at_tick(MAX_TICK - 1));

static SQRT_RATIO_CACHE: std::sync::LazyLock<Mutex<HashMap<i32, U256>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::with_capacity(SQRT_RATIO_CACHE_CAPACITY)));

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
    if let Some(cached) = SQRT_RATIO_CACHE
        .lock()
        .ok()
        .and_then(|c| c.get(&tick).copied())
    {
        return cached;
    }
    let result = compute_sqrt_ratio_at_tick(tick);
    if let Ok(mut cache) = SQRT_RATIO_CACHE.lock() {
        cache.insert(tick, result);
    }
    result
}

fn compute_sqrt_ratio_at_tick(tick: i32) -> U256 {
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
    // Uniswap V3 tick-to-sqrt-price multipliers (from Solidity reference)
    const TICK_MATH_CONSTANTS: [(u32, u128); 18] = [
        (0x2, 0xfff97272373d413259a46990580e213a),
        (0x4, 0xfff2e50f5f656932ef12357cf3c7fdcc),
        (0x8, 0xffe5caca7e10e4e61c3624eaa0941cd0),
        (0x10, 0xffcb9843d60f6159c9db58835c926644),
        (0x20, 0xff973b41fa98c081472e6896dfb254c0),
        (0x40, 0xff2ea16466c96a3843ec78b326b52861),
        (0x80, 0xfe5dee046a99a2a811c461f1969c3053),
        (0x100, 0xfcbe86c7900a88aedcffc83b479aa3a4),
        (0x200, 0xf987a7253ac413176f2b074cf7815e54),
        (0x400, 0xf3392b0822b70005940c7a398e4b70f3),
        (0x800, 0xe7159475a2c29b7443b29c7fa6e889d9),
        (0x1000, 0xd097f3bdfd2022b8845ad8f792aa5825),
        (0x2000, 0xa9f746462d870fdf8a65dc1f90e061e5),
        (0x4000, 0x70d869a156d2a1b890bb3df62baf32f7),
        (0x8000, 0x31be135f97d08fd981231505542fcfa6),
        (0x10000, 0x9aa508b5b7a84e1c677de54f3e99bc9),
        (0x20000, 0x5d6af8dedb81196699c329225ee604),
        (0x40000, 0x2216e584f5fa1ea926041bedfe98),
    ];
    for (bit, constant) in &TICK_MATH_CONSTANTS {
        if (abs_tick & bit) != 0 {
            ratio = mul_div(ratio, U256::from(*constant), one_128)
                .expect("V3 tick math constant invariant");
        }
    }
    // 0x80000 is the last multiplier (too large for 128-bit but fits in U256)
    if (abs_tick & 0x80000) != 0 {
        ratio = mul_div(ratio, U256::from(0x48a170391f7dc42444e8fa2u128), one_128)
            .expect("V3 tick math constant invariant");
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
    fee: u32,
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

    let fee_on_max = mul_div_round_up(max_in, U256::from(fee as u64), U256::from(1_000_000u64))
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
        let fee_amount =
            mul_div_round_up(remaining, U256::from(fee as u64), U256::from(1_000_000u64))
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

fn get_next_sqrt_price_from_output(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount_out: U256,
    zero_for_one: bool,
) -> Option<U256> {
    if liquidity == 0 || amount_out.is_zero() {
        return None;
    }
    if zero_for_one {
        let ratio = mul_div(amount_out, U256::from(1u128 << 96), U256::from(liquidity))?;
        if ratio >= sqrt_price_x96 {
            return None;
        }
        Some(sqrt_price_x96 - ratio)
    } else {
        let numerator: U256 = U256::from(liquidity) << 96;
        let product = amount_out * sqrt_price_x96;
        let denominator = numerator.checked_sub(product)?;
        if denominator.is_zero() {
            return None;
        }
        mul_div(numerator, sqrt_price_x96, denominator)
    }
}

fn compute_swap_step_exact_out(
    sqrt_ratio_current_x96: U256,
    sqrt_ratio_target_x96: U256,
    liquidity: u128,
    amount_remaining: U256,
    fee: u32,
) -> (U256, U256, U256, U256) {
    let zero_for_one = sqrt_ratio_target_x96 < sqrt_ratio_current_x96;

    let max_out = if zero_for_one {
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

    if amount_remaining >= max_out {
        let amount_in = if zero_for_one {
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

        let fee_amount =
            mul_div_round_up(amount_in, U256::from(fee as u64), U256::from(1_000_000u64))
                .unwrap_or(U256::ZERO);

        (sqrt_ratio_target_x96, amount_in, max_out, fee_amount)
    } else {
        let next_sqrt = get_next_sqrt_price_from_output(
            sqrt_ratio_current_x96,
            liquidity,
            amount_remaining,
            zero_for_one,
        )
        .unwrap_or(sqrt_ratio_current_x96);

        let amount_in = if zero_for_one {
            get_amount_0_delta(next_sqrt, sqrt_ratio_current_x96, liquidity, true)
        } else {
            get_amount_1_delta(sqrt_ratio_current_x96, next_sqrt, liquidity, true)
        }
        .unwrap_or(U256::ZERO);

        let fee_amount =
            mul_div_round_up(amount_in, U256::from(fee as u64), U256::from(1_000_000u64))
                .unwrap_or(U256::ZERO);

        (next_sqrt, amount_in, amount_remaining, fee_amount)
    }
}

fn find_next_initialized_tick(
    ticks: &BTreeMap<i32, i128>,
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

/// Determine the effective target sqrt price for a V3 swap, considering:
/// 1. Real initialized ticks (with known liquidity data)
/// 2. Full range when no ticks are known (no synthetic cap)
///
/// When only a synthetic boundary is available, the swap uses the full pool liquidity
/// and the quoting loop will not attempt to cross it (no phantom liquidity beyond).
fn get_swap_target(pool: &UniswapV3PoolState, zero_for_one: bool) -> (U256, bool) {
    let next_tick = find_next_initialized_tick(&pool.ticks, pool.tick, zero_for_one);
    match next_tick {
        Some(t) => {
            let r = get_sqrt_ratio_at_tick(t);
            let sqrt = if zero_for_one {
                r.max(*MIN_SQRT_RATIO).min(pool.sqrt_price_x96)
            } else {
                r.min(*MAX_SQRT_RATIO).max(pool.sqrt_price_x96)
            };
            (sqrt, true) // true = has real tick data
        }
        None => {
            // No real initialized ticks found. Go to the full range instead of
            // capping at the nearest tick_spacing boundary. This is more accurate
            // for the first-block case where tick data hasn't been bootstrapped yet
            // (C2 / M2 fixes). Without tick knowledge, using the full pool.liquidity
            // is a better estimate than truncating at one spacing interval.
            if zero_for_one {
                (*MIN_SQRT_RATIO, false)
            } else {
                (*MAX_SQRT_RATIO, false)
            }
        }
    }
}

/// Estimate gas cost for a V3 swap in the given direction, accounting for
/// initialized tick crossings. Each tick crossing costs ~25k gas on top of
/// the base swap cost (~80k). Capped at 20 crossings to avoid runaway estimates.
///
/// H7: V3 gas varies from ~80k (no tick crossing) to ~500k+ (many crossings),
/// so direction-aware estimation is essential for accurate per-opportunity gas.
pub fn estimate_v3_swap_gas(pool: &UniswapV3PoolState, zero_for_one: bool) -> u64 {
    const BASE_SWAP_GAS: u64 = 80_000;
    const PER_TICK_CROSSING_GAS: u64 = 25_000;
    const MAX_CROSSINGS: u64 = 20;

    if pool.ticks.is_empty() || pool.liquidity == 0 {
        // When no tick data, assume ~3 crossings based on tick_spacing
        return BASE_SWAP_GAS + PER_TICK_CROSSING_GAS * 3;
    }

    // Count initialized (non-zero liquidity net) ticks between current tick
    // and the end of the tick map in the swap direction. This gives an upper
    // bound on crossings for a full-range swap in that direction.
    let crossings = if zero_for_one {
        pool.ticks
            .range(..pool.tick)
            .filter(|(_, &liq)| liq != 0)
            .count()
    } else {
        pool.ticks
            .range(pool.tick + 1..)
            .filter(|(_, &liq)| liq != 0)
            .count()
    };

    BASE_SWAP_GAS + PER_TICK_CROSSING_GAS * (crossings as u64).min(MAX_CROSSINGS)
}

/// Compute the maximum tradeable input amount for a V3 pool given its current
/// state and the nearest initialized tick in the swap direction.
///
/// Returns the amount that would move the price exactly to the nearest
/// initialized tick boundary (or tick_spacing boundary if no ticks known).
pub fn max_v3_tradeable_amount(pool: &UniswapV3PoolState, zero_for_one: bool) -> u128 {
    if pool.liquidity == 0 || pool.sqrt_price_x96.is_zero() {
        return 0;
    }

    let (target_sqrt, _) = get_swap_target(pool, zero_for_one);

    if target_sqrt == pool.sqrt_price_x96 {
        return pool.liquidity.saturating_div(LIQUIDITY_FRACTION_DENOM);
    }

    let max_in = if zero_for_one {
        get_amount_0_delta(target_sqrt, pool.sqrt_price_x96, pool.liquidity, true)
    } else {
        get_amount_1_delta(pool.sqrt_price_x96, target_sqrt, pool.liquidity, true)
    }
    .unwrap_or(U256::ZERO);

    if max_in.is_zero() {
        return pool.liquidity.saturating_div(LIQUIDITY_FRACTION_DENOM);
    }

    let fee = pool.info.fee as u128;
    let max_input_with_fee = max_in * U256::from(PPM_DENOMINATOR as u64)
        / U256::from(PPM_DENOMINATOR as u64 - fee.min(PPM_DENOMINATOR as u128 - 1) as u64);

    let limbs = max_input_with_fee.as_limbs();
    let result = limbs[0] as u128;
    if result == 0 {
        pool.liquidity.saturating_div(LIQUIDITY_FRACTION_DENOM)
    } else {
        result
    }
}

/// Cumulative input amounts at which an exact-in swap crosses into the next
/// initialized-tick liquidity band, up to `max_input`.
///
/// These thresholds partition the input domain into brackets inside which the
/// pool's quote is smooth and concave (single liquidity band), enabling
/// deterministic global optimization via per-bracket golden-section search.
///
/// Returns at most `max_points` thresholds, ascending.
pub fn v3_breakpoints(
    pool: &UniswapV3PoolState,
    zero_for_one: bool,
    max_input: u128,
    max_points: usize,
) -> Vec<u128> {
    let mut res = Vec::new();
    if max_input == 0 || pool.liquidity == 0 || pool.sqrt_price_x96.is_zero() {
        return res;
    }

    let mut sqrt_price = pool.sqrt_price_x96;
    let mut current_tick = pool.tick;
    let mut liquidity = pool.liquidity;
    let mut cumulative = 0u128;

    while res.len() < max_points {
        let (target_sqrt_price, has_real_tick) = get_swap_target_for_tick(
            &pool.ticks,
            pool.info.tick_spacing,
            current_tick,
            sqrt_price,
            zero_for_one,
        );
        if target_sqrt_price == sqrt_price {
            break;
        }

        // Input (including fee) required to move price exactly onto the band edge
        let (_, amount_in_step, _, fee_step) = compute_swap_step(
            sqrt_price,
            target_sqrt_price,
            liquidity,
            U256::MAX,
            pool.info.fee,
        );
        let consumed = match amount_in_step.checked_add(fee_step) {
            Some(v) => v.min(U256::from(u128::MAX)).to::<u128>(),
            None => break,
        };
        let next_cumulative = cumulative.saturating_add(consumed);
        if next_cumulative >= max_input || next_cumulative == cumulative {
            break;
        }
        cumulative = next_cumulative;
        res.push(cumulative);

        if !has_real_tick {
            break; // synthetic full-range target — no further known bands
        }
        let next_tick = find_next_initialized_tick(&pool.ticks, current_tick, zero_for_one);
        if cross_tick(
            &pool.ticks,
            &mut current_tick,
            &mut liquidity,
            next_tick,
            zero_for_one,
        ) {
            break; // liquidity exhausted beyond this band
        }
        sqrt_price = target_sqrt_price;
    }

    res
}

/// Quote a Uniswap V3 exact-input swap.
///
/// Simulates stepping through the pool's initialized ticks, crossing each one
/// while applying the fee, until `amount_in` is consumed or there are no more
/// reachable ticks. Returns the total amount of `token_out` the swap would receive.
///
/// When no initialized ticks are known, the swap uses the full pool liquidity
/// (M2 fix) instead of being capped at the nearest tick_spacing boundary.
///
/// Returns `None` for zero input, zero liquidity, or zero sqrt-price.
/// Update tick and liquidity when crossing an initialized tick boundary.
/// Returns `true` if the caller should break out of the swap loop
/// (liquidity reached zero).
fn cross_tick(
    ticks: &BTreeMap<i32, i128>,
    current_tick: &mut i32,
    liquidity: &mut u128,
    next_tick: Option<i32>,
    zero_for_one: bool,
) -> bool {
    *current_tick = if zero_for_one {
        next_tick.unwrap_or(MIN_TICK)
    } else {
        next_tick.unwrap_or(MAX_TICK)
    };
    if let Some(liq_delta) = next_tick.and_then(|t| ticks.get(&t)) {
        let delta = *liq_delta;
        if zero_for_one {
            *liquidity = if delta > 0 {
                liquidity.saturating_sub(delta as u128)
            } else {
                liquidity.saturating_add((-delta) as u128)
            };
        } else {
            *liquidity = if delta > 0 {
                liquidity.saturating_add(delta as u128)
            } else {
                liquidity.saturating_sub((-delta) as u128)
            };
        }
    }
    *liquidity == 0
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
        let (target_sqrt_price, has_real_tick) = get_swap_target_for_tick(
            &pool.ticks,
            pool.info.tick_spacing,
            current_tick,
            sqrt_price,
            zero_for_one,
        );

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

        if next_sqrt_price == target_sqrt_price && has_real_tick {
            let next = find_next_initialized_tick(&pool.ticks, current_tick, zero_for_one);
            if cross_tick(
                &pool.ticks,
                &mut current_tick,
                &mut liquidity,
                next,
                zero_for_one,
            ) {
                break;
            }
        } else if next_sqrt_price == target_sqrt_price {
            break;
        }

        sqrt_price = next_sqrt_price;
    }

    if total_amount_out.is_zero() {
        return None;
    }
    let limbs = total_amount_out.as_limbs();
    if limbs[1] != 0 || limbs[2] != 0 || limbs[3] != 0 {
        return None;
    }
    Some(limbs[0] as u128)
}

/// Like `get_swap_target` but uses a passed-in sqrt_price and current_tick
/// for use inside the quoting loop where these values change across tick crossings.
///
/// When no initialized ticks are known, goes to the full range
/// (MIN_SQRT_RATIO / MAX_SQRT_RATIO) instead of capping at a synthetic
/// tick_spacing boundary (M2 fix). Without tick knowledge, using the full
/// pool.liquidity is a better estimate than truncating at one spacing interval.
fn get_swap_target_for_tick(
    ticks: &BTreeMap<i32, i128>,
    _tick_spacing: Option<u32>,
    current_tick: i32,
    sqrt_price: U256,
    zero_for_one: bool,
) -> (U256, bool) {
    let next_tick = find_next_initialized_tick(ticks, current_tick, zero_for_one);
    match next_tick {
        Some(t) => {
            let r = get_sqrt_ratio_at_tick(t);
            let sqrt = if zero_for_one {
                r.max(*MIN_SQRT_RATIO).min(sqrt_price)
            } else {
                r.min(*MAX_SQRT_RATIO).max(sqrt_price)
            };
            (sqrt, true)
        }
        None => {
            if zero_for_one {
                (*MIN_SQRT_RATIO, false)
            } else {
                (*MAX_SQRT_RATIO, false)
            }
        }
    }
}

/// Simulates a V3 exact-output swap: determine how much input is required
/// to receive exactly `amount_out` of the output token.
///
/// Walks the tick range in the opposite direction of `quote_v3_exact_in`,
/// accumulating the input required per tick step.
///
/// When no initialized ticks are known, caps at the nearest tick_spacing boundary.
///
/// Returns `None` for zero output, zero liquidity, or zero sqrt-price.
pub fn quote_v3_exact_out(
    pool: &UniswapV3PoolState,
    amount_out: u128,
    zero_for_one: bool,
) -> Option<u128> {
    if amount_out == 0 || pool.liquidity == 0 || pool.sqrt_price_x96.is_zero() {
        return None;
    }

    let mut sqrt_price = pool.sqrt_price_x96;
    let mut current_tick = pool.tick;
    let mut liquidity = pool.liquidity;
    let mut amount_remaining = U256::from(amount_out);
    let mut total_amount_in = U256::ZERO;

    while amount_remaining > U256::ZERO {
        let (target_sqrt_price, has_real_tick) = get_swap_target_for_tick(
            &pool.ticks,
            pool.info.tick_spacing,
            current_tick,
            sqrt_price,
            zero_for_one,
        );

        if target_sqrt_price == sqrt_price {
            break;
        }

        let (next_sqrt_price, amount_in_step, amount_out_step, fee_step) =
            compute_swap_step_exact_out(
                sqrt_price,
                target_sqrt_price,
                liquidity,
                amount_remaining,
                pool.info.fee,
            );

        total_amount_in += amount_in_step + fee_step;

        if amount_out_step >= amount_remaining {
            amount_remaining = U256::ZERO;
        } else {
            amount_remaining -= amount_out_step;
        }

        if next_sqrt_price == target_sqrt_price && has_real_tick {
            let next = find_next_initialized_tick(&pool.ticks, current_tick, zero_for_one);
            if cross_tick(
                &pool.ticks,
                &mut current_tick,
                &mut liquidity,
                next,
                zero_for_one,
            ) {
                break;
            }
        } else if next_sqrt_price == target_sqrt_price {
            break;
        }

        sqrt_price = next_sqrt_price;
    }

    if total_amount_in.is_zero() {
        return None;
    }
    let limbs = total_amount_in.as_limbs();
    if limbs[1] != 0 || limbs[2] != 0 || limbs[3] != 0 {
        return None;
    }
    Some(limbs[0] as u128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex_type::DexType;
    use crate::pool::state::PoolInfo;

    fn test_pool(ticks: BTreeMap<i32, i128>) -> UniswapV3PoolState {
        UniswapV3PoolState {
            info: PoolInfo {
                address: alloy::primitives::address!("1111111111111111111111111111111111111111"),
                token0: alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                token1: alloy::primitives::address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                fee: 3000,
                name: None,
                dex_type: DexType::UniswapV3,
                tick_spacing: Some(60),
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
                dex_name: None,
                token0_symbol: None,
                token1_symbol: None,
                tvl_usd: None,
                volume_usd_24h: None,
                volume_usd_30d: None,
            },
            sqrt_price_x96: get_sqrt_ratio_at_tick(0),
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            ticks,
            fee_growth_global_0_x128: U256::ZERO,
            fee_growth_global_1_x128: U256::ZERO,
        }
    }

    #[test]
    fn v3_breakpoints_ascending_and_within_budget() {
        let mut ticks = BTreeMap::new();
        ticks.insert(120i32, 500_000_000_000i128);
        ticks.insert(240, 500_000_000_000);
        ticks.insert(360, 500_000_000_000);
        let pool = test_pool(ticks);

        // Budget large enough to cross all three bands (~6e9 + ~9e9 + ~12e9 input)
        let budget = 50_000_000_000u128;
        let bps = v3_breakpoints(&pool, false, budget, 10);
        assert_eq!(bps.len(), 3, "three initialized bands above tick 0");
        assert!(
            bps.windows(2).all(|w| w[0] < w[1]),
            "must be strictly ascending"
        );
        assert!(bps.iter().all(|&b| b > 0 && b < budget));
    }

    #[test]
    fn v3_breakpoints_respects_max_input_and_points() {
        let mut ticks = BTreeMap::new();
        for t in [60, 120, 180, 240, 300, 360] {
            ticks.insert(t, 500_000_000_000i128);
        }
        let pool = test_pool(ticks);

        // Budget stops enumeration mid-way (first crossing alone needs ~3e9)
        let bps = v3_breakpoints(&pool, false, 20_000_000_000, 10);
        assert!(!bps.is_empty());
        assert!(bps.iter().all(|&b| b < 20_000_000_000));

        // Point cap limits the result
        let capped = v3_breakpoints(&pool, false, 50_000_000_000, 2);
        assert_eq!(capped.len(), 2);

        // No ticks → no breakpoints
        assert!(v3_breakpoints(&test_pool(BTreeMap::new()), true, 1_000_000_000, 8).is_empty());
    }
}
