//! Shared swap-leg helpers for the causal backrun pass.
use std::collections::HashMap;

use alloy::primitives::{Address, B256, U256};

use super::{flow_attributable, has_unresolved_tokens, BlockInput, TxInput};
use crate::explorer::decode;
use crate::explorer::profit::{
    has_closed_cycle, has_inventory_spend, select_profit_token, DeltaLedger,
};
use crate::explorer::types::Amm;

#[derive(Debug, Clone, Default)]
pub(super) struct SwapLeg {
    pub(super) tx_hash: B256,
    pub(super) token_in: Address,
    pub(super) token_out: Address,
    pub(super) amount_in: U256,
    pub(super) amount_out: U256,
}

/// Max tx-index distance that still counts as "adjacent" for the causal
/// backrun pass: a pair farther apart than this is not considered causally
/// linked. A window of 8 txs spans the activity between the two legs
/// without bridging unrelated traffic.
///
/// Heuristic, not yet calibrated against a pinned real seed (Phase 3 re-hunt
/// 2026-10-24: 0 `backrun`/`skim` ops in 95681722..=95682322 and
/// 98415954..=98416254). Kept as a named constant rather than an inline magic
/// number; the window boundary behavior is pinned by the causal golden set ship
/// gate (`core/tests/explorer_golden.rs`) and the offline FP check
/// (`core/tests/causal_fp_corpus.rs`). See
/// `docs/explorer_module_improvements_b59a6f27.plan.md`, Phase 4.
pub(crate) const CAUSAL_WINDOW_TXS: u64 = 8;

/// Minimum relative price advantage (percentage points) a causal leg must
/// show to be counted as *material* rather than float / gas-ordering noise.
/// A backrun requires the closer to execute at least this much better than the
/// pre-move reference. `0.5` means 0.5%.
///
/// Heuristic, not yet calibrated against a pinned real seed (Phase 3 re-hunt
/// 2026-10-24: 0 ops). Kept as a named constant rather than an inline magic
/// number; the threshold boundary behavior is pinned by the causal golden set
/// ship gate (`core/tests/explorer_golden.rs`) and the offline FP check
/// (`core/tests/causal_fp_corpus.rs`). Revisit once a real `backrun` corpus seed
/// is available (see `docs/explorer_module_improvements_b59a6f27.plan.md`,
/// Phase 4).
pub(crate) const CAUSAL_PRICE_MIN_PCT: f64 = 0.5;

/// True when `a` and `b` swap the same token pair in the same direction.
pub(super) fn same_dir(a: &SwapLeg, b: &SwapLeg) -> bool {
    !a.token_in.is_zero()
        && !a.token_out.is_zero()
        && a.token_in == b.token_in
        && a.token_out == b.token_out
}

/// True when `a` and `b` swap the same token pair in opposite directions.
pub(super) fn opposite_dir(a: &SwapLeg, b: &SwapLeg) -> bool {
    !a.token_in.is_zero()
        && !a.token_out.is_zero()
        && a.token_in == b.token_out
        && a.token_out == b.token_in
}

/// Execution price of a leg: output per input (same-direction comparison only).
pub(super) fn exec_price(leg: &SwapLeg) -> Option<f64> {
    if leg.amount_in.is_zero() {
        return None;
    }
    Some(
        crate::explorer::pricing::u256_to_f64(leg.amount_out)
            / crate::explorer::pricing::u256_to_f64(leg.amount_in),
    )
}

/// True when `b`'s execution price is measurably better than `r`'s, both
/// trading the same direction on the same pool. `min_pct` is the minimum
/// relative improvement that counts as material (guards float noise).
pub(super) fn price_better_by(b: &SwapLeg, r: &SwapLeg, min_pct: f64) -> bool {
    match (exec_price(b), exec_price(r)) {
        (Some(pb), Some(pr)) if pr > 0.0 => (pb - pr) / pr * 100.0 > min_pct,
        _ => false,
    }
}

/// Resolved directional cycle edges for a tx: `(token_in, token_out, pool)`.
///
/// Single source of the cycle-edge filter shared by `tx_cycle_profit` (the
/// causal backrun pass) and `classify_atomic_arbs`. Aggregator/router legs are
/// excluded: those edges are the router's *internal* routing (e.g. a
/// token→USDC sale split across its own pools), decoded only by token
/// proximity. Letting them close a cycle mislabels a one-way aggregator
/// conversion as the searcher's atomic arb. Unresolved / sentinel legs are
/// also excluded because their direction is not yet authoritative.
pub(super) fn resolved_cycle_edges(tx: &TxInput) -> Vec<(Address, Address, Address)> {
    tx.swaps
        .iter()
        .filter(|s| {
            s.amm != Amm::Aggregator
                && !has_unresolved_tokens(s)
                && s.token_in != decode::TOKEN0_SENTINEL
                && s.token_in != decode::TOKEN1_SENTINEL
        })
        .map(|s| (s.token_in, s.token_out, s.pool))
        .collect()
}

/// Flatten every successful tx's direction-resolved swaps in block order,
/// grouped per pool. Used by the causal backrun pass.
pub(super) fn pool_legs(input: &BlockInput) -> HashMap<Address, Vec<(u64, Address, SwapLeg)>> {
    let mut by_pool: HashMap<Address, Vec<(u64, Address, SwapLeg)>> = HashMap::new();
    for tx in &input.txs {
        if !tx.success {
            continue; // not-a-drop: a failed tx has no executable leg
        }
        for s in &tx.swaps {
            if has_unresolved_tokens(s) {
                continue; // not-a-drop: unresolved direction cannot anchor a leg
            }
            by_pool.entry(s.pool).or_default().push((
                tx.tx_index,
                tx.from,
                SwapLeg {
                    tx_hash: tx.tx_hash,
                    token_in: s.token_in,
                    token_out: s.token_out,
                    amount_in: s.amount_in,
                    amount_out: s.amount_out,
                },
            ));
        }
    }
    for legs in by_pool.values_mut() {
        legs.sort_by_key(|(i, _, _)| *i);
    }
    by_pool
}

/// Searcher-attribution candidates for a tx: EOA sender, its receiver-side
/// contract, plus any address with ≥2 swap participations via transfers.
/// Order-stable (sender first) for deterministic downstream matching.
///
/// The `>= 2` participation threshold is the single chain-configurable knob
/// Task D12 is meant to expose.
pub(super) fn searcher_candidates(tx: &TxInput) -> Vec<Address> {
    let mut candidates: Vec<Address> = vec![tx.from];
    if let Some(to) = tx.to {
        candidates.push(to);
    }
    let mut participation: std::collections::HashMap<Address, usize> = Default::default();
    for t in &tx.transfers {
        *participation.entry(t.from).or_insert(0) += 1;
        *participation.entry(t.to).or_insert(0) += 1;
    }
    // A6: mint/burn transfers touch `Address::ZERO`; it is never a real
    // searcher and must not qualify even with ≥2 participations.
    let mut qualified: Vec<Address> = participation
        .into_iter()
        .filter(|(addr, count)| *count >= 2 && !addr.is_zero() && !candidates.contains(addr))
        .map(|(addr, _)| addr)
        .collect();
    // HashMap order would shuffle multi-candidate txs between runs.
    qualified.sort_unstable();
    candidates.extend(qualified);
    candidates
}

/// Why [`tx_cycle_profit`] returned no residual. Callers record these; the
/// function itself does not touch the reject sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CycleMiss {
    /// Fewer than two resolved edges. Not an anchor — aggregate per block.
    TooFewEdges,
    /// Two or more resolved edges that do not close.
    NotClosed,
    /// Closed cycle whose legs are funded by more than one external owner.
    NotAttributable,
    /// Every searcher candidate spends a pre-existing inventory residual.
    InventorySpend,
    /// Cycle closes and is attributable, but nobody holds a positive residual.
    NoResidual,
}

/// Logs-only closed-cycle profit of a tx, mirroring the arb pass: the sender
/// (or a qualified participant) nets a positive delta in a priority token.
pub(super) fn tx_cycle_profit(
    input: &BlockInput,
    tx: &TxInput,
) -> Result<(Address, Address, U256), CycleMiss> {
    let ledger =
        DeltaLedger::from_transfers(&tx.transfers, input.ctx.wrapped_native, (tx.from, tx.value));
    let candidates = searcher_candidates(tx);
    let resolved = resolved_cycle_edges(tx);
    if resolved.len() < 2 {
        return Err(CycleMiss::TooFewEdges);
    }
    if !has_closed_cycle(&resolved) {
        return Err(CycleMiss::NotClosed);
    }
    if !flow_attributable(&tx.swaps, &candidates) {
        return Err(CycleMiss::NotAttributable);
    }
    let mut saw_inventory = false;
    let mut saw_clean = false;
    for searcher in candidates {
        if has_inventory_spend(&ledger, searcher) {
            saw_inventory = true;
            continue; // not-a-drop: try the next searcher candidate
        }
        saw_clean = true;
        if let Some(token) = select_profit_token(&ledger, searcher, &input.ctx.profit_policy) {
            let amount = ledger.net(searcher, token);
            if !amount.is_zero() {
                return Ok((searcher, token, amount));
            }
        }
    }
    if saw_inventory && !saw_clean {
        Err(CycleMiss::InventorySpend)
    } else {
        Err(CycleMiss::NoResidual)
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;

    use super::{resolved_cycle_edges, searcher_candidates, tx_cycle_profit, CycleMiss};
    use crate::explorer::test_fixtures::{
        block, swap, swap_agg, transfer, tx, ATK, MARKET, POOL_A, POOL_B, TOKA, USDC, VICTIM,
        WNATIVE,
    };

    #[test]
    fn sender_leads_and_no_participation_is_invented() {
        // `to == from` (self-call): the contract push is unconditional, so the
        // anchor address can repeat. Consumers treat the list as a membership
        // set (`contains`) or an idempotent loop, so no new candidate appears.
        let mut t = tx(0, ATK, true, vec![], vec![]);
        t.to = Some(ATK);
        let candidates = searcher_candidates(&t);
        assert_eq!(candidates[0], ATK);
        assert!(
            candidates.iter().all(|c| *c == ATK),
            "no transfers ⇒ no other candidate: {candidates:?}"
        );
    }

    #[test]
    fn participants_need_two_legs_and_sort_deterministically() {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![
                transfer(0, USDC, POOL_A, ATK, 100),
                transfer(1, USDC, ATK, POOL_B, 50),
                transfer(2, USDC, POOL_A, VICTIM, 50),
            ],
        );
        t.to = Some(POOL_B);
        let candidates = searcher_candidates(&t);
        assert_eq!(candidates, vec![ATK, POOL_B, POOL_A]);
        assert!(
            !candidates.contains(&VICTIM),
            "one participation must not qualify as a searcher"
        );
        assert_eq!(
            candidates,
            searcher_candidates(&t),
            "candidate order must not depend on hash iteration"
        );
    }

    /// A2: aggregator/router legs are internal routing and must be dropped from
    /// the cycle graph, so a one-way conversion that only "closes" via the
    /// aggregator must not be counted as a closed cycle.
    #[test]
    fn resolved_edges_exclude_aggregator_legs() {
        let t = tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, TOKA, USDC, 100, 200),
                swap_agg(MARKET, USDC, WNATIVE, 200, 300),
            ],
            vec![],
        );
        let edges = resolved_cycle_edges(&t);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0], (TOKA, USDC, POOL_A));
        assert!(edges.iter().all(|(_, _, pool)| *pool != MARKET));
    }

    /// A2: two real DEX legs that chain but never return to the start token,
    /// plus an aggregator leg that would close the loop. With the aggregator
    /// excluded the chain is open, so `tx_cycle_profit` reports NotClosed.
    #[test]
    fn aggregator_only_cycle_is_not_closed() {
        let my_tx = tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, TOKA, USDC, 100, 200),
                swap(POOL_B, USDC, WNATIVE, 200, 300),
                swap_agg(MARKET, WNATIVE, TOKA, 300, 400),
            ],
            vec![],
        );
        let input = block(vec![my_tx.clone()]);
        assert_eq!(tx_cycle_profit(&input, &my_tx), Err(CycleMiss::NotClosed));
    }

    /// A6: mint/burn transfers (`from`/`to` = zero) give `Address::ZERO`
    /// ≥2 participations, but zero is never a real searcher and must be
    /// excluded from candidates. A real participant with ≥2 legs still
    /// qualifies.
    #[test]
    fn zero_address_never_a_searcher_candidate() {
        let t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![
                transfer(0, USDC, Address::ZERO, ATK, 100),
                transfer(1, USDC, Address::ZERO, VICTIM, 100),
                transfer(2, USDC, VICTIM, POOL_A, 50),
                transfer(3, USDC, POOL_A, VICTIM, 50),
            ],
        );
        let candidates = searcher_candidates(&t);
        assert!(!candidates.contains(&Address::ZERO));
        assert!(candidates.contains(&VICTIM));
        assert!(candidates.contains(&ATK));
    }
}
