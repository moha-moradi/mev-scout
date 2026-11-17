//! UniV2-style skim capture pass (Phase 3 positive-evidence gate).
//!
//! A skim is the only V2 operation that moves tokens out of a pair without a
//! pair-level event: UniswapV2 `skim()` transfers the excess balance and the
//! pair emits nothing. The pass therefore treats a one-way pair outflow as a
//! candidate only while **every** protocol explanation for those same tokens
//! is provably absent:
//!
//! * the pool emitted at least one log (any AMM or liquidity event);
//! * the pool received tokens in the same tx (swap-like, not one-way);
//! * a Swap/Sync/Mint/Burn on the pool was decoded;
//! * a decoded liquidity fact names the pool or the same actor (V3 CL and
//!   LB/DLMM bins), or the tx carries any LB bin action at all;
//! * an outflow token is not one of the pair's own two tokens;
//! * the transfer pays a real recipient (a burn to the zero address is not
//!   a capture).
//!
//! A failed gate records a reject (`skim_counter_evidence`, detail = gate)
//! instead of dropping silently. The gate cannot read reserve/balance state
//! (block+receipt ingest performs no state reads), so a passing skim is
//! always `Confidence::Inferred`: `pnl_basis = R` records the searcher's
//! ledger residual, never an `Exact` claim.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::reject::RejectReason;
use crate::explorer::types::{Confidence, MevEvent, MevKind, PnlBasis};

use super::drops::DropSink;
use super::{BlockInput, TxInput};

/// Protocol evidence available in one transaction that can explain a pool
/// outflow without a skim.
struct CounterEvidence<'a> {
    /// Pools that received tokens in this tx (swap-like, not one-way).
    inbound: &'a HashSet<Address>,
    /// Pools with a decoded Swap.
    swapped: &'a HashSet<Address>,
    /// Pools with a decoded Sync/Mint/Burn.
    pair_ops: &'a HashSet<Address>,
    /// Pools named by a decoded liquidity fact (V3 CL or LB/DLMM bins).
    liquidity_pools: &'a HashSet<Address>,
    /// Owners/senders/recipients of decoded liquidity facts.
    liquidity_actors: &'a HashSet<Address>,
    /// Any LB bin liquidity fact in the tx: a position exit can move tokens
    /// through a relayer, so the whole tx is disqualified.
    bin_liquidity: bool,
    tx: &'a TxInput,
}

/// Why a candidate outflow failed the gate.
enum SkimMiss {
    /// Pool both receives and pays: swap-like activity.
    Inbound,
    /// Routine venue activity explains the outflow (pool logs, decoded
    /// Swap/Sync/Mint/Burn). Aggregated per block: every DEX swap looks like
    /// this, so per-tx rows would drown the reject table.
    Routine(&'static str),
    /// Ambiguous counter-evidence that deserves a tx anchor.
    Counter(&'static str),
}

fn gate_pool(ev: &CounterEvidence<'_>, pool: Address) -> Result<(), SkimMiss> {
    if ev.bin_liquidity {
        return Err(SkimMiss::Counter("tx_bin_liquidity_action"));
    }
    if ev.tx.log_emitters.contains(&pool) {
        return Err(SkimMiss::Routine("pool_emitted_logs"));
    }
    if ev.swapped.contains(&pool) {
        return Err(SkimMiss::Routine("decoded_swap_on_pool"));
    }
    if ev.pair_ops.contains(&pool) {
        return Err(SkimMiss::Routine("pair_op_on_pool"));
    }
    if ev.inbound.contains(&pool) {
        return Err(SkimMiss::Inbound);
    }
    if ev.liquidity_pools.contains(&pool) {
        return Err(SkimMiss::Counter("decoded_liquidity_on_pool"));
    }
    if ev.liquidity_actors.contains(&ev.tx.from)
        || ev.tx.to.is_some_and(|to| ev.liquidity_actors.contains(&to))
    {
        return Err(SkimMiss::Counter("same_actor_liquidity_event"));
    }
    Ok(())
}

/// Realized UniV2 `skim` capture: registry V2-like pair outbound Transfers
/// that pass the positive-evidence gate.
pub(super) fn classify_skims(input: &BlockInput, drops: &mut DropSink) -> Vec<MevEvent> {
    let mut events = Vec::new();
    for tx in &input.txs {
        if !tx.success {
            continue; // not-a-drop: a failed tx did not skim
        }
        // Pair-op evidence (Sync/Mint/Burn/Swap) names a V2 venue even when
        // the pool is absent from the registry. That miss is why a chain can
        // show zero skims: the outflow existed and the registry did not.
        let venue: HashSet<Address> = tx.v2_pair_ops.iter().map(|op| op.pool).collect();
        let mut reported_miss = false;
        for t in &tx.transfers {
            if venue.contains(&t.from)
                && !t.from.is_zero()
                && !t.amount.is_zero()
                && !input.ctx.v2_like_pools.contains(&t.from)
                && !reported_miss
            {
                drops.bump(
                    RejectReason::SkimPoolNotV2Like,
                    "venue_outflow_not_in_v2_like_registry",
                );
                reported_miss = true;
            }
        }
        if input.ctx.v2_like_pools.is_empty() {
            continue; // not-a-drop: no registry pools to score as skim
        }

        // pool → (token → amount, recipients)
        let mut by_pool: HashMap<Address, HashMap<Address, (U256, HashSet<Address>)>> =
            HashMap::new();
        // Pools that also *receive* tokens in this tx are doing swap-like
        // activity (even when the AMM's Swap event is undecoded). Real skim
        // is one-way outbound with no inbound.
        let mut inbound_pools: HashSet<Address> = HashSet::new();
        for t in &tx.transfers {
            if t.amount.is_zero() {
                continue; // not-a-drop: a zero transfer is not a skim
            }
            if input.ctx.v2_like_pools.contains(&t.to) {
                inbound_pools.insert(t.to);
            }
            if t.from.is_zero() {
                continue; // not-a-drop: mint-from-zero is not a pair outflow
            }
            if t.to.is_zero() {
                continue; // not-a-drop: a burn to zero is not a skim capture
            }
            if !input.ctx.v2_like_pools.contains(&t.from) {
                continue; // not-a-drop: not a registry pair
            }
            let entry = by_pool.entry(t.from).or_default();
            let slot = entry.entry(t.token).or_insert((U256::ZERO, HashSet::new()));
            slot.0 = slot.0.saturating_add(t.amount);
            slot.1.insert(t.to);
        }

        let swapped: HashSet<Address> = tx.swaps.iter().map(|s| s.pool).collect();
        let pair_ops: HashSet<Address> = tx.v2_pair_ops.iter().map(|op| op.pool).collect();
        let liquidity_pools: HashSet<Address> = tx.jit.iter().map(|j| j.pool).collect();
        let liquidity_actors: HashSet<Address> = tx
            .jit
            .iter()
            .flat_map(|j| [Some(j.owner), j.sender, j.recipient].into_iter().flatten())
            .collect();
        let evidence = CounterEvidence {
            inbound: &inbound_pools,
            swapped: &swapped,
            pair_ops: &pair_ops,
            liquidity_pools: &liquidity_pools,
            liquidity_actors: &liquidity_actors,
            bin_liquidity: tx.jit.iter().any(|j| j.bin_amm),
            tx,
        };

        for (pool, tokens) in by_pool {
            match gate_pool(&evidence, pool) {
                Ok(()) => {}
                Err(SkimMiss::Inbound) => {
                    drops.anchor(
                        tx.tx_index,
                        RejectReason::SkimLooksLikeSwap,
                        "pool_also_receives",
                    );
                    continue; // not-a-drop: swap-like pair already recorded
                }
                Err(SkimMiss::Counter(detail)) => {
                    drops.anchor(tx.tx_index, RejectReason::SkimCounterEvidence, detail);
                    continue; // recorded above
                }
                Err(SkimMiss::Routine(detail)) => {
                    drops.bump(RejectReason::SkimCounterEvidence, detail);
                    continue; // recorded above
                }
            }
            // A V2 skim can only move the pair's own token0/token1. An
            // unregistered pair or a foreign outflow token means the pool
            // identity is not proven, so the candidate fails closed.
            let pair_tokens = match input.ctx.pool_tokens.get(&pool) {
                Some(tokens) => tokens,
                None => {
                    let reason = RejectReason::SkimCounterEvidence;
                    drops.anchor(tx.tx_index, reason, "pair_tokens_unresolved");
                    continue; // recorded above
                }
            };
            let mut profit_tokens: Vec<(Address, U256)> = tokens
                .iter()
                .filter(|(_, (amt, _))| !amt.is_zero())
                .map(|(tok, (amt, _))| (*tok, *amt))
                .collect();
            if profit_tokens.is_empty() {
                continue; // not-a-drop: every token slot netted to zero
            }
            if profit_tokens
                .iter()
                .any(|(tok, _)| *tok != pair_tokens.0 && *tok != pair_tokens.1)
            {
                let reason = RejectReason::SkimCounterEvidence;
                drops.anchor(tx.tx_index, reason, "outflow_token_not_in_pair");
                continue; // recorded above
            }
            profit_tokens.sort_by_key(|a| a.0);
            // Primary display token: prefer profit-policy priority, else largest
            // amount. `profit_tokens` is non-empty (early return above), so the
            // best-by-amount search always yields a member — no synthetic
            // zero-address fallback.
            let (profit_token, profit_amount) = profit_tokens
                .iter()
                .find(|(tok, _)| input.ctx.profit_policy.priority.contains(tok))
                .copied()
                .or_else(|| profit_tokens.iter().max_by(|a, b| a.1.cmp(&b.1)).copied())
                .unwrap_or(profit_tokens[0]);

            let recipients: Vec<String> = tokens
                .values()
                .flat_map(|(_, recips)| recips.iter())
                .copied()
                .collect::<HashSet<_>>()
                .into_iter()
                .map(|a| format!("{a:#x}"))
                .collect();
            let amounts: Vec<serde_json::Value> = profit_tokens
                .iter()
                .map(|(tok, amt)| {
                    serde_json::json!({
                        "token": format!("{:#x}", tok),
                        "amount": amt.to_string(),
                    })
                })
                .collect();

            events.push(MevEvent {
                pools: vec![pool],
                profit_token: Some(profit_token),
                profit_amount: Some(profit_amount),
                profit_tokens: profit_tokens.clone(),
                gas_cost_wei: super::gas::tx_gas_cost_wei(tx),
                confidence: Confidence::Inferred,
                details: serde_json::json!({
                    "pool": format!("{:#x}", pool),
                    "recipients": recipients,
                    "amounts": amounts,
                    "detection": "v2_pair_outflow_without_counter_evidence",
                    "confidence_reason": "transfer_only_skim_inference",
                    "evidence": {
                        "verified_pool": true,
                        "pair_tokens": {
                            "token0": format!("{:#x}", pair_tokens.0),
                            "token1": format!("{:#x}", pair_tokens.1),
                        },
                        "one_way_outflow": true,
                        "no_pool_logs": true,
                        "no_pair_events": true,
                        "no_liquidity_action": true,
                        "reserve_delta": "unavailable",
                    },
                    "pnl_basis": PnlBasis::R.as_str(),
                    "pnl": {
                        "basis": PnlBasis::R.as_str(),
                        "profit_amount": profit_amount.to_string(),
                        "profit_token": format!("{:#x}", profit_token),
                    },
                }),
                ..MevEvent::for_tx(
                    input.block,
                    input.ts,
                    tx.tx_index,
                    tx.tx_hash,
                    MevKind::Skim,
                    tx.from,
                    tx.to,
                )
            });
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::classify::classify_block;
    use crate::explorer::test_fixtures::{
        block, block_with_v2, classify_kind, jit, pnl_basis, swap, transfer, tx, tx_to, ATK,
        POOL_A, POOL_B, TOKA, USDC, VICTIM,
    };
    use crate::explorer::types::{JitFact, V2PairOpFact, V2PairOpKind};
    use alloy::primitives::{address, U256};

    const TOKB: Address = address!("70000000000000000000000000000000000000b1");

    fn has_skim(input: &BlockInput) -> bool {
        classify_block(input).events.iter().any(|e| e.kind == MevKind::Skim)
    }

    fn has_reject(out: &crate::explorer::classify::ClassifyOutput, detail: &str) -> bool {
        out.rejects.iter().any(|r| {
            r.reject_reason == RejectReason::SkimCounterEvidence.as_str()
                && r.detail.as_deref() == Some(detail)
        })
    }

    /// Positive control (Part 6 scenario 2): a one-way pair outflow from a
    /// verified pool is still a skim — but transfer-only inference can never
    /// claim `Exact` and records the missing reserve proof.
    #[test]
    fn one_way_pair_outflow_is_skim_but_never_exact() {
        let t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        let ev = classify_kind(&block_with_v2(vec![t], &[POOL_A]), MevKind::Skim);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(42)));
        assert_eq!(pnl_basis(&ev), Some("R"));
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(
            ev.details["confidence_reason"],
            "transfer_only_skim_inference"
        );
        assert_eq!(ev.details["evidence"]["reserve_delta"], "unavailable");
    }

    /// A real V2 skim emits no pair logs. Any pool-emitted log proves other
    /// AMM activity, decoded or not (scenario 3).
    #[test]
    fn pool_emitted_log_is_counter_evidence() {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        t.log_emitters = vec![POOL_A];
        let input = block_with_v2(vec![t], &[POOL_A]);
        let out = classify_block(&input);
        assert!(!has_skim(&input), "pool logs disqualify a skim");
        assert!(has_reject(&out, "pool_emitted_logs"), "{:?}", out.rejects);
    }

    #[test]
    fn decoded_swap_on_pool_is_counter_evidence() {
        let t = tx(
            0,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        let input = block_with_v2(vec![t], &[POOL_A]);
        let out = classify_block(&input);
        assert!(!has_skim(&input));
        assert!(has_reject(&out, "decoded_swap_on_pool"), "{:?}", out.rejects);
    }

    #[test]
    fn pair_op_on_pool_is_counter_evidence() {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        t.v2_pair_ops = vec![V2PairOpFact {
            tx_index: 0,
            log_index: 1,
            pool: POOL_A,
            kind: V2PairOpKind::Sync,
        }];
        let input = block_with_v2(vec![t], &[POOL_A]);
        let out = classify_block(&input);
        assert!(!has_skim(&input));
        assert!(has_reject(&out, "pair_op_on_pool"), "{:?}", out.rejects);
    }

    /// Scenario 9: the outflow equals a decoded burn/withdraw amount. The
    /// liquidity fact is positive counter-evidence, not profit.
    #[test]
    fn decoded_burn_equal_to_outflow_is_counter_evidence() {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        t.jit = vec![JitFact {
            amount0: U256::from(42),
            ..jit(0, false)
        }];
        let input = block_with_v2(vec![t], &[POOL_A]);
        let out = classify_block(&input);
        assert!(!has_skim(&input));
        assert!(
            has_reject(&out, "decoded_liquidity_on_pool"),
            "{:?}",
            out.rejects
        );
    }

    /// A liquidity action by the same actor on a different pool still
    /// disqualifies: the tx is position management, not skim capture.
    #[test]
    fn same_actor_liquidity_event_elsewhere_is_counter_evidence() {
        let mut deposit = jit(0, true);
        deposit.pool = POOL_B;
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        t.jit = vec![deposit];
        let input = block_with_v2(vec![t], &[POOL_A, POOL_B]);
        let out = classify_block(&input);
        assert!(!has_skim(&input));
        assert!(
            has_reject(&out, "same_actor_liquidity_event"),
            "{:?}",
            out.rejects
        );
    }

    /// Any LB bin action disqualifies the whole tx even when the actor is a
    /// relayer: Phase 2's audited withdraw+claim shape must never resurface.
    #[test]
    fn bin_liquidity_in_tx_suppresses_skim() {
        let mut withdrawal = jit(0, false);
        withdrawal.pool = POOL_B;
        withdrawal.owner = VICTIM;
        withdrawal.sender = None;
        withdrawal.bin_amm = true;
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        t.jit = vec![withdrawal];
        let input = block_with_v2(vec![t], &[POOL_A, POOL_B]);
        let out = classify_block(&input);
        assert!(!has_skim(&input));
        assert!(
            has_reject(&out, "tx_bin_liquidity_action"),
            "{:?}",
            out.rejects
        );
    }

    /// Only the pair's own tokens can leave a V2 pair via `skim()`.
    #[test]
    fn outflow_token_outside_the_pair_is_counter_evidence() {
        let t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, TOKB, POOL_A, ATK, 42)],
        );
        let input = block_with_v2(vec![t], &[POOL_A]);
        let out = classify_block(&input);
        assert!(!has_skim(&input));
        assert!(
            has_reject(&out, "outflow_token_not_in_pair"),
            "{:?}",
            out.rejects
        );
    }

    /// Without the pair's token metadata the pool identity is unproven.
    #[test]
    fn unresolved_pair_tokens_fail_closed() {
        let t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        let mut input = block(vec![t]);
        input.ctx.v2_like_pools = std::sync::Arc::new([POOL_A].into_iter().collect());
        let out = classify_block(&input);
        assert!(!has_skim(&input));
        assert!(
            has_reject(&out, "pair_tokens_unresolved"),
            "{:?}",
            out.rejects
        );
    }

    /// Swap-shaped activity on the pool (inbound + outbound, undecoded Swap)
    /// stays on the existing `skim_looks_like_swap` reason.
    #[test]
    fn pool_also_receiving_stays_looks_like_swap() {
        let t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 1000),
                transfer(1, USDC, POOL_A, ATK, 42),
            ],
        );
        let input = block_with_v2(vec![t], &[POOL_A]);
        let out = classify_block(&input);
        assert!(!has_skim(&input));
        assert!(out.rejects.iter().any(|r| {
            r.reject_reason == RejectReason::SkimLooksLikeSwap.as_str()
                && r.detail.as_deref() == Some("pool_also_receives")
        }));
    }

    /// A dex-transfer from a pool to the tx's executor (not the EOA) still
    /// attributes to the EOA and keeps the gate's evidence shape.
    #[test]
    fn executor_recipient_does_not_confuse_actor_gate() {
        const EXEC: Address = address!("90000000000000000000000000000000000000e1");
        let t = tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, EXEC, 42)],
        );
        let ev = classify_kind(&block_with_v2(vec![t], &[POOL_A]), MevKind::Skim);
        assert_eq!(ev.contract, Some(EXEC));
        assert_eq!(ev.profit_amount, Some(U256::from(42)));
    }
}
