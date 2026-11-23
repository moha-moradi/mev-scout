//! Profit accounting — the explorer's core primitive.
//!
//! Builds per-address per-token balance deltas from a tx's ERC-20 Transfer
//! stream (+ native value), identifies the searcher (EOA sender or its
//! contract), and computes gross profit as the positive residual delta of the
//! profit token after netting.
//!
//! Explicit noise filters:
//! - pool fee accruals never touch user deltas (fees stay inside the pool
//!   contract; they only appear as transfers *from/to* it, which net out)
//! - WETH/WMATIC-style wrap noise: Deposit/Withdrawal pairs on the wrapped
//!   native token are excluded from deltas when the paired native flow exists
//! - flash-loan borrow/repay is netted before residual computation when the
//!   same (token, counterparty=pools) pair loops
//!
//! Native *inflows* (unwrap-to-beneficiary, builder/MEV-share kickbacks paid
//! coinbase→searcher) are not observable in logs-only mode — only the tx
//! `value` debit is. A NULL or low native-denominated figure is that data
//! limit, not evidence of "no profit".
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::types::TransferFact;

/// Known wrapped-native / stable token set for profit-token priority.
/// Priority order: USDC → USDT → DAI → wrapped native → WETH.
#[derive(Debug, Clone, Default)]
pub struct ProfitTokenPolicy {
    /// Addresses in strict priority order (already chain-resolved).
    pub priority: Vec<Address>,
    /// Wrapped native token (WMATIC/WAVAX/WBNB/WETH) for wrap-noise filtering.
    pub wrapped_native: Address,
    /// Canonical WETH address (on non-ETH chains this equals wrapped_native).
    pub weth: Address,
}

/// Wrap-pair noise: a transfer from/to the wrapped-native contract in the
/// same tx as native-value movement. WETH `deposit` emits
/// `Transfer(0x0, me, wad)` + `Withdrawal(me, wad)`; `withdraw` emits
/// `Transfer(me, 0x0, wad)`. Mints/burns on the wrapper are delta noise only
/// when the tx actually moves native value to offset them (`tx.value`); with
/// no paired native flow they are real token deltas and suppressing them
/// leaves a phantom spend behind (A17).
fn is_wrap_noise(t: &TransferFact, wrapped_native: Address, has_native_flow: bool) -> bool {
    has_native_flow
        && t.token == wrapped_native
        && (t.from == Address::ZERO || t.to == Address::ZERO)
}

/// Per-address per-token delta map.
pub type Deltas = HashMap<(Address, Address), U256>; // (address, token) -> net delta

/// Build signed per-address token deltas from a transfer stream.
/// Signed arithmetic via I256; results saturate to U256 (positive) or are
/// tracked as negative via a companion map.
#[derive(Debug, Default)]
pub struct DeltaLedger {
    pub pos: Deltas,
    pub neg: Deltas,
}

impl DeltaLedger {
    pub fn from_transfers(
        transfers: &[TransferFact],
        wrapped_native: Address,
        native_value: (Address, U256), // (spender_of_value, value)
    ) -> DeltaLedger {
        Self::from_transfer_iter(transfers.iter(), wrapped_native, native_value)
    }

    /// Same as [`Self::from_transfers`] over borrowed facts, so a caller that
    /// already holds the window does not clone each `TransferFact`.
    pub fn from_transfer_iter<'a>(
        transfers: impl IntoIterator<Item = &'a TransferFact>,
        wrapped_native: Address,
        native_value: (Address, U256),
    ) -> DeltaLedger {
        let mut ledger = DeltaLedger::default();
        let (spender, v) = native_value;
        let has_native_flow = !v.is_zero();
        for t in transfers {
            if is_wrap_noise(t, wrapped_native, has_native_flow) {
                continue;
            }
            ledger.apply(t.token, t.from, t.amount, false);
            ledger.apply(t.token, t.to, t.amount, true);
        }
        // Native POL/AVAX/BNB *spent* by the sender via `value` (a contract
        // call carries no ERC-20 transfer for the native leg). This MUST be a
        // debit: crediting the sender hid the spend, so `has_inventory_spend`
        // returned false and native→token inventory trades (aggregator/router
        // splits) masqueraded as closed-cycle atomic arbitrage.
        if !v.is_zero() && !spender.is_zero() {
            ledger.apply(wrapped_native_marker(), spender, v, false);
        }
        ledger
    }

    fn apply(&mut self, token: Address, addr: Address, amount: U256, positive: bool) {
        if addr == Address::ZERO {
            return;
        }
        if positive {
            let e = self.pos.entry((addr, token)).or_default();
            *e = e.saturating_add(amount);
        } else {
            let e = self.neg.entry((addr, token)).or_default();
            *e = e.saturating_add(amount);
        }
    }

    /// Net delta (pos − neg, clamped at zero on the negative side).
    pub fn net(&self, addr: Address, token: Address) -> U256 {
        let p = self.pos.get(&(addr, token)).copied().unwrap_or(U256::ZERO);
        let n = self.neg.get(&(addr, token)).copied().unwrap_or(U256::ZERO);
        p.saturating_sub(n)
    }

    /// Signed net delta as I256 (true sign, for cycle detection).
    pub fn net_signed(&self, addr: Address, token: Address) -> alloy::primitives::I256 {
        let p = self.pos.get(&(addr, token)).copied().unwrap_or(U256::ZERO);
        let n = self.neg.get(&(addr, token)).copied().unwrap_or(U256::ZERO);
        alloy::primitives::I256::from_raw(p) - alloy::primitives::I256::from_raw(n)
    }

    /// Tokens where the address holds a strictly positive net delta.
    pub fn positive_tokens(&self, addr: Address) -> Vec<Address> {
        let mut out: Vec<Address> = self
            .pos
            .keys()
            .filter(|(a, t)| *a == addr && self.net(addr, *t) > U256::ZERO)
            .map(|(_, t)| *t)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Tokens that appear on either side of the ledger for `addr`.
    pub fn touched_tokens(&self, addr: Address) -> Vec<Address> {
        let mut out: Vec<Address> = self
            .pos
            .keys()
            .chain(self.neg.keys())
            .filter(|(a, _)| *a == addr)
            .map(|(_, t)| *t)
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// True when `addr` has a strictly negative signed residual in any token.
///
/// Closed-cycle arb leaves intermediate tokens at ~0 and only the profit
/// token positive. An inventory trade (e.g. WAVAX→USDC aggregator split
/// across pools) leaves −WAVAX +USDC — the swap-edge graph can still look
/// cyclic because legs run in both directions, but it is not atomic arb.
pub fn has_inventory_spend(ledger: &DeltaLedger, addr: Address) -> bool {
    ledger
        .touched_tokens(addr)
        .into_iter()
        .any(|t| ledger.net_signed(addr, t).is_negative())
}

/// Sentinel marker used as "token" for native-value flows (no ERC-20).
pub const NATIVE_MARKER: Address = Address::new([0xFFu8; 20]);

fn wrapped_native_marker() -> Address {
    NATIVE_MARKER
}

/// Pick the profit token for a candidate searcher: the highest-priority token
/// (from `policy.priority`) with a positive net delta; else the largest
/// positive delta overall; else None.
pub fn select_profit_token(
    ledger: &DeltaLedger,
    searcher: Address,
    policy: &ProfitTokenPolicy,
) -> Option<Address> {
    for t in &policy.priority {
        if ledger.net(searcher, *t) > U256::ZERO {
            return Some(*t);
        }
    }
    // Fallback: largest *net* positive residual (deterministic tie-break by
    // address). Gross `pos` alone is wrong when the same address later
    // forwards the residual — pos stays high while net is zero.
    let mut best: Option<(U256, Address)> = None;
    for t in ledger.positive_tokens(searcher) {
        let net = ledger.net(searcher, t);
        if net.is_zero() {
            continue;
        }
        match best {
            Some((bamt, _)) if bamt >= net => {}
            _ => best = Some((net, t)),
        }
    }
    best.map(|(_, t)| t)
}

/// Atomic-arb cycle check: walk the directed graph formed by resolved
/// swap edges (`token_in → token_out`) and report whether any closed walk
/// returns to its start using ≥2 edges over ≥2 distinct pools.
///
/// Unlike a naive log-order chain, this tolerates interleaved multi-hop
/// cycles. `edges` = `(token_in, token_out, pool)` per resolved swap.
pub fn has_closed_cycle(edges: &[(Address, Address, Address)]) -> bool {
    find_closed_cycle(edges).is_some()
}

/// Hard cap on the number of edges in a closed walk. Atomic arbs are short
/// (usually 2–4 hops); the cap keeps DFS bounded on dense token graphs while
/// still admitting longer multi-hop routes.
const MAX_CYCLE_EDGES: usize = 8;

/// Upper bound on enumerated closed walks per transaction. Real atomic arbs
/// are short; this keeps a dense token graph bounded while still surfacing
/// more than one candidate cycle for attribution/scoring.
const MAX_CYCLE_CANDIDATES: usize = 64;

/// Every distinct simple cycle as `(anchor_token, ordered_edge_indices)`.
/// A walk may revisit its anchor only on the final edge; intermediate tokens
/// are not repeated.
///
/// `edges` is `(token_in, token_out, pool)` per resolved swap; the indices
/// point back into that slice, so callers can recover the ordered pools and
/// amounts of a specific cycle instead of receiving only a token set.
///
/// Walks are canonicalised by requiring the smallest edge index in the walk
/// to be the start edge, so the same rotated cycle is reported once. Order is
/// deterministic: start edges ascending, then DFS edge order.
pub fn closed_cycle_walks(edges: &[(Address, Address, Address)]) -> Vec<(Address, Vec<usize>)> {
    if edges.len() < 2 {
        return Vec::new();
    }
    let n = edges.len();
    let mut used = vec![false; n];
    let mut walk = Vec::new();
    let mut out = Vec::new();
    for start in 0..n {
        if out.len() >= MAX_CYCLE_CANDIDATES {
            break;
        }
        let mut seen = HashSet::from([edges[start].0]);
        dfs_cycle_walks(
            edges, start, start, &mut used, &mut seen, &mut walk, &mut out,
        );
    }
    out
}

fn dfs_cycle_walks(
    edges: &[(Address, Address, Address)],
    start_edge: usize,
    cur_edge: usize,
    used: &mut [bool],
    seen_tokens: &mut HashSet<Address>,
    walk: &mut Vec<usize>,
    out: &mut Vec<(Address, Vec<usize>)>,
) {
    if out.len() >= MAX_CYCLE_CANDIDATES {
        return;
    }
    let (_, next, _) = edges[cur_edge];
    let start = edges[start_edge].0;
    let closes_at_start = next == start;
    // A route is a *simple* cycle: no token repeats before the final return.
    // Otherwise two independent cycles in one receipt would also surface as
    // one concatenated candidate and compete for attribution.
    if !closes_at_start && !seen_tokens.insert(next) {
        return;
    }
    used[cur_edge] = true;
    walk.push(cur_edge);

    if closes_at_start && walk.len() >= 2 {
        let pools: HashSet<Address> = walk.iter().map(|&i| edges[i].2).collect();
        // Canonical rotation: only the lowest-indexed edge may start a walk.
        if pools.len() >= 2 && walk.iter().min().copied() == Some(start_edge) {
            if !out.iter().any(|(_, w)| w == walk) {
                out.push((start, walk.clone()));
            }
        }
    } else if walk.len() < MAX_CYCLE_EDGES {
        for e in 0..edges.len() {
            if !used[e] && edges[e].0 == next {
                dfs_cycle_walks(edges, start_edge, e, used, seen_tokens, walk, out);
                if out.len() >= MAX_CYCLE_CANDIDATES {
                    break;
                }
            }
        }
    }

    walk.pop();
    used[cur_edge] = false;
    if !closes_at_start {
        seen_tokens.remove(&next);
    }
}

/// First closed walk: `(anchor_token, tokens_on_walk)`. The walk is
/// deterministic for a given edge order and at most [`MAX_CYCLE_EDGES`] edges
/// long.
///
/// `anchor` is the token the walk returns to (`edges[start].token_in`).
/// Walk tokens are every `token_in` / `token_out` on the used edges.
pub fn find_closed_cycle(
    edges: &[(Address, Address, Address)],
) -> Option<(Address, std::collections::HashSet<Address>)> {
    if edges.len() < 2 {
        return None;
    }
    let n = edges.len();
    let mut used = vec![false; n];
    for start_edge in 0..n {
        if let Some(walk) = dfs_cycle(edges, start_edge, start_edge, &mut used, 0) {
            return Some(walk);
        }
    }
    None
}

/// True when a negative residual is dust: ≤1 wei, or ≤1 bps of `hop_max`
/// (largest same-token hop amount on the tx). Material deficits still veto.
pub fn is_dust_deficit(deficit: U256, hop_max: U256) -> bool {
    if deficit.is_zero() || deficit <= U256::from(1u64) {
        return true;
    }
    if hop_max.is_zero() {
        return false;
    }
    deficit <= hop_max / U256::from(10_000u64)
}

/// Absolute magnitude of a negative signed net (zero when non-negative).
pub fn deficit_amount(ledger: &DeltaLedger, addr: Address, token: Address) -> U256 {
    let signed = ledger.net_signed(addr, token);
    if signed.is_negative() {
        (-signed).into_raw()
    } else {
        U256::ZERO
    }
}

/// DFS from `start_edge` looking for a path back to the start token. The walk
/// is closed only when it consumes ≥2 edges spanning ≥2 distinct pools.
fn dfs_cycle(
    edges: &[(Address, Address, Address)],
    start_edge: usize,
    cur_edge: usize,
    used: &mut [bool],
    depth: usize,
) -> Option<(Address, std::collections::HashSet<Address>)> {
    let (_, next, _) = edges[cur_edge];
    let start = edges[start_edge].0;
    used[cur_edge] = true;
    let mut found = None;
    if depth + 1 >= 2 && next == start {
        // A closed walk of ≥2 edges; require ≥2 distinct pools among used edges.
        let mut pools: Vec<Address> = edges
            .iter()
            .enumerate()
            .filter(|(i, _)| used[*i])
            .map(|(_, e)| e.2)
            .collect();
        pools.sort();
        pools.dedup();
        if pools.len() >= 2 {
            let mut tokens = std::collections::HashSet::new();
            for (i, e) in edges.iter().enumerate() {
                if used[i] {
                    tokens.insert(e.0);
                    tokens.insert(e.1);
                }
            }
            found = Some((start, tokens));
        }
    } else if depth + 1 < MAX_CYCLE_EDGES {
        for e in 0..edges.len() {
            if !used[e] && edges[e].0 == next {
                if let Some(walk) = dfs_cycle(edges, start_edge, e, used, depth + 1) {
                    found = Some(walk);
                    break;
                }
            }
        }
    }
    used[cur_edge] = false;
    found
}

/// Net a flash-loan loop: if the searcher borrowed amount X of token T at the
/// start and repaid X (+fee) at the end, the gross delta of T overcounts.
/// `repaid` is subtracted from the searcher's positive delta of `token`.
pub fn net_flash_loan(
    ledger: &mut DeltaLedger,
    searcher: Address,
    token: Address,
    borrowed: U256,
    fee: U256,
) {
    // Borrow adds to pos; we strip borrowed + fee from pos by adding to neg.
    let e = ledger.neg.entry((searcher, token)).or_default();
    *e = e.saturating_add(borrowed.saturating_add(fee));
}

// --- Capital-free atomic-arb net-profit accounting ------------------------
//
// `gross_profit_wei` is the native-denominated residual of the FULL simulated
// route (Borrow → Callback → all swaps → Repay). Swap fees and realized
// slippage are already inside that residual, so subtracting them here would be
// a double count. The flash-loan premium is a separate external cost in the
// plan's accounting: callers whose `gross` already has it netted
// (`paper::sim::SimPolicy::flash_loan_fee`) must pass
// `ProfitCosts::flashloan_fee_wei = 0` so it is never subtracted twice.

/// Cost breakdown used by [`compute_net_profit`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfitCosts {
    /// Total gas cost in wei (`(basefee + priority_fee) * gas_used`).
    pub gas_wei: U256,
    /// Flash-loan premium in wei; set to zero when `gross` already netted it.
    pub flashloan_fee_wei: U256,
    /// Native wei burned on reverted attempts (external cost, subtract once).
    pub revert_gas_wei: U256,
}

/// Net native profit after external costs. All arithmetic saturates at zero:
/// a losing atomic attempt can at worst burn its gas, it cannot create debt.
///
/// Acceptance example from the plan:
/// `gross=100, gas=10, flash=2, revert=3 → net=85`.
pub fn compute_net_profit(gross_profit_wei: U256, costs: ProfitCosts) -> U256 {
    gross_profit_wei
        .saturating_sub(costs.gas_wei)
        .saturating_sub(costs.flashloan_fee_wei)
        .saturating_sub(costs.revert_gas_wei)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::TransferFact;
    use alloy::primitives::{address, I256, U256};
    use std::ops::Neg;

    const W: Address = Address::new([0x0du8; 20]); // stand-in wrapped native

    fn tf(li: u64, token: Address, from: Address, to: Address, amt: u64) -> TransferFact {
        TransferFact {
            tx_index: 0,
            log_index: li,
            token,
            from,
            to,
            amount: U256::from(amt),
        }
    }

    #[test]
    fn deltas_net_simple_arb() {
        let me = address!("1000000000000000000000000000000000000000");
        let pool_a = address!("2000000000000000000000000000000000000000");
        let pool_b = address!("3000000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let foo = address!("5000000000000000000000000000000000000000");
        // buy FOO with 100 USDC on pool_a; sell FOO for 110 USDC on pool_b
        let transfers = vec![
            tf(0, usdc, me, pool_a, 100),
            tf(1, foo, pool_a, me, 500),
            tf(2, foo, me, pool_b, 500),
            tf(3, usdc, pool_b, me, 110),
        ];
        let ledger = DeltaLedger::from_transfers(&transfers, W, (Address::ZERO, U256::ZERO));
        assert_eq!(ledger.net(me, foo), U256::ZERO);
        assert_eq!(ledger.net(me, usdc), U256::from(10));
        assert_eq!(ledger.net(pool_a, usdc), U256::from(100));
        assert_eq!(ledger.net(pool_a, foo), U256::ZERO);
    }

    #[test]
    fn wrap_noise_filtered() {
        let me = address!("1000000000000000000000000000000000000000");
        let mint = tf(0, W, Address::ZERO, me, 1000); // deposit()
        let burn = tf(1, W, me, Address::ZERO, 1000); // withdraw()
        let ledger = DeltaLedger::from_transfers(&[mint, burn], W, (Address::ZERO, U256::ZERO));
        assert_eq!(ledger.net(me, W), U256::ZERO);
    }

    /// A17: without a paired native flow the wrapper mint is a REAL incoming
    /// delta. Dropping it unconditionally left only the outgoing spend, a
    /// phantom inventory spend that poisoned the arb inventory vetoes.
    #[test]
    fn wrap_then_spend_without_native_flow_keeps_the_mint() {
        let me = address!("1000000000000000000000000000000000000000");
        let pool = address!("2000000000000000000000000000000000000000");
        let mint = tf(0, W, Address::ZERO, me, 1000); // wrap (no native leg here)
        let spend = tf(1, W, me, pool, 1000); // spend it
        let ledger = DeltaLedger::from_transfers(&[mint, spend], W, (Address::ZERO, U256::ZERO));
        assert!(
            !ledger.net_signed(me, W).is_negative(),
            "mint + spend must net to zero, not a phantom spend of 1000"
        );
        assert_eq!(ledger.net(me, W), U256::ZERO);
    }

    /// A17: with the paired native flow (tx.value) present, wrapper
    /// mint/burn is still delta noise and must stay suppressed.
    #[test]
    fn wrap_pair_with_native_flow_is_still_noise() {
        let me = address!("1000000000000000000000000000000000000000");
        let mint = tf(0, W, Address::ZERO, me, 1000); // deposit() pairs tx.value
        let ledger = DeltaLedger::from_transfers(&[mint], W, (me, U256::from(1000u64)));
        assert_eq!(
            ledger.net(me, W),
            U256::ZERO,
            "paired wrapper mint must remain suppressed"
        );
    }

    #[test]
    fn closed_cycle_detects() {
        let a = address!("1000000000000000000000000000000000000000");
        let b = address!("2000000000000000000000000000000000000000");
        let p1 = address!("3000000000000000000000000000000000000000");
        let p2 = address!("3000000000000000000000000000000000000001");
        assert!(has_closed_cycle(&[(a, b, p1), (b, a, p2)]));
        assert!(!has_closed_cycle(&[(a, b, p1)]));
        // Same pool round-trip is not an arb (needs ≥2 pools).
        assert!(!has_closed_cycle(&[(a, b, p1), (b, a, p1)]));
    }

    #[test]
    fn interleaved_cycle_detected() {
        // Log order A->B (p1), A->C (p2), C->A (p2), B->A (p1): the cycle walk
        // must tolerate the interleaving rather than requiring a chain.
        let a = address!("1000000000000000000000000000000000000000");
        let b = address!("2000000000000000000000000000000000000000");
        let c = address!("3000000000000000000000000000000000000000");
        let p1 = address!("4000000000000000000000000000000000000000");
        let p2 = address!("4000000000000000000000000000000000000001");
        let edges = vec![(a, b, p1), (a, c, p2), (c, a, p2), (b, a, p1)];
        assert!(has_closed_cycle(&edges));
        // Acyclic edge set (no path back to start) is rejected.
        let no_cycle = vec![(a, b, p1), (b, c, p2)];
        assert!(!has_closed_cycle(&no_cycle));
    }

    #[test]
    fn closed_cycle_walks_lists_both_cycles_in_edge_order() {
        let usdc = token(1, 0);
        let weth = token(2, 0);
        let avax = token(3, 0);
        let p1 = token(1, 1);
        let p2 = token(2, 1);
        let p3 = token(3, 1);
        let p4 = token(4, 1);
        let edges = [
            (usdc, weth, p1),
            (weth, usdc, p2),
            (usdc, avax, p3),
            (avax, usdc, p4),
        ];
        let walks = closed_cycle_walks(&edges);
        assert_eq!(walks.len(), 2, "both cycles must be candidates: {walks:?}");
        let first = &walks[0];
        assert_eq!(first.0, usdc);
        assert_eq!(
            first.1.iter().map(|&i| edges[i].2).collect::<Vec<_>>(),
            vec![p1, p2]
        );
        let second = &walks[1];
        assert_eq!(
            second.1.iter().map(|&i| edges[i].2).collect::<Vec<_>>(),
            vec![p3, p4]
        );
        // A rotated duplicate of the first cycle is not reported again.
        let rotated = [edges[1], edges[0], edges[3], edges[2]];
        assert_eq!(closed_cycle_walks(&rotated).len(), 2);
    }

    #[test]
    fn cycle_search_is_depth_bounded() {
        assert!(has_closed_cycle(&ring(MAX_CYCLE_EDGES)));
        assert!(
            !has_closed_cycle(&ring(MAX_CYCLE_EDGES + 1)),
            "a cycle longer than MAX_CYCLE_EDGES must not be walked"
        );
        // Acyclic chain of ten edges terminates without finding a cycle.
        let chain: Vec<_> = (0..10)
            .map(|i| (token(i, 0xC0), token(i + 1, 0xC0), token(i, 0xD0)))
            .collect();
        assert!(!has_closed_cycle(&chain));
    }

    #[test]
    fn walk_tokens_cover_only_the_selected_cycle() {
        let (a, b, c, d) = (token(1, 0), token(2, 0), token(3, 0), token(4, 0));
        let edges = [
            (a, b, token(1, 1)),
            (b, a, token(2, 1)),
            (a, c, token(3, 1)), // decorative branch: never returns to a
            (c, d, token(4, 1)),
        ];
        let (anchor, tokens) = find_closed_cycle(&edges).expect("cycle");
        assert_eq!(anchor, a);
        let mut got: Vec<_> = tokens.into_iter().collect();
        got.sort_unstable();
        let mut want = vec![a, b];
        want.sort_unstable();
        assert_eq!(got, want, "unused edges must not leak into walk_tokens");
    }

    #[test]
    fn cycle_selection_is_deterministic() {
        let (a, b, c, d) = (token(1, 0), token(2, 0), token(3, 0), token(4, 0));
        let edges = [
            (a, b, token(1, 1)),
            (b, a, token(2, 1)),
            (c, d, token(3, 1)),
            (d, c, token(4, 1)),
        ];
        let first = sorted_cycle(&edges);
        for _ in 0..8 {
            assert_eq!(sorted_cycle(&edges), first);
        }
    }

    fn token(i: u8, salt: u8) -> Address {
        let mut a = [0u8; 20];
        a[0] = salt;
        a[1] = i;
        Address::from(a)
    }

    /// Token ring: edge `i` goes `i -> i+1` over its own pool.
    fn ring(n: usize) -> Vec<(Address, Address, Address)> {
        (0..n)
            .map(|i| {
                (
                    token(i as u8, 0xA0),
                    token(((i + 1) % n) as u8, 0xA0),
                    token(i as u8, 0xB0),
                )
            })
            .collect()
    }

    fn sorted_cycle(edges: &[(Address, Address, Address)]) -> Option<(Address, Vec<Address>)> {
        find_closed_cycle(edges).map(|(anchor, tokens)| {
            let mut v: Vec<_> = tokens.into_iter().collect();
            v.sort_unstable();
            (anchor, v)
        })
    }

    #[test]
    fn profit_token_priority() {
        let me = address!("1000000000000000000000000000000000000000");
        let pool = address!("2000000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let junk = address!("6000000000000000000000000000000000000000");
        let transfers = vec![tf(0, junk, pool, me, 10_000), tf(1, usdc, pool, me, 7)];
        let ledger = DeltaLedger::from_transfers(&transfers, W, (Address::ZERO, U256::ZERO));
        let policy = ProfitTokenPolicy {
            priority: vec![usdc],
            wrapped_native: W,
            weth: W,
        };
        assert_eq!(select_profit_token(&ledger, me, &policy), Some(usdc));
    }

    #[test]
    fn inventory_spend_detects_open_leg() {
        let me = address!("1000000000000000000000000000000000000000");
        let pool = address!("2000000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let wavax = address!("5000000000000000000000000000000000000000");
        // Spend WAVAX, receive USDC — inventory swap, not closed-cycle arb.
        let transfers = vec![tf(0, wavax, me, pool, 1000), tf(1, usdc, pool, me, 500)];
        let ledger = DeltaLedger::from_transfers(&transfers, W, (Address::ZERO, U256::ZERO));
        assert!(has_inventory_spend(&ledger, me));
        assert_eq!(ledger.net(me, usdc), U256::from(500));
        assert!(ledger.net_signed(me, wavax).is_negative());
    }

    #[test]
    fn inventory_spend_absent_on_closed_cycle() {
        let me = address!("1000000000000000000000000000000000000000");
        let pool_a = address!("2000000000000000000000000000000000000000");
        let pool_b = address!("3000000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let foo = address!("5000000000000000000000000000000000000000");
        let transfers = vec![
            tf(0, usdc, me, pool_a, 100),
            tf(1, foo, pool_a, me, 500),
            tf(2, foo, me, pool_b, 500),
            tf(3, usdc, pool_b, me, 110),
        ];
        let ledger = DeltaLedger::from_transfers(&transfers, W, (Address::ZERO, U256::ZERO));
        assert!(!has_inventory_spend(&ledger, me));
        assert_eq!(ledger.net(me, usdc), U256::from(10));
    }

    #[test]
    fn positive_tokens_lists_net_positives_only() {
        let me = address!("1000000000000000000000000000000000000000");
        let pool = address!("2000000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let junk = address!("6000000000000000000000000000000000000000");
        // junk: bought+resold (net zero) must not appear; a third token
        // (received, never spent) must appear next to usdc.
        let third = address!("7000000000000000000000000000000000000000");
        let transfers = vec![
            tf(0, junk, me, pool, 10),
            tf(1, junk, pool, me, 10),
            tf(2, usdc, pool, me, 7),
            tf(3, third, pool, me, 42),
        ];
        let ledger = DeltaLedger::from_transfers(&transfers, W, (Address::ZERO, U256::ZERO));
        let mut toks = ledger.positive_tokens(me);
        toks.sort();
        assert_eq!(
            toks,
            vec![usdc, third],
            "zero-net and negative tokens excluded"
        );
        assert_eq!(ledger.net(me, junk), U256::ZERO);
        assert_eq!(ledger.net(me, third), U256::from(42));
    }

    #[test]
    fn native_value_is_debited_from_sender() {
        // Regression: a native→token inventory swap (e.g. an aggregator split)
        // must show the native spend, so `has_inventory_spend` is true and the
        // tx cannot be mislabelled as closed-cycle atomic arbitrage.
        let me = address!("1000000000000000000000000000000000000000");
        let router = address!("2000000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        // Sender pays 30 AVAX (value) and receives 300 USDC; no AVAX back.
        let transfers = vec![tf(0, usdc, router, me, 300)];
        let ledger = DeltaLedger::from_transfers(&transfers, W, (me, U256::from(30)));
        assert!(
            has_inventory_spend(&ledger, me),
            "native spend must be visible"
        );
        assert!(ledger.net_signed(me, wrapped_native_marker()).is_negative());
        assert_eq!(ledger.net(me, usdc), U256::from(300));
    }

    /// G-13: in logs-only mode native inflows other than the tx's `value`
    /// debit are unobservable (coinbase→searcher kickbacks, unwrap-to-beneficiary
    /// credits). `DeltaLedger` has no native-credit channel — the only native
    /// entry is a debit on the `value` spender — so such inflows can never be
    /// counted as native profit.
    #[test]
    fn native_inflows_other_than_value_debit_cannot_be_credited() {
        let me = address!("1000000000000000000000000000000000000000");
        let stranger = address!("9000000000000000000000000000000000000000");

        let ledger = DeltaLedger::from_transfers(&[], W, (me, U256::from(1_000_000)));

        // The visible native flow is exactly the sender's debit:
        assert_eq!(
            ledger.net_signed(me, NATIVE_MARKER),
            I256::from_raw(U256::from(1_000_000)).neg()
        );
        assert_eq!(ledger.net(me, NATIVE_MARKER), U256::ZERO);
        // Never positive: a kickback must not inflate native profit.
        assert!(!ledger.positive_tokens(me).contains(&NATIVE_MARKER));
        // A non-sender (coinbase, unwrap beneficiary) cannot touch the marker
        // through this API at all.
        assert!(ledger.touched_tokens(stranger).is_empty());
    }

    /// WP1 acceptance: gas, flash premium and revert gas are external costs,
    /// each subtracted exactly once (no swap-fee/slippage double count).
    #[test]
    fn compute_net_profit_subtracts_each_external_cost_once() {
        let costs = ProfitCosts {
            gas_wei: U256::from(10),
            flashloan_fee_wei: U256::from(2),
            revert_gas_wei: U256::from(3),
        };
        assert_eq!(
            compute_net_profit(U256::from(100), costs),
            U256::from(85),
            "100 - 10 gas - 2 flash - 3 revert"
        );
        // A zero-cost op is the identity, and losses saturate at zero.
        assert_eq!(
            compute_net_profit(U256::from(100), ProfitCosts::default()),
            U256::from(100)
        );
        // When the simulation already netted the premium into gross, callers
        // pass fee = 0 so it is not charged twice.
        assert_eq!(
            compute_net_profit(
                U256::from(100),
                ProfitCosts {
                    gas_wei: U256::from(10),
                    flashloan_fee_wei: U256::ZERO,
                    revert_gas_wei: U256::from(3),
                }
            ),
            U256::from(87)
        );
        assert_eq!(
            compute_net_profit(
                U256::from(5),
                ProfitCosts {
                    gas_wei: U256::from(10),
                    ..Default::default()
                }
            ),
            U256::ZERO
        );
    }
}
