use super::scan_factory_creation_events_pinned;
use super::CAMELOT_PAIR_CREATED_TOPIC;
use super::{DiscoveredPool, DiscoveryConfig, ScanBatchResult};
use crate::dex_type::DexType;
use crate::rpc::RpcClient;
use alloy::primitives::Address;
use alloy::primitives::U256;

pub(crate) async fn scan_camelot_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    provider_idx: Option<usize>,
) -> ScanBatchResult {
    if let Some(factories) = config.camelot_factories {
        return scan_factory_creation_events_pinned(
            rpc,
            factories,
            *CAMELOT_PAIR_CREATED_TOPIC,
            current,
            batch_end,
            provider_idx,
            |log| {
                let log_data = log.data();
                let topics = log.topics();
                if log_data.data.len() < 96 || topics.len() < 3 {
                    return None;
                }
                let pair_addr = Address::from_slice(&log_data.data[12..32]);
                let token0 = Address::from_slice(&topics[1][12..]);
                let token1 = Address::from_slice(&topics[2][12..]);
                let fee = U256::from_be_slice(&log_data.data[32..64]).to::<u64>() as u32;
                let is_stable = log_data.data[95] != 0;
                let creation_block = log.block_number.unwrap_or(0);
                Some((
                    pair_addr,
                    DiscoveredPool::new(
                        pair_addr,
                        token0,
                        token1,
                        fee,
                        DexType::Camelot,
                        creation_block,
                    )
                    .with_factory(Some(log.address()))
                    .with_is_stable(Some(is_stable)),
                ))
            },
        )
        .await;
    }
    ScanBatchResult::default()
}
