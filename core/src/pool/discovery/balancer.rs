use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig, PoolHits};
use super::BALANCER_POOL_REGISTERED_TOPIC;

pub(crate) async fn scan_balancer_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    pool_hits: &mut PoolHits,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(vault) = config.balancer_vault {
        let filter = Filter::new()
            .address(vault)
            .event_signature(*BALANCER_POOL_REGISTERED_TOPIC)
            .from_block(current)
            .to_block(batch_end);
        match get_logs_pinned!(rpc, &filter, provider_idx) {
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
                    factory_pools.entry(pool_addr).or_insert(
                        DiscoveredPool::new(pool_addr, Address::ZERO, Address::ZERO, 0, DexType::Balancer, creation_block)
                            .with_pool_id(Some(pool_id))
                            .with_factory(Some(vault))
                            .with_balancer_pool_type(Some(pool_type)));
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Balancer vault scan failed for {current}..{batch_end}: {e:#}"
                );
            }
        }
    }
}
