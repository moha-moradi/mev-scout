//! Phase-2 pool metadata fetch — per-DEX `token0()`/`token1()`/`fee()`
//! resolution for pools discovered only through DEX activity events.

use std::future::Future;
use std::pin::Pin;

use alloy::primitives::Address;

use super::{DexType, RpcClient};
use crate::pool::selectors::{
    FEE, PENDLE_SY, TICK_SPACING, TOKEN0, TOKEN1, TRADER_JOE_TOKEN_X, TRADER_JOE_TOKEN_Y,
};

/// Per-DEX fallback swap fee (ppm) applied when metadata fetch returns none or
/// the pool type has no on-chain fee. V2/Solidly default to 30 bps unless the
/// caller overrides; Camelot defaults to 0; concentrated-liquidity DEXes to 3000.
pub(super) fn default_fee(
    dex_type: DexType,
    fee_opt: Option<u32>,
    v2_fee_override: Option<u32>,
) -> u32 {
    match dex_type {
        DexType::UniswapV2 | DexType::Solidly | DexType::Camelot => {
            v2_fee_override.unwrap_or(match dex_type {
                DexType::Solidly => 30,
                DexType::Camelot => 0,
                _ => 30,
            })
        }
        DexType::UniswapV3 | DexType::UniswapV4 | DexType::PancakeInfinity => {
            fee_opt.unwrap_or(3000)
        }
        DexType::Curve
        | DexType::Balancer
        | DexType::TraderJoeLB
        | DexType::Pendle
        | DexType::Metric
        | DexType::Fluid => fee_opt.unwrap_or(0),
    }
}

/// One async metadata-fetch result for an event-discovered pool.
#[derive(Clone, Copy)]
pub(super) struct PoolMetadataFetch {
    pub(super) addr: Address,
    pub(super) dex_type: DexType,
    pub(super) token0: Option<Address>,
    pub(super) token1: Option<Address>,
    pub(super) fee: Option<u32>,
    pub(super) tick_spacing: Option<u32>,
    pub(super) first_seen_block: u64,
}

pub(super) type FetchTask = Pin<Box<dyn Future<Output = PoolMetadataFetch> + Send>>;

/// Build the metadata fetch task for one event-discovered pool.
///
/// Returns `None` for singleton architectures whose synthetic pool key has no
/// contract code to call (V4, Pancake-Infinity-only activity, Metric): the
/// caller counts those as unmatched and does not spend RPC on them.
///
/// - `event_tokens` is the token pair recovered directly from the event.
/// - `factory_tokens` supplies token0/token1 already resolved by the factory
///   scan (Pancake Infinity CL, whose synthetic key has no code to call).
pub(super) fn fetch_pool_metadata(
    rpc: &RpcClient,
    addr: Address,
    dex_type: DexType,
    event_tokens: Option<(Address, Address)>,
    first_seen_block: u64,
    factory_tokens: Option<(Address, Address)>,
) -> Option<FetchTask> {
    let rpc = rpc.clone();
    let sel0 = TOKEN0.clone();
    let sel1 = TOKEN1.clone();
    let sel_fee = FEE.clone();
    let sel_ts = TICK_SPACING.clone();
    let fsb = first_seen_block;
    match dex_type {
        DexType::UniswapV2 | DexType::Solidly | DexType::Camelot | DexType::Fluid => {
            Some(Box::pin(async move {
                let (r0, r1) =
                    futures::future::join(
                        async {
                            rpc.call_latest(addr, sel0).await.ok().and_then(|b| {
                                (b.len() >= 32).then(|| Address::from_slice(&b[12..32]))
                            })
                        },
                        async {
                            rpc.call_latest(addr, sel1).await.ok().and_then(|b| {
                                (b.len() >= 32).then(|| Address::from_slice(&b[12..32]))
                            })
                        },
                    )
                    .await;
                PoolMetadataFetch {
                    addr,
                    dex_type,
                    token0: r0,
                    token1: r1,
                    fee: None,
                    tick_spacing: None,
                    first_seen_block: fsb,
                }
            }))
        }
        DexType::TraderJoeLB => {
            // Per-pair contracts work with direct calls, but an LBPair exposes
            // `tokenX()`/`tokenY()`, NOT `token0()`/`token1()`; the standard
            // selectors revert and would leave tokens unresolved.
            let sel_x = TRADER_JOE_TOKEN_X.clone();
            let sel_y = TRADER_JOE_TOKEN_Y.clone();
            Some(Box::pin(async move {
                let (r0, r1) =
                    futures::future::join(
                        async {
                            rpc.call_latest(addr, sel_x).await.ok().and_then(|b| {
                                (b.len() >= 32).then(|| Address::from_slice(&b[12..32]))
                            })
                        },
                        async {
                            rpc.call_latest(addr, sel_y).await.ok().and_then(|b| {
                                (b.len() >= 32).then(|| Address::from_slice(&b[12..32]))
                            })
                        },
                    )
                    .await;
                PoolMetadataFetch {
                    addr,
                    dex_type,
                    token0: r0,
                    token1: r1,
                    fee: None,
                    tick_spacing: None,
                    first_seen_block: fsb,
                }
            }))
        }
        DexType::UniswapV3 => Some(Box::pin(async move {
            let (token0, token1, fee, tick_spacing) = futures::future::join4(
                async {
                    rpc.call_latest(addr, sel0)
                        .await
                        .ok()
                        .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])))
                },
                async {
                    rpc.call_latest(addr, sel1)
                        .await
                        .ok()
                        .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])))
                },
                async {
                    rpc.call_latest(addr, sel_fee).await.ok().and_then(|b| {
                        (b.len() >= 32).then(|| u32::from_be_bytes([b[28], b[29], b[30], b[31]]))
                    })
                },
                async {
                    rpc.call_latest(addr, sel_ts).await.ok().and_then(|b| {
                        (b.len() >= 32).then(|| {
                            let mut ts = [0u8; 4];
                            ts.copy_from_slice(&b[28..32]);
                            i32::from_be_bytes(ts) as u32
                        })
                    })
                },
            )
            .await;
            PoolMetadataFetch {
                addr,
                dex_type: DexType::UniswapV3,
                token0,
                token1,
                fee,
                tick_spacing,
                first_seen_block: fsb,
            }
        })),
        DexType::UniswapV4 | DexType::Metric => {
            // Singleton PoolManager / unpublished-pool-ABI: per-pool RPC calls
            // would burn requests on guaranteed reverts. Activity hits that the
            // factory scan or SQLite cache did not resolve stay out of the
            // index; a later run observing the Initialize/PoolCreated event (or
            // a cache hit) makes them resolvable.
            None
        }
        DexType::PancakeInfinity => {
            let (t0, t1) = factory_tokens.unwrap_or((Address::ZERO, Address::ZERO));
            Some(Box::pin(async move {
                PoolMetadataFetch {
                    addr,
                    dex_type,
                    token0: Some(t0),
                    token1: Some(t1),
                    fee: None,
                    tick_spacing: None,
                    first_seen_block: fsb,
                }
            }))
        }
        DexType::Curve | DexType::Balancer => {
            let (t0, t1) = event_tokens.unwrap_or((Address::ZERO, Address::ZERO));
            Some(Box::pin(async move {
                PoolMetadataFetch {
                    addr,
                    dex_type,
                    token0: Some(t0),
                    token1: Some(t1),
                    fee: None,
                    tick_spacing: None,
                    first_seen_block: fsb,
                }
            }))
        }
        DexType::Pendle => {
            // Pendle markets from activity events: token0 is PT (known), token1
            // is its SY yield source, resolved via PT.SY().
            let (t0, _) = event_tokens.unwrap_or((Address::ZERO, Address::ZERO));
            Some(Box::pin(async move {
                let token1 = if !t0.is_zero() {
                    match rpc.call_latest(t0, PENDLE_SY.clone()).await {
                        Ok(b) if b.len() >= 32 => Some(Address::from_slice(&b[12..32])),
                        _ => None,
                    }
                } else {
                    None
                };
                PoolMetadataFetch {
                    addr,
                    dex_type,
                    token0: Some(t0),
                    token1,
                    fee: None,
                    tick_spacing: None,
                    first_seen_block: fsb,
                }
            }))
        }
    }
}
