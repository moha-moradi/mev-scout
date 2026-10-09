//! Interest-accrual liquidation attribution (explorer plan P1.1 / §4.13).
//!
//! Mode B/A (§17.8.4): a realized liquidation is flagged
//! `details.interest_accrued = true` when the debt reserve's borrow index
//! moved in-block (`ReserveDataUpdated`) while Chainlink feeds for the
//! debt/collateral assets did **not** poke in the same block (price-flat
//! proxy). The liquidation P&L itself stays the standard `O` formula; this
//! module only partitions an already-computed number.
//!
//! Full archive window checks (multi-block rate series) are out of the
//! single-block classifier path — the co-block heuristic is declared
//! `Inferred` by the caller when set.

use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;

use crate::explorer::types::{LiquidationFact, OracleUpdateFact, ReserveDataFact};

/// Return true when the liquidation looks interest-driven under the co-block
/// fingerprint described above.
///
/// `feed_to_asset` maps Chainlink aggregator → underlying asset. When empty,
/// any in-block oracle update suppresses the interest flag (conservative:
/// without a feed map we cannot prove price-flat for the relevant assets).
pub fn interest_accrued(
    liq: &LiquidationFact,
    reserves: &[ReserveDataFact],
    oracle_updates: &[OracleUpdateFact],
    feed_to_asset: &HashMap<Address, Address>,
) -> bool {
    if reserves.is_empty() {
        return false;
    }

    let debt = liq.debt_asset;
    let collateral = liq.collateral_asset;
    let reserve_hit = if debt.is_zero() {
        // Absorb-style events lack a debt asset; any reserve update is a weak
        // positive that still requires a price-flat check below.
        true
    } else {
        reserves.iter().any(|r| r.reserve == debt)
    };
    if !reserve_hit {
        return false;
    }

    let price_moved = if feed_to_asset.is_empty() {
        !oracle_updates.is_empty()
    } else {
        let relevant: HashSet<Address> = feed_to_asset
            .iter()
            .filter(|(_, asset)| **asset == debt || **asset == collateral)
            .map(|(feed, _)| *feed)
            .collect();
        if relevant.is_empty() {
            // No configured feed for these assets → cannot assert price-flat.
            return false;
        }
        oracle_updates.iter().any(|o| relevant.contains(&o.feed))
    };

    !price_moved
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
        }
    }

    #[test]
    fn interest_when_reserve_updated_and_price_flat() {
        let feeds = HashMap::from([(FEED, DEBT), (OTHER_FEED, COLL)]);
        assert!(interest_accrued(
            &liq(),
            &[reserve(DEBT)],
            &[], // no poke
            &feeds,
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
        ));
    }

    #[test]
    fn not_interest_without_reserve_update() {
        let feeds = HashMap::from([(FEED, DEBT)]);
        assert!(!interest_accrued(&liq(), &[], &[], &feeds));
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
        ));
    }
}
