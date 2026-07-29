use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use alloy::primitives::U256;
use alloy::rpc::types::Filter;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig};
use super::PENDLE_NEW_MARKET_TOPIC;

pub(crate) async fn scan_pendle_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(factory) = config.pendle_factory {
        let filter = Filter::new()
            .address(factory)
            .event_signature(*PENDLE_NEW_MARKET_TOPIC)
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
                    if topics.len() < 3 || log_data.data.len() < 32 {
                        continue;
                    }
                    let market_addr = Address::from_slice(&topics[1][12..32]);
                    let pt_addr = Address::from_slice(&topics[2][12..32]);
                    let expiry = U256::from_be_slice(&log_data.data[..32])
                        .to::<u64>();
                    let creation_block = log.block_number.unwrap_or(0);
                    factory_pools.entry(market_addr).or_insert(
                        DiscoveredPool::new(market_addr, pt_addr, Address::ZERO, 0, DexType::Pendle, creation_block)
                            .with_factory(Some(factory))
                            .with_maturity_timestamp(Some(expiry)));
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Pendle factory scan failed for {current}..{batch_end}: {e:#}"
                );
            }
        }
    }
}
