use super::scan_factory_creation_events_pinned;
use super::METRIC_POOL_CREATED_TOPIC;
use super::{DiscoveredPool, PoolHitCandidate, ScanBatchResult, ScanContext};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::utils::{abi_word_address, topic_address};
use alloy::primitives::Address;

/// Metric V2 activity: per-pool contracts emit Swap from the pool address.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    if log.topics()[0] == *topics::METRIC_SWAP {
        Some(PoolHitCandidate::simple(DexType::Metric))
    } else {
        None
    }
}

/// Decode a Metric `PoolCreated(address,address,address,address,bytes32)` log.
///
/// The event carries tokens in topics[1..3], pool address + poolId in data.
/// Fee is not exposed (quoting is disabled for Metric), so it stays 0.
pub(super) fn decode_metric_pool_created(
    log: &alloy::rpc::types::Log,
) -> Option<(Address, DiscoveredPool)> {
    let log_data = log.data();
    let topics = log.topics();
    if log_data.data.len() < 64 || topics.len() < 4 {
        return None;
    }
    let pool_addr = abi_word_address(&log_data.data, 0);
    let token0 = topic_address(topics[1]);
    let token1 = topic_address(topics[2]);
    let creation_block = log.block_number.unwrap_or(0);
    Some((
        pool_addr,
        DiscoveredPool::new(
            pool_addr,
            token0,
            token1,
            0,
            DexType::Metric,
            creation_block,
        )
        .with_factory(Some(log.address())),
    ))
}

/// Metric V2 factory scan — see [`decode_metric_pool_created`].
pub(crate) async fn scan_metric_batch(ctx: &ScanContext<'_>) -> ScanBatchResult {
    let ScanContext {
        rpc,
        config,
        current,
        batch_end,
        provider_idx,
    } = *ctx;
    if let Some(factories) = config.metric_factory {
        let factories = std::slice::from_ref(&factories);
        return scan_factory_creation_events_pinned(
            rpc,
            factories,
            *METRIC_POOL_CREATED_TOPIC,
            current,
            batch_end,
            provider_idx,
            decode_metric_pool_created,
        )
        .await;
    }
    ScanBatchResult::default()
}
