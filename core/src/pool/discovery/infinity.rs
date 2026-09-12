use super::INF_CL_INITIALIZE_TOPIC;
use super::{DiscoveredPool, DiscoveryConfig};
use crate::dex_type::DexType;
use crate::rpc::RpcClient;
use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use std::collections::{HashMap, HashSet};

/// Pancake Infinity CL pools live inside the singleton `CLPoolManager`: the
/// `Initialize` event carries the pool's bytes32 `PoolId` in topics[1] (the
/// synthetic pool address is its first 20 bytes, same scheme as V4). The
/// remaining indexed topics carry currency0/currency1; non-indexed data is
/// `hooks, fee, parameters, sqrtPriceX96, tick` (5 ABI-padded words).
pub(crate) async fn scan_infinity_cl_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(pool_manager) = config.infinity_cl_pool_manager {
        let filter = Filter::new()
            .address(pool_manager)
            .event_signature(*INF_CL_INITIALIZE_TOPIC)
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
                    // non-indexed: hooks(20B right-aligned) + fee(uint24) + parameters(32B)
                    // + sqrtPriceX96(20B) + tick(int24) => 5 words, 160 bytes.
                    if topics.len() < 4 || log_data.data.len() < 160 {
                        continue;
                    }
                    let pool_id: [u8; 32] = topics[1].0;
                    let pool_addr = Address::from_slice(&pool_id[12..32]);
                    let token0 = Address::from_slice(&topics[2][12..32]);
                    let token1 = Address::from_slice(&topics[3][12..32]);
                    let fee = {
                        let mut fb = [0u8; 4];
                        fb[1] = log_data.data[61];
                        fb[2] = log_data.data[62];
                        fb[3] = log_data.data[63];
                        u32::from_be_bytes(fb)
                    };
                    let hook_address = Address::from_slice(&log_data.data[12..32]);
                    let hook_address = (!hook_address.is_zero()).then_some(hook_address);
                    let creation_block = log.block_number.unwrap_or(0);
                    factory_pools.entry(pool_addr).or_insert(
                        DiscoveredPool::new(
                            pool_addr,
                            token0,
                            token1,
                            fee,
                            DexType::PancakeInfinity,
                            creation_block,
                        )
                        .with_pool_id(Some(pool_id))
                        .with_factory(Some(pool_manager))
                        .with_hook_address(hook_address),
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Pancake Infinity CLPoolManager scan failed for {current}..{batch_end}: {e:#}"
                );
            }
        }
    }
}
