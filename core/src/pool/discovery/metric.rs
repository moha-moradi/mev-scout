use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig};
use super::METRIC_POOL_CREATED_TOPIC;
use super::scan_factory_creation_events_pinned;

/// Metric V2 factory scan — `PoolCreated(address indexed token0, address
/// indexed token1, address indexed priceProvider, address pool, bytes32
/// poolId)`: tokens in topics[1..3], pool address + poolId in the data words.
/// The pool's fee is not exposed by the event (quoting is disabled for Metric
/// anyway until the oracle ABI is verified), so it stays 0.
pub(crate) async fn scan_metric_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(factories) = config.metric_factory {
        let factories = std::slice::from_ref(&factories);
        scan_factory_creation_events_pinned(
            rpc, factories, *METRIC_POOL_CREATED_TOPIC, current, batch_end,
            active_blocks, factory_pools, provider_idx,
            |log| {
                let log_data = log.data();
                let topics = log.topics();
                if log_data.data.len() < 64 || topics.len() < 4 {
                    return None;
                }
                let pool_addr = Address::from_slice(&log_data.data[12..32]);
                let token0 = Address::from_slice(&topics[1][12..]);
                let token1 = Address::from_slice(&topics[2][12..]);
                let creation_block = log.block_number.unwrap_or(0);
                Some((pool_addr, DiscoveredPool::new(pool_addr, token0, token1, 0, DexType::Metric, creation_block)
                    .with_factory(Some(log.address()))))
            },
        ).await;
    }
}
