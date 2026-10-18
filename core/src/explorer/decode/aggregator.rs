//! Aggregator/router swap edges, brand signals, and DEX-edge dedup.
use crate::utils::{abi_word_address, topic_address};
use alloy::primitives::{address, Address, B256, U256};

use crate::data::LogData;
use crate::explorer::types::{AggregatorSignal, Amm, LegSource, SwapFact};

use super::sentinels::is_unresolved_token;

/// Decode an aggregator/DEX-router edge. Returns fully-directional
/// facts for 1inch `Swapped`, Paraswap `Swapped`/`SwappedV3`, 0x `Fill`,
/// and Odos Router V2 `Swap`. Brand is resolved via router allowlist with
/// topic fallback (Kyber MetaAggregation shares the 1inch `Swapped` topic).
/// 0x tokens are not in the event payload (asset encodings live in dynamic
/// data), so token slots stay unresolved for the transfer-pairing fallback.
pub(super) fn decode_aggregator_swap(log: &LogData) -> Option<(Amm, SwapFact)> {
    decode_aggregator_inner(log).map(|(_, fact)| (Amm::Aggregator, fact))
}

/// Side-channel intent signal (brand + amounts). Survives dedup of swap edges.
pub fn decode_aggregator_signal(log: &LogData) -> Option<AggregatorSignal> {
    let (brand, fact) = decode_aggregator_inner(log)?;
    Some(AggregatorSignal {
        brand,
        router: fact.pool,
        token_in: fact.token_in,
        token_out: fact.token_out,
        amount_in: fact.amount_in,
        amount_out: fact.amount_out,
        log_index: 0,
    })
}

fn decode_aggregator_inner(log: &LogData) -> Option<(&'static str, SwapFact)> {
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

    // 1inch V4/V5 (and Kyber MetaAggregation with the same ABI):
    // Swapped(sender, srcToken, dstToken, dstReceiver, spentAmount, returnAmount).
    if topic0 == ONEINCH_SWAPPED_TOPIC {
        if log.data.len() < 192 {
            return None;
        }
        let mut fact = base(log.address);
        fact.token_in = abi_word_address(&log.data, 1);
        fact.token_out = abi_word_address(&log.data, 2);
        fact.amount_in = U256::from_be_slice(&log.data[128..160]);
        fact.amount_out = U256::from_be_slice(&log.data[160..192]);
        let brand = brand_for_router(log.address).unwrap_or("1inch");
        return Some((brand, fact));
    }

    // Paraswap v3/v4: Swapped(initiator, beneficiary, srcToken, destToken,
    // srcAmount, receivedAmount, expectedAmount, referrer); beneficiary/
    // srcToken/destToken indexed.
    if topic0 == PARASWAP_SWAPPED_TOPIC {
        if log.topics.len() < 4 || log.data.len() < 160 {
            return None;
        }
        let mut fact = base(log.address);
        fact.token_in = topic_address(log.topics[2]);
        fact.token_out = topic_address(log.topics[3]);
        fact.amount_in = U256::from_be_slice(&log.data[32..64]);
        fact.amount_out = U256::from_be_slice(&log.data[64..96]);
        let brand = brand_for_router(log.address).unwrap_or("paraswap");
        return Some((brand, fact));
    }

    // Paraswap v5/v6: SwappedV3(uuid, partner, feePercent, initiator,
    // beneficiary, srcToken, destToken, srcAmount, receivedAmount,
    // expectedAmount); beneficiary/srcToken/destToken indexed.
    if topic0 == PARASWAP_SWAPPED_V3_TOPIC {
        if log.topics.len() < 4 || log.data.len() < 224 {
            return None;
        }
        let mut fact = base(log.address);
        fact.token_in = topic_address(log.topics[2]);
        fact.token_out = topic_address(log.topics[3]);
        fact.amount_in = U256::from_be_slice(&log.data[128..160]);
        fact.amount_out = U256::from_be_slice(&log.data[160..192]);
        let brand = brand_for_router(log.address).unwrap_or("paraswap");
        return Some((brand, fact));
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
        let brand = brand_for_router(log.address).unwrap_or("0x");
        return Some((brand, fact));
    }

    // Odos Router V2: Swap(sender, inputAmount, inputToken, amountOut,
    // outputToken, slippage, referralCode) — all non-indexed.
    if topic0 == ODOS_SWAP_TOPIC {
        if log.data.len() < 224 {
            return None;
        }
        let mut fact = base(log.address);
        fact.amount_in = U256::from_be_slice(&log.data[32..64]);
        fact.token_in = abi_word_address(&log.data, 2);
        fact.amount_out = U256::from_be_slice(&log.data[96..128]);
        fact.token_out = abi_word_address(&log.data, 4);
        let brand = brand_for_router(log.address).unwrap_or("odos");
        return Some((brand, fact));
    }

    None
}

/// Known DEX-router deployments → brand. Kyber shares the 1inch `Swapped`
/// topic, so allowlist is the only way to label it correctly.
pub fn brand_for_router(router: Address) -> Option<&'static str> {
    // Keep lowercase hex comparisons via Address equality.
    match router {
        // 1inch AggregationRouter V4 / V5 / V6 (canonical multi-chain)
        a if a == address!("0x1111111254fb6c44bac0bed2854e76f90643097d") => Some("1inch"),
        a if a == address!("0x111111125421ca6dc452d289314280a0f8842a65") => Some("1inch"),
        a if a == address!("0x1111111254eeb25477b68fb85ed929f73a960582") => Some("1inch"),
        // Paraswap AugustusSwapper
        a if a == address!("0xDEF171Fe48CF0115B1d80b88dc8eAB59176FEe57") => Some("paraswap"),
        // Odos Router V2 (deterministic / shared across many chains)
        a if a == address!("0xCf5540fFFCdC3d510B18bFcA6d2b9987b0772559") => Some("odos"),
        a if a == address!("0xca423977156bb05b13a2ba3b76bc5419e2fe9680") => Some("odos"),
        // Kyber MetaAggregationRouterV2 (multi-chain)
        a if a == address!("0x6131B5fae19EA4f9D964eAc0408E4408b66337b5") => Some("kyber"),
        _ => None,
    }
}

/// Drop aggregator edges that duplicate registry/DEX swap edges in the same tx
/// (dedup). Aggregator events often coexist with the underlying pool
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

// Odos Router V2 `Swap(address sender, uint256 inputAmount, address inputToken,
// uint256 amountOut, address outputToken, int256 slippage, uint32 referralCode)`.
pub const ODOS_SWAP_TOPIC: B256 =
    alloy::primitives::b256!("823eaf01002d7353fbcadb2ea3305cc46fa35d799cb0914846d185ac06f8ad05");
