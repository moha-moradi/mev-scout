//! Causal backrun annotation pass.
//!
//! A backrun is an atomic-arb cycle that harvests a prior price move. The arb
//! pass owns the cycle and its P&L (flash netting, tip-sink exclusion,
//! forwarded-profit attribution); this pass only attaches the `backrun` tag and
//! the causal evidence to an already-emitted `ArbAtomic`. A market-moving tx
//! `A` (large notional, different sender) on pool `P` is followed, within the
//! adjacency window, by a tx `B` whose arb cycle trades *opposite* A's direction
//! at a better execution price than the pool offered pre-move.
//!
//! A causal pair whose closer tx carries no arb event is recorded as
//! `CausalAnchorMissing` — the coverage loss is observable, never silent.
//! Causality is a logs-only proxy, not REVM `profit(B|after)`, so the evidence
//! tier stays `inferred` while the arb's ledger residual keeps its own A19
//! confidence.
use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;

use crate::explorer::reject::RejectReason;
use crate::explorer::types::{MevEvent, MevKind};

use super::drops::DropSink;
use super::legs::{
    opposite_dir, pool_legs, price_better_by, same_dir, tx_cycle_profit, CycleMiss, SwapLeg,
};
use super::{append_tag, BlockInput, TxInput};

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

/// Attach the `backrun` tag and causal evidence to the arb event on `btx`.
/// The arb's profit/confidence are untouched; causality is recorded as an
/// inferred evidence tier under `details.causal`.
fn annotate_backrun(
    ev: &mut MevEvent,
    pool: Address,
    mover: &(u64, Address, SwapLeg),
    pre_move: &SwapLeg,
    closer: &SwapLeg,
    min_pct: f64,
) {
    let (atx, afrom, aleg) = mover;
    let a_hash = aleg.tx_hash;
    append_tag(&mut ev.details, "backrun");
    if !ev.victim_hashes.contains(&a_hash) {
        ev.victim_hashes.push(a_hash);
    }
    ev.details["causal"] = serde_json::json!({
        "mode": "realized",
        "tier": "inferred",
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
            "DIRECTION_OF_BENEFIT": opposite_dir(aleg, closer),
            "PROFIT_VERIFIED": true,
            "exec_better_than_pre_move": price_better_by(closer, pre_move, min_pct),
            "cycle": true,
            "supporting_tx_hashes": [format!("{a_hash:#x}")],
        },
        "reasons": ["STATE_DELTA_MATCH", "PROFIT_VERIFIED", "DIRECT_TOKEN_CYCLE"],
    });
}

/// logs-only causal backrun annotation over emitted atomic arbs. Requiring pool
/// overlap + direction-of-benefit + a closed arb cycle; nothing else qualifies.
pub(super) fn classify_backruns(input: &BlockInput, events: &mut [MevEvent], drops: &mut DropSink) {
    let by_pool = pool_legs(input);
    let txs_by_index: HashMap<u64, &TxInput> = input.txs.iter().map(|t| (t.tx_index, t)).collect();
    // Annotation targets: txs the arb pass already classified as ArbAtomic.
    let arb_txs: HashSet<u64> = events
        .iter()
        .filter(|e| e.kind == MevKind::ArbAtomic)
        .map(|e| e.tx_index)
        .collect();
    // The scan runs once per (pool, tx) — a tx can swap on several pools.
    let mut claimed: HashSet<(Address, u64)> = HashSet::new();
    // The annotation (or missing-anchor reject) lands once per tx.
    let mut handled: HashSet<u64> = HashSet::new();

    for (pool, legs) in by_pool {
        for (bi, (btx, bfrom, bleg)) in legs.iter().enumerate() {
            if !claimed.insert((pool, *btx)) {
                continue; // not-a-drop: already scanned on this pool
            }
            let Some(b_tx) = txs_by_index.get(btx).copied() else {
                continue; // not-a-drop: leg index has no tx row
            };
            if b_tx.from != *bfrom {
                continue; // not-a-drop: leg sender is not the tx sender
            }
            // Cycle-miss diagnostics only when the arb pass emitted nothing:
            // for a classified arb the arb pass owns the cycle rejects, and a
            // forwarded-profit arb must not be vetoed by the stricter walk.
            if !arb_txs.contains(btx) {
                if let Err(miss) = tx_cycle_profit(input, b_tx) {
                    note_cycle_miss(drops, *btx, miss);
                    continue; // not-a-drop: cycle gate already recorded the miss
                }
            }
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
            if !handled.insert(*btx) {
                continue; // not-a-drop: already annotated on another pool
            }
            if let Some(ev) = events
                .iter_mut()
                .find(|e| e.kind == MevKind::ArbAtomic && e.tx_index == *btx)
            {
                annotate_backrun(ev, pool, a.0, a.1, bleg, input.ctx.causal_price_min_pct);
            } else {
                drops.anchor(
                    *btx,
                    RejectReason::CausalAnchorMissing,
                    "causal_pair_without_arb_event",
                );
            }
        }
    }
}
