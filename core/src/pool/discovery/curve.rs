use super::{DiscoveredPool, PoolHit, PoolHitCandidate, ScanBatchResult, ScanContext};
use super::{CURVE_POOL_ADDED_TOPIC, CURVE_POOL_DEPLOYED_TOPIC};
use crate::dex_type::DexType;
use crate::pipeline::topics;
use crate::pool::selectors::{CURVE_COINS_I128, CURVE_COINS_U256};
use crate::rpc::RpcClient;
use crate::utils::{abi_word_address, topic_address};
use alloy::primitives::{Address, Bytes, B256};
use alloy::rpc::types::Filter;
use std::sync::LazyLock;

/// Curve activity: per-pool contracts emit TokenExchange variants from the
/// pool address.
pub(super) fn classify_activity(log: &alloy::rpc::types::Log) -> Option<PoolHitCandidate> {
    let Some(t0) = super::topic0(log) else {
        return None;
    };
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

/// Decode a legacy Curve `PoolAdded(address indexed pool, uint256)` log.
pub(super) fn decode_curve_pool_added(
    log: &alloy::rpc::types::Log,
) -> Option<(Address, DiscoveredPool)> {
    let topics = log.topics();
    if topics.len() < 2 {
        return None;
    }
    let pool_addr = topic_address(topics[1]);
    let creation_block = log.block_number.unwrap_or(0);
    Some((
        pool_addr,
        DiscoveredPool::new(
            pool_addr,
            Address::ZERO,
            Address::ZERO,
            0,
            DexType::Curve,
            creation_block,
        )
        .with_factory(Some(log.address())),
    ))
}

/// Decode a CurveStableswapFactoryNG `PoolDeployed(address pool)` log.
pub(super) fn decode_curve_pool_deployed(
    log: &alloy::rpc::types::Log,
) -> Option<(Address, DiscoveredPool)> {
    let data = log.data();
    if data.data.len() < 32 {
        return None;
    }
    let pool_addr = abi_word_address(&data.data, 0);
    let creation_block = log.block_number.unwrap_or(0);
    Some((
        pool_addr,
        DiscoveredPool::new(
            pool_addr,
            Address::ZERO,
            Address::ZERO,
            0,
            DexType::Curve,
            creation_block,
        )
        .with_factory(Some(log.address())),
    ))
}

pub(crate) async fn scan_curve_batch(ctx: &ScanContext<'_>) -> ScanBatchResult {
    let ScanContext {
        rpc,
        config,
        current,
        batch_end,
        provider_idx,
    } = *ctx;
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
    for (topic, decode) in [
        (
            &CURVE_POOL_ADDED_TOPIC as &LazyLock<B256>,
            decode_curve_pool_added
                as fn(&alloy::rpc::types::Log) -> Option<(Address, DiscoveredPool)>,
        ),
        (&CURVE_POOL_DEPLOYED_TOPIC, decode_curve_pool_deployed),
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
                    if let Some((addr, pool)) = decode(log) {
                        out.pool_hits.entry(addr).or_insert(PoolHit {
                            dex_type: DexType::Curve,
                            pool_id: None,
                            tokens: None,
                            first_seen_block: pool.creation_block,
                        });
                        out.factory_pools.entry(addr).or_insert(pool);
                    }
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
                            let token = abi_word_address(&result.0, 0);
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
