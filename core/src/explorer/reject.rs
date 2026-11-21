//! Rejection capture — the scanner's negative space.
//!
//! When `record_rejections` is enabled in config, the runner buffers every
//! candidate dropped by its filters with a reason;
//! `run`/`live` drain the buffer into the explorer store's
//! `rejected_candidates` table.
use serde::{Deserialize, Serialize};

/// Why a candidate was rejected (reason enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    /// Pool for an edge is absent from `PoolManager`.
    NoPool,
    /// Venue class not supported / wrong fee tier.
    NoPath,
    /// Quote returned zero/negative expected profit.
    QuoteNonpositive,
    /// Profit below `min_profit_wei` threshold.
    BelowMinProfit,
    /// Expected profit does not cover gas cost.
    GasDominates,
    /// Dropped by the per-tx top-N cap (normal competition pressure).
    MaxCandidates,
    /// Legacy scanner taxonomy value. Classify never emits it: a block the
    /// scanner never visited is the absence of a row, not a reject. Retained so
    /// historical `rejected_candidates` rows keep a stable reason label.
    /// Superseded for live candidates by [`Self::QuoteExpired`] /
    /// [`Self::QuoteDrift`], which distinguish deadline expiry from a stale
    /// price; the label itself is never rewritten in the database.
    StateStale,
    /// Capital-free live gate: quote deadline passed before simulation/submit.
    QuoteExpired,
    /// Capital-free live gate: quoted price moved beyond the drift tolerance.
    QuoteDrift,
    /// Capital-free live gate: no flash provider could fund the atomic route.
    FundingUnavailable,
    /// Capital-free live gate: the atomic repay leg failed in simulation.
    RepayFailed,
    /// Capital-free live gate: the executor's token allowance is missing/low.
    AllowanceMissing,
    /// Capital-free live gate: the searcher balance cannot cover gas/escrow.
    BalanceInsufficient,
    /// Capital-free live gate: the searcher nonce conflicts with a pending tx.
    NonceConflict,
    /// Capital-free live gate: state simulation failed before quoting.
    SimFailed,
    /// Capital-free live gate: a risk flag on the opportunity blocks live use.
    RiskFlagBlocked,
    /// Explorer classify: closed swap-edge cycle failed (≥2 resolved edges).
    UnresolvedCycle,
    /// Explorer classify: flow owners do not collapse to one searcher candidate.
    MixedFlowOwner,
    /// Explorer classify: no profit-positive candidate (and forwarded residual
    /// either absent or tip-sink).
    NetZeroSearcher,
    /// Explorer classify: material inventory/deficit on the profit holder.
    MaterialDeficit,
    /// Explorer classify: initiator spent outside the closed walk (open ledger).
    OpenInventory,
    /// Explorer classify: residual forwarded only to coinbase / tip sink.
    BuilderTip,
    /// Anything else (M-detail in `detail`).
    Other,
    /// Causal/cycle gate: the swap-edge walk did not close, or closed with
    /// fewer than two resolved edges (the latter is aggregated, not anchored).
    CycleNotClosed,
    /// Causal backrun: a closed cycle had no prior opposite-direction leg.
    NoCounterpartyLeg,
    /// Causal gate: the counterparty (or its pre-move reference) sits outside
    /// the adjacency window (`CAUSAL_WINDOW_TXS`).
    CausalWindowTooWide,
    /// Causal gate: an in-window counterparty failed the price-advantage floor.
    CausalPriceMoveBelowFloor,
    /// Causal backrun: the causal gates passed but the closer tx carries no
    /// emitted `ArbAtomic` to annotate (the arb pass rejected the cycle). The
    /// coverage loss is recorded here rather than dropped silently.
    CausalAnchorMissing,
    /// Skim: a pair outflow has V2 venue evidence but the pool is not in the
    /// v2-like registry. Aggregated per block.
    SkimPoolNotV2Like,
    /// Skim: a registry pool both pays out and receives in the same tx.
    SkimLooksLikeSwap,
    /// Skim: a V2-like pool outflow failed the positive-evidence gate. The
    /// `detail` names the counter-evidence (pool logs, decoded swap/pair-op,
    /// liquidity action, or an outflow token the pair does not hold).
    SkimCounterEvidence,
    /// JIT: a Mint has no later Burn, or the pair has no in-range validating swap.
    JitPairingUnmatched,
    /// Liquidation event stands, but seized collateral is not covered by transfers.
    LiquidationNotReconciled,
}

impl RejectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::NoPool => "no_pool",
            RejectReason::NoPath => "no_path",
            RejectReason::QuoteNonpositive => "quote_nonpositive",
            RejectReason::BelowMinProfit => "below_min_profit",
            RejectReason::GasDominates => "gas_dominates",
            RejectReason::MaxCandidates => "max_candidates",
            RejectReason::StateStale => "state_stale",
            RejectReason::QuoteExpired => "quote_expired",
            RejectReason::QuoteDrift => "quote_drift",
            RejectReason::FundingUnavailable => "funding_unavailable",
            RejectReason::RepayFailed => "repay_failed",
            RejectReason::AllowanceMissing => "allowance_missing",
            RejectReason::BalanceInsufficient => "balance_insufficient",
            RejectReason::NonceConflict => "nonce_conflict",
            RejectReason::SimFailed => "sim_failed",
            RejectReason::RiskFlagBlocked => "risk_flag_blocked",
            RejectReason::UnresolvedCycle => "unresolved_cycle",
            RejectReason::MixedFlowOwner => "mixed_flow_owner",
            RejectReason::NetZeroSearcher => "net_zero_searcher",
            RejectReason::MaterialDeficit => "material_deficit",
            RejectReason::OpenInventory => "open_inventory",
            RejectReason::BuilderTip => "builder_tip",
            RejectReason::Other => "other",
            RejectReason::CycleNotClosed => "cycle_not_closed",
            RejectReason::NoCounterpartyLeg => "no_counterparty_leg",
            RejectReason::CausalWindowTooWide => "causal_window_too_wide",
            RejectReason::CausalPriceMoveBelowFloor => "causal_price_move_below_floor",
            RejectReason::CausalAnchorMissing => "causal_anchor_missing",
            RejectReason::SkimPoolNotV2Like => "skim_pool_not_v2_like",
            RejectReason::SkimLooksLikeSwap => "skim_looks_like_swap",
            RejectReason::SkimCounterEvidence => "skim_counter_evidence",
            RejectReason::JitPairingUnmatched => "jit_pairing_unmatched",
            RejectReason::LiquidationNotReconciled => "liquidation_not_reconciled",
        }
    }

    /// Quote freshness gates (deadline and price drift). Kept separate so the
    /// WP4 reporter can distinguish expiry from stale pricing without touching
    /// the reject ladder's stable labels.
    pub const fn is_quote_freshness(self) -> bool {
        matches!(self, Self::QuoteExpired | Self::QuoteDrift)
    }

    /// All capital-free atomic-execution preflight gates. `StateStale` is
    /// deliberately excluded: it is a legacy classify label, never emitted by
    /// the live gate.
    pub const fn is_live_gate(self) -> bool {
        matches!(
            self,
            Self::QuoteExpired
                | Self::QuoteDrift
                | Self::FundingUnavailable
                | Self::RepayFailed
                | Self::AllowanceMissing
                | Self::BalanceInsufficient
                | Self::NonceConflict
                | Self::SimFailed
                | Self::RiskFlagBlocked
        )
    }
}

/// One rejected scanner or classify candidate (schema minus run_id/chain,
/// which the persistence layer stamps at insert time).
///
/// Volume rule (A20): a classify pass emits an anchored row
/// (`tx_index = Some`) only for a candidate that already had a plausible
/// anchor — a victim set, a matched mint/burn or opposite leg, or a swap
/// cycle of at least two resolved edges. Every other miss is aggregated to
/// one row per block per `(reject_reason, detail)` with `tx_index = None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RejectedCandidate {
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub strategy: String,
    pub pool_a: Option<String>,
    pub pool_b: Option<String>,
    pub path: Option<String>,
    pub token_in: Option<String>,
    pub token_out: Option<String>,
    pub input_amount: Option<String>,
    pub expected_profit: Option<String>,
    pub expected_profit_usd: Option<f64>,
    pub gas_cost_wei: Option<String>,
    pub reject_reason: String,
    pub detail: Option<String>,
    pub created_at: u64,
    /// Capital-free context (V18 `rejected_candidates` columns). All additive:
    /// classify-only rejects leave them `None`, while runner rejects carry the
    /// same quote/funding snapshot as the opportunity row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funding_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim_block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_flags_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_gas_cost_wei: Option<u128>,
}

impl RejectedCandidate {
    /// Build a reject from the opportunity it was derived from, preserving the
    /// quote/funding context needed to attribute a live-gate decision (M-04).
    /// `block_number` is the pass's block, which may differ from the
    /// opportunity's own `block_number` for pending-backrun candidates.
    pub fn from_opportunity(
        opp: &crate::types::MevOpportunity,
        block_number: u64,
        reason: RejectReason,
        detail: Option<String>,
        created_at: u64,
    ) -> Self {
        Self {
            block_number,
            tx_index: Some(opp.tx_index as u64),
            strategy: opp.strategy.to_string(),
            pool_a: Some(format!("{:#x}", opp.pool_a)),
            pool_b: (opp.pool_b != alloy::primitives::Address::ZERO)
                .then(|| format!("{:#x}", opp.pool_b)),
            path: opp.path.as_ref().map(|p| {
                p.iter()
                    .map(|a| format!("{a:#x}"))
                    .collect::<Vec<_>>()
                    .join(",")
            }),
            token_in: (!opp.token_in.is_zero()).then(|| format!("{:#x}", opp.token_in)),
            token_out: (!opp.token_out.is_zero()).then(|| format!("{:#x}", opp.token_out)),
            input_amount: Some(opp.input_amount.to_string()),
            expected_profit: Some(opp.expected_profit.to_string()),
            expected_profit_usd: None,
            gas_cost_wei: Some(opp.gas_cost_wei.to_string()),
            reject_reason: reason.as_str().to_string(),
            detail,
            created_at,
            quote_expires_at: opp.quote_expires_at,
            funding_source: opp.funding_source.clone(),
            sim_block: opp.sim_block,
            risk_flags_json: if opp.risk_flags.is_empty() {
                None
            } else {
                serde_json::to_string(&opp.risk_flags).ok()
            },
            revert_gas_cost_wei: opp.revert_gas_cost_wei,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::strategy::Strategy;
    use crate::types::MevOpportunity;
    use alloy::primitives::{address, U256};

    #[test]
    fn m04_reason_labels_are_stable_and_grouped() {
        for (reason, label) in [
            (RejectReason::QuoteExpired, "quote_expired"),
            (RejectReason::QuoteDrift, "quote_drift"),
            (RejectReason::FundingUnavailable, "funding_unavailable"),
            (RejectReason::RepayFailed, "repay_failed"),
            (RejectReason::AllowanceMissing, "allowance_missing"),
            (RejectReason::BalanceInsufficient, "balance_insufficient"),
            (RejectReason::NonceConflict, "nonce_conflict"),
            (RejectReason::SimFailed, "sim_failed"),
            (RejectReason::RiskFlagBlocked, "risk_flag_blocked"),
        ] {
            assert_eq!(reason.as_str(), label);
            assert!(reason.is_live_gate());
        }
        assert!(RejectReason::QuoteExpired.is_quote_freshness());
        assert!(RejectReason::QuoteDrift.is_quote_freshness());
        assert!(!RejectReason::FundingUnavailable.is_quote_freshness());
        // Legacy label keeps its meaning and is not a live-gate reason.
        assert_eq!(RejectReason::StateStale.as_str(), "state_stale");
        assert!(!RejectReason::StateStale.is_live_gate());
        assert!(!RejectReason::GasDominates.is_live_gate());
    }

    #[test]
    fn rejection_from_opportunity_carries_quote_context() {
        let mut opp = MevOpportunity::new(
            100,
            3,
            Strategy::TwoHopArb,
            address!("0x1111111111111111111111111111111111111111"),
            1_700_000_000,
        );
        opp.pool_b = address!("0x2222222222222222222222222222222222222222");
        opp.token_in = address!("0x3333333333333333333333333333333333333333");
        opp.token_out = address!("0x4444444444444444444444444444444444444444");
        opp.input_amount = U256::from(42u64);
        opp.expected_profit = U256::from(85u64);
        opp.gas_cost_wei = 10;
        opp.quote_expires_at = Some(1_700_000_005);
        opp.funding_source = Some("balancer_v2_vault".into());
        opp.sim_block = Some(99);
        opp.risk_flags = vec!["funding_reentrancy".into()];
        opp.revert_gas_cost_wei = Some(3);

        let reject = RejectedCandidate::from_opportunity(
            &opp,
            101,
            RejectReason::QuoteExpired,
            Some("deadline passed".into()),
            1_700_000_006,
        );
        assert_eq!(reject.block_number, 101);
        assert_eq!(reject.tx_index, Some(3));
        assert_eq!(reject.reject_reason, "quote_expired");
        assert_eq!(reject.quote_expires_at, Some(1_700_000_005));
        assert_eq!(reject.funding_source.as_deref(), Some("balancer_v2_vault"));
        assert_eq!(reject.sim_block, Some(99));
        assert_eq!(
            reject.risk_flags_json.as_deref(),
            Some(r#"["funding_reentrancy"]"#)
        );
        assert_eq!(reject.revert_gas_cost_wei, Some(3));
        assert_eq!(reject.gas_cost_wei.as_deref(), Some("10"));
    }
}
