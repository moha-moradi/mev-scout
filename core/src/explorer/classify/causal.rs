//! Causal backrun / frontrun classify passes.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::profit::{deficit_amount, is_dust_deficit, DeltaLedger, NATIVE_MARKER};
use crate::explorer::types::{Confidence, MevEvent, MevKind, PnlBasis};

use crate::explorer::reject::RejectReason;

use super::drops::DropSink;
use super::legs::{
    opposite_dir, pool_legs, price_better_by, price_worse_by, same_dir, tx_cycle_profit, CycleMiss,
    SwapLeg, CAUSAL_PRICE_MIN_PCT, CAUSAL_WINDOW_TXS,
};
use super::{BlockInput, TxInput};

fn note_cycle_miss(drops: &mut DropSink, tx_index: u64, miss: CycleMiss) {
    match miss {
        CycleMiss::TooFewEdges => {
            drops.bump(RejectReason::CycleNotClosed, "fewer_than_2_resolved_edges");
        }
        CycleMiss::NotClosed => {
            drops.anchor(tx_index, RejectReason::CycleNotClosed, "no_closed_cycle");
        }
        CycleMiss::NotAttributable => {
            drops.anchor(
                tx_index,
                RejectReason::MixedFlowOwner,
                "cycle_flow_not_attributable",
            );
        }
        CycleMiss::InventorySpend => {
            drops.anchor(
                tx_index,
                RejectReason::MaterialDeficit,
                "every_candidate_spends_pre_existing_inventory",
            );
        }
        CycleMiss::NoResidual => {
            drops.anchor(
                tx_index,
                RejectReason::NetZeroSearcher,
                "no_candidate_holds_a_positive_residual",
            );
        }
    }
}

/// logs-only causal backrun. A market-moving tx `A` (large notional,
/// different sender) on pool `P` is followed, within the adjacency window, by a
/// tx `B` whose sender profitably closes a directed cycle whose leg on `P`
/// trades *opposite* A's direction at a better execution price than the pool
/// offered pre-move. Requiring pool overlap + direction-of-benefit + closed
/// cycle; nothing else qualifies. Never Exact — this is a logs-only
/// proxy, not REVM `profit(B|after)`.
pub(super) fn classify_backruns(
    input: &BlockInput,
    consumed: &HashSet<u64>,
    drops: &mut DropSink,
) -> Vec<MevEvent> {
    let by_pool = pool_legs(input);
    let txs_by_index: HashMap<u64, &TxInput> = input.txs.iter().map(|t| (t.tx_index, t)).collect();
    let mut out: Vec<MevEvent> = Vec::new();
    // The backrun leg can only anchor the backrun tx once **per pool**; a tx
    // can swap on several pools, so pool must be part of the claim key.
    let mut claimed: HashSet<(Address, u64)> = HashSet::new();

    for (pool, legs) in by_pool {
        for (bi, (btx, bfrom, bleg)) in legs.iter().enumerate() {
            if consumed.contains(btx) || !claimed.insert((pool, *btx)) {
                continue; // not-a-drop: sandwich-consumed or already claimed on this pool
            }
            let Some(b_tx) = txs_by_index.get(btx).copied() else {
                continue; // not-a-drop: leg index has no tx row
            };
            if b_tx.from != *bfrom {
                continue; // not-a-drop: leg sender is not the tx sender
            }
            let cycle = match tx_cycle_profit(input, b_tx) {
                Ok(v) => v,
                Err(miss) => {
                    note_cycle_miss(drops, *btx, miss);
                    continue; // not-a-drop: cycle gate already recorded the miss
                }
            };
            // Nearest prior leg on the same pool in the opposite direction,
            // within the adjacency window, from a different sender.
            let mut move_: Option<(&(u64, Address, SwapLeg), &SwapLeg)> = None;
            let mut named_miss = false;
            for (ai, (atx, afrom, aleg)) in legs.iter().enumerate().take(bi).rev() {
                if *atx >= *btx {
                    continue; // not-a-drop: not a prior leg
                }
                if *btx - *atx > CAUSAL_WINDOW_TXS {
                    if *afrom != *bfrom && opposite_dir(aleg, bleg) {
                        drops.anchor(
                            *atx,
                            RejectReason::CausalWindowTooWide,
                            "counterparty_leg_behind_causal_window",
                        );
                        named_miss = true;
                    }
                    continue; // not-a-drop: outside the adjacency window
                }
                if *afrom == *bfrom {
                    continue; // not-a-drop: same sender is not a counterparty
                }
                if !opposite_dir(aleg, bleg) {
                    continue; // not-a-drop: same direction is not a backrun counterparty
                }
                // Same-token size: victim received `token_out` which the
                // backrun spends as `token_in` (opposite dirs). Comparing
                // `amount_in` across directions mixes decimals/units.
                if aleg.amount_out < bleg.amount_in {
                    continue; // not-a-drop: counterparty notional is smaller than the closer
                }
                // Pre-move price reference: nearest earlier same-direction leg.
                let ref_ = legs[..ai]
                    .iter()
                    .rev()
                    .find(|(_, f, l)| *f != *afrom && same_dir(l, bleg));
                let Some((ref_tx, _, ref_leg)) = ref_ else {
                    continue; // not-a-drop: no pre-move same-direction reference
                };
                let ref_far = *btx - *ref_tx > CAUSAL_WINDOW_TXS;
                if ref_far {
                    drops.anchor(
                        *atx,
                        RejectReason::CausalWindowTooWide,
                        "counterparty_leg_behind_causal_window",
                    );
                    named_miss = true;
                }
                if price_better_by(bleg, ref_leg, CAUSAL_PRICE_MIN_PCT) {
                    // A far reference still counts for classification (the
                    // window check above is observational). Tightening it
                    // would move P&L, which is B8/calibration work, not A20.
                    move_ = Some((&legs[ai], ref_leg));
                    break;
                }
                if !ref_far {
                    drops.anchor(
                        *atx,
                        RejectReason::CausalPriceMoveBelowFloor,
                        "backrun_price_below_floor",
                    );
                    named_miss = true;
                }
                continue; // not-a-drop: this prior leg is not the move; keep scanning
            }
            let Some(a) = move_ else {
                if !named_miss {
                    drops.anchor(
                        *btx,
                        RejectReason::NoCounterpartyLeg,
                        "no_prior_opposite_leg",
                    );
                }
                continue; // not-a-drop: no qualifying counterparty; reason already recorded
            };
            let atx = a.0 .0;
            let afrom = a.0 .1;
            let aleg = &a.0 .2;
            let a_hash = a.0 .2.tx_hash;
            let (_searcher, btoken, bamount) = cycle;
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
                gas_cost_wei: super::gas::tx_gas_cost_wei(b_tx),
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
                        "exec_better_than_pre_move": price_better_by(bleg, a.1, CAUSAL_PRICE_MIN_PCT),
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
pub(super) fn classify_frontruns(
    input: &BlockInput,
    consumed: &HashSet<u64>,
    drops: &mut DropSink,
) -> Vec<MevEvent> {
    let by_pool = pool_legs(input);
    // Same-pool close after a third-party victim is owned by the sandwich
    // pass (front/back anchors land in `consumed`). Cross-pool / multi-hop
    // close uses the sender's transfer ledger from front through close.
    let mut out: Vec<MevEvent> = Vec::new();
    let mut claimed: HashSet<(Address, u64, u64)> = HashSet::new();

    for (pool, legs) in by_pool {
        for (fi, (ftx, ffrom, fleg)) in legs.iter().enumerate() {
            if consumed.contains(ftx) {
                continue; // not-a-drop: sandwich already claimed this front
            }
            // Victim: next third-party same-direction swap, adjacent window.
            let mut victim_: Option<(u64, Address, &SwapLeg)> = None;
            let mut saw_third_party = false;
            for (vt, vfrom, vleg) in legs.iter().skip(fi + 1) {
                if *vt == *ftx || *vt - *ftx > CAUSAL_WINDOW_TXS {
                    continue; // not-a-drop: outside the adjacency window
                }
                if *vfrom == *ffrom {
                    continue; // not-a-drop: same sender is not a victim
                }
                if !same_dir(vleg, fleg) {
                    continue; // not-a-drop: opposite direction is not a frontrun victim
                }
                saw_third_party = true;
                if price_worse_by(vleg, fleg, CAUSAL_PRICE_MIN_PCT) {
                    victim_ = Some((*vt, *vfrom, vleg));
                    break;
                }
            }
            let Some((vtx, vfrom, vleg)) = victim_ else {
                if !saw_third_party {
                    drops.bump(
                        RejectReason::VictimSetEmpty,
                        "no_third_party_same_direction_leg",
                    );
                }
                continue; // not-a-drop: no degraded victim; empty set already aggregated
            };
            if claimed.contains(&(pool, *ftx, vtx)) {
                continue; // not-a-drop: this front/victim pair is already claimed
            }
            // Close: earliest later tx from the same sender inside the window
            // whose cross-tx ledger (front..=close) is net-positive on the
            // front's token_in with only in-walk dust deficits.
            // The window is borrowed once; each close candidate takes a prefix
            // instead of cloning every transfer in front..=close.
            let anchor = fleg.token_in;
            let window_hi = ftx.saturating_add(CAUSAL_WINDOW_TXS);
            let window: Vec<&TxInput> = input
                .txs
                .iter()
                .filter(|tx| tx.tx_index >= *ftx && tx.tx_index <= window_hi)
                .collect();
            let mut close_idx: Option<u64> = None;
            let mut profit = U256::ZERO;
            let mut close_hash = fleg.tx_hash;
            for tx in &input.txs {
                if tx.tx_index < vtx || tx.from != *ffrom {
                    continue; // not-a-drop: not a later tx from the mover
                }
                if tx.tx_index.saturating_sub(*ftx) > CAUSAL_WINDOW_TXS {
                    break;
                }
                if consumed.contains(&tx.tx_index) {
                    continue; // not-a-drop: sandwich already claimed the close tx
                }
                let Some((net, ok)) = frontrun_cross_tx_profit(
                    &window,
                    input.wrapped_native,
                    *ffrom,
                    *ftx,
                    tx.tx_index,
                    anchor,
                ) else {
                    continue; // not-a-drop: this tx does not close the anchor token
                };
                if !ok || net.is_zero() {
                    continue; // not-a-drop: close ledger is dusty or zero
                }
                close_idx = Some(tx.tx_index);
                profit = net;
                close_hash = tx.tx_hash;
                break;
            }
            let Some(ctx) = close_idx else {
                drops.anchor(
                    *ftx,
                    RejectReason::CycleNotClosed,
                    "no_profitable_close_leg",
                );
                continue; // not-a-drop: victim-anchored miss already recorded
            };
            if !claimed.insert((pool, *ftx, vtx)) {
                continue; // not-a-drop: lost the claim race for this pair
            }
            // Charge gas for every tx in the front..=close window, matching the
            // sandwich pass (both legs). The front-run P&L sums transfers
            // across the window, so the gas side must cover it too.
            let gas_cost_wei = input
                .txs
                .iter()
                .filter(|t| t.tx_index >= *ftx && t.tx_index <= ctx)
                .fold(U256::ZERO, |acc, t| {
                    acc.saturating_add(super::gas::tx_gas_cost_wei(t))
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
                profit_token: Some(anchor),
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
                        "profit_token": format!("{anchor:#x}"),
                    },
                    "pool": format!("{pool:#x}"),
                    "victim_tx_index": vtx,
                    "close_tx_index": ctx,
                    "close_tx_hash": format!("{close_hash:#x}"),
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

/// Cross-tx ledger for a frontrun close: sum `searcher`'s transfers from
/// `front_tx` through `close_tx`. Returns `(anchor_net, deficits_are_dust)`.
///
/// `window` is the borrowed `[front, front+CAUSAL_WINDOW]` txs, built once
/// per front. Facts are borrowed into the ledger; nothing is cloned.
fn frontrun_cross_tx_profit(
    window: &[&TxInput],
    wrapped_native: Address,
    searcher: Address,
    front_tx: u64,
    close_tx: u64,
    anchor: Address,
) -> Option<(U256, bool)> {
    let mut hop_max: HashMap<Address, U256> = HashMap::new();
    let mut walk_tokens: HashSet<Address> = HashSet::new();
    walk_tokens.insert(anchor);
    let mut native_value = U256::ZERO;
    for tx in window {
        if tx.tx_index < front_tx || tx.tx_index > close_tx {
            continue; // not-a-drop: outside the front..=close window
        }
        if tx.from == searcher {
            native_value = native_value.saturating_add(tx.value);
        }
        for s in &tx.swaps {
            if s.token_in.is_zero() || s.token_out.is_zero() {
                continue; // not-a-drop: zero address is not a walk token
            }
            walk_tokens.insert(s.token_in);
            walk_tokens.insert(s.token_out);
            for (tok, amt) in [(s.token_in, s.amount_in), (s.token_out, s.amount_out)] {
                let e = hop_max.entry(tok).or_insert(U256::ZERO);
                if amt > *e {
                    *e = amt;
                }
            }
        }
    }
    let ledger = DeltaLedger::from_transfer_iter(
        window
            .iter()
            .filter(|tx| tx.tx_index >= front_tx && tx.tx_index <= close_tx)
            .flat_map(|tx| tx.transfers.iter()),
        wrapped_native,
        (searcher, native_value),
    );
    let net = ledger.net(searcher, anchor);
    if net.is_zero() {
        return None;
    }
    for t in ledger.touched_tokens(searcher) {
        if t == NATIVE_MARKER {
            continue; // not-a-drop: native marker is not an inventory token
        }
        let def = deficit_amount(&ledger, searcher, t);
        if def.is_zero() {
            continue; // not-a-drop: token has no deficit
        }
        if !walk_tokens.contains(&t) {
            return Some((net, false));
        }
        let max = hop_max.get(&t).copied().unwrap_or(U256::ZERO);
        if !is_dust_deficit(def, max) {
            return Some((net, false));
        }
    }
    Some((net, true))
}
