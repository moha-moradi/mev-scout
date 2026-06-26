use alloy::primitives::{Address, B256};
use serde::{Deserialize, Serialize};

use crate::pool::dex_type::DexType;
use crate::pool::discovery::DiscoveredPool;

/// A single subgraph endpoint with optional fee override and human-readable label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubgraphEndpoint {
    pub url: String,
    /// Fee override in bps (applied to V2 pools; V3 pools read fee from on-chain data).
    /// When None for V2, a default of 30 bps is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_override: Option<u32>,
    /// Optional human-readable label (e.g. "QuickSwap", "SushiSwap V2").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl SubgraphEndpoint {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            fee_override: None,
            label: None,
        }
    }

    pub fn with_fee(mut self, fee: u32) -> Self {
        self.fee_override = Some(fee);
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Collection of subgraph endpoints organized by DEX type/version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubgraphEndpoints {
    /// V2 fork endpoints (each can have its own fee override and label).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub v2: Vec<SubgraphEndpoint>,
    /// V3 endpoints (fee is read from pool data, fee_override is ignored).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub v3: Vec<SubgraphEndpoint>,
    /// Balancer endpoints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub balancer: Vec<SubgraphEndpoint>,
    /// Curve endpoints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curve: Vec<SubgraphEndpoint>,
}

impl Default for SubgraphEndpoints {
    fn default() -> Self {
        Self {
            v2: Vec::new(),
            v3: Vec::new(),
            balancer: Vec::new(),
            curve: Vec::new(),
        }
    }
}

/// Built-in subgraph endpoints for each chain, used as fallback when neither
/// the TOML config nor CLI overrides provide URLs.
///
/// Each DEX protocol gets its own endpoint with a distinct `fee_override` for V2
/// and an optional `label` so discovered pools can be traced back to their DEX.
pub fn builtin_endpoints(chain: &str) -> SubgraphEndpoints {
    use SubgraphEndpoint as E;
    match chain.to_lowercase().as_str() {
        "ethereum" => SubgraphEndpoints {
            v2: vec![
                E::new("https://api.thegraph.com/subgraphs/name/ianlapham/uniswapv2")
                    .with_fee(30).with_label("Uniswap V2"),
                E::new("https://api.thegraph.com/subgraphs/name/sushiswap/exchange")
                    .with_fee(30).with_label("SushiSwap V2"),
            ],
            v3: vec![
                E::new("https://api.thegraph.com/subgraphs/name/uniswap/uniswap-v3")
                    .with_label("Uniswap V3"),
            ],
            balancer: vec![
                E::new("https://api.thegraph.com/subgraphs/name/balancer-labs/balancer-v2")
                    .with_label("Balancer V2"),
            ],
            curve: vec![
                E::new("https://api.thegraph.com/subgraphs/name/curvefi/curve")
                    .with_label("Curve"),
            ],
        },
        "polygon" => SubgraphEndpoints {
            v2: vec![
                E::new("https://api.thegraph.com/subgraphs/name/sameepsi/quickswap06")
                    .with_fee(30).with_label("QuickSwap V2"),
                E::new("https://api.thegraph.com/subgraphs/name/sushiswap/matic-exchange")
                    .with_fee(30).with_label("SushiSwap V2"),
            ],
            v3: vec![
                E::new("https://api.thegraph.com/subgraphs/name/ianlapham/uniswap-v3-polygon")
                    .with_label("Uniswap V3"),
                E::new("https://api.thegraph.com/subgraphs/name/quickswap-layer2/quickswap-v3-polygon")
                    .with_label("QuickSwap V3"),
            ],
            balancer: vec![
                E::new("https://api.thegraph.com/subgraphs/name/balancer-labs/balancer-polygon-v2")
                    .with_label("Balancer V2"),
            ],
            curve: vec![],
        },
        "bsc" => SubgraphEndpoints {
            v2: vec![
                E::new("https://api.thegraph.com/subgraphs/name/pancakeswap/pairs")
                    .with_fee(25).with_label("PancakeSwap V2"),
            ],
            v3: vec![
                E::new("https://api.thegraph.com/subgraphs/name/pancakeswap/exchange-v3")
                    .with_label("PancakeSwap V3"),
            ],
            balancer: vec![],
            curve: vec![],
        },
        "arbitrum" => SubgraphEndpoints {
            v2: vec![
                E::new("https://api.thegraph.com/subgraphs/name/camelotlabs/camelot-arbitrum")
                    .with_fee(30).with_label("Camelot V2"),
            ],
            v3: vec![
                E::new("https://api.thegraph.com/subgraphs/name/ianlapham/arbitrum-dev")
                    .with_label("Uniswap V3"),
            ],
            balancer: vec![],
            curve: vec![],
        },
        "avalanche" => SubgraphEndpoints {
            v2: vec![
                E::new("https://api.thegraph.com/subgraphs/name/traderjoe-xyz/exchange")
                    .with_fee(30).with_label("Trader Joe V2"),
            ],
            v3: vec![
                E::new("https://api.thegraph.com/subgraphs/name/ianlapham/avalanche-dev")
                    .with_label("Uniswap V3"),
            ],
            balancer: vec![],
            curve: vec![],
        },
        "base" => SubgraphEndpoints {
            v2: vec![
                E::new("https://api.thegraph.com/subgraphs/name/ianlapham/base-v2")
                    .with_fee(30).with_label("Aerodrome"),
            ],
            v3: vec![],
            balancer: vec![],
            curve: vec![],
        },
        "optimism" => SubgraphEndpoints {
            v2: vec![
                E::new("https://api.thegraph.com/subgraphs/name/sushiswap/optimism")
                    .with_fee(30).with_label("SushiSwap V2"),
            ],
            v3: vec![
                E::new("https://api.thegraph.com/subgraphs/name/ianlapham/optimism-dev")
                    .with_label("Uniswap V3"),
            ],
            balancer: vec![],
            curve: vec![],
        },
        _ => SubgraphEndpoints::default(),
    }
}

/// GraphQL client for querying The Graph subgraphs.
pub struct SubgraphClient {
    inner: reqwest::Client,
}

impl Default for SubgraphClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SubgraphClient {
    pub fn new() -> Self {
        Self {
            inner: reqwest::Client::builder()
                .user_agent("mev-scout/0.1")
                .build()
                .expect("reqwest Client::new"),
        }
    }

    async fn query_raw(&self, url: &str, query: &str) -> anyhow::Result<serde_json::Value> {
        let body = serde_json::json!({ "query": query });
        let resp = self.inner.post(url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("Subgraph {} returned HTTP {}: {}", url, status, text);
        }
        let value: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(errors) = value.get("errors") {
            anyhow::bail!("Subgraph {} error: {}", url, errors);
        }
        value
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Subgraph response missing 'data' field"))
    }
}

// ---------------------------------------------------------------------------
// GraphQL response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct V2PairsData {
    pairs: Vec<V2PairEntry>,
}

#[derive(Debug, Deserialize)]
struct V2PairEntry {
    id: String,
    token0: TokenId,
    token1: TokenId,
    #[serde(default)]
    created_at_block_number: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenId {
    id: String,
}

#[derive(Debug, Deserialize)]
struct V3PoolsData {
    pools: Vec<V3PoolEntry>,
}

#[derive(Debug, Deserialize)]
struct V3PoolEntry {
    id: String,
    token0: TokenId,
    token1: TokenId,
    fee_tier: String,
    #[serde(default)]
    tick_spacing: Option<String>,
    #[serde(default)]
    created_at_block_number: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BalancerPoolsData {
    pools: Vec<BalancerPoolEntry>,
}

#[derive(Debug, Deserialize)]
struct BalancerPoolEntry {
    id: String,
    #[serde(default)]
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CurvePoolsData {
    pools: Vec<CurvePoolEntry>,
}

#[derive(Debug, Deserialize)]
struct CurvePoolEntry {
    id: String,
    #[serde(default)]
    address: Option<String>,
}

// ---------------------------------------------------------------------------
// Discovery functions
// ---------------------------------------------------------------------------

const PAGE_SIZE: i32 = 1000;

/// Discover Uniswap V2 (or fork) pools from a subgraph.
///
/// `fee_override` is applied to all discovered pools (e.g. 30 for 0.30%,
/// 25 for PancakeSwap 0.25%).
pub async fn discover_v2_pools_from_subgraph(
    client: &SubgraphClient,
    url: &str,
    fee_override: u32,
    dex_type_label: &str,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let query_template = r#"
{{
  pairs(first: {page_size}, skip: {skip}, orderBy: createdAtBlockNumber, orderDirection: asc) {{
    id
    token0 {{ id }}
    token1 {{ id }}
    createdAtBlockNumber
  }}
}}
"#;

    let mut all_pools = Vec::new();
    let mut skip: i32 = 0;

    loop {
        let query = query_template
            .replace("{page_size}", &PAGE_SIZE.to_string())
            .replace("{skip}", &skip.to_string());

        let data = client.query_raw(url, &query).await?;
        let pairs_data: V2PairsData = serde_json::from_value(data)?;

        let batch: Vec<DiscoveredPool> = pairs_data
            .pairs
            .iter()
            .filter_map(|p| {
                let addr = p.id.parse::<Address>().ok()?;
                let token0 = p.token0.id.parse::<Address>().ok()?;
                let token1 = p.token1.id.parse::<Address>().ok()?;
                let creation_block = p
                    .created_at_block_number
                    .as_deref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                Some(DiscoveredPool {
                    address: addr,
                    token0,
                    token1,
                    fee: fee_override,
                    tick_spacing: None,
                    dex_type: DexType::UniswapV2,
                    creation_block,
                    pool_id: None,
                    factory: None,
                })
            })
            .collect();

        let batch_len = batch.len();
        all_pools.extend(batch);

        if batch_len < PAGE_SIZE as usize {
            break;
        }
        skip += PAGE_SIZE;
    }

    tracing::info!(
        "Subgraph V2 ({dex_type_label}): discovered {} pools from {}",
        all_pools.len(),
        url
    );
    Ok(all_pools)
}

/// Discover Uniswap V3 pools from a subgraph.
pub async fn discover_v3_pools_from_subgraph(
    client: &SubgraphClient,
    url: &str,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let query_template = r#"
{{
  pools(first: {page_size}, skip: {skip}, orderBy: createdAtBlockNumber, orderDirection: asc) {{
    id
    token0 {{ id }}
    token1 {{ id }}
    feeTier
    tickSpacing
    createdAtBlockNumber
  }}
}}
"#;

    let mut all_pools = Vec::new();
    let mut skip: i32 = 0;

    loop {
        let query = query_template
            .replace("{page_size}", &PAGE_SIZE.to_string())
            .replace("{skip}", &skip.to_string());

        let data = client.query_raw(url, &query).await?;
        let pools_data: V3PoolsData = serde_json::from_value(data)?;

        let batch: Vec<DiscoveredPool> = pools_data
            .pools
            .iter()
            .filter_map(|p| {
                let addr = p.id.parse::<Address>().ok()?;
                let token0 = p.token0.id.parse::<Address>().ok()?;
                let token1 = p.token1.id.parse::<Address>().ok()?;
                let fee = p.fee_tier.parse::<u32>().ok()?;
                let tick_spacing = p
                    .tick_spacing
                    .as_deref()
                    .and_then(|s| s.parse::<i32>().ok());
                let creation_block = p
                    .created_at_block_number
                    .as_deref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                Some(DiscoveredPool {
                    address: addr,
                    token0,
                    token1,
                    fee,
                    tick_spacing,
                    dex_type: DexType::UniswapV3,
                    creation_block,
                    pool_id: None,
                    factory: None,
                })
            })
            .collect();

        let batch_len = batch.len();
        all_pools.extend(batch);

        if batch_len < PAGE_SIZE as usize {
            break;
        }
        skip += PAGE_SIZE;
    }

    tracing::info!(
        "Subgraph V3: discovered {} pools from {}",
        all_pools.len(),
        url
    );
    Ok(all_pools)
}

/// Discover Balancer V2 pools (Weighted and Stable) from a subgraph.
pub async fn discover_balancer_pools_from_subgraph(
    client: &SubgraphClient,
    url: &str,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let query_template = r#"
{{
  pools(
    first: {page_size}
    skip: {skip}
    where: {{ poolType_in: ["Weighted", "Stable"] }}
    orderBy: createTime
    orderDirection: asc
  ) {{
    id
    address
    poolType
  }}
}}
"#;

    let mut all_pools = Vec::new();
    let mut skip: i32 = 0;

    loop {
        let query = query_template
            .replace("{page_size}", &PAGE_SIZE.to_string())
            .replace("{skip}", &skip.to_string());

        let data = client.query_raw(url, &query).await?;
        let pools_data: BalancerPoolsData = serde_json::from_value(data)?;

        let batch: Vec<DiscoveredPool> = pools_data
            .pools
            .iter()
            .filter_map(|p| {
                let pool_addr_str = p.address.as_deref().unwrap_or(&p.id);
                let addr = pool_addr_str.parse::<Address>().ok()?;
                let pool_id_bytes = if p.id.starts_with("0x") && p.id.len() == 66 {
                    p.id.parse::<B256>().unwrap_or_default()
                } else {
                    B256::default()
                };
                Some(DiscoveredPool {
                    address: addr,
                    token0: Address::ZERO,
                    token1: Address::ZERO,
                    fee: 0,
                    tick_spacing: None,
                    dex_type: DexType::Balancer,
                    creation_block: 0,
                    pool_id: Some(pool_id_bytes.0),
                    factory: None,
                })
            })
            .collect();

        let batch_len = batch.len();
        all_pools.extend(batch);

        if batch_len < PAGE_SIZE as usize {
            break;
        }
        skip += PAGE_SIZE;
    }

    tracing::info!(
        "Subgraph Balancer: discovered {} pools from {}",
        all_pools.len(),
        url
    );
    Ok(all_pools)
}

/// Discover Curve pools from a subgraph.
pub async fn discover_curve_pools_from_subgraph(
    client: &SubgraphClient,
    url: &str,
) -> anyhow::Result<Vec<DiscoveredPool>> {
    let query_template = r#"
{{
  pools(first: {page_size}, skip: {skip}) {{
    id
    address
  }}
}}
"#;

    let mut all_pools = Vec::new();
    let mut skip: i32 = 0;

    loop {
        let query = query_template
            .replace("{page_size}", &PAGE_SIZE.to_string())
            .replace("{skip}", &skip.to_string());

        let data = client.query_raw(url, &query).await?;
        let pools_data: CurvePoolsData = serde_json::from_value(data)?;

        let batch: Vec<DiscoveredPool> = pools_data
            .pools
            .iter()
            .filter_map(|p| {
                let pool_addr_str = p.address.as_deref().unwrap_or(&p.id);
                let addr = pool_addr_str.parse::<Address>().ok()?;
                Some(DiscoveredPool {
                    address: addr,
                    token0: Address::ZERO,
                    token1: Address::ZERO,
                    fee: 0,
                    tick_spacing: None,
                    dex_type: DexType::Curve,
                    creation_block: 0,
                    pool_id: None,
                    factory: None,
                })
            })
            .collect();

        let batch_len = batch.len();
        all_pools.extend(batch);

        if batch_len < PAGE_SIZE as usize {
            break;
        }
        skip += PAGE_SIZE;
    }

    tracing::info!(
        "Subgraph Curve: discovered {} pools from {}",
        all_pools.len(),
        url
    );
    Ok(all_pools)
}
