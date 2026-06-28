//! MEV detection strategies: JIT liquidity, sandwich attacks, arbitrage (two-hop, multi-hop, JIT arb),
//! PGA simulation for competition-adjusted profit estimates, and competitor extraction analysis.

pub mod competition;
pub mod detectors;
pub mod execution;
pub mod verify;
pub use competition::*;
pub use detectors::*;
pub use execution::*;
pub use verify::*;
