//! DEX pool management: decoders, discovery, math, state, and V3 quoting.

pub mod decoders;
pub mod dex_type;
pub mod discovery;
pub mod math;
pub mod state;
pub mod v3_quote;

pub use decoders::{V3SwapDecoded, V3MintBurnDecoded, CurveSwapDecoded, BalancerSwapDecoded};
pub use dex_type::DexType;
pub use discovery::DiscoveredPool;
pub use math::TwoHopArbResult;
pub use state::{PoolInfo, PoolManager, PoolState, UniswapV2PoolState, UniswapV3PoolState, CurvePoolState, BalancerPoolState};
pub use v3_quote::{quote_v3_exact_in, quote_v3_exact_out};
