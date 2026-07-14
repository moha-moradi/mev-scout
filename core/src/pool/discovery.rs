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
use std::time::Duration;

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

// Solidly-style PairCreated (bool stable, address pair) — Velodrome, Aerodrome, Equalizer, Thena
pub static SOLIDLY_PAIR_CREATED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"PairCreated(address,address,bool,address)")
});

// Camelot PairCreated (address pair, uint256 fee, bool stable)
pub static CAMELOT_PAIR_CREATED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"PairCreated(address,address,address,uint256,bool)")
});

// Curve exchange_underlying emits separate event variants
pub static CURVE_TOKEN_EXCHANGE_UNDERLYING: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"TokenExchangeUnderlying(address,int128,uint256,int128,uint256)")
});
pub static CURVE_V2_TOKEN_EXCHANGE_UNDERLYING: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"TokenExchangeUnderlying(address,int128,uint256,int128,uint256,uint256)")
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

/// Configuration for pool discovery parameters.
pub struct DiscoveryConfig<'a> {
    pub batch_size: u64,
    pub v2_fee_override: Option<u32>,
    pub balancer_vault: Option<Address>,
    pub v2_factories: Option<&'a [Address]>,
    pub v3_factories: Option<&'a [Address]>,
    pub curve_registry: Option<Address>,
    pub solidly_factories: Option<&'a [Address]>,
    pub camelot_factories: Option<&'a [Address]>,
}

// ── Helper: retry wrapper for eth_getLogs ──

const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 1_000;

async fn get_logs_with_retry(
    rpc: &RpcClient,
    filter: &Filter,
    batch_start: u64,
    batch_end: u64,
) -> anyhow::Result<Vec<alloy::rpc::types::Log>> {
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match rpc.get_logs(filter).await {
            Ok(logs) => return Ok(logs),
            Err(e) => {
                if attempt < MAX_RETRIES - 1 {
                    let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                    tracing::warn!(
                        "get_logs failed for {batch_start}..{batch_end} (attempt {}/{}): {:#}. \
                         Retrying in {}ms...",
                        attempt + 1,
                        MAX_RETRIES,
                        e,
                        delay
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap())
}

// ── Helper: classify a DEX event log ──

/// Returns `(dex_type, pool_id, tokens)` for a recognized DEX event log.
fn classify_dex_event(
    log: &alloy::rpc::types::Log,
) -> Option<(DexType, Option<[u8; 32]>, Option<(Address, Address)>)> {
    let topic0 = log.topics()[0];
    if topic0 == topics::V2_SWAP || topic0 == topics::V2_SYNC {
        return Some((DexType::UniswapV2, None, None));
    }
    if topic0 == *topics::DODO_SWAP {
        return Some((DexType::Dodo, None, None));
    }
    if topic0 == *topics::CLIPPER_SWAPPED {
        let topics = log.topics();
        let tokens = if topics.len() >= 4 {
            let token_in = Address::from_slice(&topics[3][12..]);
            let token_out = if log.data().data.len() >= 32 {
                Address::from_slice(&log.data().data[12..32])
            } else {
                Address::ZERO
            };
            Some((token_in, token_out))
        } else {
            None
        };
        return Some((DexType::Clipper, None, tokens));
    }
    if topic0 == topics::V3_SWAP
        || topic0 == *topics::V3_MINT
        || topic0 == topics::V3_BURN
    {
        return Some((DexType::UniswapV3, None, None));
    }
    if topic0 == *topics::CURVE_TOKEN_EXCHANGE
        || topic0 == *topics::CURVE_V2_TOKEN_EXCHANGE
        || topic0 == *topics::CURVE_TOKEN_EXCHANGE_UNDERLYING
        || topic0 == *topics::CURVE_V2_TOKEN_EXCHANGE_UNDERLYING
    {
        return Some((DexType::Curve, None, None));
    }
    if topic0 == *topics::BALANCER_SWAP {
        let topics = log.topics();
        if topics.len() >= 4 {
            let mut pool_id = [0u8; 32];
            pool_id.copy_from_slice(topics[1].as_slice());
            let token_in = Address::from_slice(&topics[2][12..]);
            let token_out = Address::from_slice(&topics[3][12..]);
            return Some((DexType::Balancer, Some(pool_id), Some((token_in, token_out))));
        }
        return Some((DexType::Balancer, None, None));
    }
    None
}

// ── Helper: scan factory creation events ──

async fn scan_factory_creation_events(
    rpc: &RpcClient,
    factories: &[Address],
    topic: B256,
    from_block: u64,
    to_block: u64,
    active_blocks: &mut HashSet<u64>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    decode_fn: impl Fn(&alloy::rpc::types::Log) -> Option<(Address, DiscoveredPool)>,
) {
    if factories.is_empty() {
        return;
    }
    let filter = Filter::new()
        .address(factories.to_vec())
        .event_signature(topic)
        .from_block(from_block)
        .to_block(to_block);
    match get_logs_with_retry(rpc, &filter, from_block, to_block).await {
        Ok(logs) => {
            for log in &logs {
                if let Some(bn) = log.block_number {
                    active_blocks.insert(bn);
                }
                if let Some((addr, pool)) = decode_fn(log) {
                    factory_pools.entry(addr).or_insert(pool);
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "Factory scan failed for {from_block}..{to_block} (topic {topic}): {e:#}"
            );
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
    config: &DiscoveryConfig<'_>,
    on_batch: Option<&dyn Fn()>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let mut active_blocks = HashSet::new();
    // Pools discovered via DEX activity events (may need metadata fetch)
    // Value: (dex_type, pool_id, tokens, first_seen_block)
    let mut pool_hits: HashMap<
        Address,
        (DexType, Option<[u8; 32]>, Option<(Address, Address)>, u64),
    > = HashMap::new();
    // Pools with full metadata from factory creation events (skip RPC fetch)
    let mut factory_pools: HashMap<Address, DiscoveredPool> = HashMap::new();

    // Pre-build DEX activity topic lists
    let fast_topics: Vec<B256> = vec![
        topics::V2_SWAP,
        topics::V2_SYNC,
        topics::V3_SWAP,
        *topics::V3_MINT,
        topics::V3_BURN,
        *topics::DODO_SWAP,
        *topics::CLIPPER_SWAPPED,
    ];
    let all_dex_topics: Vec<B256> = vec![
        topics::V2_SWAP,
        topics::V2_SYNC,
        topics::V3_SWAP,
        *topics::V3_MINT,
        topics::V3_BURN,
        *topics::CURVE_TOKEN_EXCHANGE,
        *topics::CURVE_V2_TOKEN_EXCHANGE,
        *topics::CURVE_TOKEN_EXCHANGE_UNDERLYING,
        *topics::CURVE_V2_TOKEN_EXCHANGE_UNDERLYING,
        *topics::BALANCER_SWAP,
        *topics::DODO_SWAP,
        *topics::CLIPPER_SWAPPED,
    ];

    let mut current = from_block;
    while current <= to_block {
        let batch_end = (current + config.batch_size - 1).min(to_block);

        // ── Provider health check (wait if all providers in cooldown) ──
        if !rpc.has_healthy_providers().await {
            tracing::warn!(
                "All RPC providers in cooldown before batch {current}..{batch_end}, waiting 5s..."
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        // ── DEX activity scan (fast path: skip Curve/Balancer topics) ──
        let fast_filter = Filter::new()
            .event_signature(fast_topics.clone())
            .from_block(current)
            .to_block(batch_end);

        match rpc.get_logs(&fast_filter).await {
            Ok(logs) => {
                for log in &logs {
                    if let Some(bn) = log.block_number {
                        active_blocks.insert(bn);
                    }
                    let addr = log.address();
                    if let Some((dex_type, pool_id, tokens)) = classify_dex_event(log) {
                        let block_num = log.block_number.unwrap_or(0);
                        let entry = pool_hits.entry(addr).or_insert((dex_type, pool_id, tokens, block_num));
                        // Track earliest block for creation_block
                        if block_num > 0 && block_num < entry.3 {
                            entry.3 = block_num;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Swap scan fast path failed for blocks {current}..{batch_end}: {e:#}. \
                     Trying full topic set."
                );
                let full_filter = Filter::new()
                    .event_signature(all_dex_topics.clone())
                    .from_block(current)
                    .to_block(batch_end);
                match rpc.get_logs(&full_filter).await {
                    Ok(logs) => {
                        for log in &logs {
                            if let Some(bn) = log.block_number {
                                active_blocks.insert(bn);
                            }
                            let addr = log.address();
                            if let Some((dex_type, pool_id, tokens)) = classify_dex_event(log) {
                                let block_num = log.block_number.unwrap_or(0);
                                let entry = pool_hits.entry(addr).or_insert((dex_type, pool_id, tokens, block_num));
                                if block_num > 0 && block_num < entry.3 {
                                    entry.3 = block_num;
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
        if let Some(factories) = config.v2_factories {
            let fee = config.v2_fee_override.unwrap_or(30);
            scan_factory_creation_events(
                rpc, factories, *V2_PAIR_CREATED_TOPIC, current, batch_end,
                &mut active_blocks, &mut factory_pools,
                |log| {
                    let log_data = log.data();
                    let topics = log.topics();
                    if log_data.data.len() < 64 || topics.len() < 3 {
                        return None;
                    }
                    let addr = Address::from_slice(&log_data.data[12..32]);
                    let token0 = Address::from_slice(&topics[1][12..]);
                    let token1 = Address::from_slice(&topics[2][12..]);
                    let creation_block = log.block_number.unwrap_or(0);
                    Some((addr, DiscoveredPool {
                        address: addr, token0, token1,
                        fee, tick_spacing: None, dex_type: DexType::UniswapV2,
                        creation_block, pool_id: None, factory: Some(log.address()),
                    }))
                },
            ).await;
        }

        // ── V3 factory creation scan (PoolCreated) ──
        if let Some(factories) = config.v3_factories {
            scan_factory_creation_events(
                rpc, factories, *V3_POOL_CREATED_TOPIC, current, batch_end,
                &mut active_blocks, &mut factory_pools,
                |log| {
                    let log_data = log.data();
                    let topics = log.topics();
                    if log_data.data.len() < 64 || topics.len() < 4 {
                        return None;
                    }
                    let pool_addr = Address::from_slice(&log_data.data[44..64]);
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
                    let creation_block = log.block_number.unwrap_or(0);
                    Some((pool_addr, DiscoveredPool {
                        address: pool_addr, token0, token1,
                        fee, tick_spacing, dex_type: DexType::UniswapV3,
                        creation_block, pool_id: None, factory: Some(log.address()),
                    }))
                },
            ).await;
        }

        // ── Balancer vault scan (PoolRegistered) ──
        if let Some(vault) = config.balancer_vault {
            let filter = Filter::new()
                .address(vault)
                .event_signature(*BALANCER_POOL_REGISTERED_TOPIC)
                .from_block(current)
                .to_block(batch_end);
            match get_logs_with_retry(rpc, &filter, current, batch_end).await {
                Ok(logs) => {
                    for log in &logs {
                        if let Some(bn) = log.block_number {
                            active_blocks.insert(bn);
                        }
                        let topics = log.topics();
                        if topics.len() < 4 {
                            continue;
                        }
                        let pool_type = topics[3][31];
                        // Weighted (0), Weighted2Tokens (1), ComposableStable (3) are valid.
                        if pool_type == 2 || pool_type > 3 {
                            continue;
                        }
                        let mut pool_id = [0u8; 32];
                        pool_id.copy_from_slice(topics[1].as_slice());
                        let pool_addr = Address::from_slice(&topics[2][12..32]);
                        let creation_block = log.block_number.unwrap_or(0);
                        pool_hits.entry(pool_addr).or_insert((
                            DexType::Balancer, Some(pool_id), None, creation_block,
                        ));
                        factory_pools.entry(pool_addr).or_insert(DiscoveredPool {
                            address: pool_addr,
                            token0: Address::ZERO, token1: Address::ZERO,
                            fee: 0, tick_spacing: None,
                            dex_type: DexType::Balancer,
                            creation_block, pool_id: Some(pool_id),
                            factory: Some(vault),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Balancer vault scan failed for {current}..{batch_end}: {e:#}"
                    );
                }
            }
        }

        // ── Curve registry scan (PoolAdded) ──
        if let Some(registry) = config.curve_registry {
            let filter = Filter::new()
                .address(registry)
                .event_signature(*CURVE_POOL_ADDED_TOPIC)
                .from_block(current)
                .to_block(batch_end);
            match get_logs_with_retry(rpc, &filter, current, batch_end).await {
                Ok(logs) => {
                    for log in &logs {
                        if let Some(bn) = log.block_number {
                            active_blocks.insert(bn);
                        }
                        let topics = log.topics();
                        if topics.len() < 2 {
                            continue;
                        }
                        let pool_addr = Address::from_slice(&topics[1][12..32]);
                        let creation_block = log.block_number.unwrap_or(0);
                        pool_hits.entry(pool_addr).or_insert((
                            DexType::Curve, None, None, creation_block,
                        ));
                        factory_pools.entry(pool_addr).or_insert(DiscoveredPool {
                            address: pool_addr,
                            token0: Address::ZERO, token1: Address::ZERO,
                            fee: 0, tick_spacing: None,
                            dex_type: DexType::Curve,
                            creation_block, pool_id: None, factory: Some(registry),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Curve registry scan failed for {current}..{batch_end}: {e:#}"
                    );
                }
            }
        }

        // ── Solidly factory creation scan ──
        if let Some(factories) = config.solidly_factories {
            let fee = config.v2_fee_override.unwrap_or(30);
            scan_factory_creation_events(
                rpc, factories, *SOLIDLY_PAIR_CREATED_TOPIC, current, batch_end,
                &mut active_blocks, &mut factory_pools,
                |log| {
                    let log_data = log.data();
                    let topics = log.topics();
                    if log_data.data.len() < 64 || topics.len() < 3 {
                        return None;
                    }
                    let pair_addr = Address::from_slice(&log_data.data[44..64]);
                    let token0 = Address::from_slice(&topics[1][12..]);
                    let token1 = Address::from_slice(&topics[2][12..]);
                    let creation_block = log.block_number.unwrap_or(0);
                    Some((pair_addr, DiscoveredPool {
                        address: pair_addr, token0, token1,
                        fee, tick_spacing: None, dex_type: DexType::Solidly,
                        creation_block, pool_id: None, factory: Some(log.address()),
                    }))
                },
            ).await;
        }

        // ── Camelot factory creation scan ──
        if let Some(factories) = config.camelot_factories {
            scan_factory_creation_events(
                rpc, factories, *CAMELOT_PAIR_CREATED_TOPIC, current, batch_end,
                &mut active_blocks, &mut factory_pools,
                |log| {
                    let log_data = log.data();
                    let topics = log.topics();
                    if log_data.data.len() < 96 || topics.len() < 3 {
                        return None;
                    }
                    let pair_addr = Address::from_slice(&log_data.data[12..32]);
                    let token0 = Address::from_slice(&topics[1][12..]);
                    let token1 = Address::from_slice(&topics[2][12..]);
                    let creation_block = log.block_number.unwrap_or(0);
                    Some((pair_addr, DiscoveredPool {
                        address: pair_addr, token0, token1,
                        fee: 0, tick_spacing: None, dex_type: DexType::Camelot,
                        creation_block, pool_id: None, factory: Some(log.address()),
                    }))
                },
            ).await;
        }

        if let Some(ref f) = on_batch {
            f();
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

    let ref_block = to_block;

    type FetchTask = Pin<Box<dyn Future<Output = (Address, DexType, Option<Address>, Option<Address>, Option<u32>, Option<u32>, u64)> + Send>>;

    let mut fetch_tasks: Vec<FetchTask> = Vec::new();

    // Collect pool_hits data to avoid borrowing pool_hits across async boundaries
    let pool_hits_vec: Vec<_> = pool_hits.iter().map(|(addr, (dt, pid, tokens, fsb))| {
        (*addr, *dt, *pid, *tokens, *fsb)
    }).collect();

    for (addr, dex_type, _balancer_pool_id, balancer_tokens, first_seen_block) in &pool_hits_vec {
        // Skip pools already fully resolved via factory events
        if let Some(fp) = factory_pools.get(addr) {
            match fp.dex_type {
                DexType::UniswapV2 | DexType::UniswapV3 => continue,
                _ => {}
            }
        }
        match dex_type {
            DexType::UniswapV2 | DexType::Solidly | DexType::Camelot => {
                let rpc = rpc.clone();
                let addr = *addr;
                let sel0 = token0_selector.clone();
                let sel1 = token1_selector.clone();
                let fsb = *first_seen_block;
                let dex_type = *dex_type;
                fetch_tasks.push(Box::pin(async move {
                    let token0 = rpc.call(addr, sel0, ref_block).await.ok()
                        .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])));
                    let token1 = rpc.call(addr, sel1, ref_block).await.ok()
                        .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])));
                    (addr, dex_type, token0, token1, None, None, fsb)
                }));
            }
            DexType::UniswapV3 => {
                let rpc = rpc.clone();
                let addr = *addr;
                let sel0 = token0_selector.clone();
                let sel1 = token1_selector.clone();
                let sel_fee = fee_selector.clone();
                let sel_ts = tick_spacing_selector.clone();
                let fsb = *first_seen_block;
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
                    (addr, DexType::UniswapV3, token0, token1, fee, tick_spacing, fsb)
                }));
            }
            DexType::Curve | DexType::Balancer => {
                let (t0, t1) = balancer_tokens.unwrap_or((Address::ZERO, Address::ZERO));
                let addr = *addr;
                let dt = *dex_type;
                let fsb = *first_seen_block;
                fetch_tasks.push(Box::pin(async move {
                    (addr, dt, Some(t0), Some(t1), None, None, fsb)
                }));
            }
            DexType::Dodo => {
                let rpc = rpc.clone();
                let addr = *addr;
                let sel_base = Bytes::from_static(&[0xe1, 0x50, 0x31, 0x08]); // _BASE_TOKEN_()
                let sel_quote = Bytes::from_static(&[0x0f, 0xd8, 0xba, 0xfe]); // _QUOTE_TOKEN_()
                let fsb = *first_seen_block;
                fetch_tasks.push(Box::pin(async move {
                    let token0 = rpc.call(addr, sel_base, ref_block).await.ok()
                        .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])));
                    let token1 = rpc.call(addr, sel_quote, ref_block).await.ok()
                        .and_then(|b| (b.len() >= 32).then(|| Address::from_slice(&b[12..32])));
                    (addr, DexType::Dodo, token0, token1, None, None, fsb)
                }));
            }
            DexType::Clipper => {
                let (t0, t1) = balancer_tokens.unwrap_or((Address::ZERO, Address::ZERO));
                let addr = *addr;
                let dt = *dex_type;
                let fsb = *first_seen_block;
                fetch_tasks.push(Box::pin(async move {
                    (addr, dt, Some(t0), Some(t1), None, None, fsb)
                }));
            }
        }
    }

    // Bounded concurrency: limit parallel RPC calls to avoid overwhelming public RPCs
    let rpc_concurrency = std::env::var("MEV_SCOUT_RPC_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);

    use futures::stream::{self, StreamExt};
    let results: Vec<_> = stream::iter(fetch_tasks)
        .buffer_unordered(rpc_concurrency)
        .collect()
        .await;

    // ── Phase 3: Build output ──
    // Use a HashSet for O(1) dedup instead of O(n²) linear scan
    let mut resolved_addrs: HashSet<Address> = HashSet::new();
    let mut discovered_pools = Vec::new();

    // First, add all factory-discovered pools (they have creation_block, factory, etc.)
    for (_, dp) in factory_pools.drain() {
        resolved_addrs.insert(dp.address);
        discovered_pools.push(dp);
    }

    // Then, add metadata-fetched pools not already resolved
    for (addr, dex_type, token0_opt, token1_opt, fee_opt, tick_spacing, first_seen_block) in results {
        if !resolved_addrs.insert(addr) {
            continue;
        }
        let token0 = token0_opt.unwrap_or(Address::ZERO);
        let token1 = token1_opt.unwrap_or(Address::ZERO);
        let fee = match dex_type {
            DexType::UniswapV2 | DexType::Solidly | DexType::Camelot => {
                config.v2_fee_override.unwrap_or(match dex_type {
                    DexType::Solidly => 30,
                    DexType::Camelot => 0,
                    _ => 30,
                })
            }
            DexType::UniswapV3 => fee_opt.unwrap_or(3000),
            DexType::Curve | DexType::Balancer => fee_opt.unwrap_or(0),
            DexType::Dodo => fee_opt.unwrap_or(0),
            DexType::Clipper => fee_opt.unwrap_or(0),
        };
        let pool_id = pool_hits.get(&addr).and_then(|(_, pid, _, _)| *pid);
        let creation_block = pool_hits.get(&addr).map(|(_, _, _, b)| *b).unwrap_or(first_seen_block);

        discovered_pools.push(DiscoveredPool {
            address: addr,
            token0,
            token1,
            fee,
            tick_spacing: tick_spacing.map(|ts| ts as i32),
            dex_type,
            creation_block,
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
    solidly_factories: Option<&[Address]>,
    camelot_factories: Option<&[Address]>,
    on_batch: Option<&dyn Fn()>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let disc_config = DiscoveryConfig {
        batch_size,
        v2_fee_override,
        balancer_vault,
        v2_factories,
        v3_factories,
        curve_registry,
        solidly_factories,
        camelot_factories,
    };
    let (pools, active_blocks) = discover_pools(
        rpc, from_block, to_block, &disc_config, on_batch,
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

/// Discover pools from multiple sources with configurable priority.
///
/// If `dune_primary_pool_discovery` is true in config and a Dune API key is
/// available, Dune pool discovery runs first and its results are supplemented
/// by on-chain event scanning. Otherwise, only on-chain discovery is used.
///
/// All discovered pools are cached via `discover_and_cache`.
pub async fn discover_pools_with_sources(
    rpc: &RpcClient,
    cache: &SqliteStore,
    config: &crate::config::Config,
    chain_name: crate::types::ChainName,
    from_block: u64,
    to_block: u64,
    batch_size: u64,
    v2_fee_override: Option<u32>,
    balancer_vault: Option<Address>,
    v2_factories: Option<&[Address]>,
    v3_factories: Option<&[Address]>,
    v2_factory_fees: Option<&[Option<u32>]>,
    curve_registry: Option<Address>,
    solidly_factories: Option<&[Address]>,
    camelot_factories: Option<&[Address]>,
    on_batch: Option<&dyn Fn()>,
) -> anyhow::Result<(Vec<DiscoveredPool>, HashSet<u64>)> {
    let use_dune = config.dune_primary_pool_discovery && config.dune_api_key.is_some();
    let chain_str = chain_name.to_string();

    let mut all_pools: Vec<DiscoveredPool> = Vec::new();

    if use_dune {
        let api_key = config.dune_api_key.as_ref().expect("checked above");
        let dune = crate::dune::DuneClient::new(api_key.clone());

        let fee = v2_fee_override.unwrap_or(30);
        match crate::dune::pool_discovery::discover_v2_pools_from_dune(
            &dune, &chain_str, from_block, to_block, fee,
        ).await {
            Ok(pools) => {
                tracing::info!("[pipeline] Dune V2: {} pools", pools.len());
                all_pools.extend(pools);
            }
            Err(e) => tracing::warn!("[pipeline] Dune V2 discovery failed: {e:#}"),
        }
        match crate::dune::pool_discovery::discover_v3_pools_from_dune(
            &dune, &chain_str, from_block, to_block,
        ).await {
            Ok(pools) => {
                tracing::info!("[pipeline] Dune V3: {} pools", pools.len());
                all_pools.extend(pools);
            }
            Err(e) => tracing::warn!("[pipeline] Dune V3 discovery failed: {e:#}"),
        }
        match crate::dune::pool_discovery::discover_active_pools_from_dune(
            &dune, &chain_str, from_block, to_block,
        ).await {
            Ok(pools) => {
                tracing::info!("[pipeline] Dune active: {} pools", pools.len());
                all_pools.extend(pools);
            }
            Err(e) => tracing::warn!("[pipeline] Dune active pool discovery failed: {e:#}"),
        }

        // Dedup Dune results by address, cache them
        let mut seen = std::collections::HashSet::new();
        let mut deduped = Vec::with_capacity(all_pools.len());
        let taken = std::mem::take(&mut all_pools);
        for pool in taken {
            if seen.insert(pool.address) {
                let info: crate::pool::state::PoolInfo = pool.clone().into();
                if let Err(e) = cache.put_discovered_pool(&info) {
                    tracing::warn!("Failed to cache Dune pool {}: {}", pool.address, e);
                }
                deduped.push(pool);
            }
        }
        all_pools = deduped;
    }

    // Always run on-chain discovery to catch pools Dune may have missed
    let (onchain_pools, active_blocks) = discover_and_cache(
        rpc, cache, from_block, to_block, batch_size,
        v2_fee_override, balancer_vault, v2_factories, v3_factories,
        v2_factory_fees, curve_registry, solidly_factories, camelot_factories,
        on_batch,
    ).await?;

    // Merge: on-chain pools take priority (richer metadata), but keep all
    let mut seen: std::collections::HashSet<Address> = all_pools.iter().map(|p| p.address).collect();
    for pool in onchain_pools {
        if seen.insert(pool.address) {
            all_pools.push(pool);
        }
    }

    tracing::info!(
        "[pipeline] Total pools after multi-source discovery: {}",
        all_pools.len(),
    );

    Ok((all_pools, active_blocks))
}
