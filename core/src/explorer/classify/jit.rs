//! JIT / JitArb classify passes.
use std::collections::HashMap;

use alloy::primitives::{Address, B256, U256};

use crate::explorer::types::{Confidence, JitFact, MevEvent, MevKind, PnlBasis, SwapFact};

use super::BlockInput;

/// JIT pairing: same pool, same owner, same tick range, Mint before Burn.
///
/// Phase 1.5: pairs an in-block Mint+Burn *or* a prior-block open position
/// (loaded from `jit_open_positions`) with an in-block exact-liquidity Burn.
/// A candidate is only emitted when the block contains an in-window swap in
/// that pool whose tick lies inside the range (tick-overlap validation).
pub(super) fn classify_jit(input: &BlockInput) -> Vec<MevEvent> {
    let mints: Vec<JitFact> = input
        .txs
        .iter()
        .flat_map(|t| {
            t.jit.iter().map(move |j| {
                let mut j2 = j.clone();
                j2.tx_index = t.tx_index;
                j2
            })
        })
        .filter(|j| j.is_mint)
        .collect();
    let burns: Vec<(u64, &JitFact)> = input
        .txs
        .iter()
        .flat_map(|t| t.jit.iter().map(move |j| (t.tx_index, j)))
        .filter(|(_, j)| !j.is_mint)
        .collect();

    let mut out = Vec::new();

    // Same-block Mint → Burn.
    for mint in &mints {
        let burn = burns.iter().find(|(_, b)| {
            b.pool == mint.pool
                && b.owner == mint.owner
                && b.tick_lower == mint.tick_lower
                && b.tick_upper == mint.tick_upper
                && b.liquidity >= mint.liquidity
        });
        if let Some((burn_tx, _)) = burn {
            if let Some(ev) = build_jit_event(
                input,
                mint.pool,
                mint.owner,
                mint.tick_lower,
                mint.tick_upper,
                mint.tx_index,
                mint.liquidity,
                mint.amount0,
                mint.amount1,
                *burn_tx,
                input.block,
                mint.bin_amm,
            ) {
                out.push(ev);
            }
        }
    }

    // Cross-block: a prior-block open position closed by an in-block Burn.
    for pos in &input.open_positions {
        let burn = burns.iter().find(|(_, b)| {
            b.pool == pos.pool
                && b.owner == pos.owner
                && b.tick_lower == pos.tick_lower
                && b.tick_upper == pos.tick_upper
                && b.liquidity >= pos.liquidity
        });
        if let Some((burn_tx, b)) = burn {
            // Anchor to the burn tx when the opening tx is not in this block.
            if let Some(ev) = build_jit_event(
                input,
                pos.pool,
                pos.owner,
                pos.tick_lower,
                pos.tick_upper,
                *burn_tx,
                pos.liquidity,
                U256::ZERO,
                U256::ZERO,
                *burn_tx,
                pos.opened_block,
                b.bin_amm,
            ) {
                out.push(ev);
            }
        }
    }

    out
}

/// Build one JIT event, or `None` when no in-window swap validates the range.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_jit_event(
    input: &BlockInput,
    pool: Address,
    owner: Address,
    tick_lower: i32,
    tick_upper: i32,
    mint_tx: u64,
    liquidity: u128,
    amount0: U256,
    amount1: U256,
    burn_tx: u64,
    opened_block: u64,
    bin_amm: bool,
) -> Option<MevEvent> {
    // V3: require an in-range tick. LB: any same-pool swap (Swap has no tick).
    let in_range = |s: &SwapFact| {
        if s.pool != pool {
            return false;
        }
        if bin_amm {
            return true;
        }
        s.tick.is_some_and(|k| k >= tick_lower && k <= tick_upper)
    };
    if !input.txs.iter().flat_map(|t| &t.swaps).any(in_range) {
        return None;
    }

    let gas_cost_wei = input
        .txs
        .iter()
        .filter(|t| t.tx_index == mint_tx)
        .map(|t| {
            U256::from(t.gas_used)
                .saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128))
        })
        .next()
        .unwrap_or(U256::ZERO);

    // Documented estimate: in-range swap volume × pool fee (30 bps default).
    // V3 fees are not in logs and principal amounts are not fees, so this is
    // explicitly inferred (`Confidence::Exact` remains on the detection).
    const POOL_FEE_BPS: u32 = 30;
    let mut by_token: std::collections::BTreeMap<String, U256> = std::collections::BTreeMap::new();
    for t in &input.txs {
        for s in &t.swaps {
            if in_range(s) {
                let fee =
                    s.amount_in.saturating_mul(U256::from(POOL_FEE_BPS)) / U256::from(10_000u32);
                *by_token.entry(format!("{:#x}", s.token_in)).or_default() += fee;
            }
        }
    }
    let fees_estimated: serde_json::Map<String, serde_json::Value> = by_token
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.to_string())))
        .collect();

    let range_key = if bin_amm { "bin" } else { "tick" };
    Some(MevEvent {
        block: input.block,
        ts: input.ts,
        tx_index: mint_tx,
        tx_hash: B256::ZERO, // mint tx hash resolved at ingest stamping
        kind: MevKind::Jit,
        searcher: owner,
        contract: None,
        pools: vec![pool],
        profit_token: None, // fee capture — priced via pool tokens
        profit_amount: None,
        profit_tokens: vec![],
        profit_usd: None,
        gas_cost_wei,
        flashloan_fee_wei: None,
        flashloan_fee_token: None,
        confidence: Confidence::Exact,
        victim_hashes: vec![],
        victim_swap_size: None,
        details: serde_json::json!({
            "pool": format!("{:#x}", pool),
            "owner": format!("{:#x}", owner),
            "tick_lower": tick_lower,
            "tick_upper": tick_upper,
            // Liquidity is a u128 and routinely exceeds u64::MAX, which
            // `serde_json::Value` cannot represent, so it is stringified like
            // amount0/amount1 below.
            "liquidity": liquidity.to_string(),
            "burn_tx_index": burn_tx,
            "opened_block": opened_block,
            "held_blocks": input.block.saturating_sub(opened_block),
            "amount0": amount0.to_string(),
            "amount1": amount1.to_string(),
            "bin_amm": bin_amm,
            "mode": "realized",
            // P3.1: LB bin-JIT — tip/fees as F-basis estimate; detection Exact.
            "tags": if bin_amm {
                serde_json::json!(["lb_bin_jit"])
            } else {
                serde_json::json!([])
            },
            "pnl_basis": PnlBasis::F.as_str(),
            "reasons": if bin_amm {
                serde_json::json!(["SHORT_LP_LIFETIME", "SAME_POOL_SWAP_DURING_LB_POSITION"])
            } else {
                serde_json::json!(["SHORT_LP_LIFETIME", "OVERLAPPING_TICK_RANGE"])
            },
            "fees_estimated": {
                "method": "in_range_volume_x_pool_fee",
                "pool_fee_bps": POOL_FEE_BPS,
                "confidence": "inferred",
                "range": range_key,
                "by_token": fees_estimated,
            },
            "pnl": {
                "basis": PnlBasis::F.as_str(),
                "method": "in_range_volume_x_pool_fee",
            },
        }),
    })
}

/// Stamp tx hashes onto JIT events (mint tx lookup happens at ingest where
/// hashes are available in the same loop as decoding).
pub(crate) fn stamp_jit_tx_hashes(events: &mut [MevEvent], tx_hashes: &HashMap<u64, B256>) {
    for ev in events.iter_mut() {
        if ev.kind == MevKind::Jit && ev.tx_hash == B256::ZERO {
            if let Some(h) = tx_hashes.get(&ev.tx_index) {
                ev.tx_hash = *h;
            }
        }
    }
}
