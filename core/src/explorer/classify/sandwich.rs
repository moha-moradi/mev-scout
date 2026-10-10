//! Classic realized-sandwich detection.
use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::explorer::types::{Confidence, MevEvent, MevKind, PnlBasis};

use super::legs::{opposite_dir, same_dir, SandwichWalk, SwapLeg};
use super::{has_unresolved_tokens, BlockInput};

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
pub(super) fn classify_sandwiches(input: &BlockInput) -> Vec<MevEvent> {
    // Flatten every successful tx's resolved-direction swaps in block order.
    let mut by_pool: HashMap<Address, Vec<(u64, Address, SwapLeg)>> = HashMap::new();
    for tx in &input.txs {
        if !tx.success {
            continue;
        }
        for s in &tx.swaps {
            if has_unresolved_tokens(s) {
                continue; // unresolved direction cannot anchor a leg
            }
            by_pool.entry(s.pool).or_default().push((
                tx.tx_index,
                tx.from,
                SwapLeg {
                    tx_index: tx.tx_index,
                    tx_hash: tx.tx_hash,
                    token_in: s.token_in,
                    token_out: s.token_out,
                    amount_in: s.amount_in,
                    amount_out: s.amount_out,
                },
            ));
        }
    }

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
            for (_, from, leg) in &pool_swaps {
                if *from == attacker {
                    if front.is_none() {
                        front = Some(leg); // position open
                    } else if !walk.victims.is_empty() {
                        let Some(f) = front else {
                            continue;
                        };
                        if opposite_dir(f, leg) {
                            // First opposite-direction attacker swap after a
                            // victim closes the sandwich.
                            out.push(fold_sandwich(input, pool, attacker, f, leg, &walk));
                            break;
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
    // Sum gas across the front-run and back-run txs (Phase 1.4).
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
            gas_cost_wei = gas_cost_wei.saturating_add(
                U256::from(t.gas_used)
                    .saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128)),
            );
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

    MevEvent {
        block: input.block,
        ts: input.ts,
        tx_index: front.tx_index, // bundle anchor = front-run
        tx_hash: front.tx_hash,
        kind: MevKind::Sandwich,
        searcher: attacker,
        contract,
        pools: vec![pool],
        profit_token: Some(front.token_in),
        profit_amount: Some(profit),
        profit_tokens: vec![],
        profit_usd: None,
        gas_cost_wei,
        flashloan_fee_wei: None,
        flashloan_fee_token: None,
        confidence: Confidence::Exact,
        victim_hashes: walk.victims.iter().map(|v| v.tx_hash).collect(),
        victim_swap_size: walk.victims.last().map(|v| v.amount_in),
        details: serde_json::json!({
            "mode": "realized",
            "pnl_basis": PnlBasis::R.as_str(),
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
    }
}
