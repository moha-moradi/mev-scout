use std::collections::{HashMap, HashSet};
use alloy::primitives::Address;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig};
use super::FLUID_DEX_DEPLOYED_TOPIC;
use super::scan_factory_creation_events_pinned;

/// Fluid DEX factory scan — `LogDexDeployed(address indexed dex, uint256
/// indexed dexId)` (verified against Instadapp/fluid-contracts-public
/// factory/main.sol). The event carries only the pool address; token metadata
/// is fetched later via the pool's `token0()`/`token1()` getters (Phase 2).
pub(crate) async fn scan_fluid_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
    if let Some(factories) = config.fluid_factory {
        let factories = std::slice::from_ref(&factories);
        scan_factory_creation_events_pinned(
            rpc, factories, *FLUID_DEX_DEPLOYED_TOPIC, current, batch_end,
            active_blocks, factory_pools, provider_idx,
            |log| {
                let topics = log.topics();
                if topics.len() < 2 {
                    return None;
                }
                let pool_addr = Address::from_slice(&topics[1][12..]);
                let creation_block = log.block_number.unwrap_or(0);
                Some((pool_addr, DiscoveredPool::new(
                    pool_addr, Address::ZERO, Address::ZERO, 0, DexType::Fluid, creation_block,
                ).with_factory(Some(log.address()))))
            },
        ).await;
    }
}
