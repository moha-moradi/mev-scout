use super::{DiscoveredPool, DiscoveryConfig, PoolHit, PoolHitCandidate, ScanBatchResult};
use super::{CURVE_POOL_ADDED_TOPIC, CURVE_POOL_DEPLOYED_TOPIC};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::pool::selectors::{CURVE_COINS_I128, CURVE_COINS_U256};
use crate::rpc::RpcClient;
use alloy::primitives::{Address, Bytes, B256};
use alloy::rpc::types::Filter;
use std::sync::LazyLock;

/// Curve activity: per-pool contracts emit TokenExchange variants from the
/// pool address.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    let t0 = log.topics()[0];
    if t0 == *topics::CURVE_TOKEN_EXCHANGE
        || t0 == *topics::CURVE_V2_TOKEN_EXCHANGE
        || t0 == *topics::CURVE_TOKEN_EXCHANGE_UNDERLYING
        || t0 == *topics::CURVE_V2_TOKEN_EXCHANGE_UNDERLYING
    {
        Some(PoolHitCandidate::simple(DexType::Curve))
    } else {
        None
    }
}

pub(crate) async fn scan_curve_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    provider_idx: Option<usize>,
) -> ScanBatchResult {
    let mut out = ScanBatchResult::default();
    // Curve authorities: per-chain CurveStableswapFactoryNG deployments plus the
    // legacy mainnet registry. Older factories emit `PoolAdded(address indexed pool, uint256)`;
    // CurveStableswapFactoryNG deployments emit `PoolDeployed(address pool)`.
    let mut authorities: Vec<Address> = config.curve_factories.unwrap_or(&[]).to_vec();
    if let Some(registry) = config.curve_registry {
        if !authorities.contains(&registry) {
            authorities.push(registry);
        }
    }
    if authorities.is_empty() {
        return out;
    }
    for (topic, pool_indexed) in [
        (&CURVE_POOL_ADDED_TOPIC as &LazyLock<B256>, true),
        (&CURVE_POOL_DEPLOYED_TOPIC, false),
    ] {
        let filter = Filter::new()
            .address(authorities.clone())
            .event_signature(**topic)
            .from_block(current)
            .to_block(batch_end);
        match get_logs_pinned!(rpc, &filter, provider_idx) {
            Ok(logs) => {
                for log in &logs {
                    if let Some(bn) = log.block_number {
                        out.active_blocks.insert(bn);
                    }
                    let topics = log.topics();
                    let pool_addr = if pool_indexed {
                        if topics.len() < 2 {
                            continue;
                        }
                        Address::from_slice(&topics[1][12..32])
                    } else {
                        let data = log.data();
                        if data.data.len() < 32 {
                            continue;
                        }
                        Address::from_slice(&data.data[12..32])
                    };
                    let creation_block = log.block_number.unwrap_or(0);
                    out.pool_hits.entry(pool_addr).or_insert(PoolHit {
                        dex_type: DexType::Curve,
                        pool_id: None,
                        tokens: None,
                        first_seen_block: creation_block,
                    });
                    out.factory_pools.entry(pool_addr).or_insert(
                        DiscoveredPool::new(
                            pool_addr,
                            Address::ZERO,
                            Address::ZERO,
                            0,
                            DexType::Curve,
                            creation_block,
                        )
                        .with_factory(Some(log.address())),
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Curve factory scan failed for {current}..{batch_end}: {e:#}");
            }
        }
    }
    out
}

/// Resolve underlying tokens for Curve pools discovered via the registry.
/// Classic Vyper factories expose `coins(int128)`, NG factories `coins(uint256)`
/// — try the canonical int128 signature first, then the uint256 variant (same
/// fallback order as `fetch_curve_state` in state/factory.rs).
pub(super) async fn resolve_underlying_tokens(
    rpc: &RpcClient,
    addrs: Vec<Address>,
    concurrency: usize,
) -> Vec<(Address, Vec<Address>)> {
    use futures::stream::{self, StreamExt};

    let resolve_tasks: Vec<_> = addrs
        .into_iter()
        .map(|addr| {
            let rpc = rpc.clone();
            async move {
                let mut tokens = Vec::new();
                for i in 0u8..8u8 {
                    let mut arg = [0u8; 32];
                    arg[31] = i;
                    let mut calldata = Vec::with_capacity(36);
                    calldata.extend_from_slice(&CURVE_COINS_I128);
                    calldata.extend_from_slice(&arg);
                    let call = match rpc.call_latest(addr, Bytes::from(calldata)).await {
                        Ok(result) if result.0.len() >= 32 => Some(result),
                        _ => {
                            // NG deployment: retry with coins(uint256)
                            let mut calldata = Vec::with_capacity(36);
                            calldata.extend_from_slice(&CURVE_COINS_U256);
                            calldata.extend_from_slice(&arg);
                            rpc.call_latest(addr, Bytes::from(calldata)).await.ok()
                        }
                    };
                    match call {
                        Some(result) if result.0.len() >= 32 => {
                            let token = Address::from_slice(&result.0[12..32]);
                            if token.is_zero() {
                                break;
                            }
                            tokens.push(token);
                        }
                        _ => break,
                    }
                }
                (!tokens.is_empty()).then_some((addr, tokens))
            }
        })
        .collect();

    stream::iter(resolve_tasks)
        .buffer_unordered(concurrency)
        .filter_map(|hit| async move { hit })
        .collect()
        .await
}
