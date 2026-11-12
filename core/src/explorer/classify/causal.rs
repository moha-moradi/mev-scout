//! Causal backrun classify pass.
use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;

use crate::explorer::types::{Confidence, MevEvent, MevKind, PnlBasis};

use crate::explorer::reject::RejectReason;

use super::drops::DropSink;
use super::legs::{
    opposite_dir, pool_legs, price_better_by, same_dir, tx_cycle_profit, CycleMiss, SwapLeg,
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
            if !claimed.insert((pool, *btx)) {
                continue; // not-a-drop: already claimed on this pool
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
                if *btx - *atx > input.ctx.causal_window_txs {
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
                let ref_far = *btx - *ref_tx > input.ctx.causal_window_txs;
                if ref_far {
                    drops.anchor(
                        *atx,
                        RejectReason::CausalWindowTooWide,
                        "counterparty_leg_behind_causal_window",
                    );
                    named_miss = true;
                }
                if price_better_by(bleg, ref_leg, input.ctx.causal_price_min_pct) {
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
                pools: vec![pool],
                profit_token: Some(btoken),
                profit_amount: Some(bamount),
                gas_cost_wei: super::gas::tx_gas_cost_wei(b_tx),
                confidence: Confidence::Inferred,
                victim_hashes: vec![a_hash],
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
                        "exec_better_than_pre_move": price_better_by(bleg, a.1, input.ctx.causal_price_min_pct),
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
                ..MevEvent::for_tx(
                    input.block,
                    input.ts,
                    *btx,
                    b_tx.tx_hash,
                    MevKind::Backrun,
                    *bfrom,
                    None,
                )
            });
        }
    }
    out
}
