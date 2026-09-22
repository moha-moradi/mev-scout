//! Pure paper ledger — gas wallet + greedy per-block fill selection.
//!
//! No I/O. Wallet delta per fill is a **single** subtraction:
//! `wallet += expected_profit - gas_cost_wei` (never debit gas then credit net).

use std::collections::{BTreeMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::paper::types::{FillSkipReason, LedgerResult, PaperFill, PaperSkip};
use crate::types::{MevOpportunity, Strategy};

/// Hard safety cap so a pathological block cannot produce a huge session.
pub const HARD_MAX_FILLS_PER_BLOCK: usize = 32;

/// Ledger knobs (from `[paper]` config + CLI).
#[derive(Debug, Clone, Copy)]
pub struct LedgerPolicy {
    pub starting_gas_wei: u128,
    pub reserve_wei: u128,
    /// Soft cap; also clamped to [`HARD_MAX_FILLS_PER_BLOCK`].
    pub max_fills_per_block: usize,
}

impl Default for LedgerPolicy {
    fn default() -> Self {
        Self {
            starting_gas_wei: default_starting_gas_wei(),
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        }
    }
}

/// Default: 10 native units (10 × 10^18 wei).
pub fn default_starting_gas_wei() -> u128 {
    10 * 10u128.pow(18)
}

/// Strategies whose `expected_profit` is native-normalized for paper accounting.
pub fn is_native_eligible(strategy: Strategy) -> bool {
    matches!(
        strategy,
        Strategy::TwoHopArb
            | Strategy::MultiHopArb
            | Strategy::Jit
            | Strategy::JitArb
            | Strategy::Sandwich
    )
}

fn u256_to_u128(v: U256) -> u128 {
    // Saturate on overflow — paper ledger is research accounting, not consensus.
    if v > U256::from(u128::MAX) {
        u128::MAX
    } else {
        v.to::<u128>()
    }
}

fn net_wei(opp: &MevOpportunity) -> i128 {
    let gross = u256_to_u128(opp.expected_profit) as i128;
    let gas = opp.gas_cost_wei as i128;
    gross - gas
}

fn pools_of(opp: &MevOpportunity) -> Vec<Address> {
    let mut pools = Vec::new();
    if !opp.pool_a.is_zero() {
        pools.push(opp.pool_a);
    }
    if !opp.pool_b.is_zero() {
        pools.push(opp.pool_b);
    }
    if let Some(path) = &opp.path {
        for p in path {
            if !p.is_zero() && !pools.contains(p) {
                pools.push(*p);
            }
        }
    }
    pools
}

/// Sort key: higher net first; then tx-anchored before mempool; then (tx_index, canonical_id).
fn sort_key(opp: &MevOpportunity) -> (i128, u8, usize, String) {
    let net = net_wei(opp);
    let mempool_rank = if opp.mempool_only { 1 } else { 0 };
    let cid = opp.canonical_id.clone().unwrap_or_default();
    // Negate net for ascending sort → descending net via BTreeMap reverse, or sort_by.
    (net, mempool_rank, opp.tx_index, cid)
}

impl LedgerPolicy {
    /// Apply the ledger over `opportunities`. Pure — no I/O.
    pub fn apply(&self, opportunities: &[MevOpportunity]) -> LedgerResult {
        let max_fills = self
            .max_fills_per_block
            .min(HARD_MAX_FILLS_PER_BLOCK)
            .max(1);
        let mut wallet = self.starting_gas_wei;
        let mut peak = wallet;
        let mut max_drawdown: u128 = 0;
        let mut fills: Vec<PaperFill> = Vec::new();
        let mut skips: Vec<PaperSkip> = Vec::new();
        let mut gross_total: u128 = 0;
        let mut gas_total: u128 = 0;
        let mut net_total: i128 = 0;
        let mut best_net: i128 = 0;
        let mut start_block: Option<u64> = None;
        let mut end_block: Option<u64> = None;

        // Group by block (BTreeMap keeps chronological order).
        let mut by_block: BTreeMap<u64, Vec<&MevOpportunity>> = BTreeMap::new();
        for opp in opportunities {
            by_block.entry(opp.block_number).or_default().push(opp);
            start_block = Some(start_block.map_or(opp.block_number, |b| b.min(opp.block_number)));
            end_block = Some(end_block.map_or(opp.block_number, |b| b.max(opp.block_number)));
        }

        for (block, mut opps) in by_block {
            opps.sort_by(|a, b| {
                // Descending net, then mempool last, then tx_index, then canonical_id.
                sort_key(b)
                    .0
                    .cmp(&sort_key(a).0)
                    .then_with(|| sort_key(a).1.cmp(&sort_key(b).1))
                    .then_with(|| sort_key(a).2.cmp(&sort_key(b).2))
                    .then_with(|| sort_key(a).3.cmp(&sort_key(b).3))
            });

            let mut used_pools: HashSet<Address> = HashSet::new();
            let mut fills_this_block = 0usize;

            for opp in opps {
                let strategy = opp.strategy.to_string();
                let cid = opp.canonical_id.clone();
                let tx_index = if opp.mempool_only {
                    None
                } else {
                    Some(opp.tx_index)
                };

                if !is_native_eligible(opp.strategy) {
                    skips.push(PaperSkip {
                        block_number: block,
                        tx_index,
                        canonical_id: cid,
                        strategy,
                        reason: FillSkipReason::NotNativeUnit,
                    });
                    continue;
                }

                let net = net_wei(opp);
                if net <= 0 {
                    skips.push(PaperSkip {
                        block_number: block,
                        tx_index,
                        canonical_id: cid,
                        strategy,
                        reason: FillSkipReason::NonPositiveNet,
                    });
                    continue;
                }

                if fills_this_block >= max_fills {
                    skips.push(PaperSkip {
                        block_number: block,
                        tx_index,
                        canonical_id: cid,
                        strategy,
                        reason: FillSkipReason::MaxFillsPerBlock,
                    });
                    continue;
                }

                let surplus = wallet.saturating_sub(self.reserve_wei);
                if opp.gas_cost_wei > surplus {
                    skips.push(PaperSkip {
                        block_number: block,
                        tx_index,
                        canonical_id: cid,
                        strategy,
                        reason: FillSkipReason::InsufficientGas,
                    });
                    continue;
                }

                let pools = pools_of(opp);
                if pools.iter().any(|p| used_pools.contains(p)) {
                    skips.push(PaperSkip {
                        block_number: block,
                        tx_index,
                        canonical_id: cid,
                        strategy,
                        reason: FillSkipReason::PoolConflict,
                    });
                    continue;
                }

                // Single wallet delta: expected_profit - gas (never debit+credit).
                let gross = u256_to_u128(opp.expected_profit);
                let gas = opp.gas_cost_wei;
                let wallet_before = wallet;
                // net is positive here; apply as signed then clamp.
                if net >= 0 {
                    wallet = wallet.saturating_add(net as u128);
                } else {
                    wallet = wallet.saturating_sub((-net) as u128);
                }
                let wallet_after = wallet;

                if wallet_after > peak {
                    peak = wallet_after;
                } else {
                    let dd = peak.saturating_sub(wallet_after);
                    if dd > max_drawdown {
                        max_drawdown = dd;
                    }
                }

                for p in &pools {
                    used_pools.insert(*p);
                }
                fills_this_block += 1;
                fills.push(PaperFill {
                    block_number: block,
                    tx_index,
                    canonical_id: cid,
                    strategy,
                    gross_wei: gross,
                    gas_wei: gas,
                    net_wei: net,
                    wallet_before,
                    wallet_after,
                    pools,
                    mempool_only: opp.mempool_only,
                });
                gross_total = gross_total.saturating_add(gross);
                gas_total = gas_total.saturating_add(gas);
                net_total += net;
                if net > best_net {
                    best_net = net;
                }
            }
        }

        LedgerResult {
            starting_gas_wei: self.starting_gas_wei,
            ending_gas_wei: wallet,
            reserve_wei: self.reserve_wei,
            max_drawdown_wei: max_drawdown,
            fills,
            skips,
            gross_wei: gross_total,
            gas_wei: gas_total,
            net_profit_wei: net_total,
            best_net_wei: best_net,
            start_block,
            end_block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};

    fn opp(
        block: u64,
        tx: usize,
        strategy: Strategy,
        pool_a: Address,
        pool_b: Address,
        profit: u128,
        gas: u128,
    ) -> MevOpportunity {
        let mut o = MevOpportunity::new(block, tx, strategy, pool_a, 0);
        o.pool_b = pool_b;
        o.expected_profit = U256::from(profit);
        o.gas_cost_wei = gas;
        o.canonical_id = Some(format!("{strategy}|{block}|{tx}"));
        o
    }

    #[test]
    fn wallet_delta_is_single_subtraction() {
        // Guards double-count: must be profit - gas, not profit - 2*gas.
        let policy = LedgerPolicy {
            starting_gas_wei: 1_000_000,
            reserve_wei: 0,
            max_fills_per_block: 32,
        };
        let a = address!("0x0000000000000000000000000000000000000001");
        let b = address!("0x0000000000000000000000000000000000000002");
        let opps = [opp(1, 0, Strategy::TwoHopArb, a, b, 500, 100)];
        let r = policy.apply(&opps);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].net_wei, 400);
        assert_eq!(r.fills[0].wallet_before, 1_000_000);
        assert_eq!(r.fills[0].wallet_after, 1_000_400);
        assert_eq!(r.ending_gas_wei, 1_000_400);
        assert_eq!(r.net_profit_wei, 400);
    }

    #[test]
    fn pool_conflict_skips_second() {
        let policy = LedgerPolicy {
            starting_gas_wei: 10_000_000,
            reserve_wei: 0,
            max_fills_per_block: 32,
        };
        let a = address!("0x0000000000000000000000000000000000000001");
        let b = address!("0x0000000000000000000000000000000000000002");
        let c = address!("0x0000000000000000000000000000000000000003");
        // Higher net first (900), then conflicting on pool_a.
        let opps = [
            opp(1, 0, Strategy::TwoHopArb, a, b, 1000, 100),
            opp(1, 1, Strategy::TwoHopArb, a, c, 800, 100),
        ];
        let r = policy.apply(&opps);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.skips.len(), 1);
        assert_eq!(r.skips[0].reason, FillSkipReason::PoolConflict);
    }

    #[test]
    fn insufficient_gas_respects_reserve() {
        let policy = LedgerPolicy {
            starting_gas_wei: 1_000,
            reserve_wei: 500,
            max_fills_per_block: 32,
        };
        let a = address!("0x0000000000000000000000000000000000000001");
        let b = address!("0x0000000000000000000000000000000000000002");
        // surplus = 500; gas 600 > surplus
        let opps = [opp(1, 0, Strategy::TwoHopArb, a, b, 10_000, 600)];
        let r = policy.apply(&opps);
        assert!(r.fills.is_empty());
        assert_eq!(r.skips[0].reason, FillSkipReason::InsufficientGas);
    }

    #[test]
    fn liquidation_skipped_as_not_native() {
        let policy = LedgerPolicy::default();
        let a = address!("0x0000000000000000000000000000000000000001");
        let opps = [opp(
            1,
            0,
            Strategy::Liquidation,
            a,
            Address::ZERO,
            1_000_000,
            1,
        )];
        let r = policy.apply(&opps);
        assert!(r.fills.is_empty());
        assert_eq!(r.skips[0].reason, FillSkipReason::NotNativeUnit);
    }

    #[test]
    fn deterministic_tie_break_prefers_tx_anchored() {
        let policy = LedgerPolicy {
            starting_gas_wei: 10_000_000,
            reserve_wei: 0,
            max_fills_per_block: 1, // only one fill — winner must be deterministic
        };
        let a = address!("0x0000000000000000000000000000000000000001");
        let b = address!("0x0000000000000000000000000000000000000002");
        let c = address!("0x0000000000000000000000000000000000000003");
        let d = address!("0x0000000000000000000000000000000000000004");
        let mut mem = opp(1, 99, Strategy::TwoHopArb, a, b, 500, 100);
        mem.mempool_only = true;
        mem.canonical_id = Some("mempool".into());
        let mut tx = opp(1, 5, Strategy::TwoHopArb, c, d, 500, 100);
        tx.canonical_id = Some("tx".into());
        // Same net; mempool should lose to tx-anchored when only one fill allowed.
        let r = policy.apply(&[mem, tx]);
        assert_eq!(r.fills.len(), 1);
        assert!(!r.fills[0].mempool_only);
        assert_eq!(r.fills[0].canonical_id.as_deref(), Some("tx"));
    }

    #[test]
    fn max_fills_per_block_cap() {
        let policy = LedgerPolicy {
            starting_gas_wei: 100_000_000,
            reserve_wei: 0,
            max_fills_per_block: 1,
        };
        let pools: Vec<(Address, Address)> = (1u8..=4)
            .map(|i| {
                (
                    Address::repeat_byte(i),
                    Address::repeat_byte(i.wrapping_add(10)),
                )
            })
            .collect();
        let opps: Vec<_> = pools
            .iter()
            .enumerate()
            .map(|(i, (a, b))| opp(1, i, Strategy::TwoHopArb, *a, *b, 1000 - i as u128, 10))
            .collect();
        let r = policy.apply(&opps);
        assert_eq!(r.fills.len(), 1);
        assert!(r
            .skips
            .iter()
            .any(|s| s.reason == FillSkipReason::MaxFillsPerBlock));
    }
}
