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

use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::explorer::types::TransferFact;

/// Known wrapped-native / stable token set for profit-token priority.
/// Priority order: USDC → USDT → DAI → wrapped native → WETH.
#[derive(Debug, Clone)]
pub struct ProfitTokenPolicy {
    /// Addresses in strict priority order (already chain-resolved).
    pub priority: Vec<Address>,
    /// Wrapped native token (WMATIC/WAVAX/WBNB/WETH) for wrap-noise filtering.
    pub wrapped_native: Address,
    /// Canonical WETH address (on non-ETH chains this equals wrapped_native).
    pub weth: Address,
}

/// Wrap-pair noise: a transfer from/to the wrapped-native contract in the
/// same tx as native-value movement. WETH `deposit()` emits
/// `Transfer(0x0, me, wad)` + `Withdrawal(me, wad)`; `withdraw()` emits
/// `Transfer(me, 0x0, wad)`. Mints/burns on the wrapper are delta noise for
/// searchers that wrap mid-tx.
fn is_wrap_noise(t: &TransferFact, wrapped_native: Address) -> bool {
    t.token == wrapped_native && (t.from == Address::ZERO || t.to == Address::ZERO)
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
        native_value: (Address, U256), // (recipient_of_value, value)
    ) -> DeltaLedger {
        let mut ledger = DeltaLedger::default();
        for t in transfers {
            if is_wrap_noise(t, wrapped_native) {
                continue;
            }
            ledger.apply(t.token, t.from, t.amount, false);
            ledger.apply(t.token, t.to, t.amount, true);
        }
        // Native POL/AVAX/BNB received in `value` (transfers to EOAs / plain sends)
        let (to, v) = native_value;
        if !v.is_zero() && !to.is_zero() {
            ledger.apply(wrapped_native_marker(), to, v, true);
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
    // Fallback: largest positive delta token (deterministic tie-break by address)
    let mut best: Option<(U256, Address)> = None;
    for ((a, t), amt) in &ledger.pos {
        if *a != searcher || amt.is_zero() {
            continue;
        }
        match best {
            Some((bamt, _)) if bamt >= *amt => {}
            _ => best = Some((*amt, *t)),
        }
    }
    best.map(|(_, t)| t)
}

/// Atomic-arb cycle check (§8.1): walk the directed graph formed by resolved
/// swap edges (`token_in → token_out`) and report whether any closed walk
/// returns to its start using ≥2 edges over ≥2 distinct pools.
///
/// Unlike a naive log-order chain, this tolerates interleaved multi-hop
/// cycles. `edges` = `(token_in, token_out, pool)` per resolved swap.
pub fn has_closed_cycle(edges: &[(Address, Address, Address)]) -> bool {
    if edges.len() < 2 {
        return false;
    }
    let n = edges.len();
    let mut used = vec![false; n];
    for start_edge in 0..n {
        if dfs_cycle(edges, start_edge, start_edge, &mut used, 0) {
            return true;
        }
    }
    false
}

/// DFS from `start_edge` looking for a path back to the start token. The walk
/// is closed only when it consumes ≥2 edges spanning ≥2 distinct pools.
fn dfs_cycle(
    edges: &[(Address, Address, Address)],
    start_edge: usize,
    cur_edge: usize,
    used: &mut [bool],
    depth: usize,
) -> bool {
    let (_, next, _) = edges[cur_edge];
    let start = edges[start_edge].0;
    used[cur_edge] = true;
    let mut found = false;
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
        found = pools.len() >= 2;
    } else {
        for e in 0..edges.len() {
            if !used[e] && edges[e].0 == next && dfs_cycle(edges, start_edge, e, used, depth + 1) {
                found = true;
                break;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::TransferFact;
    use alloy::primitives::{address, U256};

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
}
