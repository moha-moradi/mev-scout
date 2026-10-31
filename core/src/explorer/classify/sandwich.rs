//! Classic realized-sandwich detection.
use alloy::primitives::{Address, U256};

use crate::explorer::reject::RejectReason;
use crate::explorer::types::{MevEvent, MevKind, PnlBasis};

use super::drops::DropSink;
use super::legs::{opposite_dir, pool_legs, same_dir, SandwichWalk, SwapLeg};
use super::BlockInput;

/// Classic realized-sandwich detection (pass 4): one attacker
/// opens a position on a pool (front-run), a **third-party** same-direction
/// swap (the victim) lands between it and the attacker's closing opposite-
/// direction swap (back-run). Victims are any EOA, not just one bundled in
/// the attacker's own tx, so the classic three-distinct-EOA pattern matches.
///
/// Walk per (pool, attacker): track the attacker's first swap on the pool as
/// the front-run; every other-sender swap in the same direction while the
/// position is open is a victim; the first attacker swap in the opposite
/// direction after ≥1 victim is the back-run.
pub(super) fn classify_sandwiches(input: &BlockInput, drops: &mut DropSink) -> Vec<MevEvent> {
    // Every successful tx's resolved-direction swaps, flattened per pool and
    // sorted by tx index so the walk never depends on `input.txs` order.
    let by_pool = pool_legs(input);

    let mut out = Vec::new();
    for (pool, pool_swaps) in by_pool {
        // Candidate attackers on the pool (block order preserved in the
        // outer loop; per-pool walks are internally tx-index ordered).
        let mut attackers: Vec<Address> = pool_swaps.iter().map(|(_, f, _)| *f).collect();
        attackers.sort();
        attackers.dedup();

        for attacker in attackers {
            let mut front: Option<&SwapLeg> = None;
            let mut walk = SandwichWalk::default();
            let mut saw_opposite = false;
            let mut closed = false;
            for (_, from, leg) in &pool_swaps {
                if *from == attacker {
                    if front.is_none() {
                        front = Some(leg); // position open
                    } else if !walk.victims.is_empty() {
                        let Some(f) = front else {
                            continue; // not-a-drop: front is set in this branch
                        };
                        if opposite_dir(f, leg) {
                            // First opposite-direction attacker swap after a
                            // victim closes the sandwich.
                            out.push(fold_sandwich(input, pool, attacker, f, leg, &walk));
                            closed = true;
                            break;
                        }
                    } else if let Some(f) = front {
                        if opposite_dir(f, leg) {
                            saw_opposite = true;
                        }
                    }
                    // Same-direction or pre-victim opposite swaps keep the
                    // position open; only the first back-run closes it.
                } else if let Some(f) = front {
                    if same_dir(f, leg) {
                        // Third-party swap in the same direction: the victim.
                        walk.victims.push(leg.clone());
                    }
                }
            }
            if closed {
                continue; // not-a-drop: the walk emitted a sandwich
            }
            if let Some(f) = front {
                if saw_opposite && walk.victims.is_empty() {
                    drops.anchor(
                        f.tx_index,
                        RejectReason::VictimSetEmpty,
                        "closed_without_third_party_victim",
                    );
                } else if !walk.victims.is_empty() {
                    drops.anchor(
                        f.tx_index,
                        RejectReason::CycleNotClosed,
                        "no_closing_back_run_leg",
                    );
                }
            }
        }
    }
    out
}

pub(super) fn fold_sandwich(
    input: &BlockInput,
    pool: Address,
    attacker: Address,
    front: &SwapLeg,
    back: &SwapLeg,
    walk: &SandwichWalk,
) -> MevEvent {
    // Profit = back-run output − front-run input, netted in the profit token
    // (the token both legs trade against).
    let profit = back.amount_out.saturating_sub(front.amount_in);
    // Sum gas across the front-run and back-run txs.
    let mut gas_cost_wei = U256::ZERO;
    let mut seen = std::collections::HashSet::new();
    let mut front_to: Option<Address> = None;
    let mut back_to: Option<Address> = None;
    for t in &input.txs {
        if t.tx_index == front.tx_index {
            front_to = t.to;
        }
        if t.tx_index == back.tx_index {
            back_to = t.to;
        }
        if (t.tx_index == front.tx_index || t.tx_index == back.tx_index) && seen.insert(t.tx_index)
        {
            gas_cost_wei = gas_cost_wei.saturating_add(super::gas::tx_gas_cost_wei(t));
        }
    }
    // Contract-mediated legs (logs-only): either sandwich leg targets a
    // contract (`tx.to`). Store-backed labeled-searcher enrichment is out of
    // scope here — classify stays store-free.
    let contract_mediated = front_to.is_some() || back_to.is_some();
    let contract = front_to.or(back_to);

    // Logs-only victim degradation vs the front-run execution price
    // (`amount_out/amount_in`). Evidence only — never a classification gate.
    let price = |l: &SwapLeg| -> Option<f64> {
        if l.amount_in.is_zero() {
            return None;
        }
        Some(
            crate::explorer::pricing::u256_to_f64(l.amount_out)
                / crate::explorer::pricing::u256_to_f64(l.amount_in),
        )
    };
    let front_price = price(front);
    let victim_price = walk.victims.last().and_then(price);
    let degradation_pct = match (front_price, victim_price) {
        (Some(f), Some(v)) if f > 0.0 => Some((f - v) / f * 100.0),
        _ => None,
    };

    let reverse_backrun = opposite_dir(front, back);
    let mut reasons: Vec<&str> = Vec::new();
    if reverse_backrun {
        reasons.push("REVERSE_DIRECTION");
    }
    reasons.push("SAME_SEARCHER");
    if degradation_pct.is_some_and(|d| d > 0.0) {
        reasons.push("VICTIM_EXECUTION_DEGRADED");
    }
    if contract_mediated {
        reasons.push("CONTRACT_MEDIATED");
    }

    // Aggregator intent side-channel: victim tx emitted a router event and/or
    // targeted a known router (`tx.to`). Does not change classification.
    let mut victim_via_aggregator = false;
    let mut victim_aggregator_brand: Option<&'static str> = None;
    for v in &walk.victims {
        if let Some(vtx) = input.txs.iter().find(|t| t.tx_index == v.tx_index) {
            if let Some(sig) = vtx.aggregator_signals.first() {
                victim_via_aggregator = true;
                victim_aggregator_brand = Some(sig.brand);
                break;
            }
            if let Some(to) = vtx.to {
                if let Some(brand) = crate::explorer::decode::brand_for_router(to) {
                    victim_via_aggregator = true;
                    victim_aggregator_brand = Some(brand);
                    break;
                }
            }
        }
    }
    if victim_via_aggregator {
        reasons.push("VICTIM_VIA_AGGREGATOR");
    }

    MevEvent {
        pools: vec![pool],
        profit_token: Some(front.token_in),
        profit_amount: Some(profit),
        gas_cost_wei,
        victim_hashes: walk.victims.iter().map(|v| v.tx_hash).collect(),
        victim_swap_size: walk.victims.last().map(|v| v.amount_in),
        details: serde_json::json!({
            "mode": "realized",
            "pnl_basis": PnlBasis::GrossAmountDiff.as_str(),
            "pnl": {
                "basis": PnlBasis::R.as_str(),
                "profit_amount": profit.to_string(),
                "profit_token": format!("{:#x}", front.token_in),
            },
            "pool": format!("{pool:#x}"),
            "front_run": {
                "tx_index": front.tx_index,
                "tx_hash": format!("{:#x}", front.tx_hash),
                "amount_in": front.amount_in.to_string(),
                "amount_out": front.amount_out.to_string(),
                "to": front_to.map(|a| format!("{a:#x}")),
            },
            "back_run": {
                "tx_index": back.tx_index,
                "tx_hash": format!("{:#x}", back.tx_hash),
                "amount_in": back.amount_in.to_string(),
                "amount_out": back.amount_out.to_string(),
                "to": back_to.map(|a| format!("{a:#x}")),
            },
            "backrun_tx_index": back.tx_index,
            "victims_seen": walk.victims.len(),
            "victim_hashes": walk
                .victims
                .iter()
                .map(|v| format!("{:#x}", v.tx_hash))
                .collect::<Vec<_>>(),
            "victim_swap_sizes": walk
                .victims
                .iter()
                .map(|v| v.amount_in.to_string())
                .collect::<Vec<_>>(),
            "victim_via_aggregator": victim_via_aggregator,
            "victim_aggregator_brand": victim_aggregator_brand,
            "evidence": {
                "same_pool": true,
                "direction_match": true,
                "reverse_backrun": reverse_backrun,
                "same_searcher": true,
                "contract_mediated": contract_mediated,
            },
            "victim_execution": {
                "front_price": front_price,
                "victim_price": victim_price,
                "degradation_pct": degradation_pct,
            },
            "reasons": reasons,
        }),
        ..MevEvent::for_tx(
            input.block,
            input.ts,
            front.tx_index, // bundle anchor = front-run
            front.tx_hash,
            MevKind::Sandwich,
            attacker,
            contract,
        )
    }
}
