//! DefiLlama REST client — free aggregator fallback.
//!
//! Uses `GET https://yields.llama.fi/pools` (chain-filtered) and
//! `GET https://api.llama.fi/overview/dexs/{chain}`.
//! Whitelisted pools only, no fee/tickSpacing → cross-check tier primarily.
//! Symbol resolution is not available, so we return RemotePools with
//! underlying_tokens when possible and TVL for ranking.

use std::time::Duration;

use alloy::primitives::Address;
use serde_json::Value;

use crate::dex_type::DexType;

use super::RemotePool;

pub struct DefiLlamaClient {
    client: reqwest::Client,
    base_url: String,
}

impl DefiLlamaClient {
    pub fn new() -> Self {
        Self::with_base("https://yields.llama.fi".to_string())
    }

    pub fn with_base(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("mev-scout/0.1")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client, base_url }
    }

    /// Fetch pools for a chain via DefiLlama yields API.
    pub async fn fetch_pools(
        &self,
        chain: &str,
        max_pools: Option<usize>,
        min_tvl: Option<f64>,
    ) -> anyhow::Result<Vec<RemotePool>> {
        let limit = max_pools.unwrap_or(1000);
        let url = format!("{}/pools", self.base_url);

        let resp = self.get_with_retry(&url).await?;
        let pools = parse_defillama_response(&resp, chain, min_tvl)?;

        let mut out = pools;
        if out.len() > limit {
            out.truncate(limit);
        }
        // Sort by TVL descending
        out.sort_by(|a, b| b.tvl_usd.partial_cmp(&a.tvl_usd).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out)
    }

    async fn get_with_retry(&self, url: &str) -> anyhow::Result<Value> {
        const MAX_RETRIES: u32 = 3;
        let mut last_err = None;
        for attempt in 0..MAX_RETRIES {
            match self.client.get(url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let text = resp.text().await.unwrap_or_default();
                    let json: Value = serde_json::from_str(&text)
                        .map_err(|e| anyhow::anyhow!("invalid JSON from DefiLlama: {} — {}", e, &text[..text.len().min(500)]))?;
                    return Ok(json);
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let msg = format!("HTTP {} from DefiLlama: {}", status.as_u16(), &body[..body.len().min(300)]);
                    if status.as_u16() == 429 && attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(Duration::from_millis(600 * 2u64.pow(attempt))).await;
                        last_err = Some(anyhow::anyhow!(msg));
                        continue;
                    }
                    last_err = Some(anyhow::anyhow!(msg));
                    break;
                }
                Err(e) => {
                    let msg = format!("DefiLlama request failed: {:#}", e);
                    if (e.is_timeout() || e.is_connect()) && attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
                        last_err = Some(anyhow::anyhow!(msg));
                        continue;
                    }
                    last_err = Some(anyhow::anyhow!(msg));
                    break;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("DefiLlama request failed")))
    }
}

impl Default for DefiLlamaClient {
    fn default() -> Self { Self::new() }
}

fn parse_defillama_response(json: &Value, chain: &str, min_tvl: Option<f64>) -> anyhow::Result<Vec<RemotePool>> {
    let data = json.get("data").and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing data array"))?;

    let mut out = Vec::new();
    for item in data {
        let item_chain = item.get("chain").and_then(|v| v.as_str()).unwrap_or("");
        if !item_chain.eq_ignore_ascii_case(chain) {
            continue;
        }

        let tvl = item.get("tvlUsd").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if let Some(min) = min_tvl {
            if tvl < min { continue; }
        }
        if tvl == 0.0 { continue; }

        // Try to get underlying tokens
        let underlying = item.get("underlyingTokens")
            .or_else(|| item.get("underlying_tokens"))
            .and_then(|v| v.as_array());

        let mut tokens: Vec<Address> = Vec::new();
        if let Some(arr) = underlying {
            for tok in arr {
                if let Some(s) = tok.as_str() {
                    if let Some(a) = parse_addr(s) { tokens.push(a); }
                }
            }
        }

        if tokens.len() < 2 { continue; }

        // Pool address: try `pool` field if it looks like an address, else skip
        // DefiLlama pool ids are often like "aaa-bbb" not hex, so we cannot construct pool address reliably.
        // Use synthetic address derived from pool id hash if needed? For now require valid pool address.
        let pool_id = item.get("pool").and_then(|v| v.as_str()).unwrap_or("");
        let mut pool_addr = parse_addr(pool_id);

        // Alternative: if pool id not address, try to synthesize via token hash? Skip for now.
        // Some entries have `pool` as address-like after stripping prefix.
        if pool_addr.is_none() {
            // Try extracting hex from pool id (e.g., "0xabc...")
            if let Some(hex) = pool_id.rfind("0x") {
                pool_addr = parse_addr(&pool_id[hex..]);
            }
        }
        let pool_addr = match pool_addr {
            Some(a) if !a.is_zero() => a,
            _ => continue, // skip entries without real pool address — tvl-only cross-check would need synthetic
        };

        let token0 = tokens[0];
        let token1 = tokens[1];

        let project = item.get("project").and_then(|v| v.as_str()).unwrap_or("defillama");
        let dex_type = match project.to_ascii_lowercase().as_str() {
            s if s.contains("uniswap") => DexType::UniswapV3,
            s if s.contains("quickswap") => DexType::UniswapV2,
            s if s.contains("balancer") => DexType::Balancer,
            s if s.contains("curve") => DexType::Curve,
            _ => DexType::UniswapV2,
        };

        out.push(RemotePool {
            address: pool_addr,
            token0,
            token1,
            fee: 0,
            tick_spacing: None,
            dex_type,
            dex_name: Some(project.to_string()),
            token0_symbol: None,
            token1_symbol: None,
            tvl_usd: Some(tvl),
            volume_usd_24h: None,
            volume_usd_30d: None,
            underlying_tokens: Some(tokens),
            creation_block: 0,
        });
    }
    Ok(out)
}

fn parse_addr(s: &str) -> Option<Address> {
    let s = s.trim();
    let hex = s.trim_start_matches("0x").trim_start_matches("0X");
    if hex.len() != 40 { return None; }
    let mut bytes = [0u8; 20];
    hex::decode_to_slice(hex, &mut bytes).ok()?;
    Some(Address::from_slice(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_defillama_single() {
        let json = json!({
            "data": [{
                "chain": "Polygon",
                "project": "quickswap",
                "pool": "0x1234567890123456789012345678901234567890",
                "tvlUsd": 1500000.0,
                "underlyingTokens": [
                    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                ]
            }]
        });
        let pools = parse_defillama_response(&json, "Polygon", None).unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].tvl_usd, Some(1500000.0));
    }

    #[test]
    fn test_parse_defillama_chain_filter() {
        let json = json!({
            "data": [
                {"chain": "Polygon", "project": "quickswap", "pool": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "tvlUsd": 1000.0, "underlyingTokens": ["0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]},
                {"chain": "Ethereum", "project": "uniswap", "pool": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "tvlUsd": 1000.0, "underlyingTokens": ["0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]}
            ]
        });
        let pools = parse_defillama_response(&json, "Polygon", None).unwrap();
        assert_eq!(pools.len(), 1);
    }

    #[test]
    fn test_min_tvl_filter() {
        let json = json!({
            "data": [{
                "chain": "Polygon",
                "project": "quickswap",
                "pool": "0x1234567890123456789012345678901234567890",
                "tvlUsd": 100.0,
                "underlyingTokens": ["0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]
            }]
        });
        let pools = parse_defillama_response(&json, "Polygon", Some(1000.0)).unwrap();
        assert_eq!(pools.len(), 0);
    }

    #[tokio::test]
    async fn test_defillama_mock() {
        let mock_server = wiremock::MockServer::start().await;
        let body = serde_json::json!({
            "data": [{
                "chain": "Polygon",
                "project": "quickswap",
                "pool": "0x1111111111111111111111111111111111111111",
                "tvlUsd": 5000000.0,
                "underlyingTokens": ["0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .mount(&mock_server)
            .await;

        let client = DefiLlamaClient::with_base(mock_server.uri());
        let pools = client.fetch_pools("Polygon", Some(10), None).await.unwrap();
        assert_eq!(pools.len(), 1);
    }
}
