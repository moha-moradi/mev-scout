//! Remote pool sourcing — off-chain subgraph + aggregator clients.
//!
//! Primary sources are The Graph subgraphs (one per DEX, config-driven via
//! `chains.toml [[<chain>.subgraphs]]`). Fallback tier is free aggregators
//! (GeckoTerminal, DefiLlama). Every `RemotePool` is converted into a
//! `DiscoveredPool` via `From<RemotePool>` and then deduped by address with
//! on-chain results using `DiscoveredPool::merge_from`.
//!
//! All remote fetching is best-effort: a failing source logs a warning and
//! is skipped. Discovery never hard-fails because a remote source is down.
//! If every remote source fails, the caller falls back to pure RPC.

pub mod defillama;
pub mod geckoterminal;
pub mod graphql;
pub mod schemas;

use std::collections::HashSet;

use alloy::primitives::Address;

use crate::dex_type::DexType;
use crate::pool::discovery::DiscoveredPool;
use crate::types::{SubgraphConfig, SubgraphSchema};

/// Pool metadata returned by a remote source (subgraph or aggregator).
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
    /// Creation block if the subgraph exposes it, else 0 (incremental mode stays RPC-only).
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

/// Expand `${GRAPH_API_KEY}` templates and filter gateway URLs when the env var is absent.
///
/// Returns the list of URLs that are actually tryable. If `GRAPH_API_KEY` is
/// set via env var it overrides any TOML value; if unset, any URL containing
/// the template is skipped (hosted/Goldsky URLs still tried).
pub fn expand_urls(urls: &[String]) -> Vec<String> {
    let key = std::env::var("GRAPH_API_KEY").ok();
    let mut out = Vec::new();
    for raw in urls {
        if raw.contains("${GRAPH_API_KEY}") || raw.contains("$GRAPH_API_KEY") {
            if let Some(ref k) = key {
                if k.is_empty() {
                    continue;
                }
                let expanded = raw
                    .replace("${GRAPH_API_KEY}", k)
                    .replace("$GRAPH_API_KEY", k);
                out.push(expanded);
            } else {
                tracing::debug!("Skipping gateway URL (GRAPH_API_KEY not set): {}", raw);
            }
        } else {
            out.push(raw.clone());
        }
    }
    out
}

/// Resolve DexType from a `SubgraphConfig`.
///
/// Priority: explicit `dex_type` string → schema default.
pub fn dex_type_for_config(cfg: &SubgraphConfig) -> DexType {
    if let Some(ref s) = cfg.dex_type {
        if let Ok(dt) = s.parse::<DexType>() {
            return dt;
        }
        // Map common aliases that strum doesn't cover
        match s.to_ascii_lowercase().as_str() {
            "uniswap_v3" | "uniswapv3" | "v3" => return DexType::UniswapV3,
            "uniswap_v2" | "uniswapv2" | "v2" => return DexType::UniswapV2,
            "balancer" | "balancer_v2" => return DexType::Balancer,
            "curve" => return DexType::Curve,
            "solidly" => return DexType::Solidly,
            "camelot" => return DexType::Camelot,
            _ => {}
        }
    }
    match cfg.schema {
        SubgraphSchema::UniswapV2 => DexType::UniswapV2,
        SubgraphSchema::UniswapV3 | SubgraphSchema::Algebra => DexType::UniswapV3,
        SubgraphSchema::BalancerV2 => DexType::Balancer,
        SubgraphSchema::Curve => DexType::Curve,
    }
}

/// Orchestrator: fetch pools from all configured subgraphs, converting to `DiscoveredPool`.
///
/// * `subgraphs` — ordered list from `chains.toml` or `ChainName::default_subgraphs()`.
/// * `max_pools` — per-subgraph pagination cap (None = unbounded, typically 1000).
/// * `min_tvl` — optional TVL floor (pools below are filtered client-side and via query).
///
/// Failures per subgraph are logged and skipped; the overall result is the union
/// of all successful sources. Returns empty vec if nothing succeeded (caller should
/// fall back to RPC).
pub async fn discover_via_remote(
    subgraphs: &[SubgraphConfig],
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> Vec<DiscoveredPool> {
    if subgraphs.is_empty() {
        tracing::info!("Remote discovery: no subgraphs configured, skipping");
        return Vec::new();
    }

    let mut all: Vec<DiscoveredPool> = Vec::new();
    let mut seen: HashSet<Address> = HashSet::new();

    for cfg in subgraphs {
        let dex_type = dex_type_for_config(cfg);
        let urls = expand_urls(&cfg.urls);
        if urls.is_empty() {
            tracing::warn!(
                "Remote discovery: no tryable URLs for {} (schema {:?}) — skipping",
                cfg.dex_name, cfg.schema
            );
            continue;
        }

        tracing::info!(
            "Remote discovery: querying {} (schema {:?}, {} URLs, dex_type={})",
            cfg.dex_name, cfg.schema, urls.len(), dex_type
        );

        let client = graphql::GraphClient::new(
            urls.clone(),
            cfg.schema.clone(),
            dex_type,
            cfg.dex_name.clone(),
        );

        let pools = match client.fetch_pools(max_pools, min_tvl).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "Remote discovery: {} failed (tried {} URLs): {:#}",
                    cfg.dex_name,
                    urls.len(),
                    e
                );
                continue;
            }
        };

        tracing::info!("Remote discovery: {} returned {} pools", cfg.dex_name, pools.len());

        for rp in pools {
            if seen.insert(rp.address) {
                all.push(rp.into());
            }
        }
    }

    tracing::info!("Remote discovery: total {} unique pools from {} subgraphs", all.len(), subgraphs.len());
    all
}

/// Fetch pools from GeckoTerminal fallback (free, no key).
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

/// Fetch pools from DefiLlama fallback (free, no key).
pub async fn discover_via_defillama(
    chain: &str,
    max_pools: Option<usize>,
    min_tvl: Option<f64>,
) -> Vec<DiscoveredPool> {
    let client = defillama::DefiLlamaClient::new();
    match client.fetch_pools(chain, max_pools, min_tvl).await {
        Ok(pools) => pools.into_iter().map(|r| r.into()).collect(),
        Err(e) => {
            tracing::warn!("DefiLlama fetch failed: {:#}", e);
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
    fn test_expand_urls_without_key_skips_gateway() {
        // Ensure no env var set for this test — save and restore
        let saved = std::env::var("GRAPH_API_KEY").ok();
        std::env::remove_var("GRAPH_API_KEY");

        let urls = vec![
            "https://gateway.thegraph.com/api/${GRAPH_API_KEY}/subgraphs/id/abc".to_string(),
            "https://api.thegraph.com/subgraphs/name/foo/bar".to_string(),
        ];
        let expanded = expand_urls(&urls);
        assert_eq!(expanded.len(), 1);
        assert!(expanded[0].contains("api.thegraph.com"));

        if let Some(v) = saved { std::env::set_var("GRAPH_API_KEY", v); }
    }

    #[test]
    fn test_expand_urls_with_key_expands() {
        let saved = std::env::var("GRAPH_API_KEY").ok();
        std::env::set_var("GRAPH_API_KEY", "test123");

        let urls = vec![
            "https://gateway.thegraph.com/api/${GRAPH_API_KEY}/subgraphs/id/abc".to_string(),
            "https://api.thegraph.com/subgraphs/name/foo/bar".to_string(),
        ];
        let expanded = expand_urls(&urls);
        assert_eq!(expanded.len(), 2);
        assert!(expanded[0].contains("test123"));
        assert!(!expanded[0].contains("${GRAPH_API_KEY}"));

        if let Some(v) = saved { std::env::set_var("GRAPH_API_KEY", v); } else { std::env::remove_var("GRAPH_API_KEY"); }
    }

    #[test]
    fn test_dex_type_for_config_explicit() {
        let cfg = SubgraphConfig {
            dex_name: "Bal".into(),
            dex_type: Some("balancer".into()),
            schema: SubgraphSchema::UniswapV3,
            urls: vec![],
        };
        assert_eq!(dex_type_for_config(&cfg), DexType::Balancer);
    }

    #[test]
    fn test_dex_type_for_config_schema_fallback() {
        let cfg = SubgraphConfig {
            dex_name: "Curve".into(),
            dex_type: None,
            schema: SubgraphSchema::Curve,
            urls: vec![],
        };
        assert_eq!(dex_type_for_config(&cfg), DexType::Curve);
    }

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
