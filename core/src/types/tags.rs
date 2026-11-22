//! ArbAtomic attribution tags shared by the explorer classifier and the live
//! detectors. String values match `details.tags` in the explorer store.
//!
//! Chain-gated N/A (explicit, never silent drops):
//! * `fot_arb` — the static registry (`data/fot_tokens.json`) is Ethereum-only;
//!   Avalanche has no entries, so on AVAX the tag only ever comes from
//!   dynamically measured taxes.
//! * `rebase_arb` — only the static registry can produce it; Avalanche has no
//!   rebase entries.
//! * `forwarded_profit` — realized-only explorer concept, intentionally absent
//!   from live detection (spec out-of-scope).

pub const BACKRUN: &str = "backrun";
pub const FLASH_ARB: &str = "flash_arb";
pub const LONG_TAIL: &str = "long_tail";
pub const CURVE_IMBALANCE: &str = "curve_imbalance";
pub const RATE_PROVIDER_STALENESS: &str = "rate_provider_staleness";
pub const FOT_ARB: &str = "fot_arb";
pub const REBASE_ARB: &str = "rebase_arb";
pub const SAVAX_RATE_ARB: &str = "savax_rate_arb";
pub const EPOCH_TRANSITION: &str = "epoch_transition";
pub const GMX_ADL_ARB: &str = "gmx_adl_arb";

/// Risk flag (not an attribution tag): the flash-funding vault is also a pool
/// in the arbitrage route, so the callback re-enters the funding contract.
pub const RISK_FUNDING_REENTRANCY: &str = "funding_reentrancy";

// --- Multi-axis taxonomy (WP2 / M-05) ------------------------------------
// Legacy values above are a persisted/dashboard contract and are never
// renamed. New tags are namespaced by axis so a funding source, router and
// optimizer mode can be reported independently without inventing MevKinds.

/// Taxonomy axes. Every emitted tag belongs to exactly one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagAxis {
    Family,
    Mechanism,
    Venue,
    Funding,
    Execution,
    Optimizer,
    Routing,
}

impl TagAxis {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Family => "family",
            Self::Mechanism => "mechanism",
            Self::Venue => "venue",
            Self::Funding => "funding",
            Self::Execution => "execution",
            Self::Optimizer => "optimizer",
            Self::Routing => "routing",
        }
    }
}

pub const FUNDING_BALANCER_V2: &str = "funding:balancer_v2_vault";
pub const FUNDING_AAVE_V3: &str = "funding:aave_v3";
pub const FUNDING_AAVE_V2: &str = "funding:aave_v2";
pub const FUNDING_UNIV3_FLASH: &str = "funding:univ3_flash";
pub const FUNDING_LFJ_LB_FLASH: &str = "funding:lfj_lb_flash";
pub const FUNDING_V2_FLASH_SWAP: &str = "funding:v2_flash_swap";
pub const FUNDING_MORPHO_BLUE: &str = "funding:morpho_blue";
/// Fallback for a flash provider with no dedicated tag yet.
pub const FUNDING_FLASH_LOAN: &str = "funding:flash_loan";

/// Canonical per-opportunity funding source for a config provider or a
/// decoded event protocol (WP5 / M-10). This is the *value* stored on
/// `MevOpportunity.funding_source` and `MevEvent.details.funding_source`;
/// the tags below are its namespaced dashboard representation.
///
/// Accepts both config spellings (`aave`, `balancer`, `lfj`) and event
/// protocols (`aave_v3`, `balancer_v2`, `lfj_lb_flash`) so the live detector
/// and the realized explorer write exactly the same value.
pub fn canonical_funding_source(source: &str) -> &'static str {
    match source {
        "auto" | "balancer" | "balancer_v2" | "balancer_v2_vault" => "balancer_v2_vault",
        "aave" | "aave_v3" => "aave_v3",
        "aave_v2" => "aave_v2",
        "uniswap" | "uniswap_v3" | "univ3_flash" => "univ3_flash",
        "lfj" | "lfj_lb_flash" => "lfj_lb_flash",
        "v2_flash_swap" => "v2_flash_swap",
        "morpho_blue" => "morpho_blue",
        _ => "flash_loan",
    }
}

/// Map a config provider or decoded event protocol to its funding-axis tag.
/// Flash loans are a funding layer, never an arbitrage family (A-01).
pub fn funding_tag_for_source(source: &str) -> &'static str {
    match canonical_funding_source(source) {
        "balancer_v2_vault" => FUNDING_BALANCER_V2,
        "aave_v3" => FUNDING_AAVE_V3,
        "aave_v2" => FUNDING_AAVE_V2,
        "univ3_flash" => FUNDING_UNIV3_FLASH,
        "lfj_lb_flash" => FUNDING_LFJ_LB_FLASH,
        "v2_flash_swap" => FUNDING_V2_FLASH_SWAP,
        "morpho_blue" => FUNDING_MORPHO_BLUE,
        _ => FUNDING_FLASH_LOAN,
    }
}

/// Split-route is an optimizer/execution mode, never a venue or route shape.
pub const OPTIMIZER_SPLIT_ROUTE: &str = "optimizer:split_route";
pub const ROUTING_PARASWAP_VELORA: &str = "routing:paraswap_velora";
pub const EXECUTION_SINGLE_TX: &str = "execution:single_tx";

/// Axis registry: legacy literals keep their values, new namespaced tags are
/// added additively. `axis_of_tag` is the single source of truth for dashboards
/// and the live/explorer parity test.
pub const TAG_AXIS_REGISTRY: &[(&str, TagAxis)] = &[
    (BACKRUN, TagAxis::Family),
    (FLASH_ARB, TagAxis::Funding),
    (LONG_TAIL, TagAxis::Mechanism),
    (CURVE_IMBALANCE, TagAxis::Mechanism),
    (RATE_PROVIDER_STALENESS, TagAxis::Mechanism),
    (FOT_ARB, TagAxis::Mechanism),
    (REBASE_ARB, TagAxis::Mechanism),
    (SAVAX_RATE_ARB, TagAxis::Mechanism),
    (EPOCH_TRANSITION, TagAxis::Mechanism),
    (GMX_ADL_ARB, TagAxis::Mechanism),
    (FUNDING_BALANCER_V2, TagAxis::Funding),
    (FUNDING_AAVE_V3, TagAxis::Funding),
    (FUNDING_AAVE_V2, TagAxis::Funding),
    (FUNDING_UNIV3_FLASH, TagAxis::Funding),
    (FUNDING_LFJ_LB_FLASH, TagAxis::Funding),
    (FUNDING_V2_FLASH_SWAP, TagAxis::Funding),
    (FUNDING_MORPHO_BLUE, TagAxis::Funding),
    (FUNDING_FLASH_LOAN, TagAxis::Funding),
    (OPTIMIZER_SPLIT_ROUTE, TagAxis::Optimizer),
    (ROUTING_PARASWAP_VELORA, TagAxis::Routing),
    (EXECUTION_SINGLE_TX, TagAxis::Execution),
];

/// Axis of a tag, or `None` for an unregistered/foreign tag.
pub fn axis_of_tag(tag: &str) -> Option<TagAxis> {
    TAG_AXIS_REGISTRY
        .iter()
        .find(|(name, _)| *name == tag)
        .map(|(_, axis)| *axis)
}

/// Every tag live can attach to an arb opportunity.
pub const LIVE_ARB_TAGS: &[&str] = &[
    BACKRUN,
    FLASH_ARB,
    LONG_TAIL,
    CURVE_IMBALANCE,
    RATE_PROVIDER_STALENESS,
    FOT_ARB,
    REBASE_ARB,
    SAVAX_RATE_ARB,
    EPOCH_TRANSITION,
    GMX_ADL_ARB,
    FUNDING_BALANCER_V2,
    FUNDING_AAVE_V3,
    FUNDING_AAVE_V2,
    FUNDING_UNIV3_FLASH,
    FUNDING_LFJ_LB_FLASH,
    FUNDING_V2_FLASH_SWAP,
    FUNDING_MORPHO_BLUE,
    FUNDING_FLASH_LOAN,
    OPTIMIZER_SPLIT_ROUTE,
    ROUTING_PARASWAP_VELORA,
    EXECUTION_SINGLE_TX,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_tag_has_exactly_one_axis() {
        let mut seen = std::collections::HashSet::new();
        for (tag, axis) in TAG_AXIS_REGISTRY {
            assert!(seen.insert(*tag), "duplicate tag in axis registry: {tag}");
            assert_eq!(axis_of_tag(tag), Some(*axis));
        }
        for tag in LIVE_ARB_TAGS {
            assert!(
                axis_of_tag(tag).is_some(),
                "live tag missing from axis registry: {tag}"
            );
        }
        assert_eq!(axis_of_tag(OPTIMIZER_SPLIT_ROUTE), Some(TagAxis::Optimizer));
        assert_eq!(axis_of_tag(FUNDING_AAVE_V3), Some(TagAxis::Funding));
    }

    #[test]
    fn canonical_funding_source_matches_live_provider_and_decoder() {
        use crate::types::strategy::FlashLoanProvider;
        for provider in [
            FlashLoanProvider::Auto,
            FlashLoanProvider::Balancer,
            FlashLoanProvider::Aave,
            FlashLoanProvider::Uniswap,
            FlashLoanProvider::Lfj,
            FlashLoanProvider::V2FlashSwap,
        ] {
            assert_eq!(
                canonical_funding_source(&provider.to_string()),
                provider.funding_source()
            );
            assert_eq!(
                funding_tag_for_source(&provider.to_string()),
                funding_tag_for_source(provider.funding_source())
            );
        }
        // Realized decoder protocols map to the same canonical values.
        assert_eq!(canonical_funding_source("balancer_v2"), "balancer_v2_vault");
        assert_eq!(canonical_funding_source("aave_v3"), "aave_v3");
        assert_eq!(canonical_funding_source("uniswap_v3"), "univ3_flash");
        assert_eq!(canonical_funding_source("morpho_blue"), "morpho_blue");
        assert_eq!(canonical_funding_source("lfj_lb_flash"), "lfj_lb_flash");
        assert_eq!(canonical_funding_source("v2_flash_swap"), "v2_flash_swap");
        assert_eq!(canonical_funding_source("unknown"), "flash_loan");
    }
}
