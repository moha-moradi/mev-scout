use super::BALANCER_POOL_REGISTERED_TOPIC;
use super::{DiscoveredPool, DiscoveryConfig, PoolHit, PoolHitCandidate, ScanBatchResult};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::pool::selectors::BALANCER_GET_POOL;
use crate::rpc::RpcClient;
use alloy::primitives::{Address, Bytes, U256};
use alloy::rpc::types::Filter;

/// Balancer activity: swaps emit from the singleton Vault; the poolId in
/// topics[1] encodes the pool address (top 20 bytes). The token pair is
/// indexed as topics[2]/topics[3] when the event carries non-empty slots.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    if log.topics()[0] != *topics::BALANCER_SWAP {
        return None;
    }
    let t = log.topics();
    if t.len() >= 4 {
        let mut pool_id = [0u8; 32];
        pool_id.copy_from_slice(t[1].as_slice());
        let pool_addr = Address::from_slice(&pool_id[..20]);
        let token_in = Address::from_slice(&t[2][12..]);
        let token_out = Address::from_slice(&t[3][12..]);
        Some(PoolHitCandidate {
            pool_id: Some(pool_id),
            tokens: Some((token_in, token_out)),
            addr_override: Some(pool_addr),
            ..PoolHitCandidate::simple(DexType::Balancer)
        })
    } else {
        Some(PoolHitCandidate::simple(DexType::Balancer))
    }
}

pub(crate) async fn scan_balancer_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    provider_idx: Option<usize>,
) -> ScanBatchResult {
    let mut out = ScanBatchResult::default();
    if let Some(vault) = config.balancer_vault {
        let filter = Filter::new()
            .address(vault)
            .event_signature(*BALANCER_POOL_REGISTERED_TOPIC)
            .from_block(current)
            .to_block(batch_end);
        match get_logs_pinned!(rpc, &filter, provider_idx) {
            Ok(logs) => {
                for log in &logs {
                    if let Some(bn) = log.block_number {
                        out.active_blocks.insert(bn);
                    }
                    let topics = log.topics();
                    if topics.len() < 4 {
                        continue;
                    }
                    let pool_type = topics[3][31];
                    if pool_type == 2 || pool_type > 3 {
                        continue;
                    }
                    let mut pool_id = [0u8; 32];
                    pool_id.copy_from_slice(topics[1].as_slice());
                    let pool_addr = Address::from_slice(&topics[2][12..32]);
                    let creation_block = log.block_number.unwrap_or(0);
                    out.pool_hits.entry(pool_addr).or_insert(PoolHit {
                        dex_type: DexType::Balancer,
                        pool_id: Some(pool_id),
                        tokens: None,
                        first_seen_block: creation_block,
                    });
                    out.factory_pools.entry(pool_addr).or_insert(
                        DiscoveredPool::new(
                            pool_addr,
                            Address::ZERO,
                            Address::ZERO,
                            0,
                            DexType::Balancer,
                            creation_block,
                        )
                        .with_pool_id(Some(pool_id))
                        .with_factory(Some(vault))
                        .with_balancer_pool_type(Some(pool_type)),
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Balancer vault scan failed for {current}..{batch_end}: {e:#}");
            }
        }
    }
    out
}

/// Resolve missing `pool_id`s for Balancer pools discovered via Swap events.
/// Events occasionally carry truncated topics, so we call `Vault.getPool(
/// address)` which returns `(bytes32 poolId, address[] tokens)`.
pub(super) async fn resolve_pool_ids(
    rpc: &RpcClient,
    vault: Address,
    addrs: Vec<Address>,
    concurrency: usize,
) -> Vec<(Address, [u8; 32], Option<Vec<Address>>)> {
    use futures::stream::{self, StreamExt};

    let resolve_tasks: Vec<_> = addrs
        .into_iter()
        .map(|addr| {
            let rpc = rpc.clone();
            async move {
                let mut calldata = Vec::with_capacity(36);
                calldata.extend_from_slice(&BALANCER_GET_POOL);
                let mut arg = [0u8; 32];
                arg[12..32].copy_from_slice(addr.as_slice());
                calldata.extend_from_slice(&arg);
                match rpc.call_latest(vault, Bytes::from(calldata)).await {
                    Ok(result) if result.0.len() >= 32 => {
                        let mut pool_id = [0u8; 32];
                        pool_id.copy_from_slice(&result.0[..32]);
                        Some((addr, pool_id, decode_get_pool_tokens(&result.0)))
                    }
                    _ => None,
                }
            }
        })
        .collect();

    stream::iter(resolve_tasks)
        .buffer_unordered(concurrency)
        .filter_map(|hit| async move { hit })
        .collect()
        .await
}

/// `getPool` returns `(bytes32 poolId, address[] tokens)`; decode the dynamic
/// token array: offset at 32..64, length at offset, then one address per slot.
fn decode_get_pool_tokens(data: &[u8]) -> Option<Vec<Address>> {
    if data.len() < 64 {
        return None;
    }
    let offset = U256::from_be_slice(&data[32..64]).to::<usize>();
    if data.len() < offset + 32 {
        return None;
    }
    let len = U256::from_be_slice(&data[offset..offset + 32]).to::<usize>();
    let mut tokens = Vec::with_capacity(len);
    for i in 0..len {
        let pos = offset + 32 + i * 32;
        if pos + 32 <= data.len() {
            tokens.push(Address::from_slice(&data[pos + 12..pos + 32]));
        }
    }
    (!tokens.is_empty()).then_some(tokens)
}
