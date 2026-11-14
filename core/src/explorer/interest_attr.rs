//! Interest-accrual liquidation attribution (explorer plan P1.1 /).
//!
//! Mode A: same-block `ReserveDataUpdated` for the debt reserve with no
//! relevant Chainlink poke (`details.interest_accrued = true`).
//!
//! Mode B: prior-window lookback — debt reserve borrow-rate series moved over
//! the last [`LOOKBACK_BLOCKS`] while mapped feeds stayed flat. The liquidation
//! P&L itself stays the standard `O` formula; this module only partitions an
//! already-computed number. Cause label is `Inferred`.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::types::{LiquidationFact, OracleUpdateFact, ReserveDataFact};

/// Blocks of oracle/reserve history consulted for mode-B attribution (~10 min
/// on Avalanche C-Chain at ~2s/block).
pub const LOOKBACK_BLOCKS: u64 = 300;

/// Prior-window facts loaded from the explorer store (mode B).
#[derive(Debug, Clone, Default)]
pub struct InterestLookback {
    /// `(block, feed)` AnswerUpdated rows in `[block-LOOKBACK, block)`.
    pub oracle_updates: Vec<(u64, Address)>,
    /// `(block, reserve, variable_borrow_rate)` ReserveDataUpdated rows.
    pub reserve_updates: Vec<(u64, Address, U256)>,
}

/// Auditable cause evidence behind an `interest_accrued` label.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InterestEvidence {
    /// `"same_block"` or `"lookback"`.
    pub mode: &'static str,
    pub variable_borrow_rate: U256,
    pub reserve_updates: usize,
    pub price_flat: bool,
    /// Span of the reserve updates used by the lookback mode.
    pub lookback_blocks: Option<u64>,
}

/// Evidence for the interest-accrual label; `None` when the label does not fire.
pub fn interest_evidence(
    liq: &LiquidationFact,
    reserves: &[ReserveDataFact],
    oracle_updates: &[OracleUpdateFact],
    feed_to_asset: &HashMap<Address, Address>,
    lookback: Option<&InterestLookback>,
) -> Option<InterestEvidence> {
    let debt = liq.debt_asset;
    let collateral = liq.collateral_asset;

    if let Some(ev) = same_block_interest(debt, collateral, reserves, oracle_updates, feed_to_asset)
    {
        return Some(ev);
    }

    lookback_interest(debt, collateral, lookback?, feed_to_asset)
}

fn same_block_interest(
    debt: Address,
    collateral: Address,
    reserves: &[ReserveDataFact],
    oracle_updates: &[OracleUpdateFact],
    feed_to_asset: &HashMap<Address, Address>,
) -> Option<InterestEvidence> {
    let rate = if debt.is_zero() {
        reserves
            .iter()
            .find(|r| !r.variable_borrow_rate.is_zero())?
            .variable_borrow_rate
    } else {
        reserves
            .iter()
            .find(|r| r.reserve == debt && !r.variable_borrow_rate.is_zero())?
            .variable_borrow_rate
    };
    if price_moved(debt, collateral, oracle_updates, feed_to_asset) {
        return None;
    }
    Some(InterestEvidence {
        mode: "same_block",
        variable_borrow_rate: rate,
        reserve_updates: reserves.len(),
        price_flat: true,
        lookback_blocks: None,
    })
}

fn lookback_interest(
    debt: Address,
    collateral: Address,
    lb: &InterestLookback,
    feed_to_asset: &HashMap<Address, Address>,
) -> Option<InterestEvidence> {
    let mut matching: Vec<&(u64, Address, U256)> = lb
        .reserve_updates
        .iter()
        .filter(|(_, r, rate)| (debt.is_zero() || *r == debt) && !rate.is_zero())
        .collect();
    if matching.is_empty() {
        return None;
    }
    matching.sort_by_key(|(block, _, _)| *block);
    let first = matching.first()?;
    let last = matching.last()?;

    // Price-flat over the window for mapped feeds.
    let price_flat = if feed_to_asset.is_empty() {
        lb.oracle_updates.is_empty()
    } else {
        let relevant: HashSet<Address> = feed_to_asset
            .iter()
            .filter(|(_, asset)| **asset == debt || **asset == collateral)
            .map(|(feed, _)| *feed)
            .collect();
        if relevant.is_empty() {
            return None;
        }
        !lb.oracle_updates
            .iter()
            .any(|(_, feed)| relevant.contains(feed))
    };
    if !price_flat {
        return None;
    }
    Some(InterestEvidence {
        mode: "lookback",
        variable_borrow_rate: last.2,
        reserve_updates: matching.len(),
        price_flat,
        lookback_blocks: Some(last.0.saturating_sub(first.0)),
    })
}

fn price_moved(
    debt: Address,
    collateral: Address,
    oracle_updates: &[OracleUpdateFact],
    feed_to_asset: &HashMap<Address, Address>,
) -> bool {
    if feed_to_asset.is_empty() {
        return !oracle_updates.is_empty();
    }
    let relevant: HashSet<Address> = feed_to_asset
        .iter()
        .filter(|(_, asset)| **asset == debt || **asset == collateral)
        .map(|(feed, _)| *feed)
        .collect();
    if relevant.is_empty() {
        return true; // cannot assert price-flat without a feed map
    }
    // Invert: caller wants !price_moved; here "moved" means a relevant poke.
    oracle_updates.iter().any(|o| relevant.contains(&o.feed))
}

/// Mode-B pre-poke divergence in basis points: `|new - prior| / prior * 10_000`.
/// Returns `None` when prior is missing or zero.
pub fn oracle_pre_poke_divergence_bps(prior: i128, current: i128) -> Option<u64> {
    if prior == 0 {
        return None;
    }
    let prior_abs = prior.unsigned_abs();
    let delta = current.abs_diff(prior);
    let bps = delta
        .saturating_mul(10_000)
        .checked_div(prior_abs)
        .unwrap_or(0);
    u64::try_from(bps).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::LiquidationFact;
    use alloy::primitives::{address, U256};

    const DEBT: Address = address!("4000000000000000000000000000000000000005");
    const COLL: Address = address!("5000000000000000000000000000000000000006");
    const FEED: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const OTHER_FEED: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    fn liq() -> LiquidationFact {
        LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            emitter: Address::ZERO,
            user: Address::ZERO,
            liquidator: Address::ZERO,
            collateral_asset: COLL,
            debt_asset: DEBT,
            collateral_amount: U256::from(100),
            debt_to_cover: U256::from(50),
            bad_debt_assets: U256::ZERO,
        }
    }

    fn accrued(
        liq: &LiquidationFact,
        reserves: &[ReserveDataFact],
        oracle: &[OracleUpdateFact],
        feeds: &HashMap<Address, Address>,
        lookback: Option<&InterestLookback>,
    ) -> bool {
        interest_evidence(liq, reserves, oracle, feeds, lookback).is_some()
    }

    fn reserve(reserve: Address) -> ReserveDataFact {
        ReserveDataFact {
            tx_index: 0,
            log_index: 0,
            reserve,
            variable_borrow_rate: U256::from(1),
        }
    }

    fn oracle(feed: Address) -> OracleUpdateFact {
        OracleUpdateFact {
            tx_index: 0,
            log_index: 0,
            feed,
            answer: 100,
        }
    }

    #[test]
    fn interest_when_reserve_updated_and_price_flat() {
        let feeds = HashMap::from([(FEED, DEBT), (OTHER_FEED, COLL)]);
        assert!(accrued(&liq(), &[reserve(DEBT)], &[], &feeds, None,));
    }

    #[test]
    fn not_interest_when_relevant_feed_pokes() {
        let feeds = HashMap::from([(FEED, DEBT)]);
        assert!(!accrued(
            &liq(),
            &[reserve(DEBT)],
            &[oracle(FEED)],
            &feeds,
            None,
        ));
    }

    #[test]
    fn not_interest_without_reserve_update() {
        let feeds = HashMap::from([(FEED, DEBT)]);
        assert!(!accrued(&liq(), &[], &[], &feeds, None));
    }

    #[test]
    fn unrelated_feed_poke_still_interest() {
        let other_asset = address!("1111111111111111111111111111111111111111");
        let feeds = HashMap::from([(FEED, DEBT), (OTHER_FEED, other_asset)]);
        assert!(accrued(
            &liq(),
            &[reserve(DEBT)],
            &[oracle(OTHER_FEED)],
            &feeds,
            None,
        ));
    }

    #[test]
    fn lookback_interest_when_prior_reserve_and_flat_price() {
        let feeds = HashMap::from([(FEED, DEBT)]);
        let lb = InterestLookback {
            oracle_updates: vec![],
            reserve_updates: vec![(10, DEBT, U256::from(2))],
        };
        assert!(accrued(&liq(), &[], &[], &feeds, Some(&lb)));
    }

    #[test]
    fn lookback_suppressed_when_prior_poke() {
        let feeds = HashMap::from([(FEED, DEBT)]);
        let lb = InterestLookback {
            oracle_updates: vec![(10, FEED)],
            reserve_updates: vec![(10, DEBT, U256::from(2))],
        };
        assert!(!accrued(&liq(), &[], &[], &feeds, Some(&lb)));
    }

    #[test]
    fn evidence_carries_rate_and_lookback_span() {
        let feeds = HashMap::from([(FEED, DEBT)]);
        let lb = InterestLookback {
            oracle_updates: vec![],
            reserve_updates: vec![(10, DEBT, U256::from(2)), (20, DEBT, U256::from(3))],
        };
        let ev = interest_evidence(&liq(), &[], &[], &feeds, Some(&lb)).unwrap();
        assert_eq!(ev.mode, "lookback");
        assert_eq!(ev.variable_borrow_rate, U256::from(3));
        assert_eq!(ev.reserve_updates, 2);
        assert_eq!(ev.lookback_blocks, Some(10));
        assert!(ev.price_flat);
    }

    #[test]
    fn divergence_bps() {
        assert_eq!(oracle_pre_poke_divergence_bps(100, 101), Some(100));
        assert_eq!(oracle_pre_poke_divergence_bps(0, 1), None);
    }
}
