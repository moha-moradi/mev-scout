use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig};
use super::V3_POOL_CREATED_TOPIC;
use super::scan_factory_creation_events_pinned;

pub(crate) async fn scan_v3_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(factories) = config.v3_factories {
        scan_factory_creation_events_pinned(
            rpc, factories, *V3_POOL_CREATED_TOPIC, current, batch_end,
            active_blocks, factory_pools, provider_idx,
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
                Some((pool_addr, DiscoveredPool::new(pool_addr, token0, token1, fee, DexType::UniswapV3, creation_block)
                    .with_tick_spacing(tick_spacing)
                    .with_factory(Some(log.address()))))
            },
        ).await;
    }
}
