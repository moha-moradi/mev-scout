//! DEX pool management: decoders, discovery, math, state, and V3 quoting.

pub mod balancer_math;
pub mod curve_math;
pub mod decoders;
pub mod dex_type;
pub mod discovery;
pub mod math;
pub mod state;
pub mod subgraph_discovery;
pub mod v3_quote;

pub use decoders::{V3SwapDecoded, V3MintBurnDecoded, CurveSwapDecoded, BalancerSwapDecoded};
pub use dex_type::DexType;
pub use discovery::DiscoveredPool;
pub use math::{quote_exact_in, TwoHopArbResult};
pub use state::{PoolInfo, PoolManager, PoolState, UniswapV2PoolState, UniswapV3PoolState, CurvePoolState, CurvePoolVariant, BalancerPoolState, BalancerPoolVariant};
pub use curve_math::{curve_output_amount, curve_stableswap_output_amount, curve_cryptoswap_output_amount};
pub use balancer_math::{balancer_output_amount, balancer_stable_output_amount, balancer_quote_exact_in};
pub use v3_quote::{quote_v3_exact_in, quote_v3_exact_out};
