//! MEV detection strategies: JIT liquidity and arbitrage (two-hop, multi-hop),
//! PGA simulation for competition-adjusted profit estimates, and competitor extraction analysis.
//!
//! Liquidation capture is not implemented here: there is no `Strategy`
//! variant and no detector for it. Configs that still say `liquidation` keep
//! loading — the name is retired and dropped by
//! [`crate::types::Strategy::from_comma_list`]. The explorer independently
//! classifies realized liquidations as `MevKind::Liquidation`.

pub mod detectors;
pub(crate) mod verdict;
pub use detectors::{
    balancer_quote_exact_in, capture_pending_block, curve_output_amount,
    detect_pending_opportunities, quote_path, JitDetector, MultiHopArbDetector,
    PendingBlockCapture, TwoHopArbDetector,
};
pub use verdict::{mev_verdict, MevVerdict};
