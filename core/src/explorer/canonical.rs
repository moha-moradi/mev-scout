//! Explorer-side canonical form for realized operations.
//!
//! Mirrors `compute_canonical_id` (opportunity.rs) so the explorer emits IDs
//! that two pipelines can compare. Opportunity canonical IDs are built from
//! simulated pools/route (block-agnostic, sender-agnostic); this function
//! builds the realized-side counterpart from the *observed* flow facts:
//! - arb: kind + sorted pool set + endpoint tokens (route direction varies
//!   between searchers and simulation, so the pool *set* is canonical, not order)
//! - liquidation: borrower+liquidator pair rather than asset pair
//! - jit: pool + tick range
//!
//! Note: exact canonical-id matching via these strings is *aspirational* —
//! opportunity canonical IDs come from simulation, realized IDs from flows,
//! and they rarely coincide.
use alloy::primitives::{Address, B256};

use crate::explorer::types::{MevEvent, MevKind};
use crate::types::canonical_route_suffix;

/// Compute the explorer-side canonical ID for a classified realized op.
pub fn explorer_canonical_id(ev: &MevEvent) -> String {
    match ev.kind {
        MevKind::Liquidation => {
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
        MevKind::Jit => {
            let (lo, hi) = (
                ev.details
                    .get("tick_lower")
                    .cloned()
                    .unwrap_or(serde_json::json!(0)),
                ev.details
                    .get("tick_upper")
                    .cloned()
                    .unwrap_or(serde_json::json!(0)),
            );
            // Owner and opening coordinates keep two positions in one tx (and
            // two managers on the same range) from collapsing into one row.
            let owner = ev
                .details
                .get("owner")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Address>().ok())
                .unwrap_or(ev.searcher);
            let opening = ev
                .details
                .get("opened_block")
                .and_then(|v| v.as_u64())
                .unwrap_or(ev.block);
            let opening_log = ev
                .details
                .get("opening_log_index")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into());
            // Receipt-local log indices repeat across transactions in one
            // block. Include the opening transaction identity when known so a
            // reopen in another transaction cannot collide.
            let opening_tx = ev
                .details
                .get("opening_tx_hash")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    ev.details
                        .get("opening_tx_index")
                        .and_then(|v| v.as_u64())
                        .map(|n| format!("tx:{n}"))
                })
                .unwrap_or_else(|| "-".into());
            format!(
                "Jit|{:#x}|{:#x}|{}|{}|open:{opening}:{opening_log}:{opening_tx}",
                first_pool(&ev.pools),
                owner,
                lo,
                hi
            )
        }
        MevKind::Skim => {
            format!("Skim|{:#x}|{:#x}", first_pool(&ev.pools), ev.searcher)
        }
        // Actions are evidence rows, not P&L: key them by the emitting log so
        // two withdrawals/claims in one tx stay distinct rows.
        MevKind::LiquidityAdd | MevKind::LiquidityRemove => {
            let log = ev
                .details
                .get("log_index")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            format!(
                "{:?}|{:#x}|{:#x}|log:{log}",
                ev.kind,
                first_pool(&ev.pools),
                ev.searcher
            )
        }
        MevKind::RewardClaim => {
            let log = ev
                .details
                .get("log_index")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            format!(
                "RewardClaim|{:#x}|{:#x}|log:{log}",
                first_pool(&ev.pools),
                ev.searcher
            )
        }
        _ => {
            let mut pools = ev.pools.clone();
            pools.sort();
            pools.dedup();
            let pool_strs: Vec<String> = pools.iter().map(|p| format!("{:#x}", p)).collect();
            // M-06: keep two router/pool-set/provenance variants of the same
            // pool set distinct. The suffix format mirrors the simulated side
            // (`compute_canonical_id`) and is omitted for legacy rows.
            let hash = ev
                .details
                .get("underlying_pools_hash")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<B256>().ok());
            let router_version = ev.details.get("router_version").and_then(|v| v.as_str());
            let funding_source = ev.details.get("funding_source").and_then(|v| v.as_str());
            let base = format!("{:?}|{}", ev.kind, pool_strs.join("|"));
            format!(
                "{base}{}",
                canonical_route_suffix(hash, router_version, funding_source)
            )
        }
    }
}

/// First pool of the event, or the zero address when it carries none.
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
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: Confidence::Exact,
            victim_hashes: vec![],
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
    fn arb_canonical_separates_router_pool_hash_and_funding() {
        let pool = address!("1000000000000000000000000000000000000000");
        let hash = "0x1111111111111111111111111111111111111111111111111111111111111111";
        let make = |router: Option<&str>, h: Option<&str>, funding: Option<&str>| {
            let mut e = ev(MevKind::ArbAtomic, vec![pool]);
            let mut details = serde_json::Map::new();
            if let Some(v) = router {
                details.insert("router_version".into(), serde_json::json!(v));
            }
            if let Some(v) = h {
                details.insert("underlying_pools_hash".into(), serde_json::json!(v));
            }
            if let Some(v) = funding {
                details.insert("funding_source".into(), serde_json::json!(v));
            }
            e.details = serde_json::Value::Object(details);
            explorer_canonical_id(&e)
        };
        assert_eq!(make(None, None, None), make(None, None, None));
        assert_ne!(make(Some("v1"), None, None), make(Some("v2"), None, None));
        assert_ne!(make(None, Some(hash), None), make(None, None, None));
        assert_ne!(
            make(None, None, Some("balancer_v2_vault")),
            make(None, None, Some("aave_v3"))
        );
        assert!(make(Some("v1"), Some(hash), Some("aave_v3")).ends_with("|r:v1|f:aave_v3"));
    }

    #[test]
    fn jit_identity_separates_same_log_positions_in_different_txs() {
        let pool = address!("3000000000000000000000000000000000000000");
        let owner = address!("4000000000000000000000000000000000000000");
        let make = |opening_tx: u64| {
            let mut e = ev(MevKind::Jit, vec![pool]);
            e.details = serde_json::json!({
                "owner": format!("{owner:#x}"),
                "tick_lower": -10,
                "tick_upper": 10,
                "opened_block": 5,
                "opening_log_index": 2,
                "opening_tx_index": opening_tx,
            });
            explorer_canonical_id(&e)
        };
        assert_eq!(make(3), make(3));
        assert_ne!(make(3), make(4), "receipt-local log index alone collides");
    }
}
