//! Explorer-side canonical form for realized operations.
//!
//! Mirrors `compute_canonical_id` (opportunity.rs) so the explorer emits IDs
//! that two pipelines can compare. Opportunity canonical IDs are built from
//! simulated pools/route (block-agnostic, sender-agnostic); this function
//! builds the realized-side counterpart from the *observed* flow facts:
//! - arb: kind + sorted pool set + endpoint tokens (route direction varies
//!   between searchers and simulation, so the pool *set* is canonical, not order)
//! - sandwich: kind + pool + victim/backrun tx indices (matches the
//!   opportunity-side `Sandwich|pool|victim:N|backrun:M` form)
//! - liquidation: borrower+liquidator pair rather than asset pair
//! - jit: pool + tick range
//!
//! Note (§11.1.1): T1 exact matching via these strings is *aspirational* —
//! opportunity canonical IDs come from simulation, realized IDs from flows,
//! and they rarely coincide. `validate` must still report at T2/T3 tiers.

use alloy::primitives::Address;

use crate::explorer::types::MevEvent;

/// Compute the explorer-side canonical ID for a classified realized op.
pub fn explorer_canonical_id(ev: &MevEvent) -> String {
    match ev.kind {
        crate::explorer::types::MevKind::Sandwich => {
            format!(
                "Sandwich|{:#x}|victim:{:?}|backrun:{:?}",
                first_pool(&ev.pools),
                ev.victim_hashes.len(),
                ev.details.get("backrun_tx_index").cloned().unwrap_or(serde_json::json!(null))
            )
        }
        crate::explorer::types::MevKind::Liquidation => {
            format!(
                "Liquidation|{:#x}|{:#x}",
                ev.details
                    .get("user")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Address>().ok())
                    .unwrap_or_default(),
                ev.searcher,
            )
        }
        crate::explorer::types::MevKind::Jit | crate::explorer::types::MevKind::JitArb => {
            let (lo, hi) = (
                ev.details.get("tick_lower").cloned().unwrap_or(serde_json::json!(0)),
                ev.details.get("tick_upper").cloned().unwrap_or(serde_json::json!(0)),
            );
            format!(
                "Jit|{:#x}|{}|{}",
                first_pool(&ev.pools),
                lo,
                hi
            )
        }
        _ => {
            let mut pools = ev.pools.clone();
            pools.sort();
            pools.dedup();
            let pool_strs: Vec<String> = pools.iter().map(|p| format!("{:#x}", p)).collect();
            format!(
                "{:?}|{}",
                ev.kind,
                pool_strs.join("|"),
            )
        }
    }
}

fn first_pool(pools: &[Address]) -> Address {
    pools.first().copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::{Confidence, MevEvent, MevKind};
    use alloy::primitives::{address, b256, U256};

    fn ev(kind: MevKind, pools: Vec<Address>) -> MevEvent {
        MevEvent {
            block: 1,
            ts: 0,
            tx_index: 0,
            tx_hash: b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            kind,
            searcher: Address::ZERO,
            contract: None,
            pools,
            profit_token: None,
            profit_amount: Some(U256::from(1)),
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({}),
        }
    }

    #[test]
    fn arb_canonical_uses_sorted_pool_set() {
        let a = address!("1000000000000000000000000000000000000000");
        let b = address!("2000000000000000000000000000000000000000");
        let e1 = ev(MevKind::ArbAtomic, vec![a, b]);
        let e2 = ev(MevKind::ArbAtomic, vec![b, a]);
        assert_eq!(explorer_canonical_id(&e1), explorer_canonical_id(&e2));
        assert!(explorer_canonical_id(&e1).starts_with("ArbAtomic|"));
    }

    #[test]
    fn sandwich_canonical_matches_opportunity_shape() {
        let pool = address!("3000000000000000000000000000000000000000");
        let e = ev(MevKind::Sandwich, vec![pool]);
        let id = explorer_canonical_id(&e);
        assert!(id.starts_with("Sandwich|0x3000"));
    }
}
