//! Pool discovery — scans chain event logs to find and register new DEX pools.
//!
//! The unified `discover_pools` function scans both DEX activity events
//! (Swap, Sync, Mint, Burn, TokenExchange, BalancerSwap) for active pools
//! AND factory creation events (PairCreated, PoolCreated, PoolRegistered,
//! PoolAdded) for pools created in the range. This ensures comprehensive
//! coverage for backtesting recent periods.

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
use crate::pipeline::topics;

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

/// Unified pool discovery — scans both DEX activity events and factory
/// creation events (if factory addresses provided).
///
/// # DEX activity (always)
/// Scans for Swap/Sync/Mint/Burn/TokenExchange/BalancerSwap events across
/// all contracts, collecting unique emitting pool addresses. This captures
/// all pools that were *active* in the block range regardless of when they
/// were created.
///
/// # Factory creation (optional)
/// If `v2_factories`/`v3_factories`/`balancer_vault`/`curve_registry` are
/// provided, also scans for PairCreated/PoolCreated/PoolRegistered/PoolAdded
/// events. This captures pools created in the range that may have had zero
/// activity yet.
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
/// Pools discovered via factory creation events carry `creation_block` and
/// `factory` metadata that helps `init_pools` skip non-existent pools.
pub async fn discover_pools(
    rpc: &RpcClient,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    v2_fee_override: Option<u32>,
    balancer_vault: Option<Address>,
    v2_factories: Option<&[Address]>,
    v3_factories: Option<&[Address]>,
    _v2_factory_fees: Option<&[Option<u32>]>,
    curve_registry: Option<Address>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let mut active_blocks = HashSet::new();
    // Pools discovered via DEX activity events (may need metadata fetch)
    let mut pool_hits: HashMap<
        Address,
        (DexType, Option<[u8; 32]>, Option<(Address, Address)>),
    > = HashMap::new();
    // Pools with full metadata from factory creation events (skip RPC fetch)
    let mut factory_pools: HashMap<Address, DiscoveredPool> = HashMap::new();

    // ── Phase 1: Scan events (both DEX activity and factory creation) ──
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

        // ── DEX activity scan ──
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

        match rpc.get_logs(&fast_filter).await {
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

        // ── V2 factory creation scan (PairCreated) ──
        if let Some(factories) = v2_factories {
            if !factories.is_empty() {
                let v2_filter = Filter::new()
                    .address(factories.to_vec())
                    .event_signature(*V2_PAIR_CREATED_TOPIC)
                    .from_block(current)
                    .to_block(batch_end);
                if let Ok(logs) = rpc.get_logs(&v2_filter).await {
                    for log in &logs {
                        if let Some(bn) = log.block_number {
                            active_blocks.insert(bn);
                        }
                        let log_data = log.data();
                        let topics = log.topics();
                        if log_data.data.len() < 64 || topics.len() < 3 {
                            continue;
                        }
                        let addr = Address::from_slice(&log_data.data[12..32]);
                        if factory_pools.contains_key(&addr) {
                            continue;
                        }
                        let token0 = Address::from_slice(&topics[1][12..]);
                        let token1 = Address::from_slice(&topics[2][12..]);
                        let creation_block = log.block_number.unwrap_or(to_block);
                        factory_pools.insert(addr, DiscoveredPool {
                            address: addr,
                            token0,
                            token1,
                            fee: v2_fee_override.unwrap_or(30),
                            tick_spacing: None,
                            dex_type: DexType::UniswapV2,
                            creation_block,
                            pool_id: None,
                            factory: Some(log.address()),
                        });
                    }
                }
            }
        }

        // ── V3 factory creation scan (PoolCreated) ──
        if let Some(factories) = v3_factories {
            if !factories.is_empty() {
                let v3_filter = Filter::new()
                    .address(factories.to_vec())
                    .event_signature(*V3_POOL_CREATED_TOPIC)
                    .from_block(current)
                    .to_block(batch_end);
                if let Ok(logs) = rpc.get_logs(&v3_filter).await {
                    for log in &logs {
                        if let Some(bn) = log.block_number {
                            active_blocks.insert(bn);
                        }
                        let log_data = log.data();
                        let topics = log.topics();
                        if log_data.data.len() < 64 || topics.len() < 4 {
                            continue;
                        }
                        let pool_addr = Address::from_slice(&log_data.data[44..64]);
                        if factory_pools.contains_key(&pool_addr) {
                            continue;
                        }
                        let token0 = Address::from_slice(&topics[1][12..]);
                        let token1 = Address::from_slice(&topics[2][12..]);
                        let fee = u32::from_be_bytes([
                            topics[3][28], topics[3][29], topics[3][30], topics[3][31],
                        ]);
                        let tick_spacing = {
                            let mut ts_bytes = [0u8; 4];
                            ts_bytes.copy_from_slice(&log_data.data[28..32]);
                            Some(i32::from_be_bytes(ts_bytes))
                        };
                        let creation_block = log.block_number.unwrap_or(to_block);
                        factory_pools.insert(pool_addr, DiscoveredPool {
                            address: pool_addr,
                            token0,
                            token1,
                            fee,
                            tick_spacing,
                            dex_type: DexType::UniswapV3,
                            creation_block,
                            pool_id: None,
                            factory: Some(log.address()),
                        });
                    }
                }
            }
        }

        // ── Balancer vault scan (PoolRegistered) ──
        if let Some(vault) = balancer_vault {
            let bal_filter = Filter::new()
                .address(vault)
                .event_signature(*BALANCER_POOL_REGISTERED_TOPIC)
                .from_block(current)
                .to_block(batch_end);
            if let Ok(logs) = rpc.get_logs(&bal_filter).await {
                for log in &logs {
                    if let Some(bn) = log.block_number {
                        active_blocks.insert(bn);
                    }
                    let topics = log.topics();
                    if topics.len() < 4 {
                        continue;
                    }
                    let pool_type = topics[3][31];
                    if pool_type > 1 {
                        continue;
                    }
                    let mut pool_id = [0u8; 32];
                    pool_id.copy_from_slice(topics[1].as_slice());
                    let pool_addr = Address::from_slice(&topics[2][12..32]);
                    let creation_block = log.block_number.unwrap_or(to_block);
                    // Add to pool_hits (needs RPC for tokens/fee)
                    pool_hits.entry(pool_addr).or_insert((
                        DexType::Balancer,
                        Some(pool_id),
                        None,
                    ));
                    // Also add to factory_pools with partial metadata
                    factory_pools.entry(pool_addr).or_insert(DiscoveredPool {
                        address: pool_addr,
                        token0: Address::ZERO,
                        token1: Address::ZERO,
                        fee: 0,
                        tick_spacing: None,
                        dex_type: DexType::Balancer,
                        creation_block,
                        pool_id: Some(pool_id),
                        factory: Some(vault),
                    });
                }
            }
        }

        // ── Curve registry scan (PoolAdded) ──
        if let Some(registry) = curve_registry {
            let curve_filter = Filter::new()
                .address(registry)
                .event_signature(*CURVE_POOL_ADDED_TOPIC)
                .from_block(current)
                .to_block(batch_end);
            if let Ok(logs) = rpc.get_logs(&curve_filter).await {
                for log in &logs {
                    if let Some(bn) = log.block_number {
                        active_blocks.insert(bn);
                    }
                    let topics = log.topics();
                    if topics.len() < 2 {
                        continue;
                    }
                    let pool_addr = Address::from_slice(&topics[1][12..32]);
                    let creation_block = log.block_number.unwrap_or(to_block);
                    // Add to pool_hits (needs RPC for tokens)
                    pool_hits.entry(pool_addr).or_insert((DexType::Curve, None, None));
                    // Also add to factory_pools with partial metadata
                    factory_pools.entry(pool_addr).or_insert(DiscoveredPool {
                        address: pool_addr,
                        token0: Address::ZERO,
                        token1: Address::ZERO,
                        fee: 0,
                        tick_spacing: None,
                        dex_type: DexType::Curve,
                        creation_block,
                        pool_id: None,
                        factory: Some(registry),
                    });
                }
            }
        }

        if batch_end == to_block {
            break;
        }
        current = batch_end + 1;
    }

    if pool_hits.is_empty() && factory_pools.is_empty() {
        return Ok((Vec::new(), active_blocks));
    }

    tracing::info!(
        "Event scan: found {} unique pool addresses from DEX events, {} from factory events, {} active blocks",
        pool_hits.len(),
        factory_pools.len(),
        active_blocks.len(),
    );

    // ── Phase 2: Fetch pool metadata for DEX-discovered pools ──
    let token0_selector = Bytes::from_static(&[0x0d, 0xfe, 0x16, 0x81]);
    let token1_selector = Bytes::from_static(&[0xd2, 0x12, 0x20, 0xa7]);
    let fee_selector = Bytes::from_static(&[0xdd, 0xca, 0x3f, 0x43]);
    let tick_spacing_selector = Bytes::from_static(&[0x37, 0xcf, 0xda, 0xca]);

    let ref_block = to_block.min(from_block + 1_000_000);

    type FetchTask = Pin<Box<dyn Future<Output = (Address, DexType, Option<Address>, Option<Address>, Option<u32>, Option<u32>)> + Send>>;

    let mut discovered_pools = Vec::new();
    let mut fetch_tasks: Vec<FetchTask> = Vec::new();

    for (addr, (dex_type, _balancer_pool_id, balancer_tokens)) in pool_hits.iter() {
        // Skip V2/V3 pools already fully resolved via factory events
        if let Some(fp) = factory_pools.get(addr) {
            match fp.dex_type {
                DexType::UniswapV2 | DexType::UniswapV3 => continue,
                _ => {}
            }
        }
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
                let (t0, t1) = balancer_tokens.unwrap_or((Address::ZERO, Address::ZERO));
                let addr = *addr;
                let dt = *dex_type;
                fetch_tasks.push(Box::pin(async move {
                    (addr, dt, Some(t0), Some(t1), None, None)
                }));
            }
        }
    }

    use futures::future::join_all;
    let results = join_all(fetch_tasks).await;

    // ── Phase 3: Build output ──
    // First, add all factory-discovered pools (they have creation_block, factory, etc.)
    for (_, dp) in factory_pools.drain() {
        discovered_pools.push(dp);
    }

    // Then, add metadata-fetched pools not already resolved
    for (addr, dex_type, token0_opt, token1_opt, fee_opt, tick_spacing) in results {
        if discovered_pools.iter().any(|p| p.address == addr) {
            continue;
        }
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
            creation_block: 0,
            pool_id,
            factory: None,
        });
    }

    tracing::info!(
        "Discovery complete: resolved {} pools",
        discovered_pools.len(),
    );

    Ok((discovered_pools, active_blocks))
}

/// Discover pools and save them to the cache.
/// This is the standard discovery mode used by `mev-scout run`.
pub async fn discover_and_cache(
    rpc: &RpcClient,
    cache: &SqliteStore,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    v2_fee_override: Option<u32>,
    balancer_vault: Option<Address>,
    v2_factories: Option<&[Address]>,
    v3_factories: Option<&[Address]>,
    _v2_factory_fees: Option<&[Option<u32>]>,
    curve_registry: Option<Address>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let (pools, active_blocks) = discover_pools(
        rpc,
        from_block,
        to_block,
        batch_size,
        v2_fee_override,
        balancer_vault,
        v2_factories,
        v3_factories,
        _v2_factory_fees,
        curve_registry,
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
        tracing::info!("Cached {} pools from discovery", pool_count);
    }

    Ok((pools, active_blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

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
