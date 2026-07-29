use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig};
use super::V4_INITIALIZE_TOPIC;

pub(crate) async fn scan_v4_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(pool_manager) = config.v4_pool_manager {
        let filter = Filter::new()
            .address(pool_manager)
            .event_signature(*V4_INITIALIZE_TOPIC)
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
                    if topics.len() < 4 || log_data.data.len() < 160 {
                        continue;
                    }
                    let token0 = Address::from_slice(&topics[2][12..32]);
                    let token1 = Address::from_slice(&topics[3][12..32]);
                    let fee = {
                        let mut fb = [0u8; 4];
                        fb[1] = log_data.data[29];
                        fb[2] = log_data.data[30];
                        fb[3] = log_data.data[31];
                        u32::from_be_bytes(fb)
                    };
                    let tick_spacing = {
                        let mut ts = [0u8; 4];
                        ts.copy_from_slice(&log_data.data[60..64]);
                        i32::from_be_bytes(ts)
                    };
                    let hook_address = Address::from_slice(&log_data.data[84..104]);
                    let hook_address = (!hook_address.is_zero()).then_some(hook_address);
                    let creation_block = log.block_number.unwrap_or(0);
                    let pool_addr = Address::from_slice(&topics[1][12..32]);
                    factory_pools.entry(pool_addr).or_insert(
                        DiscoveredPool::new(pool_addr, token0, token1, fee, DexType::UniswapV4, creation_block)
                            .with_tick_spacing(Some(tick_spacing))
                            .with_factory(Some(pool_manager))
                            .with_hook_address(hook_address));
                }
            }
            Err(e) => {
                tracing::warn!(
                    "V4 PoolManager scan failed for {current}..{batch_end}: {e:#}"
                );
            }
        }
    }
}
