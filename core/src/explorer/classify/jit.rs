//! JIT / JitArb classify passes.
use std::collections::HashMap;

use alloy::primitives::{Address, B256, U256};

use crate::explorer::reject::RejectReason;
use crate::explorer::types::{JitFact, MevEvent, MevKind, PnlBasis, SwapFact};

use super::drops::DropSink;
use super::BlockInput;

/// Default pool-fee-tier estimate for JIT fee capture (30 bps). Overridable via
/// `explorer.jit_pool_fee_bps`; decoding each pool's real tier is the real fix.
pub(crate) const POOL_FEE_BPS: u32 = 30;

/// Position fields for [`build_jit_event`].
pub(super) struct JitEventPosition {
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
}

/// JIT pairing: same pool, same owner, same tick range, Mint before Burn.
///
/// pairs an in-block Mint+Burn *or* a prior-block open position
/// (loaded from `jit_open_positions`) with an in-block exact-liquidity Burn.
/// A Burn strictly after the Mint's tx closes a position, and each Burn closes
/// at most one position (a `find` without consumption let one Burn satisfy
/// several identical Mints — duplicate P&L on one tx_index).
/// A candidate is only emitted when the block contains an in-window swap in
/// that pool whose tick lies inside the range (tick-overlap validation).
pub(super) fn classify_jit(input: &BlockInput, drops: &mut DropSink) -> Vec<MevEvent> {
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
    // (burn tx_index, log_index): one-shot burn consumption. Keyed by log as
    // well as tx because a single tx can carry several JIT burns.
    let mut used_burns: std::collections::HashSet<(u64, u64)> = Default::default();

    // Same-block Mint → Burn (Burn strictly after the Mint's tx).
    for mint in &mints {
        let burn = burns.iter().find(|(burn_tx, b)| {
            *burn_tx > mint.tx_index
                && !used_burns.contains(&(*burn_tx, b.log_index))
                && b.pool == mint.pool
                && b.owner == mint.owner
                && b.tick_lower == mint.tick_lower
                && b.tick_upper == mint.tick_upper
                && b.liquidity >= mint.liquidity
        });
        if let Some((burn_tx, b)) = burn {
            used_burns.insert((*burn_tx, b.log_index));
            if let Some(ev) = build_jit_event(
                input,
                JitEventPosition {
                    pool: mint.pool,
                    owner: mint.owner,
                    tick_lower: mint.tick_lower,
                    tick_upper: mint.tick_upper,
                    mint_tx: mint.tx_index,
                    liquidity: mint.liquidity,
                    amount0: mint.amount0,
                    amount1: mint.amount1,
                    burn_tx: *burn_tx,
                    opened_block: input.block,
                    bin_amm: mint.bin_amm,
                },
            ) {
                out.push(ev);
            } else {
                drops.anchor(
                    mint.tx_index,
                    RejectReason::JitPairingUnmatched,
                    "no_validating_swap_in_position_range",
                );
            }
        } else {
            drops.anchor(
                mint.tx_index,
                RejectReason::JitPairingUnmatched,
                "mint_without_burn",
            );
        }
    }

    // Cross-block: a prior-block open position closed by an in-block Burn.
    for pos in &input.open_positions {
        let burn = burns.iter().find(|(burn_tx, b)| {
            !used_burns.contains(&(*burn_tx, b.log_index))
                && b.pool == pos.pool
                && b.owner == pos.owner
                && b.tick_lower == pos.tick_lower
                && b.tick_upper == pos.tick_upper
                && b.liquidity >= pos.liquidity
        });
        if let Some((burn_tx, b)) = burn {
            used_burns.insert((*burn_tx, b.log_index));
            // Anchor to the burn tx when the opening tx is not in this block.
            if let Some(ev) = build_jit_event(
                input,
                JitEventPosition {
                    pool: pos.pool,
                    owner: pos.owner,
                    tick_lower: pos.tick_lower,
                    tick_upper: pos.tick_upper,
                    mint_tx: *burn_tx,
                    liquidity: pos.liquidity,
                    amount0: U256::ZERO,
                    amount1: U256::ZERO,
                    burn_tx: *burn_tx,
                    opened_block: pos.opened_block,
                    bin_amm: b.bin_amm,
                },
            ) {
                out.push(ev);
            }
        }
    }

    out
}

/// Build one JIT event, or `None` when no in-window swap validates the range.
pub(super) fn build_jit_event(input: &BlockInput, pos: JitEventPosition) -> Option<MevEvent> {
    let JitEventPosition {
        pool,
        owner,
        tick_lower,
        tick_upper,
        mint_tx,
        liquidity,
        amount0,
        amount1,
        burn_tx,
        opened_block,
        bin_amm,
    } = pos;
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
        .map(super::gas::tx_gas_cost_wei)
        .next()
        .unwrap_or(U256::ZERO);

    // Documented estimate: in-range swap volume × pool fee (30 bps default).
    // V3 fees are not in logs and principal amounts are not fees, so this is
    // explicitly inferred (`Confidence::Exact` remains on the detection).
    let pool_fee_bps = input.ctx.jit_pool_fee_bps;
    let mut by_token: std::collections::BTreeMap<String, U256> = std::collections::BTreeMap::new();
    for t in &input.txs {
        for s in &t.swaps {
            if in_range(s) {
                let fee =
                    s.amount_in.saturating_mul(U256::from(pool_fee_bps)) / U256::from(10_000u32);
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
        pools: vec![pool],
        // fee capture — priced via pool tokens, so no primary profit token
        gas_cost_wei,
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
                "pool_fee_bps": pool_fee_bps,
                "confidence": "inferred",
                "range": range_key,
                "by_token": fees_estimated,
            },
            "pnl": {
                "basis": PnlBasis::F.as_str(),
                "method": "in_range_volume_x_pool_fee",
            },
        }),
        ..MevEvent::for_tx(
            input.block,
            input.ts,
            mint_tx,
            B256::ZERO, // mint tx hash resolved at ingest stamping
            MevKind::Jit,
            owner,
            None,
        )
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

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use crate::explorer::test_fixtures::{block, jit, swap_at_tick, tx, ATK, POOL_A, TOKA, USDC};

    /// An in-range V3 swap on the JIT pool, so `build_jit_event` validates the
    /// range and a wrongly-paired event is actually emitted (not masked by the
    /// tick-overlap gate).
    fn in_range_swaps() -> Vec<SwapFact> {
        vec![swap_at_tick(POOL_A, USDC, TOKA, 1_000, 1_100, 0)]
    }

    /// Burn in tx0, Mint in tx1 (same pool/owner/ticks): a Burn that predates
    /// the Mint cannot close it. The old pairing had no ordering check and
    /// emitted the event.
    #[test]
    fn burn_before_mint_same_coords_does_not_pair() {
        let mut burn_tx = tx(0, ATK, true, in_range_swaps(), vec![]);
        burn_tx.jit.push(jit(0, false));
        let mut mint_tx = tx(1, ATK, true, vec![], vec![]);
        mint_tx.jit.push(jit(1, true));
        let input = block(vec![burn_tx, mint_tx]);

        let mut drops = super::super::drops::DropSink::new(input.block);
        let events = classify_jit(&input, &mut drops);
        assert!(
            events.is_empty(),
            "Burn at tx0 must not pair with Mint at tx1: {:?}",
            events
                .iter()
                .map(|e| (&e.details["burn_tx_index"], e.tx_index))
                .collect::<Vec<_>>()
        );
    }

    /// Two identical Mints (tx1, tx2) and one Burn (tx3): the Burn can close
    /// exactly one position, so exactly one JIT event may be emitted. The old
    /// `find` let the same Burn satisfy both Mints (duplicate P&L).
    #[test]
    fn one_burn_closes_one_of_two_identical_mints() {
        let mut mint_a = tx(1, ATK, true, vec![], vec![]);
        mint_a.jit.push(jit(1, true));
        let mut mint_b = tx(2, ATK, true, vec![], vec![]);
        mint_b.jit.push(jit(2, true));
        let mut burn_tx = tx(3, ATK, true, in_range_swaps(), vec![]);
        burn_tx.jit.push(jit(3, false));
        let input = block(vec![mint_a, mint_b, burn_tx]);

        let mut drops = super::super::drops::DropSink::new(input.block);
        let events = classify_jit(&input, &mut drops);
        assert_eq!(
            events.len(),
            1,
            "one Burn must close exactly one Mint, not both: {:?}",
            events
                .iter()
                .map(|e| (e.tx_index, &e.details["burn_tx_index"]))
                .collect::<Vec<_>>()
        );
    }
}
