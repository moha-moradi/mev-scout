//! Pool discovery — scans chain event logs to find and register new DEX pools.

use std::sync::LazyLock;

use alloy::primitives::{keccak256, Address, B256};
use alloy::rpc::types::Filter;
use serde::{Deserialize, Serialize};

use crate::cache::SqliteStore;
use crate::pool::dex_type::DexType;
use crate::pool::state::PoolInfo;
use crate::rpc::RpcClient;

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
        };
        let info: PoolInfo = dp.into();
        assert_eq!(info.dex_type, DexType::Balancer);
        assert_eq!(info.pool_id, Some(pool_id));
        assert_eq!(info.creation_block, 100);
    }
}
