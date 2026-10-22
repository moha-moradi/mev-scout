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
    /// Scanner never ran over the block (taxonomy placeholder; not emitted by
    /// the runner).
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
        }
    }
}

/// One rejected scanner candidate (schema minus run_id/chain, which the
/// persistence layer stamps at insert time).
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
