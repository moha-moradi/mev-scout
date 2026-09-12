//! Rejection capture — the scanner's negative space.
//!
//! Cross-validation of false negatives is only explainable if the candidates
//! the scanner rejected are observable. When `--record-rejections` is enabled,
//! the runner buffers every candidate dropped by its filters with a reason;
//! `run`/`live` drain the buffer into the explorer store's
//! `rejected_candidates` table.

use serde::{Deserialize, Serialize};

/// Why a candidate was rejected (reason enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    /// Pool for an edge is absent from `PoolManager` (M1).
    NoPool,
    /// Venue class not supported / wrong fee tier (M2).
    NoPath,
    /// Quote returned zero/negative expected profit (M5).
    QuoteNonpositive,
    /// Profit below `min_profit_wei` threshold (M3).
    BelowMinProfit,
    /// Expected profit does not cover gas cost (M4).
    GasDominates,
    /// Dropped by the per-tx top-N cap (normal competition pressure).
    MaxCandidates,
    /// Scanner never ran over the block (M7) — recorded by `validate`, not
    /// by the runner; listed here so the enum is exhaustive.
    StateStale,
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
