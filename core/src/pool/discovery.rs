//! Pool discovery — scans chain event logs to find and register new DEX pools.

use std::sync::LazyLock;

use alloy::primitives::{keccak256, Address, B256};
use alloy::rpc::types::Filter;
use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;
use crate::pool::dex_type::DexType;
use crate::pool::state::PoolInfo;
use crate::rpc::RpcClient;

pub static V2_PAIR_CREATED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"PairCreated(address,address,address,uint256)")
});

pub static V3_POOL_CREATED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256(b"PoolCreated(address,address,uint24,int24,address)")
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
        }
    }
}

pub async fn discover_v2_pools(
    rpc: &RpcClient,
    factory: Address,
    from_block: u64,
    to_block: u64,
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
            continue;
        }
        let token0 = Address::from_slice(&topics[1][12..]);
        let token1 = Address::from_slice(&topics[2][12..]);
        let mut pair_bytes = [0u8; 32];
        pair_bytes.copy_from_slice(&log_data.data[..32]);
        let pair = Address::from_slice(&pair_bytes[12..]);

        pools.push(DiscoveredPool {
            address: pair,
            token0,
            token1,
            fee: 0,
            tick_spacing: None,
            dex_type: DexType::UniswapV2,
            creation_block: log.block_number.unwrap_or(to_block),
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

        pools.push(DiscoveredPool {
            address: pool_addr,
            token0,
            token1,
            fee,
            tick_spacing,
            dex_type: DexType::UniswapV3,
            creation_block: log.block_number.unwrap_or(to_block),
        });
    }

    Ok(pools)
}

pub async fn discover_pools(
    rpc: &RpcClient,
    cache: &CacheStore,
    v2_factories: &[Address],
    v3_factories: &[Address],
    start_block: u64,
    to_block: u64,
    batch_size: u64,
) -> anyhow::Result<usize> {
    let mut total = 0usize;

    for &factory in v2_factories {
        let cursor = cache
            .get_discovery_cursor(&factory)?
            .unwrap_or(start_block);
        if cursor > to_block {
            continue;
        }
        let mut current = cursor;
        while current <= to_block {
            let end = (current + batch_size - 1).min(to_block);
            let discovered = discover_v2_pools(rpc, factory, current, end)
                .await
                .with_context(|| format!("V2 discovery {factory} blocks {current}..{end}"))?;

            for pool in &discovered {
                let info: PoolInfo = pool.clone().into();
                cache.put_discovered_pool(&info)?;
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
            let discovered = discover_v3_pools(rpc, factory, current, end)
                .await
                .with_context(|| format!("V3 discovery {factory} blocks {current}..{end}"))?;

            for pool in &discovered {
                let info: PoolInfo = pool.clone().into();
                cache.put_discovered_pool(&info)?;
            }
            total += discovered.len();
            cache.put_discovery_cursor(&factory, end)?;

            if end == to_block {
                break;
            }
            current = end + 1;
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
    fn test_discovered_pool_conversion() {
        let dp = DiscoveredPool {
            address: address!("cafe000000000000000000000000000000000001"),
            token0: address!("aaaa0000000000000000000000000000000000aa"),
            token1: address!("bbbb0000000000000000000000000000000000bb"),
            fee: 3000,
            tick_spacing: Some(60),
            dex_type: DexType::UniswapV3,
            creation_block: 0,
        };
        let info: PoolInfo = dp.into();
        assert_eq!(info.fee, 3000);
        assert_eq!(info.dex_type, DexType::UniswapV3);
        assert_eq!(info.tick_spacing, Some(60));
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
        };
        let info: PoolInfo = dp.into();
        assert_eq!(info.dex_type, DexType::UniswapV2);
        assert!(info.tick_spacing.is_none());
    }
}
