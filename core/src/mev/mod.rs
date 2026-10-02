//! MEV detection strategies: JIT liquidity and arbitrage (two-hop, multi-hop),
//! PGA simulation for competition-adjusted profit estimates, and competitor extraction analysis.
//!
//! Liquidation capture is observation-only in this tree: `Strategy::Liquidation`
//! still parses so the DB `kind` column and historical reports keep working,
//! but no detector produces it (see `docs/plan_prune_strategies.md` §6). The
//! explorer independently classifies realized liquidations as `MevKind`.

pub mod detectors;
pub mod verdict;
pub use detectors::{
    balancer_quote_exact_in, capture_pending_block, curve_output_amount, detect_pending_opportunities,
    quote_path, JitDetector, MultiHopArbDetector, PendingBlockCapture, TwoHopArbDetector,
};
pub use verdict::{mev_verdict, MevVerdict};
