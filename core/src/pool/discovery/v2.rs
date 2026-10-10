use super::scan_factory_creation_events_pinned;
use crate::utils::{abi_word_address, topic_address};
use super::V2_PAIR_CREATED_TOPIC;
use super::{resolve_dex_name, DiscoveredPool, PoolHitCandidate, ScanBatchResult, ScanContext};
use crate::dex_type::DexType;
use crate::pipeline::topics;
/// V2 activity: per-pool Pair contracts emit Swap/Sync from the pool address.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    if log.topics()[0] == topics::V2_SWAP || log.topics()[0] == topics::V2_SYNC {
        Some(PoolHitCandidate::simple(DexType::UniswapV2))
    } else {
        None
    }
}

pub(crate) async fn scan_v2_batch(ctx: &ScanContext<'_>) -> ScanBatchResult {
    let ScanContext {
        rpc,
        config,
        current,
        batch_end,
        provider_idx,
    } = *ctx;
    if let Some(factories) = config.v2_factories {
        let fee = config.v2_fee_override.unwrap_or(30);
        return scan_factory_creation_events_pinned(
            rpc,
            factories,
            *V2_PAIR_CREATED_TOPIC,
            current,
            batch_end,
            provider_idx,
            |log| {
                let log_data = log.data();
                let topics = log.topics();
                if log_data.data.len() < 64 || topics.len() < 3 {
                    return None;
                }
                let addr = abi_word_address(&log_data.data, 0);
                let token0 = topic_address(topics[1]);
                let token1 = topic_address(topics[2]);
                let creation_block = log.block_number.unwrap_or(0);
                let factory = log.address();
                Some((
                    addr,
                    DiscoveredPool::new(
                        addr,
                        token0,
                        token1,
                        fee,
                        DexType::UniswapV2,
                        creation_block,
                    )
                    .with_factory(Some(factory))
                    .with_dex_name(Some(resolve_dex_name(Some(factory), "UniswapV2"))),
                ))
            },
        )
        .await;
    }
    ScanBatchResult::default()
}
