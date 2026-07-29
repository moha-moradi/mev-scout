use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig, PoolHits};
use super::LB_PAIR_CREATED_TOPIC;

pub(crate) async fn scan_trader_joe_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    pool_hits: &mut PoolHits,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(factory) = config.trader_joe_factory {
        let filter = Filter::new()
            .address(factory)
            .event_signature(*LB_PAIR_CREATED_TOPIC)
            .from_block(current)
            .to_block(batch_end);
        match get_logs_pinned!(rpc, &filter, provider_idx) {
            Ok(logs) => {
                for log in &logs {
                    if let Some(bn) = log.block_number {
                        active_blocks.insert(bn);
                    }
                    let topics = log.topics();
                    let log_data = log.data();
                    if topics.len() < 4 || log_data.data.len() < 64 {
                        continue;
                    }
                    let lb_pair = Address::from_slice(&topics[1][12..32]);
                    let token0 = Address::from_slice(&topics[2][12..32]);
                    let token1 = Address::from_slice(&topics[3][12..32]);
                    let creation_block = log.block_number.unwrap_or(0);
                    pool_hits.entry(lb_pair).or_insert((
                        DexType::TraderJoeLB, None, None, creation_block,
                    ));
                    factory_pools.entry(lb_pair).or_insert(
                        DiscoveredPool::new(lb_pair, token0, token1, 0, DexType::TraderJoeLB, creation_block)
                            .with_factory(Some(factory)));
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Trader Joe LB factory scan failed for {current}..{batch_end}: {e:#}"
                );
            }
        }
    }
}
