//! MEV detection strategies: JIT liquidity, sandwich attacks, arbitrage (two-hop, multi-hop, JIT arb),
//! and PGA simulation for competition-adjusted profit estimates.

pub mod detectors;
pub mod execution;
pub mod verify;
pub use detectors::*;
pub use execution::*;
pub use verify::*;
