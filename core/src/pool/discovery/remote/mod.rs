//! Remote pool sourcing — free aggregator client (no API key required).
//!
//! GeckoTerminal provides chain-wide top-pool listings with TVL and volume
//! metadata. Every `RemotePool` is converted into a `DiscoveredPool` via
//! `From<RemotePool>` and then deduped by address with on-chain results
//! using `DiscoveredPool::merge_from`.
//!
//! All remote fetching is best-effort: a failing source logs a warning and
//! is skipped. Discovery never hard-fails because the remote source is down.
//!
//! Note: DefiLlama's yields API was evaluated and removed — it identifies
//! pools by UUID without on-chain contract addresses, so its entries cannot
//! become usable `DiscoveredPool`s.

pub mod dexscreener;
pub mod geckoterminal;

use alloy::primitives::Address;

use crate::dex_type::DexType;
use crate::pool::discovery::DiscoveredPool;

/// Pool metadata returned by a remote source.
///
/// Mirrors the fields of `DiscoveredPool` but is decoupled so that remote
/// parsing does not depend on storage details.
#[derive(Debug, Clone)]
pub struct RemotePool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: Option<i32>,
    pub dex_type: DexType,
    pub dex_name: Option<String>,
    pub token0_symbol: Option<String>,
    pub token1_symbol: Option<String>,
    pub tvl_usd: Option<f64>,
    pub volume_usd_24h: Option<f64>,
    pub volume_usd_30d: Option<f64>,
    /// Full token list for multi-token pools (Curve 3+, Balancer 2-8).
    pub underlying_tokens: Option<Vec<Address>>,
    /// Aggregators do not expose creation blocks; always 0 (incremental mode stays RPC-only).
    pub creation_block: u64,
}

impl From<RemotePool> for DiscoveredPool {
    fn from(r: RemotePool) -> Self {
        DiscoveredPool::new(r.address, r.token0, r.token1, r.fee, r.dex_type, r.creation_block)
            .with_tick_spacing(r.tick_spacing)
            .with_dex_name(r.dex_name)
            .with_token0_symbol(r.token0_symbol)
            .with_token1_symbol(r.token1_symbol)
            .with_tvl_usd(r.tvl_usd)
            .with_volume_usd_24h(r.volume_usd_24h)
            .with_volume_usd_30d(r.volume_usd_30d)
            .with_underlying_tokens(r.underlying_tokens)
    }
}

/// Fetch pools from GeckoTerminal (free, no key).
///
/// When the chain-wide top-pools query under-delivers (fewer pools than the
/// requested cap), the GeckoTerminal per-DEX ladder rung tops up the result:
/// each DEX endpoint on the network is queried and its label classifies the
/// pools by construction.
pub async fn discover_via_geckoterminal(
    chain: &str,
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> Vec<DiscoveredPool> {
    let client = geckoterminal::GeckoTerminalClient::new();
    match client.fetch_top_pools(chain, max_pools, min_tvl).await {
        Ok(pools) if (max_pools.is_none() || pools.len() < max_pools.unwrap_or(0)) => {
            let cap = max_pools.unwrap_or(pools.len());
            tracing::info!(
                "GeckoTerminal chain-wide query returned {}/{} pools — trying per-DEX fallback",
                pools.len(),
                cap
            );
            match supplement_via_per_dex(&client, chain, pools, max_pools, min_tvl).await {
                Ok(p) => p.into_iter().map(|r| r.into()).collect(),
                Err(e) => {
                    tracing::warn!("GeckoTerminal per-DEX fallback failed: {:#}", e);
                    Vec::new()
                }
            }
        }
        Ok(pools) => pools.into_iter().map(|r| r.into()).collect(),
        Err(e) => {
            tracing::warn!("GeckoTerminal fetch failed: {:#}", e);
            Vec::new()
        }
    }
}

/// Per-DEX ladder rung: enumerate the network's DEXes and query each one's
/// pool list until `max_pools` is reached. Best-effort — a failing DEX is skipped.
async fn supplement_via_per_dex(
    client: &geckoterminal::GeckoTerminalClient,
    chain: &str,
    mut pools: Vec<RemotePool>,
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> anyhow::Result<Vec<RemotePool>> {
    let cap = max_pools.unwrap_or(1000);
    if pools.len() >= cap {
        return Ok(pools);
    }
    let dexes = client.fetch_network_dexes(chain).await?;
    let seen: std::collections::HashSet<Address> = pools.iter().map(|p| p.address).collect();
    for dex in &dexes {
        if pools.len() >= cap {
            break;
        }
        let per_dex_cap = (cap - pools.len()).min(200);
        match client.fetch_pools_for_dex(chain, dex, Some(per_dex_cap), min_tvl).await {
            Ok(extra) => {
                let added = extra.len();
                for p in extra {
                    if !seen.contains(&p.address) {
                        pools.push(p);
                    }
                }
                if added > 0 {
                    tracing::debug!("Per-DEX '{dex}': +{added} pool(s)");
                }
            }
            Err(e) => {
                tracing::debug!("Per-DEX '{dex}' fetch failed, skipping: {e:#}");
            }
        }
    }
    // Re-sort by TVL so the merged set keeps the explorer-like ordering.
    pools.sort_by(|a, b| b.tvl_usd.partial_cmp(&a.tvl_usd).unwrap_or(std::cmp::Ordering::Equal));
    pools.truncate(cap);
    Ok(pools)
}

/// Fetch pools from DexScreener (free, no key) — redundancy source.
pub async fn discover_via_dexscreener(
    chain: &str,
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> Vec<DiscoveredPool> {
    let client = dexscreener::DexScreenerClient::new();
    match client.fetch_pools(chain, max_pools, min_tvl).await {
        Ok(pools) => pools.into_iter().map(|r| r.into()).collect(),
        Err(e) => {
            tracing::warn!("DexScreener fetch failed: {:#}", e);
            Vec::new()
        }
    }
}

/// Fetch pools from all free remote sources, unioned by address.
///
/// GeckoTerminal results take precedence per-field (`merge_from` fills only
/// missing fields); DexScreener adds unique addresses and backfills gaps, so a
/// single source going down degrades coverage instead of zeroing it out.
pub async fn discover_via_remote(
    chain: &str,
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> Vec<DiscoveredPool> {
    let mut pools = discover_via_geckoterminal(chain, max_pools, min_tvl).await;
    let extra = discover_via_dexscreener(chain, max_pools, min_tvl).await;
    if !extra.is_empty() {
        pools = merge_pools(pools, extra);
        pools.sort_by(|a, b| {
            b.tvl_usd
                .partial_cmp(&a.tvl_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    if pools.is_empty() {
        tracing::warn!("All remote pool sources returned 0 pools for chain '{chain}'");
    }
    pools
}

/// Union helper: merge multiple `DiscoveredPool` vecs by address using
/// `merge_from`. HashMap-indexed — O(n) instead of the previous O(n²) scan.
pub fn merge_pools(mut base: Vec<DiscoveredPool>, extra: Vec<DiscoveredPool>) -> Vec<DiscoveredPool> {
    use std::collections::HashMap;
    let mut index: HashMap<Address, usize> =
        base.iter().enumerate().map(|(i, p)| (p.address, i)).collect();
    for p in extra {
        match index.get(&p.address) {
            Some(&i) => base[i].merge_from(&p),
            None => {
                index.insert(p.address, base.len());
                base.push(p);
            }
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_pools_dedup() {
        use alloy::primitives::address;
        let a = address!("1111111111111111111111111111111111111111");
        let b = address!("2222222222222222222222222222222222222222");
        let t0 = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let t1 = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let base = vec![DiscoveredPool::new(a, t0, t1, 3000, DexType::UniswapV3, 1).with_tvl_usd(Some(100.0))];
        let extra = vec![
            DiscoveredPool::new(a, t0, t1, 3000, DexType::UniswapV3, 0).with_volume_usd_24h(Some(50.0)),
            DiscoveredPool::new(b, t0, t1, 3000, DexType::UniswapV3, 2),
        ];
        let merged = merge_pools(base, extra);
        assert_eq!(merged.len(), 2);
        let first = merged.iter().find(|p| p.address == a).unwrap();
        assert_eq!(first.tvl_usd, Some(100.0));
        assert_eq!(first.volume_usd_24h, Some(50.0));
    }
}
