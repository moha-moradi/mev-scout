//! Aggregator/router swap edges and DEX-edge dedup.
use alloy::primitives::{Address, B256, U256};

use crate::data::LogData;
use crate::explorer::types::{Amm, LegSource, SwapFact};

use super::sentinels::is_unresolved_token;

/// Decode an aggregator/DEX-router edge (Phase 1.6). Returns fully-directional
/// facts for 1inch `Swapped`, Paraswap `Swapped`/`SwappedV3`, and 0x `Fill`.
/// 0x tokens are not in the event payload (asset encodings live in dynamic
/// data), so token slots stay unresolved for the transfer-pairing fallback.
pub(super) fn decode_aggregator_swap(log: &LogData) -> Option<(Amm, SwapFact)> {
    let topic0 = *log.topics.first()?;
    let base = |pool: Address| SwapFact {
        tx_index: 0,
        log_index: 0,
        pool,
        amm: Amm::Aggregator,
        token_in: Address::ZERO,
        token_out: Address::ZERO,
        token_source: LegSource::Proximity,
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

