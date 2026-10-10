//! Causal backrun / frontrun classify passes.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::types::{Confidence, MevEvent, MevKind, PnlBasis};

use super::legs::{
    opposite_dir, pool_legs, price_better_by, price_worse_by, same_dir, tx_cycle_profit, SwapLeg,
    CAUSAL_WINDOW_TXS,
};
use super::{BlockInput, TxInput};

/// logs-only causal backrun. A market-moving tx `A` (large notional,
/// different sender) on pool `P` is followed, within the adjacency window, by a
/// tx `B` whose sender profitably closes a directed cycle whose leg on `P`
/// trades *opposite* A's direction at a better execution price than the pool
/// offered pre-move. Requiring pool overlap + direction-of-benefit + closed
/// cycle; nothing else qualifies. Never Exact — this is a logs-only
/// proxy, not REVM `profit(B|after)`.
pub(super) fn classify_backruns(input: &BlockInput, consumed: &HashSet<u64>) -> Vec<MevEvent> {
    let by_pool = pool_legs(input);
    let txs_by_index: HashMap<u64, &TxInput> = input.txs.iter().map(|t| (t.tx_index, t)).collect();
    let mut out: Vec<MevEvent> = Vec::new();
    // The backrun leg can only anchor the backrun tx once **per pool**; a tx
    // can swap on several pools, so pool must be part of the claim key.
    let mut claimed: HashSet<(Address, u64)> = HashSet::new();

    for (pool, legs) in by_pool {
        for (bi, (btx, bfrom, bleg)) in legs.iter().enumerate() {
            if consumed.contains(btx) || !claimed.insert((pool, *btx)) {
                continue;
            }
            let Some(b_tx) = txs_by_index.get(btx).copied() else {
                continue;
            };
            if b_tx.from != *bfrom {
                continue;
            }
            let Some(_) = tx_cycle_profit(input, b_tx) else {
                continue; // must close a profitable cycle
            };
            // Nearest prior leg on the same pool in the opposite direction,
            // within the adjacency window, from a different sender.
            let mut move_: Option<(&(u64, Address, SwapLeg), &SwapLeg)> = None;
            for (ai, (atx, afrom, aleg)) in legs.iter().enumerate().take(bi).rev() {
                if *atx >= *btx || *btx - *atx > CAUSAL_WINDOW_TXS {
                    continue;
                }
                if *afrom == *bfrom {
                    continue;
                }
                if !opposite_dir(aleg, bleg) {
                    continue;
                }
                // Large-notional market move: A at least as large as B.
                if aleg.amount_in < bleg.amount_in {
                    continue;
                }
                // Pre-move price reference: nearest earlier same-direction leg.
                let ref_ = legs[..ai]
                    .iter()
                    .rev()
                    .find(|(_, f, l)| *f != *afrom && same_dir(l, bleg));
                if let Some((_, _, ref_leg)) = ref_ {
                    if price_better_by(bleg, ref_leg, 0.5) {
                        move_ = Some((&legs[ai], ref_leg));
                        break;
                    }
                }
            }
            let Some(a) = move_ else {
                continue;
            };
            let atx = a.0 .0;
            let afrom = a.0 .1;
            let aleg = &a.0 .2;
            let a_hash = a.0 .2.tx_hash;
            let Some((_searcher, btoken, bamount)) = tx_cycle_profit(input, b_tx) else {
                continue;
            };
            out.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: *btx,
                tx_hash: b_tx.tx_hash,
                kind: MevKind::Backrun,
                searcher: *bfrom,
                contract: None,
                pools: vec![pool],
                profit_token: Some(btoken),
                profit_amount: Some(bamount),
                profit_tokens: vec![],
                profit_usd: None,
                gas_cost_wei: U256::from(b_tx.gas_used)
                    .saturating_mul(U256::from((b_tx.effective_gas_price_gwei * 1e9) as u128)),
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Inferred,
                victim_hashes: vec![a_hash],
                victim_swap_size: None,
                details: serde_json::json!({
                    "mode": "realized",
                    "tier": "inferred",
                    "pnl_basis": PnlBasis::R.as_str(),
                    "pnl": {
                        "basis": PnlBasis::R.as_str(),
                        "profit_amount": bamount.to_string(),
                        "profit_token": format!("{btoken:#x}"),
                    },
                    "pool": format!("{pool:#x}"),
                    "source_tx_index": atx,
                    "source_sender": format!("{afrom:#x}"),
                    "move": {
                        "tx_index": atx,
                        "direction": format!("{:#x}->{:#x}", aleg.token_in, aleg.token_out),
                        "amount_in": aleg.amount_in.to_string(),
                        "amount_out": aleg.amount_out.to_string(),
                    },
                    "evidence": {
                        "STATE_DELTA_MATCH": true,
                        "DIRECTION_OF_BENEFIT": opposite_dir(aleg, bleg),
                        "PROFIT_VERIFIED": true,
                        "exec_better_than_pre_move": price_better_by(bleg, a.1, 0.5),
                        "cycle": true,
                        "supporting_tx_hashes": [format!("{a_hash:#x}")],
                    },
                    "ledger": {
                        "searcher": format!("{bfrom:#x}"),
                        "profit_token": format!("{btoken:#x}"),
                        "profit_amount": bamount.to_string(),
                    },
                    "reasons": ["STATE_DELTA_MATCH", "PROFIT_VERIFIED", "DIRECT_TOKEN_CYCLE"],
                }),
            });
        }
    }
    out
}

/// logs-only causal frontrun. A searcher tx `F` moves pool `P`
/// (swap measurable from amounts); a third-party tx `V` immediately after
/// swaps the same direction at a degraded execution price vs `F`; and `F`'s
/// sender then closes the position with a profitable opposite leg on `P`
/// (PROFIT_VERIFIED). Sandwich-consumed front/back txs are excluded by
/// `consumed` ( / kind priority).
pub(super) fn classify_frontruns(input: &BlockInput, consumed: &HashSet<u64>) -> Vec<MevEvent> {
    let by_pool = pool_legs(input);
    let txs_by_index: HashMap<u64, &TxInput> = input.txs.iter().map(|t| (t.tx_index, t)).collect();
    // Global, tx-ordered leg index so the searcher's profitable opposite close
    // can land on a different pool than the victim's (: "subsequent
    // profitable opposite leg" — no same-pool requirement). A same-pool close
    // after a third-party victim is, by construction, also a commodity
    // sandwich; the sandwich pass owns that case (front/back anchors land in
    // `consumed` via kind priority).
    let mut all: Vec<(u64, Address, SwapLeg)> = by_pool
        .values()
        .flat_map(|legs| legs.iter().map(|(tx, from, leg)| (*tx, *from, leg.clone())))
        .collect();
    all.sort_by_key(|(tx, _, _)| *tx);
    let mut out: Vec<MevEvent> = Vec::new();
    let mut claimed: HashSet<(Address, u64, u64)> = HashSet::new();

    for (pool, legs) in by_pool {
        for (fi, (ftx, ffrom, fleg)) in legs.iter().enumerate() {
            if consumed.contains(ftx) {
                continue;
            }
            // Victim: next third-party same-direction swap, adjacent window.
            let mut victim_: Option<(u64, Address, &SwapLeg)> = None;
            for (vt, vfrom, vleg) in legs.iter().skip(fi + 1) {
                if *vt - *ftx == 0 || *vt - *ftx > CAUSAL_WINDOW_TXS {
                    continue;
                }
                if *vfrom == *ffrom {
                    continue;
                }
                if !same_dir(vleg, fleg) {
                    continue;
                }
                if price_worse_by(vleg, fleg, 0.5) {
                    victim_ = Some((*vt, *vfrom, vleg));
                    break;
                }
            }
            let Some((vtx, vfrom, vleg)) = victim_ else {
                continue;
            };
            if claimed.contains(&(pool, *ftx, vtx)) {
                continue;
            }
            // Searcher's profitable opposite leg after the victim
            // (PROFIT_VERIFIED), on any pool the searcher trades.
            let close = all.iter().find(|(ctx, cfrom, cleg)| {
                *ctx >= vtx
                    && *cfrom == *ffrom
                    && opposite_dir(fleg, cleg)
                    && cleg.amount_out > fleg.amount_in
            });
            let Some((_, _, cleg)) = close else {
                continue;
            };
            let profit = cleg.amount_out.saturating_sub(fleg.amount_in);
            if profit.is_zero() || !claimed.insert((pool, *ftx, vtx)) {
                continue;
            }
            let ftx_data = txs_by_index.get(ftx).copied();
            let gas_cost_wei = ftx_data.map_or(U256::ZERO, |t| {
                U256::from(t.gas_used)
                    .saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128))
            });
            out.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: *ftx,
                tx_hash: fleg.tx_hash,
                kind: MevKind::Frontrun,
                searcher: *ffrom,
                contract: None,
                pools: vec![pool],
                profit_token: Some(fleg.token_in),
                profit_amount: Some(profit),
                profit_tokens: vec![],
                profit_usd: None,
                gas_cost_wei,
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Inferred,
                victim_hashes: vec![vleg.tx_hash],
                victim_swap_size: Some(vleg.amount_in),
                details: serde_json::json!({
                    "mode": "realized",
                    "tier": "inferred",
                    "pnl_basis": PnlBasis::R.as_str(),
                    "pnl": {
                        "basis": PnlBasis::R.as_str(),
                        "profit_amount": profit.to_string(),
                        "profit_token": format!("{:#x}", fleg.token_in),
                    },
                    "pool": format!("{pool:#x}"),
                    "victim_tx_index": vtx,
                    "front_run": {
                        "tx_index": ftx,
                        "tx_hash": format!("{:#x}", fleg.tx_hash),
                        "amount_in": fleg.amount_in.to_string(),
                        "amount_out": fleg.amount_out.to_string(),
                    },
                    "victim": {
                        "tx_index": vtx,
                        "sender": format!("{vfrom:#x}"),
                        "amount_in": vleg.amount_in.to_string(),
                        "amount_out": vleg.amount_out.to_string(),
                    },
                    "evidence": {
                        "STATE_DELTA_MATCH": true,
                        "VICTIM_EXECUTION_DEGRADED": true,
                        "PROFIT_VERIFIED": true,
                        "supporting_tx_hashes": [format!("{:#x}", vleg.tx_hash)],
                    },
                    "reasons": ["STATE_DELTA_MATCH", "VICTIM_EXECUTION_DEGRADED", "PROFIT_VERIFIED"],
                }),
            });
        }
    }
    out
}
