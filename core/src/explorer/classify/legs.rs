//! Shared swap-leg helpers for sandwich / causal classify passes.
use std::collections::HashMap;

use alloy::primitives::{Address, B256, U256};

use super::{flow_attributable, has_unresolved_tokens, BlockInput, TxInput};
use crate::explorer::decode;
use crate::explorer::profit::{has_closed_cycle, select_profit_token, DeltaLedger};

#[derive(Debug, Clone, Default)]
pub(super) struct SwapLeg {
    pub(super) tx_index: u64,
    pub(super) tx_hash: B256,
    pub(super) token_in: Address,
    pub(super) token_out: Address,
    pub(super) amount_in: U256,
    pub(super) amount_out: U256,
}

/// One attacker's sandwich walk over a pool: front-run opens, victims
/// (third-party same-direction swaps in block order), and the closing back-run.
#[derive(Debug, Default)]
pub(super) struct SandwichWalk {
    /// Third-party same-direction swaps seen while the position was open.
    pub(super) victims: Vec<SwapLeg>,
}

/// Max tx-index distance that still counts as "adjacent" for causal
/// backrun/frontrun passes (§24 / §13). Blocks are sorted; a window of 8
/// keeps the pair causal without spanning unrelated traffic.
pub(super) const CAUSAL_WINDOW_TXS: u64 = 8;

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

/// True when `v`'s execution price is measurably worse than `f`'s (both same
/// direction): the observable victim-execution degradation.
pub(super) fn price_worse_by(v: &SwapLeg, f: &SwapLeg, min_pct: f64) -> bool {
    match (exec_price(v), exec_price(f)) {
        (Some(pv), Some(pf)) if pf > 0.0 => (pf - pv) / pf * 100.0 > min_pct,
        _ => false,
    }
}

/// Flatten every successful tx's direction-resolved swaps in block order,
/// grouped per pool. Used by the causal backrun/frontrun passes.
pub(super) fn pool_legs(input: &BlockInput) -> HashMap<Address, Vec<(u64, Address, SwapLeg)>> {
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
    for legs in by_pool.values_mut() {
        legs.sort_by_key(|(i, _, _)| *i);
    }
    by_pool
}

/// Logs-only closed-cycle profit of a tx, mirroring the arb pass: the sender
/// (or a qualified participant) nets a positive delta in a priority token.
pub(super) fn tx_cycle_profit(
    input: &BlockInput,
    tx: &TxInput,
) -> Option<(Address, Address, U256)> {
    let ledger =
        DeltaLedger::from_transfers(&tx.transfers, input.wrapped_native, (tx.from, tx.value));
    let mut candidates: Vec<Address> = vec![tx.from];
    if let Some(to) = tx.to {
        candidates.push(to);
    }
    let mut participation: HashMap<Address, usize> = HashMap::new();
    for t in &tx.transfers {
        *participation.entry(t.from).or_insert(0) += 1;
        *participation.entry(t.to).or_insert(0) += 1;
    }
    for (addr, count) in participation {
        if count >= 2 && !candidates.contains(&addr) {
            candidates.push(addr);
        }
    }
    let resolved: Vec<(Address, Address, _)> = tx
        .swaps
        .iter()
        .filter(|s| {
            !has_unresolved_tokens(s)
                && s.token_in != decode::TOKEN0_SENTINEL
                && s.token_in != decode::TOKEN1_SENTINEL
        })
        .map(|s| (s.token_in, s.token_out, s.pool))
        .collect();
    if resolved.len() < 2
        || !has_closed_cycle(&resolved)
        || !flow_attributable(&tx.swaps, &candidates)
    {
        return None; // "closed cycle; else rejected" (§24) + flow ownership (§7.1)
    }
    for searcher in candidates {
        if let Some(token) = select_profit_token(&ledger, searcher, &input.profit_policy) {
            let amount = ledger.net(searcher, token);
            if !amount.is_zero() {
                return Some((searcher, token, amount));
            }
        }
    }
    None
}
