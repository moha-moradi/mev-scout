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
pub async fn discover_via_geckoterminal(
    chain: &str,
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> Vec<DiscoveredPool> {
    let client = geckoterminal::GeckoTerminalClient::new();
    match client.fetch_top_pools(chain, max_pools, min_tvl).await {
        Ok(pools) => pools.into_iter().map(|r| r.into()).collect(),
        Err(e) => {
            tracing::warn!("GeckoTerminal fetch failed: {:#}", e);
            Vec::new()
        }
    }
}

/// Union helper: merge multiple `DiscoveredPool` vecs by address using `merge_from`.
pub fn merge_pools(mut base: Vec<DiscoveredPool>, extra: Vec<DiscoveredPool>) -> Vec<DiscoveredPool> {
    for p in extra {
        if let Some(existing) = base.iter_mut().find(|e| e.address == p.address) {
            existing.merge_from(&p);
        } else {
            base.push(p);
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
