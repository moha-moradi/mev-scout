//! GeckoTerminal REST client — free aggregator fallback.
//!
//! `GET https://api.geckoterminal.com/api/v2/networks/{network}/pools`
//! Paginated, sorted by `h_tvl` or `h24_volume`. No auth, ~30 req/min.
//! Each pool entry carries dex label, TVL, volume, and token addresses.

use std::time::Duration;

use alloy::primitives::Address;
use serde_json::Value;

use crate::dex_type::DexType;

use super::RemotePool;

/// Map chain name (our internal) → GeckoTerminal network slug.
fn network_slug(chain: &str) -> &str {
    match chain.to_ascii_lowercase().as_str() {
        "polygon" => "polygon_pos",
        "ethereum" | "eth" => "eth",
        "bsc" => "bsc",
        "arbitrum" => "arbitrum",
        "base" => "base",
        "avalanche" => "avax",
        "optimism" => "optimism",
        _ => "polygon_pos",
    }
}

/// GeckoTerminal client.
pub struct GeckoTerminalClient {
    client: reqwest::Client,
    base_url: String,
}

impl GeckoTerminalClient {
    pub fn new() -> Self {
        Self::with_base("https://api.geckoterminal.com".to_string())
    }

    pub fn with_base(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("mev-scout/0.1")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client, base_url }
    }

    /// Fetch top pools for a chain, up to `max_pools` (default 1000), filtered by `min_tvl`.
    pub async fn fetch_top_pools(
        &self,
        chain: &str,
        max_pools: Option<usize>,
        min_tvl: Option<f64>,
    ) -> anyhow::Result<Vec<RemotePool>> {
        let network = network_slug(chain);
        let limit = max_pools.unwrap_or(1000);
        let mut pools = Vec::new();
        let mut page = 1usize;

        loop {
            let url = format!(
                "{}/api/v2/networks/{}/pools?page={}&sort=h_tvl",
                self.base_url, network, page
            );

            let resp = self.get_with_retry(&url).await?;
            let batch = parse_geckoterminal_response(&resp, min_tvl)?;

            let empty = batch.is_empty();
            pools.extend(batch);
            if pools.len() >= limit {
                pools.truncate(limit);
                break;
            }
            if empty {
                break;
            }

            // GeckoTerminal paginates 20 per page; stop if we got less than page size
            if let Some(data) = resp.get("data").and_then(|v| v.as_array()) {
                if data.len() < 20 {
                    break;
                }
            }

            page += 1;
            if page > 50 {
                break;
            } // safety cap: 1000 pools max via pagination

            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Ok(pools)
    }

    async fn get_with_retry(&self, url: &str) -> anyhow::Result<Value> {
        const MAX_RETRIES: u32 = 3;
        const BASE_DELAY_MS: u64 = 600;
        let mut last_err = None;

        for attempt in 0..MAX_RETRIES {
            match self.client.get(url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let text = resp.text().await.unwrap_or_default();
                        let json: Value = serde_json::from_str(&text)
                            .map_err(|e| anyhow::anyhow!("invalid JSON from GeckoTerminal: {} — {}", e, &text[..text.len().min(500)]))?;
                        return Ok(json);
                    }
                    let body = resp.text().await.unwrap_or_default();
                    let msg = format!("HTTP {} from GeckoTerminal: {}", status.as_u16(), &body[..body.len().min(300)]);
                    if status.as_u16() == 429 && attempt + 1 < MAX_RETRIES {
                        let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                        tracing::debug!("GeckoTerminal 429, retry {}/{} after {}ms", attempt + 1, MAX_RETRIES, delay);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        last_err = Some(anyhow::anyhow!(msg));
                        continue;
                    }
                    last_err = Some(anyhow::anyhow!(msg));
                    break;
                }
                Err(e) => {
                    let msg = format!("GeckoTerminal request failed: {:#}", e);
                    let retryable = e.is_timeout() || e.is_connect() || msg.contains("429");
                    if retryable && attempt + 1 < MAX_RETRIES {
                        let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        last_err = Some(anyhow::anyhow!(msg));
                        continue;
                    }
                    last_err = Some(anyhow::anyhow!(msg));
                    break;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("GeckoTerminal request failed")))
    }
}

impl Default for GeckoTerminalClient {
    fn default() -> Self { Self::new() }
}

fn parse_geckoterminal_response(json: &Value, min_tvl: Option<f64>) -> anyhow::Result<Vec<RemotePool>> {
    let data = json.get("data").and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing data array"))?;

    // Included array may hold token details
    let included = json.get("included").and_then(|v| v.as_array());

    let mut out = Vec::new();
    for item in data {
        let attrs = match item.get("attributes") { Some(a) => a, None => continue };
        let addr_str = attrs.get("address").and_then(|v| v.as_str()).unwrap_or("");
        let address = match parse_addr(addr_str) { Some(a) => a, None => continue };

        // TVL and volume parsing
        let tvl = attrs.get("reserve_in_usd")
            .and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok())
            .or_else(|| attrs.get("reserve_in_usd").and_then(|v| v.as_f64()));

        if let Some(min) = min_tvl {
            if tvl.unwrap_or(0.0) < min { continue; }
        }

        let vol_24h = attrs.get("volume_usd")
            .and_then(|v| v.get("h24"))
            .and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok())
            .or_else(|| attrs.get("volume_usd").and_then(|v| v.get("h24")).and_then(|v| v.as_f64()));

        // Dex name from relationships or attributes
        let dex_name = item.get("relationships")
            .and_then(|r| r.get("dex"))
            .and_then(|d| d.get("data"))
            .and_then(|d| d.get("id")).and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| attrs.get("dex_id").and_then(|v| v.as_str()).map(|s| s.to_string()));

        // Tokens: try relationships base_token / quote_token, or attributes
        let (token0, token1) = extract_tokens(item, attrs, included);

        let (t0, t1) = match (token0, token1) {
            (Some(a), Some(b)) => (a, b),
            _ => continue, // need both tokens
        };

        // Fee heuristic: geckoterminal doesn't expose fee; default to 0
        let dex_type = infer_dex_type(dex_name.as_deref());

        out.push(RemotePool {
            address,
            token0: t0,
            token1: t1,
            fee: 0,
            tick_spacing: None,
            dex_type,
            dex_name: dex_name.clone(),
            token0_symbol: None,
            token1_symbol: None,
            tvl_usd: tvl,
            volume_usd_24h: vol_24h,
            volume_usd_30d: None,
            underlying_tokens: None,
            creation_block: 0,
        });
    }
    Ok(out)
}

fn extract_tokens(item: &Value, attrs: &Value, included: Option<&Vec<Value>>) -> (Option<Address>, Option<Address>) {
    // Try relationships.base_token / quote_token
    let rel = item.get("relationships");
    let base_id = rel.and_then(|r| r.get("base_token")).and_then(|b| b.get("data")).and_then(|d| d.get("id")).and_then(|v| v.as_str());
    let quote_id = rel.and_then(|r| r.get("quote_token")).and_then(|b| b.get("data")).and_then(|d| d.get("id")).and_then(|v| v.as_str());

    if let (Some(b), Some(q)) = (base_id, quote_id) {
        // IDs look like "polygon_pos_0xabc..."; extract hex
        let b_addr = extract_hex(b).and_then(parse_addr);
        let q_addr = extract_hex(q).and_then(parse_addr);
        if b_addr.is_some() && q_addr.is_some() {
            return (b_addr, q_addr);
        }
        // If not hex, try to resolve via included
        if let Some(inc) = included {
            let b_resolved = resolve_included(inc, b);
            let q_resolved = resolve_included(inc, q);
            if b_resolved.is_some() && q_resolved.is_some() {
                return (b_resolved, q_resolved);
            }
        }
    }

    // Fallback: attributes base_token_price_quote_token etc not reliable
    // Try attrs base_token / quote_token
    let base_attr = attrs.get("base_token_address").and_then(|v| v.as_str()).and_then(parse_addr);
    let quote_attr = attrs.get("quote_token_address").and_then(|v| v.as_str()).and_then(parse_addr);
    if base_attr.is_some() && quote_attr.is_some() {
        return (base_attr, quote_attr);
    }

    // Last fallback: try to find any two addresses in relationships
    (None, None)
}

fn resolve_included(included: &[Value], id: &str) -> Option<Address> {
    for item in included {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if item_id == id {
            let addr = item.get("attributes").and_then(|a| a.get("address")).and_then(|v| v.as_str())
                .or_else(|| extract_hex(id))
                .unwrap_or("");
            return parse_addr(addr);
        }
    }
    None
}

fn extract_hex(s: &str) -> Option<&str> {
    // id may be "polygon_pos_0xabc..." or "eth_0x..."
    if let Some(pos) = s.rfind("0x") {
        Some(&s[pos..])
    } else {
        None
    }
}

fn parse_addr(s: &str) -> Option<Address> {
    let s = s.trim();
    let hex = s.trim_start_matches("0x").trim_start_matches("0X");
    if hex.len() != 40 { return None; }
    let mut bytes = [0u8; 20];
    hex::decode_to_slice(hex, &mut bytes).ok()?;
    Some(Address::from_slice(&bytes))
}

fn infer_dex_type(dex: Option<&str>) -> DexType {
    match dex.unwrap_or("").to_ascii_lowercase().as_str() {
        s if s.contains("uniswap") && s.contains("v3") => DexType::UniswapV3,
        s if s.contains("algebra") => DexType::UniswapV3,
        s if s.contains("quickswap") => DexType::UniswapV2, // fallback, will be merged
        s if s.contains("balancer") => DexType::Balancer,
        s if s.contains("curve") => DexType::Curve,
        s if s.contains("sushi") => DexType::UniswapV2,
        _ => DexType::UniswapV2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_geckoterminal_basic() {
        let json = json!({
            "data": [{
                "id": "polygon_pos_0x1234567890123456789012345678901234567890",
                "attributes": {
                    "address": "0x1234567890123456789012345678901234567890",
                    "reserve_in_usd": "1000000",
                    "volume_usd": {"h24": "50000"},
                    "dex_id": "quickswap"
                },
                "relationships": {
                    "base_token": {"data": {"id": "polygon_pos_0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},
                    "quote_token": {"data": {"id": "polygon_pos_0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},
                    "dex": {"data": {"id": "quickswap"}}
                }
            }],
            "included": []
        });
        let pools = parse_geckoterminal_response(&json, None).unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].tvl_usd, Some(1000000.0));
        assert_eq!(pools[0].volume_usd_24h, Some(50000.0));
    }

    #[test]
    fn test_min_tvl_filter() {
        let json = json!({
            "data": [{
                "id": "x",
                "attributes": {
                    "address": "0x1234567890123456789012345678901234567890",
                    "reserve_in_usd": "100",
                    "volume_usd": {"h24": "10"}
                },
                "relationships": {
                    "base_token": {"data": {"id": "polygon_pos_0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},
                    "quote_token": {"data": {"id": "polygon_pos_0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}
                }
            }]
        });
        let pools = parse_geckoterminal_response(&json, Some(1000.0)).unwrap();
        assert_eq!(pools.len(), 0);
    }

    #[test]
    fn test_network_slug() {
        assert_eq!(network_slug("polygon"), "polygon_pos");
        assert_eq!(network_slug("ethereum"), "eth");
        assert_eq!(network_slug("bsc"), "bsc");
    }

    #[tokio::test]
    async fn test_geckoterminal_mock() {
        let mock_server = wiremock::MockServer::start().await;
        let body = serde_json::json!({
            "data": [{
                "id": "polygon_pos_0x1111111111111111111111111111111111111111",
                "attributes": {
                    "address": "0x1111111111111111111111111111111111111111",
                    "reserve_in_usd": "2000000",
                    "volume_usd": {"h24": "10000"}
                },
                "relationships": {
                    "base_token": {"data": {"id": "polygon_pos_0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},
                    "quote_token": {"data": {"id": "polygon_pos_0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}
                }
            }],
            "included": []
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .mount(&mock_server)
            .await;

        let client = GeckoTerminalClient::with_base(mock_server.uri());
        let pools = client.fetch_top_pools("polygon", Some(10), None).await.unwrap();
        assert_eq!(pools.len(), 1);
    }
}
