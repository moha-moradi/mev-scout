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
    /// Scanner taxonomy placeholder. Classify does not emit it, and neither
    /// does the explorer runner: a block the scanner never visited is the
    /// absence of a row, not a reject. The variant stays so a report that
    /// iterates this enum can show an explicit zero instead of a missing label.
    StateStale,
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
    /// Frontrun/sandwich: no third-party same-direction victim.
    VictimSetEmpty,
    /// Skim: a pair outflow has V2 venue evidence but the pool is not in the
    /// v2-like registry. Aggregated per block.
    SkimPoolNotV2Like,
    /// Skim: a registry pool both pays out and receives in the same tx.
    SkimLooksLikeSwap,
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
            RejectReason::VictimSetEmpty => "victim_set_empty",
            RejectReason::SkimPoolNotV2Like => "skim_pool_not_v2_like",
            RejectReason::SkimLooksLikeSwap => "skim_looks_like_swap",
            RejectReason::JitPairingUnmatched => "jit_pairing_unmatched",
            RejectReason::LiquidationNotReconciled => "liquidation_not_reconciled",
        }
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}
