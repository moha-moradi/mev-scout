use super::scan_factory_creation_events_pinned;
use super::FLUID_DEX_DEPLOYED_TOPIC;
use super::{DiscoveredPool, PoolHitCandidate, ScanBatchResult, ScanContext};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::utils::topic_address;
use alloy::primitives::Address;

/// Fluid DEX activity: per-pool contracts emit Swap from the pool address;
/// tokens come from token0/token1 metadata fetch or the SQLite cache.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    let Some(t0) = super::topic0(log) else {
        return None;
    };
    if t0 == *topics::FLUID_SWAP {
        Some(PoolHitCandidate::simple(DexType::Fluid))
    } else {
        None
    }
}

/// Decode a Fluid `LogDexDeployed(address indexed dex, uint256 indexed dexId)` log.
pub(super) fn decode_fluid_dex_deployed(
    log: &alloy::rpc::types::Log,
) -> Option<(Address, DiscoveredPool)> {
    let topics = log.topics();
    if topics.len() < 3 {
        return None;
    }
    let pool_addr = topic_address(topics[1]);
    let dex_id = format!("0x{}", hex::encode(topics[2].as_slice()));
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
        .with_factory(Some(log.address()))
        .with_protocol_params(serde_json::json!({ "dex_id": dex_id })),
    ))
}

/// Fluid DEX factory scan — see [`decode_fluid_dex_deployed`].
pub(crate) async fn scan_fluid_batch(ctx: &ScanContext<'_>) -> ScanBatchResult {
    let ScanContext {
        rpc,
        config,
        current,
        batch_end,
        provider_idx,
    } = *ctx;
    if let Some(factories) = config.fluid_factory {
        let factories = std::slice::from_ref(&factories);
        return scan_factory_creation_events_pinned(
            rpc,
            factories,
            *FLUID_DEX_DEPLOYED_TOPIC,
            current,
            batch_end,
            provider_idx,
            decode_fluid_dex_deployed,
        )
        .await;
    }
    ScanBatchResult::default()
}
