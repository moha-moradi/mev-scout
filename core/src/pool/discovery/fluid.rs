use super::scan_factory_creation_events_pinned;
use super::FLUID_DEX_DEPLOYED_TOPIC;
use super::{DiscoveredPool, DiscoveryConfig, PoolHitCandidate, ScanBatchResult};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::rpc::RpcClient;
use alloy::primitives::Address;

/// Fluid DEX activity: per-pool contracts emit Swap from the pool address;
/// tokens come from token0()/token1() metadata fetch or the SQLite cache.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    if log.topics()[0] == *topics::FLUID_SWAP {
        Some(PoolHitCandidate::simple(DexType::Fluid))
    } else {
        None
    }
}

/// Fluid DEX factory scan — `LogDexDeployed(address indexed dex, uint256
/// indexed dexId)` (verified against Instadapp/fluid-contracts-public
/// factory/main.sol). The event carries only the pool address; token metadata
/// is fetched later via the pool's `token0()`/`token1()` getters (Phase 2).
pub(crate) async fn scan_fluid_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    provider_idx: Option<usize>,
) -> ScanBatchResult {
    if let Some(factories) = config.fluid_factory {
        let factories = std::slice::from_ref(&factories);
        return scan_factory_creation_events_pinned(
            rpc,
            factories,
            *FLUID_DEX_DEPLOYED_TOPIC,
            current,
            batch_end,
            provider_idx,
            |log| {
                let topics = log.topics();
                if topics.len() < 2 {
                    return None;
                }
                let pool_addr = Address::from_slice(&topics[1][12..]);
                let creation_block = log.block_number.unwrap_or(0);
                Some((
                    pool_addr,
                    DiscoveredPool::new(
                        pool_addr,
                        Address::ZERO,
                        Address::ZERO,
                        0,
                        DexType::Fluid,
                        creation_block,
                    )
                    .with_factory(Some(log.address())),
                ))
            },
        )
        .await;
    }
    ScanBatchResult::default()
}
