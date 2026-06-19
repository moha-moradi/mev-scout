//! MEV detection strategies: JIT liquidity, sandwich attacks, arbitrage (two-hop, multi-hop, JIT arb),
//! and PGA simulation for competition-adjusted profit estimates.

pub mod block_builder;
pub mod jit;
pub mod jit_arb;
pub mod liquidation;
pub mod sandwich;
pub mod multi_hop;
pub mod opportunity;
pub mod two_hop;
pub mod pga;
