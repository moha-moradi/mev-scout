//! Pool discovery — scans chain event logs to find and register new DEX pools.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;

use alloy::primitives::{keccak256, Address, B256, Bytes};
use alloy::rpc::types::Filter;
use serde::{Deserialize, Serialize};

use crate::cache::SqliteStore;
use crate::pool::dex_type::DexType;
use crate::pool::state::PoolInfo;
use crate::rpc::RpcClient;
use crate::scan::topics;

pub static V2_PAIR_CREATED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"PairCreated(address,address,address,uint256)")
});

pub static V3_POOL_CREATED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"PoolCreated(address,address,uint24,int24,address)")
});

pub static BALANCER_POOL_REGISTERED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"PoolRegistered(bytes32,address,uint8)")
});

pub static CURVE_POOL_ADDED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"PoolAdded(address,uint256)")
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: Option<i32>,
    pub dex_type: DexType,
    #[serde(default)]
    pub creation_block: u64,
    /// Balancer V2 pool ID (bytes32), used to query vault for token balances.
    #[serde(default)]
    pub pool_id: Option<[u8; 32]>,
    /// Factory address that created this pool (L6: fork-aware V2 storage slots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factory: Option<Address>,
}

impl From<DiscoveredPool> for PoolInfo {
    fn from(d: DiscoveredPool) -> Self {
        PoolInfo {
            address: d.address,
            token0: d.token0,
            token1: d.token1,
            fee: d.fee,
            name: None,
            dex_type: d.dex_type,
            tick_spacing: d.tick_spacing.map(|ts| ts as u32),
            creation_block: d.creation_block,
            pool_id: d.pool_id,
            factory: d.factory,
        }
    }
}

pub async fn discover_v2_pools(
    rpc: &RpcClient,
    factory: Address,
    from_block: u64,
    to_block: u64,
    fee_override: Option<u32>,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let filter = Filter::new()
        .address(factory)
        .event_signature(*V2_PAIR_CREATED_TOPIC)
        .from_block(from_block)
        .to_block(to_block);

    let logs = rpc.get_logs(&filter).await?;
    let mut pools = Vec::with_capacity(logs.len());

    for log in logs {
        let log_data = log.data();
        let topics = log.topics();
        if log_data.data.len() < 64 || topics.len() < 3 {
            tracing::warn!(
                "Skipping malformed V2 PairCreated log at block {:?}: data.len={}, topics.len={}",
                log.block_number, log_data.data.len(), topics.len()
            );
            continue;
        }
        let token0 = Address::from_slice(&topics[1][12..]);
        let token1 = Address::from_slice(&topics[2][12..]);
        let mut pair_bytes = [0u8; 32];
        pair_bytes.copy_from_slice(&log_data.data[..32]);
        let pair = Address::from_slice(&pair_bytes[12..]);

        let fee = fee_override.unwrap_or(30);
        let creation_block = match log.block_number {
            Some(bn) => bn,
            None => {
                tracing::warn!("V2 PairCreated log missing block_number, using end of range {}", to_block);
                to_block
            }
        };

        pools.push(DiscoveredPool {
            address: pair,
            token0,
            token1,
            fee,
            tick_spacing: None,
            dex_type: DexType::UniswapV2,
            creation_block,
            pool_id: None,
            factory: Some(factory),
        });
    }

    Ok(pools)
}

pub async fn discover_v3_pools(
    rpc: &RpcClient,
    factory: Address,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let filter = Filter::new()
        .address(factory)
        .event_signature(*V3_POOL_CREATED_TOPIC)
        .from_block(from_block)
        .to_block(to_block);

    let logs = rpc.get_logs(&filter).await?;
    let mut pools = Vec::with_capacity(logs.len());

    for log in logs {
        let log_data = log.data();
        let topics = log.topics();
        // token0, token1, fee are indexed → topics[1..3]
        // tickSpacing (int24) and pool (address) are in data → 64 bytes
        if log_data.data.len() < 64 || topics.len() < 4 {
            tracing::warn!(
                "Skipping malformed V3 PoolCreated log at block {:?}: data.len={}, topics.len={}",
                log.block_number, log_data.data.len(), topics.len()
            );
            continue;
        }
        let token0 = Address::from_slice(&topics[1][12..]);
        let token1 = Address::from_slice(&topics[2][12..]);
        // fee is uint24, left-padded in topics[3]
        let fee = u32::from_be_bytes([
            topics[3][28],
            topics[3][29],
            topics[3][30],
            topics[3][31],
        ]);
        // tickSpacing is int24, left-padded in data[0..32]
        let tick_spacing = {
            let mut ts_bytes = [0u8; 4];
            ts_bytes.copy_from_slice(&log_data.data[28..32]);
            Some(i32::from_be_bytes(ts_bytes))
        };
        // pool is address, left-padded in data[32..64]
        let pool_addr = Address::from_slice(&log_data.data[44..64]);

        let creation_block = match log.block_number {
            Some(bn) => bn,
            None => {
                tracing::warn!("V3 PoolCreated log missing block_number, using end of range {}", to_block);
                to_block
            }
        };

        pools.push(DiscoveredPool {
            address: pool_addr,
            token0,
            token1,
            fee,
            tick_spacing,
            dex_type: DexType::UniswapV3,
            creation_block,
            pool_id: None,
            factory: Some(factory),
        });
    }

    Ok(pools)
}

/// Discover Balancer V2 pools by scanning `PoolRegistered` events from the vault contract.
/// Only pools with poolType 0 (weighted) or 1 (stable) are included for now.
/// The pool_id is stored alongside the pool address for later state queries.
pub async fn discover_balancer_pools(
    rpc: &RpcClient,
    vault: Address,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let filter = Filter::new()
        .address(vault)
        .event_signature(*BALANCER_POOL_REGISTERED_TOPIC)
        .from_block(from_block)
        .to_block(to_block);

    let logs = rpc.get_logs(&filter).await?;
    let mut pools = Vec::with_capacity(logs.len());

    for log in logs {
        let topics = log.topics();
        if topics.len() < 4 {
            tracing::warn!(
                "Skipping malformed Balancer PoolRegistered log at block {:?}: topics.len={}",
                log.block_number, topics.len()
            );
            continue;
        }
        // All parameters are indexed:
        //   topic[0] = event sig
        //   topic[1] = poolId (bytes32)
        //   topic[2] = poolAddress (address, left-padded)
        //   topic[3] = specialization (uint8, left-padded)
        let mut pool_id = [0u8; 32];
        pool_id.copy_from_slice(topics[1].as_slice());
        let pool_addr = Address::from_slice(&topics[2][12..32]);
        let pool_type = topics[3][31];
        // Only include weighted (0) and stable (1) pools
        if pool_type > 1 {
            continue;
        }

        let creation_block = match log.block_number {
            Some(bn) => bn,
            None => {
                tracing::warn!("Balancer PoolRegistered log missing block_number, using end of range {}", to_block);
                to_block
            }
        };

        pools.push(DiscoveredPool {
            address: pool_addr,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 0,
            tick_spacing: None,
            dex_type: DexType::Balancer,
            creation_block,
            pool_id: Some(pool_id),
            factory: None,
        });
    }

    Ok(pools)
}

/// Discover Curve pools by scanning `PoolAdded` events from a Curve registry contract.
/// Token addresses are not available in the event — they are fetched during pool
/// initialization via `fetch_curve_state()`.
pub async fn discover_curve_pools(
    rpc: &RpcClient,
    registry: Address,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let filter = Filter::new()
        .address(registry)
        .event_signature(*CURVE_POOL_ADDED_TOPIC)
        .from_block(from_block)
        .to_block(to_block);

    let logs = rpc.get_logs(&filter).await?;
    let mut pools = Vec::with_capacity(logs.len());

    for log in logs {
        let topics = log.topics();
        if topics.len() < 2 {
            tracing::warn!(
                "Skipping malformed Curve PoolAdded log at block {:?}: topics.len={}",
                log.block_number, topics.len()
            );
            continue;
        }
        // topics[1] = pool address (left-padded)
        let pool_addr = Address::from_slice(&topics[1][12..32]);

        let creation_block = match log.block_number {
            Some(bn) => bn,
            None => {
                tracing::warn!("Curve PoolAdded log missing block_number, using end of range {}", to_block);
                to_block
            }
        };

        pools.push(DiscoveredPool {
            address: pool_addr,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 0,
            tick_spacing: None,
            dex_type: DexType::Curve,
            creation_block,
            pool_id: None,
            factory: None,
        });
    }

    Ok(pools)
}

pub async fn discover_pools(
    rpc: &RpcClient,
    cache: &SqliteStore,
    v2_factories: &[Address],
    v3_factories: &[Address],
    balancer_vault: Option<Address>,
    start_block: u64,
    to_block: u64,
    batch_size: u64,
    balancer_start_block: u64,
    v2_factory_fees: &[Option<u32>],
) -> anyhow::Result<usize> {
    let mut total = 0usize;

    for (i, &factory) in v2_factories.iter().enumerate() {
        let factory_fee = v2_factory_fees.get(i).copied().flatten();
        let cursor = cache
            .get_discovery_cursor(&factory)?
            .unwrap_or(start_block);
        if cursor > to_block {
            continue;
        }
        let mut current = cursor;
        while current <= to_block {
            let end = (current + batch_size - 1).min(to_block);
            let discovered = match discover_v2_pools(rpc, factory, current, end, factory_fee).await {
                Ok(pools) => pools,
                Err(e) => {
                    tracing::warn!("V2 discovery {factory} blocks {current}..{end} failed: {e}");
                    // Don't advance cursor — retry this batch next run
                    current = end + 1;
                    continue;
                }
            };

            for pool in &discovered {
                let info: PoolInfo = pool.clone().into();
                if let Err(e) = cache.put_discovered_pool(&info) {
                    tracing::warn!("Failed to cache pool {}: {}", info.address, e);
                }
            }
            total += discovered.len();
            cache.put_discovery_cursor(&factory, end)?;

            if end == to_block {
                break;
            }
            current = end + 1;
        }
    }

    for &factory in v3_factories {
        let cursor = cache
            .get_discovery_cursor(&factory)?
            .unwrap_or(start_block);
        if cursor > to_block {
            continue;
        }
        let mut current = cursor;
        while current <= to_block {
            let end = (current + batch_size - 1).min(to_block);
            let discovered = match discover_v3_pools(rpc, factory, current, end).await {
                Ok(pools) => pools,
                Err(e) => {
                    tracing::warn!("V3 discovery {factory} blocks {current}..{end} failed: {e}");
                    // Don't advance cursor — retry this batch next run
                    current = end + 1;
                    continue;
                }
            };

            for pool in &discovered {
                let info: PoolInfo = pool.clone().into();
                if let Err(e) = cache.put_discovered_pool(&info) {
                    tracing::warn!("Failed to cache pool {}: {}", info.address, e);
                }
            }
            total += discovered.len();
            cache.put_discovery_cursor(&factory, end)?;

            if end == to_block {
                break;
            }
            current = end + 1;
        }
    }

    // Balancer V2 vault discovery (optional)
    if let Some(vault) = balancer_vault {
        let cursor = cache
            .get_discovery_cursor(&vault)?
            .unwrap_or(balancer_start_block);
        if cursor <= to_block {
            let mut current = cursor;
            while current <= to_block {
                let end = (current + batch_size - 1).min(to_block);
                let discovered = match discover_balancer_pools(rpc, vault, current, end).await {
                    Ok(pools) => pools,
                    Err(e) => {
                        tracing::warn!("Balancer discovery {vault} blocks {current}..{end} failed: {e}");
                        current = end + 1;
                        continue;
                    }
                };

                for pool in &discovered {
                    let info: PoolInfo = pool.clone().into();
                    if let Err(e) = cache.put_discovered_pool(&info) {
                        tracing::warn!("Failed to cache Balancer pool {}: {}", info.address, e);
                    }
                }
                total += discovered.len();
                cache.put_discovery_cursor(&vault, end)?;

                if end == to_block {
                    break;
                }
                current = end + 1;
            }
        }
    }

    Ok(total)
}

/// Discover pools from factory events in a block range, without requiring a cache.
/// Returns all discovered pools without persisting them anywhere.
/// This is used for live discovery during backtest replay.
pub async fn discover_pools_in_range(
    rpc: &RpcClient,
    v2_factories: &[Address],
    v3_factories: &[Address],
    from_block: u64,
    to_block: u64,
    v2_factory_fees: &[Option<u32>],
    batch_size: u64,
) -> Vec<DiscoveredPool> {
    let mut all_pools = Vec::new();

    for (i, &factory) in v2_factories.iter().enumerate() {
        let factory_fee = v2_factory_fees.get(i).copied().flatten();
        let mut current = from_block;
        while current <= to_block {
            let end = (current + batch_size - 1).min(to_block);
            match discover_v2_pools(rpc, factory, current, end, factory_fee).await {
                Ok(pools) => all_pools.extend(pools),
                Err(e) => {
                    tracing::warn!("V2 discovery {factory} blocks {current}..{end} failed: {e}");
                }
            }
            if end == to_block {
                break;
            }
            current = end + 1;
        }
    }

    for &factory in v3_factories {
        let mut current = from_block;
        while current <= to_block {
            let end = (current + batch_size - 1).min(to_block);
            match discover_v3_pools(rpc, factory, current, end).await {
                Ok(pools) => all_pools.extend(pools),
                Err(e) => {
                    tracing::warn!("V3 discovery {factory} blocks {current}..{end} failed: {e}");
                }
            }
            if end == to_block {
                break;
            }
            current = end + 1;
        }
    }

    all_pools
}

/// Discover pools from DEX events in a block range, without needing factory addresses.
///
/// Scans for all DEX event topics (V2 Swap/Sync, V3 Swap/Mint/Burn, Curve, Balancer)
/// across all contracts, collects unique emitting pool addresses, and fetches pool
/// metadata via RPC. Returns discovered pools and the set of active block numbers.
///
/// This is the default discovery mode — it enables backtesting without any
/// pre-configured pool or factory knowledge. Only pools that were actually active
/// in the block range are discovered, minimizing RPC overhead.
///
/// By including V2 Sync and V3 Mint/Burn events alongside Swap events, this captures
/// pools that had liquidity changes even if no direct swap occurred, ensuring
/// arbitrage pathfinders can find all possible routes.
///
/// ## Type-specific behavior
///
/// | DEX | RPC calls needed per pool | Notes |
/// |-----|--------------------------|-------|
/// | V2  | `token0()`, `token1()`   | Fee defaults to 30 bps or `v2_fee_override` |
/// | V3  | `token0()`, `token1()`, `fee()`, `tickSpacing()` | Full metadata on-chain |
/// | Curve | (none — populated by `init_from_rpc`) | Requires `fetch_curve_state` during pool init |
/// | Balancer | (none for tokens — from event topics) | Requires vault from config for full state |
///
/// ## RPC requirements
/// This uses `eth_getLogs` with topic-only filters (no address restriction).
/// Some public RPC providers may reject this over large ranges. Use a private
/// archive node or reduce batch sizes if needed.
pub async fn discover_pools_from_swap_events(
    rpc: &RpcClient,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    v2_fee_override: Option<u32>,
    _balancer_vault: Option<Address>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let mut active_blocks = HashSet::new();
    // (pool_address, dex_type, optional_balancer_pool_id, optional_tokens_from_event)
    let mut pool_hits: HashMap<
        Address,
        (DexType, Option<[u8; 32]>, Option<(Address, Address)>),
    > = HashMap::new();

    // ── Phase 1: Scan for DEX events (topic-only) ──
    let dex_topics = vec![
        topics::V2_SWAP,
        topics::V2_SYNC,
        topics::V3_SWAP,
        *topics::V3_MINT,
        topics::V3_BURN,
        *topics::CURVE_TOKEN_EXCHANGE,
        *topics::CURVE_V2_TOKEN_EXCHANGE,
        *topics::BALANCER_SWAP,
    ];

    let mut current = from_block;
    while current <= to_block {
        let batch_end = (current + batch_size - 1).min(to_block);
        // Fast path: V2/V3 Swap, V2 Sync, V3 Mint/Burn (covers >95% of DEX activity)
        let fast_topics: Vec<B256> = vec![
            topics::V2_SWAP,
            topics::V2_SYNC,
            topics::V3_SWAP,
            *topics::V3_MINT,
            topics::V3_BURN,
        ];
        let fast_filter = Filter::new()
            .event_signature(fast_topics)
            .from_block(current)
            .to_block(batch_end);

        let fast_logs = rpc.get_logs(&fast_filter).await;
        match fast_logs {
            Ok(logs) => {
                for log in &logs {
                    if let Some(bn) = log.block_number {
                        active_blocks.insert(bn);
                    }
                    let addr = log.address();
                    let topic0 = log.topics()[0];
                    let dex_type = if topic0 == topics::V2_SWAP || topic0 == topics::V2_SYNC {
                        DexType::UniswapV2
                    } else {
                        DexType::UniswapV3
                    };
                    pool_hits.entry(addr).or_insert((dex_type, None, None));
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Swap scan fast path failed for blocks {current}..{batch_end}: {e:#}. \
                     Trying full topic set."
                );
                // Fall back to the full topic set (including Curve/Balancer)
                let full_filter = Filter::new()
                    .event_signature(dex_topics.clone())
                    .from_block(current)
                    .to_block(batch_end);
                match rpc.get_logs(&full_filter).await {
                    Ok(logs) => {
                        for log in &logs {
                            if let Some(bn) = log.block_number {
                                active_blocks.insert(bn);
                            }
                            let addr = log.address();
                            let topic0 = log.topics()[0];
                            if topic0 == topics::V2_SWAP || topic0 == topics::V2_SYNC {
                                pool_hits.entry(addr).or_insert((DexType::UniswapV2, None, None));
                            } else if topic0 == topics::V3_SWAP
                                || topic0 == *topics::V3_MINT
                                || topic0 == topics::V3_BURN
                            {
                                pool_hits.entry(addr).or_insert((DexType::UniswapV3, None, None));
                            } else if topic0 == *topics::CURVE_TOKEN_EXCHANGE
                                || topic0 == *topics::CURVE_V2_TOKEN_EXCHANGE
                            {
                                pool_hits.entry(addr).or_insert((DexType::Curve, None, None));
                            } else if topic0 == *topics::BALANCER_SWAP {
                                // Balancer: pool_id and token addresses are in the topics
                                let topics = log.topics();
                                if topics.len() >= 4 {
                                    let mut pool_id = [0u8; 32];
                                    pool_id.copy_from_slice(topics[1].as_slice());
                                    let token_in = Address::from_slice(&topics[2][12..]);
                                    let token_out = Address::from_slice(&topics[3][12..]);
                                    pool_hits.entry(addr).or_insert((
                                        DexType::Balancer,
                                        Some(pool_id),
                                        Some((token_in, token_out)),
                                    ));
                                }
                            }
                        }
                    }
                    Err(e2) => {
                        tracing::warn!(
                            "Swap scan full topic set also failed for blocks {current}..{batch_end}: {e2:#}. \
                             Skipping batch."
                        );
                    }
                }
            }
        }

        if batch_end == to_block {
            break;
        }
        current = batch_end + 1;
    }

    if pool_hits.is_empty() {
        return Ok((Vec::new(), active_blocks));
    }

    tracing::info!(
        "Swap event scan: found {} unique pool addresses, {} active blocks",
        pool_hits.len(),
        active_blocks.len(),
    );

    // ── Phase 2: Fetch pool metadata ──
    // Standard ERC-20 / Uniswap selectors for eth_call
    let token0_selector = Bytes::from_static(&[0x0d, 0xfe, 0x16, 0x81]); // token0()
    let token1_selector = Bytes::from_static(&[0xd2, 0x12, 0x20, 0xa7]); // token1()
    let fee_selector = Bytes::from_static(&[0xdd, 0xca, 0x3f, 0x43]);    // fee() — V3 only
    let tick_spacing_selector = Bytes::from_static(&[0x37, 0xcf, 0xda, 0xca]); // tickSpacing() — V3 only

    let ref_block = to_block.min(from_block + 1_000_000);

    type FetchTask = Pin<Box<dyn Future<Output = (Address, DexType, Option<Address>, Option<Address>, Option<u32>, Option<u32>)> + Send>>;

    let mut discovered_pools = Vec::new();
    let mut fetch_tasks: Vec<FetchTask> = Vec::new();

    for (addr, (dex_type, _balancer_pool_id, balancer_tokens)) in pool_hits.iter() {
        match dex_type {
            DexType::UniswapV2 => {
                let rpc = rpc.clone();
                let addr = *addr;
                let sel0 = token0_selector.clone();
                let sel1 = token1_selector.clone();
                fetch_tasks.push(Box::pin(async move {
                    let token0 = rpc.call(addr, sel0, ref_block).await.ok()
                        .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])));
                    let token1 = rpc.call(addr, sel1, ref_block).await.ok()
                        .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])));
                    (addr, DexType::UniswapV2, token0, token1, None, None)
                }));
            }
            DexType::UniswapV3 => {
                let rpc = rpc.clone();
                let addr = *addr;
                let sel0 = token0_selector.clone();
                let sel1 = token1_selector.clone();
                let sel_fee = fee_selector.clone();
                let sel_ts = tick_spacing_selector.clone();
                fetch_tasks.push(Box::pin(async move {
                    let (token0, token1, fee, tick_spacing) = futures::future::join4(
                        async {
                            rpc.call(addr, sel0, ref_block).await.ok()
                                .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])))
                        },
                        async {
                            rpc.call(addr, sel1, ref_block).await.ok()
                                .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])))
                        },
                        async {
                            rpc.call(addr, sel_fee, ref_block).await.ok()
                                .and_then(|b| (b.len() >= 32).then(|| {
                                    u32::from_be_bytes([b[28], b[29], b[30], b[31]])
                                }))
                        },
                        async {
                            rpc.call(addr, sel_ts, ref_block).await.ok()
                                .and_then(|b| (b.len() >= 32).then(|| {
                                    let mut ts = [0u8; 4];
                                    ts.copy_from_slice(&b[28..32]);
                                    i32::from_be_bytes(ts) as u32
                                }))
                        },
                    ).await;
                    (addr, DexType::UniswapV3, token0, token1, fee, tick_spacing)
                }));
            }
            DexType::Curve | DexType::Balancer => {
                // Curve: tokens are discovered by fetch_curve_state during pool init.
                // Balancer: tokens from event topics (but need vault for full init).
                let (t0, t1) = balancer_tokens.unwrap_or((Address::ZERO, Address::ZERO));
                let addr = *addr;
                let dt = *dex_type;
                fetch_tasks.push(Box::pin(async move {
                    (addr, dt, Some(t0), Some(t1), None, None)
                }));
            }
        }
    }

    // Phase 3: Await all metadata fetches
    use futures::future::join_all;
    let results = join_all(fetch_tasks).await;

    for (addr, dex_type, token0_opt, token1_opt, fee_opt, tick_spacing) in results {
        let token0 = token0_opt.unwrap_or(Address::ZERO);
        let token1 = token1_opt.unwrap_or(Address::ZERO);
        let fee = match dex_type {
            DexType::UniswapV2 => v2_fee_override.unwrap_or(30),
            DexType::UniswapV3 => fee_opt.unwrap_or(3000),
            DexType::Curve | DexType::Balancer => fee_opt.unwrap_or(0),
        };
        let pool_id = pool_hits.get(&addr).and_then(|(_, pid, _)| *pid);

        discovered_pools.push(DiscoveredPool {
            address: addr,
            token0,
            token1,
            fee,
            tick_spacing: tick_spacing.map(|ts| ts as i32),
            dex_type,
            creation_block: 0, // unknown — set to 0, init_from_rpc handles it
            pool_id,
            factory: None,
        });
    }

    tracing::info!(
        "Swap discovery: resolved {} pools ({} unique addresses)",
        discovered_pools.len(),
        pool_hits.len(),
    );

    Ok((discovered_pools, active_blocks))
}

/// Discover pools from Swap events and save them to the cache.
/// This is the default discovery mode in `mev-scout run`.
pub async fn discover_and_cache_from_swaps(
    rpc: &RpcClient,
    cache: &SqliteStore,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    v2_fee_override: Option<u32>,
    balancer_vault: Option<Address>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let (pools, active_blocks) = discover_pools_from_swap_events(
        rpc,
        from_block,
        to_block,
        batch_size,
        v2_fee_override,
        balancer_vault,
    )
    .await?;

    let pool_count = pools.len();
    for pool in &pools {
        let info: PoolInfo = pool.clone().into();
        if let Err(e) = cache.put_discovered_pool(&info) {
            tracing::warn!("Failed to cache pool {}: {}", pool.address, e);
        }
    }
    if pool_count > 0 {
        tracing::info!("Cached {} pools from DEX event discovery", pool_count);
    }

    Ok((pools, active_blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    // Use the actual computed topics from the statics
    #[test]
    fn test_v2_pair_created_topic() {
        let expected = keccak256(b"PairCreated(address,address,address,uint256)");
        assert_eq!(*V2_PAIR_CREATED_TOPIC, expected);
    }

    #[test]
    fn test_v3_pool_created_topic() {
        let expected = keccak256(b"PoolCreated(address,address,uint24,int24,address)");
        assert_eq!(*V3_POOL_CREATED_TOPIC, expected);
    }

    #[test]
    fn test_balancer_pool_registered_topic() {
        let expected = keccak256(b"PoolRegistered(bytes32,address,uint8)");
        assert_eq!(*BALANCER_POOL_REGISTERED_TOPIC, expected);
    }

    #[test]
    fn test_discovered_pool_conversion() {
        let dp = DiscoveredPool {
            address: address!("cafe000000000000000000000000000000000001"),
            token0: address!("aaaa0000000000000000000000000000000000aa"),
            token1: address!("bbbb0000000000000000000000000000000000bb"),
            fee: 3000,
            tick_spacing: Some(60),
            dex_type: DexType::UniswapV3,
            creation_block: 0,
            pool_id: None,
            factory: None,
        };
        let info: PoolInfo = dp.into();
        assert_eq!(info.fee, 3000);
        assert_eq!(info.dex_type, DexType::UniswapV3);
        assert_eq!(info.tick_spacing, Some(60));
        assert!(info.pool_id.is_none());
    }

    #[test]
    fn test_discovered_pool_conversion_v2() {
        let dp = DiscoveredPool {
            address: address!("cafe000000000000000000000000000000000002"),
            token0: address!("aaaa0000000000000000000000000000000000aa"),
            token1: address!("bbbb0000000000000000000000000000000000bb"),
            fee: 0,
            tick_spacing: None,
            dex_type: DexType::UniswapV2,
            creation_block: 0,
            pool_id: None,
            factory: None,
        };
        let info: PoolInfo = dp.into();
        assert_eq!(info.dex_type, DexType::UniswapV2);
        assert!(info.tick_spacing.is_none());
        assert!(info.pool_id.is_none());
    }

    #[test]
    fn test_discovered_pool_conversion_balancer() {
        let pool_id = [42u8; 32];
        let dp = DiscoveredPool {
            address: address!("cafe000000000000000000000000000000000003"),
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 0,
            tick_spacing: None,
            dex_type: DexType::Balancer,
            creation_block: 100,
            pool_id: Some(pool_id),
            factory: None,
        };
        let info: PoolInfo = dp.into();
        assert_eq!(info.dex_type, DexType::Balancer);
        assert_eq!(info.pool_id, Some(pool_id));
        assert_eq!(info.creation_block, 100);
    }
}
