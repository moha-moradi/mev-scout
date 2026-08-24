//! DexScreener REST client — free secondary aggregator (no API key).
//!
//! Complements GeckoTerminal as a redundancy source. DexScreener has no
//! "top pools per chain" endpoint, so we probe a handful of hub-token
//! symbols per chain (wrapped native + stables) via the search endpoint
//! and filter results to the requested chain client-side. Major pools
//! overwhelmingly touch these hubs, giving broad coverage with ~3 requests.
//!
//! Fee/tick-spacing metadata is not exposed (`fee = 0`); the pool-init
//! metadata-repair phase resolves those from chain before quoting.

use std::collections::HashMap;
use std::time::Duration;

use alloy::primitives::Address;
use serde_json::Value;

use crate::dex_type::DexType;

use super::RemotePool;

/// Per-chain profile: DexScreener chainId + hub token symbols to query.
fn chain_profile(chain: &str) -> Option<(&'static str, &'static [&'static str])> {
    match chain.to_ascii_lowercase().as_str() {
        "polygon" => Some(("polygon", &["WMATIC", "USDC", "USDT"])),
        "ethereum" | "eth" => Some(("ethereum", &["WETH", "USDC", "USDT"])),
        "bsc" => Some(("bsc", &["WBNB", "USDC", "USDT"])),
        "arbitrum" => Some(("arbitrum", &["WETH", "USDC", "USDT"])),
        "base" => Some(("base", &["WETH", "USDC", "USDT"])),
        "avalanche" => Some(("avalanche", &["WAVAX", "USDC", "USDT"])),
        "optimism" => Some(("optimism", &["WETH", "USDC", "USDT"])),
        _ => None,
    }
}

/// DexScreener client.
pub struct DexScreenerClient {
    client: reqwest::Client,
    base_url: String,
}

impl DexScreenerClient {
    pub fn new() -> Self {
        Self::with_base("https://api.dexscreener.com".to_string())
    }

    pub fn with_base(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("mev-scout/0.1")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client, base_url }
    }

    /// Fetch pools touching the chain's hub tokens, up to `max_pools`, filtered by `min_tvl`.
    ///
    /// Individual hub queries are best-effort: one failing symbol does not
    /// abort the remaining probes. Unknown chains return an error.
    pub async fn fetch_pools(
        &self,
        chain: &str,
        max_pools: Option<usize>,
        min_tvl: Option<f64>,
    ) -> anyhow::Result<Vec<RemotePool>> {
        let (chain_id, hubs) = chain_profile(chain).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown chain '{chain}' — no DexScreener profile \
                 (supported: polygon, ethereum, bsc, arbitrum, base, avalanche, optimism)"
            )
        })?;

        let limit = max_pools.unwrap_or(1000);
        let mut by_addr: HashMap<String, RemotePool> = HashMap::new();

        for hub in hubs {
            if by_addr.len() >= limit {
                break;
            }
            let url = format!(
                "{}/latest/dex/search?q={}",
                self.base_url,
                percent_encode(hub)
            );
            match self.get_with_retry(&url).await {
                Ok(resp) => {
                    merge_pairs(&resp, chain_id, min_tvl, &mut by_addr);
                }
                Err(e) => {
                    tracing::warn!("DexScreener hub query '{hub}' failed: {e:#}");
                }
            }
            // Search endpoint allows ~300 req/min; stay well below it.
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        let mut pools: Vec<RemotePool> = by_addr.into_values().collect();
        pools.sort_by(|a, b| {
            b.tvl_usd
                .partial_cmp(&a.tvl_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        pools.truncate(limit);
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
                        let json: Value = serde_json::from_str(&text).map_err(|e| {
                            anyhow::anyhow!(
                                "invalid JSON from DexScreener: {} — {}",
                                e,
                                &text[..text.len().min(500)]
                            )
                        })?;
                        return Ok(json);
                    }
                    let body = resp.text().await.unwrap_or_default();
                    let msg = format!(
                        "HTTP {} from DexScreener: {}",
                        status.as_u16(),
                        &body[..body.len().min(300)]
                    );
                    if status.as_u16() == 429 && attempt + 1 < MAX_RETRIES {
                        let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                        tracing::debug!(
                            "DexScreener 429, retry {}/{MAX_RETRIES} after {delay}ms",
                            attempt + 1
                        );
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        last_err = Some(anyhow::anyhow!(msg));
                        continue;
                    }
                    last_err = Some(anyhow::anyhow!(msg));
                    break;
                }
                Err(e) => {
                    let msg = format!("DexScreener request failed: {e:#}");
                    let retryable = e.is_timeout() || e.is_connect();
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
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("DexScreener request failed")))
    }
}

impl Default for DexScreenerClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Merge search-response pairs into `out`, keyed by pair address.
fn merge_pairs(
    json: &Value,
    chain_id: &str,
    min_tvl: Option<f64>,
    out: &mut HashMap<String, RemotePool>,
) {
    let Some(pairs) = json.get("pairs").and_then(|v| v.as_array()) else {
        return;
    };
    for pair in pairs {
        let Some(attrs) = pair.as_object() else {
            continue;
        };

        // Filter to requested chain (search is chain-agnostic)
        let Some(pair_chain) = attrs.get("chainId").and_then(|v| v.as_str()) else {
            continue;
        };
        if !pair_chain.eq_ignore_ascii_case(chain_id) {
            continue;
        }

        let Some(addr_str) = attrs.get("pairAddress").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(address) = parse_addr(addr_str) else {
            continue;
        };

        // Tokens (required)
        let base_symbol = attrs
            .get("baseToken")
            .and_then(|t| t.get("symbol"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let quote_symbol = attrs
            .get("quoteToken")
            .and_then(|t| t.get("symbol"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let base_addr = attrs
            .get("baseToken")
            .and_then(|t| t.get("address"))
            .and_then(|v| v.as_str())
            .and_then(parse_addr);
        let quote_addr = attrs
            .get("quoteToken")
            .and_then(|t| t.get("address"))
            .and_then(|v| v.as_str())
            .and_then(parse_addr);
        let (Some(b), Some(q)) = (base_addr, quote_addr) else {
            continue;
        };

        // Canonical sorted order (token0 < token1); track which side is which
        // so symbols land on the correct positions.
        let (token0, token1, sym0, sym1) = if b <= q {
            (b, q, base_symbol, quote_symbol)
        } else {
            (q, b, quote_symbol, base_symbol)
        };

        let tvl = attrs
            .get("liquidity")
            .and_then(|l| l.get("usd"))
            .and_then(|v| v.as_f64());
        if let Some(min) = min_tvl {
            if tvl.unwrap_or(0.0) < min {
                continue;
            }
        }
        let vol_24h = attrs
            .get("volume")
            .and_then(|v| v.get("h24"))
            .and_then(|v| v.as_f64());

        let dex_name = attrs
            .get("dexId")
            .and_then(|v| v.as_str())
            .map(String::from);
        let labels: Vec<String> = attrs
            .get("labels")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let dex_type = infer_dex_type(dex_name.as_deref(), &labels);

        let key = address.to_string().to_lowercase();
        out.entry(key).or_insert_with(|| RemotePool {
            address,
            token0,
            token1,
            fee: 0,
            tick_spacing: None,
            dex_type,
            dex_name,
            token0_symbol: sym0,
            token1_symbol: sym1,
            tvl_usd: tvl,
            volume_usd_24h: vol_24h,
            volume_usd_30d: None,
            underlying_tokens: None,
            creation_block: 0,
        });
    }
}

fn parse_addr(s: &str) -> Option<Address> {
    let s = s.trim();
    let hex = s.trim_start_matches("0x").trim_start_matches("0X");
    if hex.len() != 40 {
        return None;
    }
    let mut bytes = [0u8; 20];
    hex::decode_to_slice(hex, &mut bytes).ok()?;
    Some(Address::from_slice(&bytes))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Map DexScreener `dexId` (+ optional labels like ["v3"]) to our DexType.
///
/// Unknown DEXs default to V2-style AMM semantics; misclassified concentrated-
/// liquidity pools fail state init later and are pruned rather than misquoted.
fn infer_dex_type(dex_id: Option<&str>, labels: &[String]) -> DexType {
    let id = dex_id.unwrap_or("").to_ascii_lowercase();
    let label_hit = |needle: &str| {
        labels
            .iter()
            .any(|l| l.to_ascii_lowercase().contains(needle))
    };
    if label_hit("v4") {
        return DexType::UniswapV4;
    }
    if label_hit("v3") || label_hit("algebra") || label_hit("cl") {
        return DexType::UniswapV3;
    }
    if id.contains("algebra") {
        return DexType::UniswapV3;
    }
    if id.contains("uniswap") || id.contains("quickswap") || id.contains("sushi") {
        return DexType::UniswapV2;
    }
    if id.contains("camelot") {
        return DexType::Camelot;
    }
    if id.contains("aerodrome")
        || id.contains("velodrome")
        || id.contains("solidly")
        || id.contains("equalizer")
        || id.contains("thena")
    {
        return DexType::Solidly;
    }
    if id.contains("curve") {
        return DexType::Curve;
    }
    if id.contains("balancer") {
        return DexType::Balancer;
    }
    if id.contains("traderjoe") || id.contains("lfj") {
        return DexType::TraderJoeLB;
    }
    if id.contains("pendle") {
        return DexType::Pendle;
    }
    DexType::UniswapV2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(chain: &str, dex: &str, addr: &str, base: &str, quote: &str, tvl: f64) -> Value {
        serde_json::json!({
            "chainId": chain,
            "dexId": dex,
            "pairAddress": addr,
            "baseToken": {"address": base, "symbol": "BASE"},
            "quoteToken": {"address": quote, "symbol": "QUOTE"},
            "liquidity": {"usd": tvl},
            "volume": {"h24": tvl / 10.0}
        })
    }

    #[test]
    fn test_chain_profile_unknown_chain() {
        assert!(chain_profile("fantom").is_none());
        assert_eq!(
            chain_profile("polygon"),
            Some(("polygon", &["WMATIC", "USDC", "USDT"][..]))
        );
    }

    #[test]
    fn test_merge_pairs_filters_and_canonicalizes() {
        let json = serde_json::json!({
            "pairs": [
                pair("polygon", "quickswap", "0x1111111111111111111111111111111111111111",
                     "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 5000.0),
                // wrong chain — must be dropped
                pair("ethereum", "uniswap", "0x2222222222222222222222222222222222222222",
                     "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 9000.0),
                // missing pairAddress — dropped
                {"chainId": "polygon", "dexId": "quickswap"},
            ]
        });
        let mut out = HashMap::new();
        merge_pairs(&json, "polygon", None, &mut out);
        assert_eq!(out.len(), 1);
        let p = out.values().next().unwrap();
        assert!(p.token0 < p.token1, "tokens must be canonicalized");
        assert_eq!(p.dex_type, DexType::UniswapV2);
        assert_eq!(p.tvl_usd, Some(5000.0));
    }

    #[test]
    fn test_infer_dex_type_labels_beat_defaults() {
        assert_eq!(
            infer_dex_type(Some("uniswap"), &["v3".to_string()]),
            DexType::UniswapV3
        );
        assert_eq!(infer_dex_type(Some("camelot"), &[]), DexType::Camelot);
        assert_eq!(infer_dex_type(Some("aerodrome"), &[]), DexType::Solidly);
        assert_eq!(infer_dex_type(Some("lfj"), &[]), DexType::TraderJoeLB);
        assert_eq!(infer_dex_type(Some("unknown-dex"), &[]), DexType::UniswapV2);
    }

    #[tokio::test]
    async fn test_dexscreener_mock() {
        let mock_server = wiremock::MockServer::start().await;
        let body = serde_json::json!({
            "pairs": [
                pair("polygon", "quickswap", "0x1111111111111111111111111111111111111111",
                     "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 7000.0),
            ]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/latest/dex/search"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .mount(&mock_server)
            .await;

        let client = DexScreenerClient::with_base(mock_server.uri());
        let pools = client.fetch_pools("polygon", Some(10), None).await.unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].tvl_usd, Some(7000.0));
    }

    #[tokio::test]
    async fn test_dexscreener_unknown_chain_errors() {
        let client = DexScreenerClient::with_base("http://localhost:1".to_string());
        assert!(client.fetch_pools("fantom", Some(10), None).await.is_err());
    }
}
