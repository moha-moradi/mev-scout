//! Interest-accrual liquidation attribution (explorer plan P1.1 / §4.13).
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

/// Return true when the liquidation looks interest-driven.
///
/// `feed_to_asset` maps Chainlink aggregator → underlying asset. When empty,
/// any in-block oracle update suppresses the interest flag (conservative).
/// When `lookback` is present, mode B also requires a prior-window reserve
/// move with price-flat feeds.
pub fn interest_accrued(
    liq: &LiquidationFact,
    reserves: &[ReserveDataFact],
    oracle_updates: &[OracleUpdateFact],
    feed_to_asset: &HashMap<Address, Address>,
    lookback: Option<&InterestLookback>,
) -> bool {
    let debt = liq.debt_asset;
    let collateral = liq.collateral_asset;

    let same_block = same_block_interest(debt, collateral, reserves, oracle_updates, feed_to_asset);
    if same_block {
        return true;
    }

    let Some(lb) = lookback else {
        return false;
    };
    lookback_interest(debt, collateral, lb, feed_to_asset)
}

fn same_block_interest(
    debt: Address,
    collateral: Address,
    reserves: &[ReserveDataFact],
    oracle_updates: &[OracleUpdateFact],
    feed_to_asset: &HashMap<Address, Address>,
) -> bool {
    if reserves.is_empty() {
        return false;
    }
    let reserve_hit = if debt.is_zero() {
        true
    } else {
        reserves.iter().any(|r| r.reserve == debt)
    };
    if !reserve_hit {
        return false;
    }
    !price_moved(debt, collateral, oracle_updates, feed_to_asset)
}

fn lookback_interest(
    debt: Address,
    collateral: Address,
    lb: &InterestLookback,
    feed_to_asset: &HashMap<Address, Address>,
) -> bool {
    let reserve_hit = if debt.is_zero() {
        !lb.reserve_updates.is_empty()
    } else {
        lb.reserve_updates.iter().any(|(_, r, _)| *r == debt)
    };
    if !reserve_hit {
        return false;
    }
    // Price-flat over the window for mapped feeds.
    if feed_to_asset.is_empty() {
        return lb.oracle_updates.is_empty();
    }
    let relevant: HashSet<Address> = feed_to_asset
        .iter()
        .filter(|(_, asset)| **asset == debt || **asset == collateral)
        .map(|(feed, _)| *feed)
        .collect();
    if relevant.is_empty() {
        return false;
    }
    !lb.oracle_updates
        .iter()
        .any(|(_, feed)| relevant.contains(feed))
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
    // When no feed maps to these assets, treat as moved (conservative → false interest).
    // Wait - original returned false from interest_accrued when relevant empty.
    oracle_updates.iter().any(|o| relevant.contains(&o.feed))
}

/// Mode-B pre-poke divergence in basis points: `|new - prior| / prior * 10_000`.
/// Returns `None` when prior is missing or zero.
pub fn oracle_pre_poke_divergence_bps(prior: i128, current: i128) -> Option<u64> {
    if prior == 0 {
        return None;
    }
    let prior_abs = prior.unsigned_abs();
    let delta = (current as i128 - prior as i128).unsigned_abs();
    Some(
        delta
            .saturating_mul(10_000)
            .checked_div(prior_abs)
            .unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};
    use crate::explorer::types::LiquidationFact;

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
        assert!(interest_accrued(
            &liq(),
            &[reserve(DEBT)],
            &[],
            &feeds,
            None,
        ));
    }

    #[test]
    fn not_interest_when_relevant_feed_pokes() {
        let feeds = HashMap::from([(FEED, DEBT)]);
        assert!(!interest_accrued(
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
        assert!(!interest_accrued(&liq(), &[], &[], &feeds, None));
    }

    #[test]
    fn unrelated_feed_poke_still_interest() {
        let other_asset = address!("1111111111111111111111111111111111111111");
        let feeds = HashMap::from([(FEED, DEBT), (OTHER_FEED, other_asset)]);
        assert!(interest_accrued(
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
        assert!(interest_accrued(&liq(), &[], &[], &feeds, Some(&lb)));
    }

    #[test]
    fn lookback_suppressed_when_prior_poke() {
        let feeds = HashMap::from([(FEED, DEBT)]);
        let lb = InterestLookback {
            oracle_updates: vec![(10, FEED)],
            reserve_updates: vec![(10, DEBT, U256::from(2))],
        };
        assert!(!interest_accrued(&liq(), &[], &[], &feeds, Some(&lb)));
    }

    #[test]
    fn divergence_bps() {
        assert_eq!(oracle_pre_poke_divergence_bps(100, 101), Some(100));
        assert_eq!(oracle_pre_poke_divergence_bps(0, 1), None);
    }
}
