use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig};
use super::SOLIDLY_PAIR_CREATED_TOPIC;
use super::scan_factory_creation_events_pinned;

pub(crate) async fn scan_solidly_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(factories) = config.solidly_factories {
        let fee = config.solidly_fee_bps.or(config.v2_fee_override).unwrap_or(30);
        scan_factory_creation_events_pinned(
            rpc, factories, *SOLIDLY_PAIR_CREATED_TOPIC, current, batch_end,
            active_blocks, factory_pools, provider_idx,
            |log| {
                let log_data = log.data();
                let topics = log.topics();
                if log_data.data.len() < 64 || topics.len() < 3 {
                    return None;
                }
                let pair_addr = Address::from_slice(&log_data.data[44..64]);
                let token0 = Address::from_slice(&topics[1][12..]);
                let token1 = Address::from_slice(&topics[2][12..]);
                let is_stable = log_data.data[31] != 0;
                let creation_block = log.block_number.unwrap_or(0);
                Some((pair_addr, DiscoveredPool::new(pair_addr, token0, token1, fee, DexType::Solidly, creation_block)
                    .with_factory(Some(log.address()))
                    .with_is_stable(Some(is_stable))))
            },
        ).await;
    }
}
