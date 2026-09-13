use super::LB_PAIR_CREATED_TOPIC;
use super::{DiscoveredPool, DiscoveryConfig, PoolHit, PoolHitCandidate, ScanBatchResult};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::rpc::RpcClient;
use alloy::primitives::Address;
use alloy::rpc::types::Filter;

/// Trader Joe / LFJ V2 activity: LBPair contracts are per-pool and emit their
/// own Swap events.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    let t0 = log.topics()[0];
    if t0 == *topics::TRADER_JOE_LB_SWAP || t0 == *topics::TRADER_JOE_LB_SWAP_LEGACY {
        Some(PoolHitCandidate::simple(DexType::TraderJoeLB))
    } else {
        None
    }
}

pub(crate) async fn scan_trader_joe_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    provider_idx: Option<usize>,
) -> ScanBatchResult {
    let mut out = ScanBatchResult::default();
    if let Some(factories) = config.trader_joe_factories {
        for &factory in factories {
            let filter = Filter::new()
                .address(factory)
                .event_signature(*LB_PAIR_CREATED_TOPIC)
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
                        if topics.len() < 4 || log_data.data.len() < 64 {
                            continue;
                        }
                        let lb_pair = Address::from_slice(&topics[1][12..32]);
                        let token0 = Address::from_slice(&topics[2][12..32]);
                        let token1 = Address::from_slice(&topics[3][12..32]);
                        let creation_block = log.block_number.unwrap_or(0);
                        out.pool_hits.entry(lb_pair).or_insert(PoolHit {
                            dex_type: DexType::TraderJoeLB,
                            pool_id: None,
                            tokens: None,
                            first_seen_block: creation_block,
                        });
                        out.factory_pools.entry(lb_pair).or_insert(
                            DiscoveredPool::new(
                                lb_pair,
                                token0,
                                token1,
                                0,
                                DexType::TraderJoeLB,
                                creation_block,
                            )
                            .with_factory(Some(factory)),
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Trader Joe LB factory ({factory}) scan failed for {current}..{batch_end}: {e:#}"
                    );
                }
            }
        }
    }
    out
}
