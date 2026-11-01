//! DEX pool swap decoding (V2/V3/V4/Curve/Balancer/LB/…).
use crate::utils::topic_address;
use alloy::primitives::{Address, U256};

use crate::chain::events::{
    DONASWAP_V3_SWAP_TOPIC, INF_CL_SWAP_TOPIC, TRADER_JOE_LB_SWAP_LEGACY_TOPIC, V2_SWAP_TOPIC,
    V3_SWAP_TOPIC, V4_SWAP_TOPIC,
};
use crate::data::LogData;
use crate::explorer::types::{Amm, LegSource, SwapFact};
use crate::pool::decoders::{
    BALANCER_SWAP_TOPIC, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
    FLUID_SWAP_TOPIC, LB_SWAP_TOPIC, METRIC_SWAP_TOPIC, PENDLE_SWAP_TOPIC, SOLIDLY_SWAP_TOPIC,
};

/// Signed pool deltas: negative amount is token in, positive is token out.
/// Tick sits in the word at bytes 128..160 when the log is long enough (V3/V4/Infinity).
/// Returns false when the amounts are missing or both zero.
fn apply_signed_deltas(fact: &mut SwapFact, data: &[u8]) -> bool {
    if data.len() < 64 {
        return false;
    }
    let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&data[0..32]));
    let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&data[32..64]));
    if a0.is_zero() && a1.is_zero() {
        return false;
    }
    if a0 < alloy::primitives::I256::ZERO {
        fact.amount_in = a0.wrapping_abs().into_raw();
        fact.amount_out = a1.wrapping_abs().into_raw();
        fact.token_in = super::sentinels::TOKEN0_SENTINEL;
    } else {
        fact.amount_in = a1.wrapping_abs().into_raw();
        fact.amount_out = a0.wrapping_abs().into_raw();
        fact.token_in = super::sentinels::TOKEN1_SENTINEL;
    }
    if data.len() >= 160 {
        fact.tick = Some(i32::from_be_bytes([
            data[156], data[157], data[158], data[159],
        ]));
    }
    true
}

/// Bind V2/Solidly amounts from a single direction. `amount0In` wins ties
/// (`a0i >= a1i`), matching the historical direction rule, and `amount_out`
/// is the opposite token's output — never an independent `max` of both outs.
fn apply_v2_sides(fact: &mut SwapFact, a0i: U256, a1i: U256, a0o: U256, a1o: U256) {
    if !a0i.is_zero() && a0i >= a1i {
        fact.amount_in = a0i;
        fact.amount_out = a1o;
        fact.token_in = super::sentinels::TOKEN0_SENTINEL;
        fact.token_out = super::sentinels::TOKEN1_SENTINEL;
    } else if !a1i.is_zero() {
        fact.amount_in = a1i;
        fact.amount_out = a0o;
        fact.token_in = super::sentinels::TOKEN1_SENTINEL;
        fact.token_out = super::sentinels::TOKEN0_SENTINEL;
    } else {
        fact.amount_out = a0o.max(a1o);
    }
}

/// Decode a swap fact from a receipt log. Amount semantics per AMM family:
/// - V2: (amount0In, amount1In, amount0Out, amount1Out) — direction needs
///   token metadata, so the caller resolves direction via transfer pairing.
/// - V3: signed (amount0, amount1).
/// - V4: poolId topic + signed amounts (synthetic pool address = last 20 bytes).
/// - Curve/Balancer/LB/Pendle: explicit in/out amounts.
/// - Aggregator (0x/1inch/Paraswap): explicit in/out amounts; direction is
///   either carried in topics/data or resolved via transfer pairing.
///
/// Returns the fact with zero tokens when direction is not resolvable from
/// the log alone; `attach_swap_tokens` fills them from transfer pairing.
pub fn decode_swap(log: &LogData) -> Option<(Amm, SwapFact)> {
    let topic0 = *log.topics.first()?;
    match topic0 {
        t if t == V2_SWAP_TOPIC => decode_v2_like(log, Amm::V2),
        // Donaswap V3 shares the Uniswap V3 data layout plus two ignored
        // uint128 protocol-fee words.
        t if t == V3_SWAP_TOPIC || t == *DONASWAP_V3_SWAP_TOPIC => {
            decode_signed_at(log, Amm::V3, log.address)
        }
        t if t == *V4_SWAP_TOPIC => decode_pool_id_signed(log, Amm::V4),
        t if t == *INF_CL_SWAP_TOPIC => decode_pool_id_signed(log, Amm::Infinity),
        t if t == *CURVE_TOKEN_EXCHANGE_TOPIC || t == *CURVE_V2_TOKEN_EXCHANGE_TOPIC => {
            decode_curve(log)
        }
        t if t == *BALANCER_SWAP_TOPIC => decode_balancer(log),
        t if t == *LB_SWAP_TOPIC => decode_lb(log),
        t if t == *TRADER_JOE_LB_SWAP_LEGACY_TOPIC => decode_lb_legacy(log),
        t if t == *PENDLE_SWAP_TOPIC => decode_pendle(log),
        t if t == *FLUID_SWAP_TOPIC => decode_fluid(log),
        t if t == *METRIC_SWAP_TOPIC => decode_metric(log),
        t if t == *SOLIDLY_SWAP_TOPIC => decode_v2_like(log, Amm::Solidly),
        _ => super::aggregator::decode_aggregator_swap(log),
    }
}

fn blank_swap(amm: Amm, pool: Address) -> SwapFact {
    SwapFact {
        tx_index: 0,
        log_index: 0,
        pool,
        amm,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        token_source: LegSource::Proximity,
        amount_in: U256::ZERO,
        amount_out: U256::ZERO,
        tick: None,
        owner: None,
    }
}

/// V2 and Solidly share the four-amount data layout. Direction is resolved
/// later via transfer pairing.
fn decode_v2_like(log: &LogData, amm: Amm) -> Option<(Amm, SwapFact)> {
    if log.data.len() < 128 {
        return None;
    }
    let a0i = U256::from_be_slice(&log.data[0..32]);
    let a1i = U256::from_be_slice(&log.data[32..64]);
    let a0o = U256::from_be_slice(&log.data[64..96]);
    let a1o = U256::from_be_slice(&log.data[96..128]);
    if a0i.is_zero() && a1i.is_zero() && a0o.is_zero() && a1o.is_zero() {
        return None;
    }
    let mut fact = blank_swap(amm, log.address);
    apply_v2_sides(&mut fact, a0i, a1i, a0o, a1o);
    Some((amm, fact))
}

fn decode_signed_at(log: &LogData, amm: Amm, pool: Address) -> Option<(Amm, SwapFact)> {
    let mut fact = blank_swap(amm, pool);
    if !apply_signed_deltas(&mut fact, &log.data) {
        return None;
    }
    Some((amm, fact))
}

/// V4 / Infinity: pool id is topics[1]; amounts are signed deltas.
fn decode_pool_id_signed(log: &LogData, amm: Amm) -> Option<(Amm, SwapFact)> {
    if log.topics.len() < 2 {
        return None;
    }
    let pool = topic_address(log.topics[1].as_slice());
    decode_signed_at(log, amm, pool)
}

fn decode_curve(log: &LogData) -> Option<(Amm, SwapFact)> {
    if log.data.len() < 128 {
        return None;
    }
    let mut fact = blank_swap(Amm::Curve, log.address);
    fact.amount_in = U256::from_be_slice(&log.data[32..64]);
    fact.amount_out = U256::from_be_slice(&log.data[96..128]);
    Some((Amm::Curve, fact))
}

fn decode_balancer(log: &LogData) -> Option<(Amm, SwapFact)> {
    if log.topics.len() < 4 || log.data.len() < 64 {
        return None;
    }
    let pool = topic_address(log.topics[1].as_slice());
    let mut fact = blank_swap(Amm::Balancer, pool);
    fact.token_in = topic_address(log.topics[2].as_slice());
    fact.token_out = topic_address(log.topics[3].as_slice());
    fact.amount_in = U256::from_be_slice(&log.data[0..32]);
    fact.amount_out = U256::from_be_slice(&log.data[32..64]);
    Some((Amm::Balancer, fact))
}

fn decode_lb(log: &LogData) -> Option<(Amm, SwapFact)> {
    if log.topics.len() < 3 || log.data.len() < 96 {
        return None;
    }
    let swap_for_y = log.data[31] != 0;
    let mut fact = blank_swap(Amm::Lb, log.address);
    fact.amount_in = U256::from_be_slice(&log.data[32..64]);
    fact.amount_out = U256::from_be_slice(&log.data[64..96]);
    fact.token_in = if swap_for_y {
        super::sentinels::TOKEN0_SENTINEL
    } else {
        super::sentinels::TOKEN1_SENTINEL
    };
    fact.token_out = if swap_for_y {
        super::sentinels::TOKEN1_SENTINEL
    } else {
        super::sentinels::TOKEN0_SENTINEL
    };
    Some((Amm::Lb, fact))
}

fn decode_lb_legacy(log: &LogData) -> Option<(Amm, SwapFact)> {
    if log.topics.len() < 3 || log.data.len() < 96 {
        return None;
    }
    let (in_x, in_y) = unpack_lb_amounts_xy(&log.data[32..64]);
    let (out_x, out_y) = unpack_lb_amounts_xy(&log.data[64..96]);
    let mut fact = blank_swap(Amm::Lb, log.address);
    if !in_x.is_zero() {
        fact.amount_in = in_x;
        fact.amount_out = out_y;
        fact.token_in = super::sentinels::TOKEN0_SENTINEL;
        fact.token_out = super::sentinels::TOKEN1_SENTINEL;
    } else if !in_y.is_zero() {
        fact.amount_in = in_y;
        fact.amount_out = out_x;
        fact.token_in = super::sentinels::TOKEN1_SENTINEL;
        fact.token_out = super::sentinels::TOKEN0_SENTINEL;
    } else {
        return None;
    }
    if fact.amount_in.is_zero() || fact.amount_out.is_zero() {
        return None;
    }
    Some((Amm::Lb, fact))
}

fn decode_pendle(log: &LogData) -> Option<(Amm, SwapFact)> {
    if log.topics.len() < 3 || log.data.len() < 64 {
        return None;
    }
    let pt = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
    let sy = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
    if pt.is_zero() && sy.is_zero() {
        return None;
    }
    let mut fact = blank_swap(Amm::Pendle, log.address);
    fact.amount_in = pt.wrapping_abs().into_raw();
    fact.amount_out = sy.wrapping_abs().into_raw();
    Some((Amm::Pendle, fact))
}

fn decode_fluid(log: &LogData) -> Option<(Amm, SwapFact)> {
    if log.data.len() < 128 {
        return None;
    }
    let swap0to1 = log.data[31] != 0;
    let mut fact = blank_swap(Amm::Fluid, log.address);
    fact.amount_in = U256::from_be_slice(&log.data[32..64]);
    fact.amount_out = U256::from_be_slice(&log.data[64..96]);
    fact.token_in = if swap0to1 {
        super::sentinels::TOKEN0_SENTINEL
    } else {
        super::sentinels::TOKEN1_SENTINEL
    };
    fact.token_out = if swap0to1 {
        super::sentinels::TOKEN1_SENTINEL
    } else {
        super::sentinels::TOKEN0_SENTINEL
    };
    Some((Amm::Fluid, fact))
}

fn decode_metric(log: &LogData) -> Option<(Amm, SwapFact)> {
    if log.data.len() < 160 {
        return None;
    }
    let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
    let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[64..96]));
    if a0.is_zero() && a1.is_zero() {
        return None;
    }
    let mut fact = blank_swap(Amm::Metric, log.address);
    if a0 < alloy::primitives::I256::ZERO {
        fact.amount_in = a0.wrapping_abs().into_raw();
        fact.amount_out = a1.wrapping_abs().into_raw();
        fact.token_in = super::sentinels::TOKEN0_SENTINEL;
    } else {
        fact.amount_in = a1.wrapping_abs().into_raw();
        fact.amount_out = a0.wrapping_abs().into_raw();
        fact.token_in = super::sentinels::TOKEN1_SENTINEL;
    }
    Some((Amm::Metric, fact))
}

/// LFJ PackedUint128: amountX in the low 16 bytes, amountY in the high 16.
fn unpack_lb_amounts_xy(word: &[u8]) -> (U256, U256) {
    debug_assert!(word.len() >= 32);
    let ax = U256::from_be_slice(&word[16..32]);
    let ay = U256::from_be_slice(&word[0..16]);
    (ax, ay)
}
