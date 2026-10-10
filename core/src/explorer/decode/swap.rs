//! DEX pool swap decoding (V2/V3/V4/Curve/Balancer/LB/…).
use alloy::primitives::{Address, U256};
use crate::utils::topic_address;

use crate::chain::events::{INF_CL_SWAP_TOPIC, V2_SWAP_TOPIC, V3_SWAP_TOPIC, V4_SWAP_TOPIC};
use crate::data::LogData;
use crate::explorer::types::{Amm, LegSource, SwapFact};
use crate::pool::decoders::{
    BALANCER_SWAP_TOPIC, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
    FLUID_SWAP_TOPIC, LB_SWAP_TOPIC, METRIC_SWAP_TOPIC, PENDLE_SWAP_TOPIC, SOLIDLY_SWAP_TOPIC,
};


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
    let base = |amm: Amm, pool: Address| SwapFact {
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
    };

    if topic0 == V2_SWAP_TOPIC {
        // data: amount0In, amount1In, amount0Out, amount1Out
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
        let mut fact = base(Amm::V2, log.address);
        // One direction decision drives both sides. Independent `max` calls
        // pair token0's input with token1's output when a flash swap (or a
        // router hopping twice through one pool) sets both inputs.
        apply_v2_sides(&mut fact, a0i, a1i, a0o, a1o);
        return Some((Amm::V2, fact));
    }

    if topic0 == V3_SWAP_TOPIC {
        // data: int256 amount0, int256 amount1, sqrtPriceX96, liquidity, tick
        if log.data.len() < 64 {
            return None;
        }
        let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::V3, log.address);
        // amount > 0 = pool paid out (token out); amount < 0 = pool received (token in)
        if a0 < alloy::primitives::I256::ZERO {
            fact.amount_in = a0.wrapping_abs().into_raw();
            fact.amount_out = a1.wrapping_abs().into_raw();
            fact.token_in = super::sentinels::TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = super::sentinels::TOKEN1_SENTINEL;
        }
        if log.data.len() >= 160 {
            fact.tick = Some(i32::from_be_bytes([
                log.data[156],
                log.data[157],
                log.data[158],
                log.data[159],
            ]));
        }
        return Some((Amm::V3, fact));
    }

    if topic0 == *V4_SWAP_TOPIC {
        // topics: [sig, poolId]; data: int128 amount0, int128 amount1,...
        if log.topics.len() < 2 || log.data.len() < 64 {
            return None;
        }
        let pool = topic_address(log.topics[1].as_slice());
        let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::V4, pool);
        if a0 < alloy::primitives::I256::ZERO {
            fact.amount_in = a0.wrapping_abs().into_raw();
            fact.amount_out = a1.wrapping_abs().into_raw();
            fact.token_in = super::sentinels::TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = super::sentinels::TOKEN1_SENTINEL;
        }
        if log.data.len() >= 160 {
            fact.tick = Some(i32::from_be_bytes([
                log.data[156],
                log.data[157],
                log.data[158],
                log.data[159],
            ]));
        }
        return Some((Amm::V4, fact));
    }

    if topic0 == *INF_CL_SWAP_TOPIC {
        // Pancake Infinity CL singleton manager: same layout as V4 but the
        // signature carries an extra uint16 protocolFee word (data >= 224).
        if log.topics.len() < 2 || log.data.len() < 64 {
            return None;
        }
        let pool = topic_address(log.topics[1].as_slice());
        let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::Infinity, pool);
        if a0 < alloy::primitives::I256::ZERO {
            fact.amount_in = a0.wrapping_abs().into_raw();
            fact.amount_out = a1.wrapping_abs().into_raw();
            fact.token_in = super::sentinels::TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = super::sentinels::TOKEN1_SENTINEL;
        }
        if log.data.len() >= 160 {
            fact.tick = Some(i32::from_be_bytes([
                log.data[156],
                log.data[157],
                log.data[158],
                log.data[159],
            ]));
        }
        return Some((Amm::Infinity, fact));
    }

    if topic0 == CURVE_TOKEN_EXCHANGE_TOPIC || topic0 == CURVE_V2_TOKEN_EXCHANGE_TOPIC {
        // data: int128 sold_id/coin_sold, uint256 amount_sold, int128 bought_id, uint256 amount_bought
        if log.data.len() < 128 {
            return None;
        }
        let mut fact = base(Amm::Curve, log.address);
        fact.amount_in = U256::from_be_slice(&log.data[32..64]);
        fact.amount_out = U256::from_be_slice(&log.data[96..128]);
        return Some((Amm::Curve, fact));
    }

    if topic0 == BALANCER_SWAP_TOPIC {
        // topics: [sig, poolId, tokenIn, tokenOut]; data: amountIn, amountOut
        // Pool address is the first 20 bytes of the bytes32 poolId.
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        let pool = topic_address(log.topics[1].as_slice());
        let mut fact = base(Amm::Balancer, pool);
        fact.token_in = topic_address(log.topics[2].as_slice());
        fact.token_out = topic_address(log.topics[3].as_slice());
        fact.amount_in = U256::from_be_slice(&log.data[0..32]);
        fact.amount_out = U256::from_be_slice(&log.data[32..64]);
        return Some((Amm::Balancer, fact));
    }

    if topic0 == *LB_SWAP_TOPIC {
        // LB 2.0: topics [sig, sender, to]; data = swapForY(bool), amountIn,
        // amountOut, volatilityAccumulated, fees. Direction: swapForY = X→Y
        // (token0 in), else Y→X (token1 in). Tokens resolve via transfer pairing.
        if log.topics.len() < 3 || log.data.len() < 96 {
            return None;
        }
        let swap_for_y = log.data[31] != 0;
        let mut fact = base(Amm::Lb, log.address);
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
        return Some((Amm::Lb, fact));
    }

    if topic0 == *PENDLE_SWAP_TOPIC {
        // topics: [sig, caller, receiver]; data: int256 netPt, int256 netSy,...
        if log.topics.len() < 3 || log.data.len() < 64 {
            return None;
        }
        let pt = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let sy = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if pt.is_zero() && sy.is_zero() {
            return None;
        }
        let mut fact = base(Amm::Pendle, log.address);
        fact.amount_in = pt.wrapping_abs().into_raw();
        fact.amount_out = sy.wrapping_abs().into_raw();
        return Some((Amm::Pendle, fact));
    }

    if topic0 == *FLUID_SWAP_TOPIC {
        // Fluid: Swap(bool swap0to1, uint256 amountIn, uint256 amountOut, address to),
        // no indexed params, so data = [swap0to1, amountIn, amountOut, to] = 128 bytes.
        // Direction is encoded in the first-word bool; tokens resolve via transfer pairing.
        if log.data.len() < 128 {
            return None;
        }
        let swap0to1 = log.data[31] != 0;
        let mut fact = base(Amm::Fluid, log.address);
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
        return Some((Amm::Fluid, fact));
    }

    if topic0 == *METRIC_SWAP_TOPIC {
        // Metric V2: Swap(address sender, address recipient, bool exactInput,
        // int128 amount0Delta, int128 amount1Delta, int16 newTick, uint104
        // newPositionInBin). sender/recipient indexed; data =
        // [exactInput, amount0Delta, amount1Delta, newTick, newPositionInBin] = 160 bytes.
        if log.data.len() < 160 {
            return None;
        }
        let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[64..96]));
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::Metric, log.address);
        if a0 < alloy::primitives::I256::ZERO {
            fact.amount_in = a0.wrapping_abs().into_raw();
            fact.amount_out = a1.wrapping_abs().into_raw();
            fact.token_in = super::sentinels::TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = super::sentinels::TOKEN1_SENTINEL;
        }
        return Some((Amm::Metric, fact));
    }

    if topic0 == *SOLIDLY_SWAP_TOPIC {
        // Velodrome V2/Aerodrome: topics [sig, sender, to]; data =
        // [amount0In, amount1In, amount0Out, amount1Out] — the same
        // four-amount layout as Uniswap V2's Swap data. Direction is
        // resolved via transfer pairing, exactly like V2.
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
        let mut fact = base(Amm::Solidly, log.address);
        apply_v2_sides(&mut fact, a0i, a1i, a0o, a1o);
        return Some((Amm::Solidly, fact));
    }

    if let Some(fact) = super::aggregator::decode_aggregator_swap(log) {
        return Some(fact);
    }

    None
}

