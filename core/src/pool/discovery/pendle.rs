use super::PENDLE_NEW_MARKET_TOPIC;
use super::{DiscoveredPool, DiscoveryConfig, PoolHitCandidate, ScanBatchResult};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::rpc::RpcClient;
use alloy::primitives::Address;
use alloy::primitives::U256;
use alloy::rpc::types::Filter;

/// Pendle activity: markets are per-market contracts emitting their own Swap
/// events; router-level PT/YT swaps carry the market as topics[2].
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    let t0 = log.topics()[0];
    if t0 == *topics::PENDLE_MARKET_SWAP {
        return Some(PoolHitCandidate::simple(DexType::Pendle));
    }
    if t0 == *topics::PENDLE_SWAP_PT_AND_SY
        || t0 == *topics::PENDLE_SWAP_YT_AND_SY
        || t0 == *topics::PENDLE_SWAP_PT_AND_TOKEN
        || t0 == *topics::PENDLE_SWAP_YT_AND_TOKEN
    {
        let t = log.topics();
        if t.len() >= 3 {
            let market = Address::from_slice(&t[2].as_slice()[12..32]);
            return Some(PoolHitCandidate {
                addr_override: Some(market),
                ..PoolHitCandidate::simple(DexType::Pendle)
            });
        }
        return Some(PoolHitCandidate::simple(DexType::Pendle));
    }
    None
}

pub(crate) async fn scan_pendle_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    provider_idx: Option<usize>,
) -> ScanBatchResult {
    let mut out = ScanBatchResult::default();
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
                        out.active_blocks.insert(bn);
                    }
                    let topics = log.topics();
                    let log_data = log.data();
                    if topics.len() < 3 || log_data.data.len() < 32 {
                        continue;
                    }
                    let market_addr = Address::from_slice(&topics[1][12..32]);
                    let pt_addr = Address::from_slice(&topics[2][12..32]);
                    let expiry = U256::from_be_slice(&log_data.data[..32]).to::<u64>();
                    let creation_block = log.block_number.unwrap_or(0);
                    out.factory_pools.entry(market_addr).or_insert(
                        DiscoveredPool::new(
                            market_addr,
                            pt_addr,
                            Address::ZERO,
                            0,
                            DexType::Pendle,
                            creation_block,
                        )
                        .with_factory(Some(factory))
                        .with_maturity_timestamp(Some(expiry)),
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Pendle factory scan failed for {current}..{batch_end}: {e:#}");
            }
        }
    }
    out
}
