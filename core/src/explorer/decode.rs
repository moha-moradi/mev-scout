//! Receipt-log decoders for the explorer's forensic layer.
//!
//! Works directly on `LogData` entries from `ReceiptData` (the bulk-receipt
//! path) rather than `ExecutedLog` (the replay path), so the explorer never
//! needs the EVM replayer. Covers:
//! - raw ERC-20 `Transfer` (the accounting primitive for profit attribution)
//! - DEX swap events: V2, V3, V4, Curve, Balancer, Solidly, Trader Joe LB, Pendle,
//!   Fluid, Metric
//! - liquidation registry: Aave V3 `LiquidationCall`, Compound V3 `Absorb`
//! - V3 `Mint`/`Burn` (JIT positions)
//!
//! Swap token direction is resolved by pairing each swap log with the ERC-20
//! Transfer legs that move tokens into/out of the pool in the same tx
//! (registry-free, chain-generic). Pool-registry lookups can enrich later but
//! are not required for classification.

use alloy::primitives::{Address, B256, U256};

use crate::data::LogData;
use crate::explorer::types::{
    Amm, FlashLoanFact, JitFact, LiquidationFact, SwapFact, TransferFact,
};

use crate::chain::events::{
    decode_balancer_flash, AAVE_V2_FLASH_LOAN_TOPIC, AAVE_V3_FLASH_LOAN_TOPIC,
    AAVE_V3_LIQUIDATION_CALL_TOPIC, BALANCER_FLASH_LOAN_TOPIC, COMPOUND_V2_LIQUIDATE_BORROW_TOPIC,
    COMPOUND_V3_ABSORB_TOPIC, INF_CL_SWAP_TOPIC, TRANSFER_TOPIC, V2_SWAP_TOPIC, V3_SWAP_TOPIC,
    V4_SWAP_TOPIC,
};
use crate::pool::decoders::{
    BALANCER_SWAP_TOPIC, CURVE_TOKEN_EXCHANGE_TOPIC, CURVE_V2_TOKEN_EXCHANGE_TOPIC,
    FLUID_SWAP_TOPIC, LB_SWAP_TOPIC, METRIC_SWAP_TOPIC, PENDLE_SWAP_TOPIC, SOLIDLY_SWAP_TOPIC,
    V3_BURN_TOPIC, V3_MINT_TOPIC,
};

/// Decode an ERC-20 Transfer fact from a receipt log.
pub fn decode_transfer(log: &LogData) -> Option<TransferFact> {
    if log.topics.len() < 3 || log.topics[0] != TRANSFER_TOPIC {
        return None;
    }
    if log.data.len() < 32 {
        return None;
    }
    Some(TransferFact {
        tx_index: 0,  // stamped by caller
        log_index: 0, // stamped by caller
        token: log.address,
        from: Address::from_slice(&log.topics[1][12..]),
        to: Address::from_slice(&log.topics[2][12..]),
        amount: U256::from_be_slice(&log.data[0..32]),
    })
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
        fact.amount_in = a0i.max(a1i);
        fact.amount_out = a0o.max(a1o);
        // Direction relative to token0/token1 (resolved via registry, then
        // transfer pairing as fallback).
        if !a0i.is_zero() && a0i >= a1i {
            fact.token_in = TOKEN0_SENTINEL;
            fact.token_out = TOKEN1_SENTINEL;
        } else if !a1i.is_zero() {
            fact.token_in = TOKEN1_SENTINEL;
            fact.token_out = TOKEN0_SENTINEL;
        }
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
            fact.token_in = TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = TOKEN1_SENTINEL;
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
        // topics: [sig, poolId]; data: int128 amount0, int128 amount1, ...
        if log.topics.len() < 2 || log.data.len() < 64 {
            return None;
        }
        let pool = Address::from_slice(&log.topics[1].as_slice()[12..]);
        let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::V4, pool);
        if a0 < alloy::primitives::I256::ZERO {
            fact.amount_in = a0.wrapping_abs().into_raw();
            fact.amount_out = a1.wrapping_abs().into_raw();
            fact.token_in = TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = TOKEN1_SENTINEL;
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
        let pool = Address::from_slice(&log.topics[1].as_slice()[12..]);
        let a0 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[0..32]));
        let a1 = alloy::primitives::I256::from_raw(U256::from_be_slice(&log.data[32..64]));
        if a0.is_zero() && a1.is_zero() {
            return None;
        }
        let mut fact = base(Amm::Infinity, pool);
        if a0 < alloy::primitives::I256::ZERO {
            fact.amount_in = a0.wrapping_abs().into_raw();
            fact.amount_out = a1.wrapping_abs().into_raw();
            fact.token_in = TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = TOKEN1_SENTINEL;
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
        let pool = Address::from_slice(&log.topics[1].as_slice()[12..]);
        let mut fact = base(Amm::Balancer, pool);
        fact.token_in = Address::from_slice(&log.topics[2].as_slice()[12..]);
        fact.token_out = Address::from_slice(&log.topics[3].as_slice()[12..]);
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
            TOKEN0_SENTINEL
        } else {
            TOKEN1_SENTINEL
        };
        fact.token_out = if swap_for_y {
            TOKEN1_SENTINEL
        } else {
            TOKEN0_SENTINEL
        };
        return Some((Amm::Lb, fact));
    }

    if topic0 == *PENDLE_SWAP_TOPIC {
        // topics: [sig, caller, receiver]; data: int256 netPt, int256 netSy, ...
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
            TOKEN0_SENTINEL
        } else {
            TOKEN1_SENTINEL
        };
        fact.token_out = if swap0to1 {
            TOKEN1_SENTINEL
        } else {
            TOKEN0_SENTINEL
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
            fact.token_in = TOKEN0_SENTINEL;
        } else {
            fact.amount_in = a1.wrapping_abs().into_raw();
            fact.amount_out = a0.wrapping_abs().into_raw();
            fact.token_in = TOKEN1_SENTINEL;
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
        fact.amount_in = a0i.max(a1i);
        fact.amount_out = a0o.max(a1o);
        if !a0i.is_zero() && a0i >= a1i {
            fact.token_in = TOKEN0_SENTINEL;
            fact.token_out = TOKEN1_SENTINEL;
        } else if !a1i.is_zero() {
            fact.token_in = TOKEN1_SENTINEL;
            fact.token_out = TOKEN0_SENTINEL;
        }
        return Some((Amm::Solidly, fact));
    }

    if let Some(fact) = decode_aggregator_swap(log) {
        return Some(fact);
    }

    None
}

/// Decode an aggregator/DEX-router edge (Phase 1.6). Returns fully-directional
/// facts for 1inch `Swapped`, Paraswap `Swapped`/`SwappedV3`, and 0x `Fill`.
/// 0x tokens are not in the event payload (asset encodings live in dynamic
/// data), so token slots stay unresolved for the transfer-pairing fallback.
fn decode_aggregator_swap(log: &LogData) -> Option<(Amm, SwapFact)> {
    let topic0 = *log.topics.first()?;
    let base = |pool: Address| SwapFact {
        tx_index: 0,
        log_index: 0,
        pool,
        amm: Amm::Aggregator,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        amount_in: U256::ZERO,
        amount_out: U256::ZERO,
        tick: None,
        owner: None,
    };

    // 1inch V4/V5: Swapped(sender, srcToken, dstToken, dstReceiver,
    // spentAmount, returnAmount), no indexed params.
    if topic0 == ONEINCH_SWAPPED_TOPIC {
        if log.data.len() < 192 {
            return None;
        }
        let mut fact = base(log.address);
        fact.token_in = Address::from_slice(&log.data[44..64]);
        fact.token_out = Address::from_slice(&log.data[76..96]);
        fact.amount_in = U256::from_be_slice(&log.data[128..160]);
        fact.amount_out = U256::from_be_slice(&log.data[160..192]);
        return Some((Amm::Aggregator, fact));
    }

    // Paraswap v3/v4: Swapped(initiator, beneficiary, srcToken, destToken,
    // srcAmount, receivedAmount, expectedAmount, referrer); beneficiary/
    // srcToken/destToken indexed.
    if topic0 == PARASWAP_SWAPPED_TOPIC {
        if log.topics.len() < 4 || log.data.len() < 160 {
            return None;
        }
        let mut fact = base(log.address);
        fact.token_in = Address::from_slice(&log.topics[2][12..]);
        fact.token_out = Address::from_slice(&log.topics[3][12..]);
        fact.amount_in = U256::from_be_slice(&log.data[32..64]);
        fact.amount_out = U256::from_be_slice(&log.data[64..96]);
        return Some((Amm::Aggregator, fact));
    }

    // Paraswap v5/v6: SwappedV3(uuid, partner, feePercent, initiator,
    // beneficiary, srcToken, destToken, srcAmount, receivedAmount,
    // expectedAmount); beneficiary/srcToken/destToken indexed.
    if topic0 == PARASWAP_SWAPPED_V3_TOPIC {
        if log.topics.len() < 4 || log.data.len() < 224 {
            return None;
        }
        let mut fact = base(log.address);
        fact.token_in = Address::from_slice(&log.topics[2][12..]);
        fact.token_out = Address::from_slice(&log.topics[3][12..]);
        fact.amount_in = U256::from_be_slice(&log.data[128..160]);
        fact.amount_out = U256::from_be_slice(&log.data[160..192]);
        return Some((Amm::Aggregator, fact));
    }

    // 0x Exchange `Fill`: makerAddress/feeRecipientAddress/orderHash indexed;
    // data = [makerAssetData, takerAssetData, makerFeeAssetData, takerFeeAssetData,
    // takerAddress, senderAddress, makerAssetFilled, takerAssetFilled, makerFee,
    // takerFee, protocolFee] = 11 words. Token addresses are encoded inside the
    // dynamic asset-data blobs; leave the slots unresolved and let transfer
    // pairing resolve the direction.
    if topic0 == ZRX_FILL_TOPIC {
        if log.topics.len() < 4 || log.data.len() < 352 {
            return None;
        }
        let mut fact = base(log.address);
        fact.amount_in = U256::from_be_slice(&log.data[224..256]);
        fact.amount_out = U256::from_be_slice(&log.data[192..224]);
        return Some((Amm::Aggregator, fact));
    }

    None
}

/// Drop aggregator edges that duplicate registry/DEX swap edges in the same tx
/// (Phase 1.6 dedup). Aggregator events often coexist with the underlying pool
/// `Swap` logs; keeping both would double-count edges in the cycle walk and
/// inflate Exact arb. An aggregator edge `A→B` is redundant when a DEX chain
/// `A→…→B` (≤3 hops) already carries the same flow with matching amounts.
pub fn dedup_aggregator_facts(swaps: &mut Vec<SwapFact>) {
    let dexs: Vec<&SwapFact> = swaps.iter().filter(|s| s.amm != Amm::Aggregator).collect();
    let mut redundant = std::collections::HashSet::new();
    for (i, agg) in swaps.iter().enumerate() {
        if agg.amm != Amm::Aggregator {
            continue;
        }
        if is_unresolved_token(agg.token_in)
            || is_unresolved_token(agg.token_out)
            || agg.amount_in.is_zero()
        {
            continue;
        }
        if aggregator_edge_redundant(agg, &dexs) {
            redundant.insert(i);
        }
    }
    drop(dexs);
    if !redundant.is_empty() {
        let mut keep: Vec<SwapFact> = Vec::with_capacity(swaps.len());
        for (i, s) in swaps.drain(..).enumerate() {
            if !redundant.contains(&i) {
                keep.push(s);
            }
        }
        *swaps = keep;
    }
}

/// Amounts match within `tol_bps` basis points (both zero counts as equal).
fn amounts_match(a: U256, b: U256, tol_bps: u32) -> bool {
    if a.is_zero() && b.is_zero() {
        return true;
    }
    let scaled = a.abs_diff(b).saturating_mul(U256::from(10_000u32));
    scaled <= a.max(b).saturating_mul(U256::from(tol_bps))
}

/// True when the aggregator edge duplicates a ≤3-hop DEX chain: the first
/// hop spends ≈ the aggregator's input and the last hop returns ≈ its output.
fn aggregator_edge_redundant(agg: &SwapFact, dexs: &[&SwapFact]) -> bool {
    // Direct single-pool pair match.
    if dexs.iter().any(|s| {
        !is_unresolved_token(s.token_in)
            && s.token_in == agg.token_in
            && s.token_out == agg.token_out
            && amounts_match(s.amount_in, agg.amount_in, 200)
            && amounts_match(s.amount_out, agg.amount_out, 200)
    }) {
        return true;
    }
    // Multi-hop: starts spend ≈ agg.amount_in; ends return ≈ agg.amount_out.
    let starts: Vec<&&SwapFact> = dexs
        .iter()
        .filter(|s| {
            !is_unresolved_token(s.token_in)
                && s.token_in == agg.token_in
                && amounts_match(s.amount_in, agg.amount_in, 400)
        })
        .collect();
    let ends: Vec<&&SwapFact> = dexs
        .iter()
        .filter(|s| {
            !is_unresolved_token(s.token_out)
                && s.token_out == agg.token_out
                && amounts_match(s.amount_out, agg.amount_out, 400)
        })
        .collect();
    for start in starts {
        // 2-hop: start→X, X→end.
        if ends
            .iter()
            .any(|e| start.token_out == e.token_in && start.token_out != agg.token_in)
        {
            return true;
        }
        // 3-hop: start→X, X→Y, Y→end reachable.
        for e in &ends {
            if e.token_in == start.token_out {
                continue; // already covered by 2-hop
            }
            let mid_ok = dexs.iter().any(|m| {
                m.token_in == start.token_out
                    && m.token_out == e.token_in
                    && !is_unresolved_token(m.token_in)
                    && !is_unresolved_token(m.token_out)
            });
            if mid_ok {
                return true;
            }
        }
    }
    false
}

/// Sentinel meaning "token0 of the pool" / "token1 of the pool" — resolved to
/// real addresses by transfer pairing (or pool registry when available).
pub const TOKEN0_SENTINEL: Address = Address::new([0xEEu8; 20]);
pub const TOKEN1_SENTINEL: Address = Address::new([0xE1u8; 20]);

// ── Aggregator / DEX-router events (Phase 1.6) ──────────────────────────
// Topic hashes are keccak256 of the canonical event signatures; verified in
// `topic_hashes_match_canonical_signatures`.
//
// 1inch `Swapped(address sender, address srcToken, address dstToken,
// address dstReceiver, uint256 spentAmount, uint256 returnAmount)` — no
// indexed params: tokens + amounts entirely in data.
pub const ONEINCH_SWAPPED_TOPIC: B256 =
    alloy::primitives::b256!("d6d4f5681c246c9f42c203e287975af1601f8df8035a9251f79aab5c8f09e2f8");

// Paraswap AugustusSwapper (v3/v4) `Swapped(address initiator, address
// indexed beneficiary, address indexed srcToken, address indexed destToken,
// uint256 srcAmount, uint256 receivedAmount, uint256 expectedAmount, string
// referrer)` — tokens in topics, amounts at fixed data words.
pub const PARASWAP_SWAPPED_TOPIC: B256 =
    alloy::primitives::b256!("9cc2048b8af5eadff75759a3169b369efc538fb79c760fd396a4b355410b41b7");

// Paraswap (v5/v6) `SwappedV3(bytes16 uuid, address partner, uint256
// feePercent, address initiator, address indexed beneficiary, address indexed
// srcToken, address indexed destToken, uint256 srcAmount, uint256
// receivedAmount, uint256 expectedAmount)` — tokens in topics.
pub const PARASWAP_SWAPPED_V3_TOPIC: B256 =
    alloy::primitives::b256!("e00361d207b252a464323eb23d45d42583e391f2031acdd2e9fa36efddd43cb0");

// 0x Exchange V3/V4 `Fill(address makerAddress, address feeRecipientAddress,
// bytes makerAssetData, bytes takerAssetData, bytes makerFeeAssetData, bytes
// takerFeeAssetData, bytes32 orderHash, address takerAddress, address
// senderAddress, uint256 makerAssetFilledAmount, uint256 takerAssetFilledAmount,
// uint256 makerFeePaid, uint256 takerFeePaid, uint256 protocolFeePaid)` with
// makerAddress/feeRecipientAddress/orderHash indexed.
pub const ZRX_FILL_TOPIC: B256 =
    alloy::primitives::b256!("6869791f0a34781b29882982cc39e882768cf2c96995c2a110c577c53bc932d5");

/// Decode a liquidation fact via the per-protocol event registry.
/// Aave-style `LiquidationCall` and Compound V3 `Absorb` are covered; the
/// registry is intentionally not one hardcoded topic.
pub fn decode_liquidation(log: &LogData) -> Option<LiquidationFact> {
    let topic0 = *log.topics.first()?;
    if topic0 == *AAVE_V3_LIQUIDATION_CALL_TOPIC {
        // topics: [sig, collateralAsset, debtAsset, user]
        // data: debtToCover, liquidatedCollateralAmount, receiveAToken
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            user: Address::from_slice(&log.topics[3][12..]),
            // `LiquidationCall` does not carry the liquidator (msg.sender);
            // attribution falls back to the tx sender at classify time.
            liquidator: Address::ZERO,
            collateral_asset: Address::from_slice(&log.topics[1][12..]),
            debt_asset: Address::from_slice(&log.topics[2][12..]),
            collateral_amount: U256::from_be_slice(&log.data[32..64]),
            debt_to_cover: U256::from_be_slice(&log.data[0..32]),
        });
    }
    if topic0 == *COMPOUND_V3_ABSORB_TOPIC {
        // topics: [sig, absorber]; data: (borrower[], basePaid[], basePaidTotal)
        if log.topics.len() < 2 || log.data.len() < 84 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "compound_v3",
            user: Address::from_slice(&log.data[12..32]),
            liquidator: Address::from_slice(&log.topics[1][12..]),
            collateral_asset: Address::ZERO,
            debt_asset: Address::ZERO,
            collateral_amount: U256::ZERO,
            debt_to_cover: U256::from_be_slice(&log.data[52..84]),
        });
    }
    if topic0 == *COMPOUND_V2_LIQUIDATE_BORROW_TOPIC {
        // topics: [sig, liquidator, borrower]
        // data: repayAmount, cTokenCollateral (word), seizeTokens
        if log.topics.len() < 3 || log.data.len() < 96 {
            return None;
        }
        return Some(LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "compound_v2",
            user: Address::from_slice(&log.topics[2][12..]),
            liquidator: Address::from_slice(&log.topics[1][12..]),
            // Repaid market is the emitting cToken; seized collateral is a
            // different cToken address encoded in the data.
            debt_asset: log.address,
            debt_to_cover: U256::from_be_slice(&log.data[0..32]),
            collateral_asset: Address::from_slice(&log.data[44..64]),
            collateral_amount: U256::from_be_slice(&log.data[64..96]),
        });
    }
    None
}

/// Decode a flash-loan fact from a receipt log (Phase 2.2).
///
/// Covers Aave V2/V3 (native ABI layouts) and Balancer V2 (via the scanner's
/// canonical decoder so topic + layout stay in sync). Uni V3 `Flash` is
/// intentionally deferred: callback-repay semantics differ from a classic
/// loan+fee provider event.
pub fn decode_flash_loan(log: &LogData) -> Option<FlashLoanFact> {
    let topic0 = *log.topics.first()?;

    // Aave V3: FlashLoan(address indexed target, address initiator,
    //   address indexed asset, uint256 amount, uint8 mode, uint256 premium,
    //   uint16 referral) — topics [sig, target, asset], data 160 bytes.
    if topic0 == *AAVE_V3_FLASH_LOAN_TOPIC {
        if log.topics.len() < 3 || log.data.len() < 128 {
            return None;
        }
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: Address::from_slice(&log.data[12..32]),
            recipient: Address::from_slice(&log.topics[1][12..]),
            token: Address::from_slice(&log.topics[2][12..]),
            amount: U256::from_be_slice(&log.data[32..64]),
            fee: Some(U256::from_be_slice(&log.data[96..128])),
            provider: log.address,
        });
    }

    // Aave V2: FlashLoan(address indexed target, address indexed initiator,
    //   address indexed asset, uint256 amount, uint256 premium, uint16 referral)
    //   — topics [sig, target, initiator, asset], data 96 bytes.
    if topic0 == *AAVE_V2_FLASH_LOAN_TOPIC {
        if log.topics.len() < 4 || log.data.len() < 64 {
            return None;
        }
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v2",
            initiator: Address::from_slice(&log.topics[2][12..]),
            recipient: Address::from_slice(&log.topics[1][12..]),
            token: Address::from_slice(&log.topics[3][12..]),
            amount: U256::from_be_slice(&log.data[0..32]),
            fee: Some(U256::from_be_slice(&log.data[32..64])),
            provider: log.address,
        });
    }

    // Balancer V2: reuse the scanner decoder (topic + layout shared).
    if topic0 == *BALANCER_FLASH_LOAN_TOPIC {
        let rpc_log = logdata_to_rpc_log(log);
        let ev = decode_balancer_flash(&rpc_log)?;
        return Some(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "balancer_v2",
            initiator: ev.initiator,
            token: ev.token,
            amount: ev.amount,
            fee: ev.fee,
            recipient: ev.target,
            provider: log.address,
        });
    }

    None
}

/// Minimal `LogData` → alloy `Log` bridge with placeholder block/tx metadata
/// (the explorer keys off its own tx/log indices; decoders only read the
/// event payload and topics).
fn logdata_to_rpc_log(log: &LogData) -> alloy::rpc::types::Log {
    let data = alloy::primitives::LogData::new_unchecked(log.topics.clone(), log.data.clone());
    alloy::rpc::types::Log {
        inner: alloy::primitives::Log {
            address: log.address,
            data,
        },
        block_number: Some(0),
        block_hash: Some(alloy::primitives::B256::ZERO),
        block_timestamp: None,
        transaction_hash: Some(alloy::primitives::B256::ZERO),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    }
}

/// Decode a V3 Mint/Burn fact (JIT candidate).
pub fn decode_v3_mint_burn(log: &LogData) -> Option<JitFact> {
    let topic0 = *log.topics.first()?;
    let is_mint = topic0 == V3_MINT_TOPIC;
    let is_burn = topic0 == V3_BURN_TOPIC;
    if !is_mint && !is_burn {
        return None;
    }
    // topics: [sig, sender, owner]; data: tickLower, tickUpper, amount, amount0, amount1
    if log.topics.len() < 3 || log.data.len() < 160 {
        return None;
    }
    let tick_lower = i32::from_be_bytes([log.data[28], log.data[29], log.data[30], log.data[31]]);
    let tick_upper = i32::from_be_bytes([log.data[60], log.data[61], log.data[62], log.data[63]]);
    let liquidity = crate::utils::u128_from_be_bytes(&log.data[64 + 16..96]);
    Some(JitFact {
        tx_index: 0,
        log_index: 0,
        pool: log.address,
        owner: Address::from_slice(&log.topics[2][12..]),
        tick_lower,
        tick_upper,
        is_mint,
        liquidity,
        amount0: U256::from_be_slice(&log.data[96..128]),
        amount1: U256::from_be_slice(&log.data[128..160]),
    })
}

/// True when a token slot is not yet a resolved ERC-20 address (zero or a
/// token0/token1 sentinel).
fn is_unresolved_token(t: Address) -> bool {
    t.is_zero() || t == TOKEN0_SENTINEL || t == TOKEN1_SENTINEL
}

/// Resolve swap token directions from the tx's transfer stream.
///
/// Resolution order (Phase 1.1):
/// 1. **Pool registry** (`pool_tokens`: pool → (token0, token1)) — authoritative
///    mapping for sentinel-encoded directions (V2/Solidly/LB/Fluid/V3/V4/
///    Infinity/Metric). The remaining side is filled with the opposite token.
/// 2. **Transfer pairing fallback** for still-unresolved swaps:
///    - token_in  = nearest Transfer (lower log index) with `to == P`
///    - token_out = nearest Transfer (higher log index) with `from == P`
///
/// This works across V2/V3/Curve/Solidly without pool metadata and resolves
/// the V3 sentinel direction when the paired transfer is unambiguous.
pub fn attach_swap_tokens(
    swaps: &mut [SwapFact],
    transfers: &[TransferFact],
    pool_tokens: &std::collections::HashMap<Address, (Address, Address)>,
) {
    for s in swaps.iter_mut() {
        // Balancer already carries explicit tokens from topics.
        if s.amm == Amm::Balancer {
            continue;
        }

        // 1) Registry resolution (authoritative for known pools).
        if let Some(&(t0, t1)) = pool_tokens.get(&s.pool) {
            if s.token_in == TOKEN0_SENTINEL {
                s.token_in = t0;
            } else if s.token_in == TOKEN1_SENTINEL {
                s.token_in = t1;
            }
            if s.token_out == TOKEN0_SENTINEL {
                s.token_out = t0;
            } else if s.token_out == TOKEN1_SENTINEL {
                s.token_out = t1;
            }
            if is_unresolved_token(s.token_in) && !is_unresolved_token(s.token_out) {
                s.token_in = if s.token_out == t0 { t1 } else { t0 };
            }
            if is_unresolved_token(s.token_out) && !is_unresolved_token(s.token_in) {
                s.token_out = if s.token_in == t0 { t1 } else { t0 };
            }
        }

        // 2) Transfer pairing fallback for any still-unresolved direction.
        let mut token_in = Address::ZERO;
        let mut token_out = Address::ZERO;
        let mut best_in: Option<i64> = None;
        let mut best_out: Option<i64> = None;
        // Flow-ownership (§7.1): the funder of the input leg is the address
        // that transferred the input token into the pool right before the swap.
        let mut owner: Option<Address> = None;
        for t in transfers {
            let dist = t.log_index as i64 - s.log_index as i64;
            if dist < 0 && t.to == s.pool {
                // nearest before
                match best_in {
                    Some(d) if d <= dist.abs() => {}
                    _ => {
                        best_in = Some(dist.abs());
                        token_in = t.token;
                        owner = Some(t.from);
                    }
                }
            } else if dist > 0 && t.from == s.pool {
                match best_out {
                    Some(d) if d <= dist.abs() => {}
                    _ => {
                        best_out = Some(dist.abs());
                        token_out = t.token;
                    }
                }
            }
        }
        if s.owner.is_none() {
            s.owner = owner;
        }
        if is_unresolved_token(s.token_in) && !token_in.is_zero() {
            s.token_in = token_in;
        }
        if is_unresolved_token(s.token_out) && !token_out.is_zero() {
            s.token_out = token_out;
        }

        // Fallback: router/hop layouts sometimes emit the pool→recipient
        // transfer slightly before the next Swap log. Accept nearest pool
        // outflow within a short window when still unresolved.
        if is_unresolved_token(s.token_out) {
            let mut best: Option<(i64, Address)> = None;
            for t in transfers {
                let dist = (t.log_index as i64 - s.log_index as i64).abs();
                if dist == 0 || dist > 24 {
                    continue;
                }
                if t.from != s.pool {
                    continue;
                }
                if t.token.is_zero() || t.token == s.token_in {
                    continue;
                }
                match best {
                    Some((d, _)) if d <= dist => {}
                    _ => best = Some((dist, t.token)),
                }
            }
            if let Some((_, tok)) = best {
                s.token_out = tok;
            }
        }
        if is_unresolved_token(s.token_in) && !s.token_out.is_zero() {
            let mut best: Option<(i64, Address)> = None;
            for t in transfers {
                let dist = (t.log_index as i64 - s.log_index as i64).abs();
                if dist == 0 || dist > 24 {
                    continue;
                }
                if t.to != s.pool {
                    continue;
                }
                if t.token.is_zero() || t.token == s.token_out {
                    continue;
                }
                match best {
                    Some((d, _)) if d <= dist => {}
                    _ => best = Some((dist, t.token)),
                }
            }
            if let Some((_, tok)) = best {
                s.token_in = tok;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256, B256};

    fn log(address: Address, topics: Vec<B256>, data: Vec<u8>) -> LogData {
        LogData {
            address,
            topics,
            data: alloy::primitives::Bytes::from(data),
        }
    }

    #[test]
    fn transfer_decodes() {
        let l = log(
            address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
            vec![
                TRANSFER_TOPIC,
                b256!("0000000000000000000000000000000000000000000000000000000000000001"),
                b256!("0000000000000000000000000000000000000000000000000000000000000002"),
            ],
            {
                let mut b = vec![0u8; 32];
                b[31] = 42;
                b
            },
        );
        let t = decode_transfer(&l).unwrap();
        assert_eq!(
            t.token,
            address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        );
        assert_eq!(t.amount, U256::from(42));
    }

    #[test]
    fn v3_swap_decodes_direction() {
        let pool = address!("1111111111111111111111111111111111111111");
        // amount0 = -100 (pool received), amount1 = +90 (pool paid out)
        let mut data = vec![0u8; 160];
        for b in data[0..24].iter_mut() {
            *b = 0xff;
        }
        data[24..32].copy_from_slice(&(-100i64).to_be_bytes());
        data[56..64].copy_from_slice(&90u64.to_be_bytes());
        let l = log(pool, vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO], data);
        let (amm, s) = decode_swap(&l).unwrap();
        assert_eq!(amm, Amm::V3);
        assert_eq!(s.pool, pool);
        assert_eq!(s.amount_in, U256::from(100));
        assert_eq!(s.amount_out, U256::from(90));
        assert_eq!(s.token_in, TOKEN0_SENTINEL);
    }

    #[test]
    fn aave_v3_flash_loan_decodes() {
        let target = b256!("0000000000000000000000001111111111111111111111111111111111111111");
        let asset = b256!("0000000000000000000000002222222222222222222222222222222222222222");
        let initiator = address!("3333333333333333333333333333333333333333");
        let mut data = vec![0u8; 160];
        data[12..32].copy_from_slice(initiator.as_slice());
        data[56..64].copy_from_slice(&1_000u64.to_be_bytes());
        data[64 + 31] = 1; // interestRateMode
        data[96 + 24..96 + 32].copy_from_slice(&5u64.to_be_bytes()); // premium
        let l = log(
            address!("4444444444444444444444444444444444444444"),
            vec![*AAVE_V3_FLASH_LOAN_TOPIC, target, asset],
            data,
        );
        let f = decode_flash_loan(&l).unwrap();
        assert_eq!(f.protocol, "aave_v3");
        assert_eq!(
            f.token,
            address!("2222222222222222222222222222222222222222")
        );
        assert_eq!(f.amount, U256::from(1_000));
        assert_eq!(f.fee, Some(U256::from(5)));
        assert_eq!(f.initiator, initiator);
    }

    #[test]
    fn aave_v2_flash_loan_decodes() {
        let target = b256!("0000000000000000000000001111111111111111111111111111111111111111");
        let initiator_topic =
            b256!("0000000000000000000000003333333333333333333333333333333333333333");
        let asset = b256!("0000000000000000000000002222222222222222222222222222222222222222");
        let mut data = vec![0u8; 96];
        data[24..32].copy_from_slice(&3_000u64.to_be_bytes());
        data[56..64].copy_from_slice(&7u64.to_be_bytes());
        let l = log(
            address!("4444444444444444444444444444444444444444"),
            vec![*AAVE_V2_FLASH_LOAN_TOPIC, target, initiator_topic, asset],
            data,
        );
        let f = decode_flash_loan(&l).unwrap();
        assert_eq!(f.protocol, "aave_v2");
        assert_eq!(f.amount, U256::from(3_000));
        assert_eq!(f.fee, Some(U256::from(7)));
    }

    #[test]
    fn liquidation_aave_v3_decodes() {
        let collateral = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let debt = b256!("000000000000000000000000cccccccccccccccccccccccccccccccccccccccc");
        let user = b256!("000000000000000000000000dddddddddddddddddddddddddddddddddddddddd");
        let mut data = vec![0u8; 64];
        data[24..32].copy_from_slice(&99u64.to_be_bytes()); // debtToCover
        data[56..64].copy_from_slice(&500u64.to_be_bytes()); // liquidatedCollateralAmount
        let l = log(
            address!("794a61358d6845594f94dc1db02a252b5b4814ad"),
            vec![*AAVE_V3_LIQUIDATION_CALL_TOPIC, collateral, debt, user],
            data,
        );
        let liq = decode_liquidation(&l).unwrap();
        assert_eq!(liq.protocol, "aave_v3");
        assert_eq!(
            liq.user,
            address!("dddddddddddddddddddddddddddddddddddddddd")
        );
        // liquidator is not in the event — resolved to tx.from at classify time
        assert_eq!(
            liq.liquidator,
            address!("0000000000000000000000000000000000000000")
        );
        assert_eq!(liq.debt_to_cover, U256::from(99));
        assert_eq!(liq.collateral_amount, U256::from(500));
    }

    #[test]
    fn liquidation_compound_v2_decodes() {
        let liquidator = b256!("000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let borrower = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let mut data = vec![0u8; 96];
        data[24..32].copy_from_slice(&99u64.to_be_bytes()); // repayAmount
        data[44..64]
            .copy_from_slice(address!("cccccccccccccccccccccccccccccccccccccccc").as_slice()); // cTokenCollateral
        data[88..96].copy_from_slice(&500u64.to_be_bytes()); // seizeTokens
        let l = log(
            address!("dddddddddddddddddddddddddddddddddddddddd"),
            vec![*COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, liquidator, borrower],
            data,
        );
        let liq = decode_liquidation(&l).unwrap();
        assert_eq!(liq.protocol, "compound_v2");
        assert_eq!(
            liq.liquidator,
            address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(
            liq.user,
            address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(
            liq.debt_asset,
            address!("dddddddddddddddddddddddddddddddddddddddd")
        );
        assert_eq!(
            liq.collateral_asset,
            address!("cccccccccccccccccccccccccccccccccccccccc")
        );
        assert_eq!(liq.debt_to_cover, U256::from(99));
        assert_eq!(liq.collateral_amount, U256::from(500));
    }

    #[test]
    fn attach_tokens_pairs_transfers() {
        let pool = address!("1000000000000000000000000000000000000000");
        let tin = address!("2000000000000000000000000000000000000000");
        let tout = address!("3000000000000000000000000000000000000000");
        let mk = |li: u64, token: Address, from: Address, to: Address| TransferFact {
            tx_index: 0,
            log_index: li,
            token,
            from,
            to,
            amount: U256::from(1),
        };
        let transfers = vec![
            mk(
                0,
                tin,
                address!("4000000000000000000000000000000000000000"),
                pool,
            ),
            mk(
                2,
                tout,
                pool,
                address!("4000000000000000000000000000000000000000"),
            ),
        ];
        let (_, mut s) = decode_swap(&log(pool, vec![V2_SWAP_TOPIC, B256::ZERO, B256::ZERO], {
            let mut d = vec![0u8; 128];
            d[31] = 5; // amount0In
            d[64 + 31] = 4; // amount0Out
            d
        }))
        .unwrap();
        s.log_index = 1;
        attach_swap_tokens(
            std::slice::from_mut(&mut s),
            &transfers,
            &std::collections::HashMap::new(),
        );
        assert_eq!(s.token_in, tin);
        assert_eq!(s.token_out, tout);
        // Flow ownership (§7.1): the funder of the input leg is the `from` of
        // the nearest inbound transfer to the pool before the swap log.
        assert_eq!(s.owner, Some(address!("4000000000000000000000000000000000000000")));
    }

    #[test]
    fn topic_hashes_match_canonical_signatures() {
        use alloy::primitives::keccak256;
        let k = |s: &str| keccak256(s.as_bytes());
        // 1inch AggregationRouterV4/V5.
        assert_eq!(
            k("Swapped(address,address,address,address,uint256,uint256)"),
            ONEINCH_SWAPPED_TOPIC
        );
        // Paraswap AugustusSwapper (v3/v4) + v6.x.
        assert_eq!(
            k("Swapped(address,address,address,address,uint256,uint256,uint256,string)"),
            PARASWAP_SWAPPED_TOPIC
        );
        assert_eq!(
            k("SwappedV3(bytes16,address,uint256,address,address,address,address,uint256,uint256,uint256)"),
            PARASWAP_SWAPPED_V3_TOPIC
        );
        // 0x Exchange V3/V4 `Fill` (assets + fixed tail).
        assert_eq!(
            k("Fill(address,address,bytes,bytes,bytes,bytes,bytes32,address,address,uint256,uint256,uint256,uint256,uint256)"),
            ZRX_FILL_TOPIC
        );
    }

    #[test]
    fn registry_resolves_sentinel_direction_without_transfers() {
        // V2 swap where amount0In > 0 => token0 in. Registry resolves without
        // any transfer-pairing hints.
        let pool = address!("1000000000000000000000000000000000000000");
        let t0 = address!("2000000000000000000000000000000000000000");
        let t1 = address!("3000000000000000000000000000000000000000");
        let (_, mut s) = decode_swap(&log(pool, vec![V2_SWAP_TOPIC, B256::ZERO, B256::ZERO], {
            let mut d = vec![0u8; 128];
            d[31] = 5; // amount0In
            d[64 + 31] = 4; // amount0Out
            d
        }))
        .unwrap();
        s.log_index = 1;
        let mut pools = std::collections::HashMap::new();
        pools.insert(pool, (t0, t1));
        attach_swap_tokens(std::slice::from_mut(&mut s), &[], &pools);
        assert_eq!(s.token_in, t0);
        assert_eq!(s.token_out, t1);

        // V3 token1-in sentinel: registry resolves token_in = t1, token_out = t0.
        let mut data = vec![0u8; 160];
        for b in data[0..24].iter_mut() {
            *b = 0xff; // amount0 negative
        }
        data[24..32].copy_from_slice(&(-100i64).to_be_bytes());
        data[56..64].copy_from_slice(&90u64.to_be_bytes());
        let (_, mut s3) = decode_swap(&log(
            pool,
            vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO],
            data,
        ))
        .unwrap();
        s3.log_index = 0;
        attach_swap_tokens(std::slice::from_mut(&mut s3), &[], &pools);
        assert_eq!(s3.token_in, t0); // amount0 < 0 => token0 in
        assert_eq!(s3.token_out, t1);
    }

    fn topic_addr(a: Address) -> B256 {
        let mut b = [0u8; 32];
        b[12..].copy_from_slice(a.as_slice());
        B256::from(b)
    }

    #[test]
    fn oneinch_swapped_decodes() {
        let router = address!("1111111254fb6c44bac0bed2854e76f90643097d");
        let src = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let dst = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let mut data = vec![0u8; 192];
        data[32 + 12..32 + 32].copy_from_slice(src.as_slice()); // srcToken
        data[64 + 12..64 + 32].copy_from_slice(dst.as_slice()); // dstToken
        data[128 + 24..160].copy_from_slice(&1000u64.to_be_bytes()); // spentAmount
        data[160 + 24..192].copy_from_slice(&990u64.to_be_bytes()); // returnAmount
        let (amm, s) = decode_swap(&log(router, vec![ONEINCH_SWAPPED_TOPIC], data)).unwrap();
        assert_eq!(amm, Amm::Aggregator);
        assert_eq!(s.pool, router);
        assert_eq!(s.token_in, src);
        assert_eq!(s.token_out, dst);
        assert_eq!(s.amount_in, U256::from(1000));
        assert_eq!(s.amount_out, U256::from(990));
    }

    #[test]
    fn paraswap_swapped_decodes() {
        let router = address!("def171fe48cf0115b1d80b88dc8eab59176fee57");
        let src = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let dst = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let beneficiary = address!("cccccccccccccccccccccccccccccccccccccccc");
        let mut data = vec![0u8; 160];
        data[32 + 24..64].copy_from_slice(&5000u64.to_be_bytes()); // srcAmount
        data[64 + 24..96].copy_from_slice(&4900u64.to_be_bytes()); // receivedAmount
        let (amm, s) = decode_swap(&log(
            router,
            vec![
                PARASWAP_SWAPPED_TOPIC,
                topic_addr(beneficiary),
                topic_addr(src),
                topic_addr(dst),
            ],
            data,
        ))
        .unwrap();
        assert_eq!(amm, Amm::Aggregator);
        assert_eq!(s.token_in, src);
        assert_eq!(s.token_out, dst);
        assert_eq!(s.amount_in, U256::from(5000));
        assert_eq!(s.amount_out, U256::from(4900));
    }

    #[test]
    fn paraswap_swapped_v3_decodes() {
        let router = address!("def171fe48cf0115b1d80b88dc8eab59176fee57");
        let src = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let dst = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let beneficiary = address!("cccccccccccccccccccccccccccccccccccccccc");
        let mut data = vec![0u8; 224];
        data[128 + 24..160].copy_from_slice(&7000u64.to_be_bytes()); // srcAmount
        data[160 + 24..192].copy_from_slice(&6900u64.to_be_bytes()); // receivedAmount
        let (amm, s) = decode_swap(&log(
            router,
            vec![
                PARASWAP_SWAPPED_V3_TOPIC,
                topic_addr(beneficiary),
                topic_addr(src),
                topic_addr(dst),
            ],
            data,
        ))
        .unwrap();
        assert_eq!(amm, Amm::Aggregator);
        assert_eq!(s.token_in, src);
        assert_eq!(s.token_out, dst);
        assert_eq!(s.amount_in, U256::from(7000));
        assert_eq!(s.amount_out, U256::from(6900));
    }

    #[test]
    fn zrx_fill_decodes_amounts_tokens_unresolved() {
        let exchange = address!("4f833a24e1f95d70f837921e96e4e6def2ad0bee");
        let mut data = vec![0u8; 352];
        data[192 + 24..224].copy_from_slice(&999u64.to_be_bytes()); // makerAssetFilled
        data[224 + 24..256].copy_from_slice(&100_000u64.to_be_bytes()); // takerAssetFilled
        let (amm, s) = decode_swap(&log(
            exchange,
            vec![
                ZRX_FILL_TOPIC,
                topic_addr(address!("1250a4395798a18a48c6118a7f8dff8e8479c29a")),
                topic_addr(address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")),
                alloy::primitives::b256!(
                    "1111111111111111111111111111111111111111111111111111111111111111"
                ),
            ],
            data,
        ))
        .unwrap();
        assert_eq!(amm, Amm::Aggregator);
        assert_eq!(s.pool, exchange);
        // Tokens are inside dynamic asset blobs; direction is resolved later.
        assert!(s.token_in.is_zero());
        assert!(s.token_out.is_zero());
        assert_eq!(s.amount_in, U256::from(100_000)); // taker-side input
        assert_eq!(s.amount_out, U256::from(999)); // maker-side output
    }

    #[test]
    fn aggregator_dup_edge_removed_when_dex_pair_covers() {
        let src = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let dst = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let router = address!("1111111254fb6c44bac0bed2854e76f90643097d");
        let pool = address!("3000000000000000000000000000000000000003");
        let pair =
            |amm: Amm, pool: Address, tin: Address, tout: Address, ain: u64, aout: u64| SwapFact {
                tx_index: 0,
                log_index: 0,
                pool,
                amm,
                token_in: tin,
                token_out: tout,
                amount_in: U256::from(ain),
                amount_out: U256::from(aout),
                tick: None,
                owner: None,
            };
        // Same A→B flow on a single pool: the DEX edge wins.
        let mut edges = vec![
            pair(Amm::Aggregator, router, src, dst, 1000, 990),
            pair(Amm::V2, pool, src, dst, 1000, 990),
        ];
        dedup_aggregator_facts(&mut edges);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].amm, Amm::V2);

        // Multi-hop A→X→B covers an A→B aggregator edge (2-hop chain).
        let x = address!("cccccccccccccccccccccccccccccccccccccccc");
        let mut edges = vec![
            pair(Amm::Aggregator, router, src, dst, 1000, 980),
            pair(Amm::V2, pool, src, x, 1000, 990),
            pair(Amm::V3, pool, x, dst, 990, 980),
        ];
        dedup_aggregator_facts(&mut edges);
        assert_eq!(edges.len(), 2);
        assert!(!edges.iter().any(|s| s.amm == Amm::Aggregator));

        // Unrelated aggregator flow (no DEX chain) is kept.
        let mut edges = vec![pair(Amm::Aggregator, router, src, dst, 1000, 990)];
        dedup_aggregator_facts(&mut edges);
        assert_eq!(edges.len(), 1);
    }
}
