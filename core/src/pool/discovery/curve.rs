use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use alloy::primitives::{Address, B256};
use alloy::rpc::types::Filter;
use crate::rpc::RpcClient;
use crate::dex_type::DexType;
use super::{DiscoveredPool, DiscoveryConfig, PoolHits};
use super::{CURVE_POOL_ADDED_TOPIC, CURVE_POOL_DEPLOYED_TOPIC};

pub(crate) async fn scan_curve_batch(
    rpc: &RpcClient,
    config: &DiscoveryConfig<'_>,
    current: u64,
    batch_end: u64,
    active_blocks: &mut HashSet<u64>,
    pool_hits: &mut PoolHits,
    factory_pools: &mut HashMap<Address, DiscoveredPool>,
    provider_idx: Option<usize>,
) {
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
        return;
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
                        active_blocks.insert(bn);
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
                    pool_hits.entry(pool_addr).or_insert((
                        DexType::Curve, None, None, creation_block,
                    ));
                    factory_pools.entry(pool_addr).or_insert(
                        DiscoveredPool::new(pool_addr, Address::ZERO, Address::ZERO, 0, DexType::Curve, creation_block)
                            .with_factory(Some(log.address())));
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Curve factory scan failed for {current}..{batch_end}: {e:#}"
                );
            }
        }
    }
}