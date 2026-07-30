//! MEV detection strategies: JIT liquidity, sandwich attacks, arbitrage (two-hop, multi-hop, JIT arb),
//! PGA simulation for competition-adjusted profit estimates, and competitor extraction analysis.

pub mod detectors;
pub mod execution;
pub use detectors::{
    balancer_output_amount, balancer_quote_exact_in, capture_pending_block, compute_health_factor,
    curve_output_amount, detect_pending_opportunities, estimate_pending_tx_pool_impact,
    quote_path, simulate_pending_tx_pool_impact, AaveReserveCache, AaveReserveData,
    CrossBlockDetector, JitArbDetector, JitDetector, LiquidationDetector, MultiHopArbDetector,
    PendingBlockCapture, PendingPoolEffect, SandwichDetector, TwoHopArbDetector,
};
pub use execution::{
    ExecutionRecord, LiveConfig, LiveRunner, LiveRunnerState,
};
