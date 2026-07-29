use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig, PoolHits};
use super::CURVE_POOL_ADDED_TOPIC;

pub(crate) async fn scan_curve_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    pool_hits: &mut PoolHits,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(registry) = config.curve_registry {
        let filter = Filter::new()
            .address(registry)
            .event_signature(*CURVE_POOL_ADDED_TOPIC)
            .from_block(current)
            .to_block(batch_end);
        match get_logs_pinned!(rpc, &filter, provider_idx) {
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
                    factory_pools.entry(pool_addr).or_insert(
                        DiscoveredPool::new(pool_addr, Address::ZERO, Address::ZERO, 0, DexType::Curve, creation_block)
                            .with_factory(Some(registry)));
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Curve registry scan failed for {current}..{batch_end}: {e:#}"
                );
            }
        }
    }
}
