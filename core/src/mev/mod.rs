//! MEV detection strategies: JIT liquidity, sandwich attacks, arbitrage (two-hop, multi-hop, JIT arb),
//! PGA simulation for competition-adjusted profit estimates, and competitor extraction analysis.

pub mod detectors;
pub use detectors::{
    balancer_quote_exact_in, capture_pending_block, compute_health_factor,
    curve_output_amount, detect_pending_opportunities,
    quote_path, AaveReserveCache, AaveReserveData,
    JitArbDetector, JitDetector, LiquidationDetector, MultiHopArbDetector,
    PendingBlockCapture, SandwichDetector, TwoHopArbDetector,
};
