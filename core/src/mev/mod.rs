//! MEV detection strategies: JIT liquidity, sandwich attacks, arbitrage (two-hop, multi-hop, JIT arb),
//! PGA simulation for competition-adjusted profit estimates, and competitor extraction analysis.

pub mod detectors;
pub mod execution;
pub use detectors::*;
pub use execution::*;
