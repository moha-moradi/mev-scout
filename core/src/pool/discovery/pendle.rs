use super::PENDLE_NEW_MARKET_TOPIC;
use super::{DiscoveredPool, PoolHitCandidate, ScanBatchResult, ScanContext};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::utils::topic_address;
use alloy::primitives::Address;
use alloy::primitives::U256;
use alloy::rpc::types::Filter;

/// Pendle activity: markets are per-market contracts emitting their own Swap
/// events; router-level PT/YT swaps carry the market as topics[2].
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    let Some(t0) = super::topic0(log) else {
        return None;
    };
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
            let market = topic_address(t[2].as_slice());
            return Some(PoolHitCandidate {
                addr_override: Some(market),
                ..PoolHitCandidate::simple(DexType::Pendle)
            });
        }
        return Some(PoolHitCandidate::simple(DexType::Pendle));
    }
    None
}

/// Decode a Pendle `NewMarket(address,address,uint256)` log.
pub(super) fn decode_pendle_new_market(
    log: &alloy::rpc::types::Log,
    factory: Address,
) -> Option<(Address, DiscoveredPool)> {
    let topics = log.topics();
    let log_data = log.data();
    if topics.len() < 3 || log_data.data.len() < 32 {
        return None;
    }
    let market_addr = topic_address(topics[1]);
    let pt_addr = topic_address(topics[2]);
    let expiry = U256::from_be_slice(&log_data.data[..32]).to::<u64>();
    let creation_block = log.block_number.unwrap_or(0);
    Some((
        market_addr,
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
    ))
}

pub(crate) async fn scan_pendle_batch(ctx: &ScanContext<'_>) -> ScanBatchResult {
    let ScanContext {
        rpc,
        config,
        current,
        batch_end,
        provider_idx,
    } = *ctx;
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
                    if let Some((addr, pool)) = decode_pendle_new_market(log, factory) {
                        out.factory_pools.entry(addr).or_insert(pool);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Pendle factory scan failed for {current}..{batch_end}: {e:#}");
            }
        }
    }
    out
}
