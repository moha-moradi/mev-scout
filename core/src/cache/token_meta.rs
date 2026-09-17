//! Remote token metadata enrichment (DefiLlama coins + CoinGecko contract).
//!
//! DefiLlama `coins.llama.fi` supplies symbol / decimals / price but **not**
//! ERC-20 name or icon. CoinGecko's contract endpoint fills name + image URL.
//! Both paths are best-effort and never fail the tokens job.

use std::collections::HashMap;
use std::time::Duration;

use alloy::primitives::Address;
use serde_json::Value;

use crate::types::ChainName;

use super::token_cache::CachedToken;

/// DefiLlama coins-API chain prefix (shared with explorer pricing).
pub fn llama_chain_prefix(chain: ChainName) -> &'static str {
    match chain {
        ChainName::Polygon => "polygon",
        ChainName::Avalanche => "avax",
        ChainName::Bsc => "bsc",
        ChainName::Ethereum => "ethereum",
        ChainName::Arbitrum => "arbitrum",
        ChainName::Base => "base",
        ChainName::Optimism => "optimism",
    }
}

/// CoinGecko asset-platform id for `/coins/{platform}/contract/{address}`.
pub fn coingecko_platform_id(chain: ChainName) -> &'static str {
    match chain {
        ChainName::Ethereum => "ethereum",
        ChainName::Polygon => "polygon-pos",
        ChainName::Arbitrum => "arbitrum-one",
        ChainName::Base => "base",
        ChainName::Optimism => "optimistic-ethereum",
        ChainName::Bsc => "binance-smart-chain",
        ChainName::Avalanche => "avalanche",
    }
}

fn http_client(timeout_secs: u64) -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("mev-scout/0.1 (+https://github.com/local/mev-scout)")
        .build()?)
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

/// Parse a DefiLlama coins current/historical JSON body into address → partial metadata.
pub fn parse_llama_coins_response(json: &Value, prefix: &str) -> HashMap<Address, CachedToken> {
    let mut out = HashMap::new();
    let Some(coins) = json.get("coins").and_then(|v| v.as_object()) else {
        return out;
    };
    let prefix_colon = format!("{prefix}:");
    for (key, coin) in coins {
        let addr_str = key
            .strip_prefix(&prefix_colon)
            .or_else(|| key.split(':').next_back())
            .unwrap_or(key);
        let Ok(addr) = addr_str.parse::<Address>() else {
            continue;
        };
        let symbol = coin
            .get("symbol")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();
        if symbol.is_empty() {
            continue;
        }
        let decimals = coin
            .get("decimals")
            .and_then(|v| v.as_u64())
            .map(|d| d as i32);
        out.insert(
            addr,
            CachedToken {
                symbol,
                decimals,
                name: None,
                icon_url: None,
            },
        );
    }
    out
}

/// Batch-fetch symbol/decimals from DefiLlama coins current endpoint.
///
/// Chunks addresses to keep URL length reasonable. Failures for a chunk log a
/// warning and skip that chunk.
pub async fn enrich_from_llama(
    chain: ChainName,
    addresses: &[Address],
) -> HashMap<Address, CachedToken> {
    let mut out = HashMap::new();
    if addresses.is_empty() {
        return out;
    }
    let prefix = llama_chain_prefix(chain);
    let Ok(client) = http_client(15) else {
        tracing::warn!("token meta: failed to build HTTP client for DefiLlama");
        return out;
    };

    const CHUNK: usize = 40;
    for chunk in addresses.chunks(CHUNK) {
        let keys: Vec<String> = chunk
            .iter()
            .map(|a| format!("{prefix}:{a:#x}"))
            .collect();
        let joined = keys.join(",");
        let url = format!(
            "https://coins.llama.fi/prices/current/{}",
            urlencode(&joined)
        );
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(json) => {
                    for (addr, meta) in parse_llama_coins_response(&json, prefix) {
                        out.insert(addr, meta);
                    }
                }
                Err(e) => tracing::warn!("DefiLlama coins JSON parse failed: {e:#}"),
            },
            Ok(resp) => {
                tracing::warn!(
                    "DefiLlama coins HTTP {}: skipping chunk of {}",
                    resp.status(),
                    chunk.len()
                );
            }
            Err(e) => tracing::warn!("DefiLlama coins request failed: {e:#}"),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    out
}

/// Parse CoinGecko contract-endpoint JSON into name + icon URL (+ optional symbol).
pub fn parse_coingecko_contract_response(json: &Value) -> Option<CachedToken> {
    let name = json
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)?;
    let symbol = json
        .get("symbol")
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    let icon_url = json
        .get("image")
        .and_then(|img| {
            img.get("small")
                .or_else(|| img.get("thumb"))
                .or_else(|| img.get("large"))
        })
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(CachedToken {
        symbol,
        decimals: None,
        name: Some(name),
        icon_url,
    })
}

/// Fetch name + icon_url from CoinGecko for addresses still missing those fields.
///
/// Sequential with a short delay to stay within free-tier rate limits.
pub async fn enrich_from_coingecko(
    chain: ChainName,
    addresses: &[Address],
) -> HashMap<Address, CachedToken> {
    let mut out = HashMap::new();
    if addresses.is_empty() {
        return out;
    }
    let platform = coingecko_platform_id(chain);
    let Ok(client) = http_client(15) else {
        tracing::warn!("token meta: failed to build HTTP client for CoinGecko");
        return out;
    };

    for addr in addresses {
        let url = format!(
            "https://api.coingecko.com/api/v3/coins/{platform}/contract/{addr:#x}"
        );
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(json) => {
                    if let Some(meta) = parse_coingecko_contract_response(&json) {
                        out.insert(*addr, meta);
                    }
                }
                Err(e) => tracing::debug!("CoinGecko contract JSON parse failed for {addr}: {e:#}"),
            },
            Ok(resp) if resp.status().as_u16() == 429 => {
                tracing::warn!("CoinGecko rate limited; stopping token icon/name enrich early");
                break;
            }
            Ok(resp) if resp.status().as_u16() == 404 => {
                // Unknown / long-tail token — skip quietly.
            }
            Ok(resp) => {
                tracing::debug!("CoinGecko HTTP {} for {addr}", resp.status());
            }
            Err(e) => tracing::debug!("CoinGecko request failed for {addr}: {e:#}"),
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use serde_json::json;

    #[test]
    fn parse_llama_coins_fixture() {
        let addr = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let json = json!({
            "coins": {
                "ethereum:0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48": {
                    "decimals": 6,
                    "symbol": "USDC",
                    "price": 0.999,
                    "timestamp": 1640995200,
                    "confidence": 0.99
                }
            }
        });
        let got = parse_llama_coins_response(&json, "ethereum");
        let meta = got.get(&addr).expect("USDC present");
        assert_eq!(meta.symbol, "USDC");
        assert_eq!(meta.decimals, Some(6));
        assert!(meta.name.is_none());
        assert!(meta.icon_url.is_none());
    }

    #[test]
    fn parse_coingecko_contract_fixture() {
        let json = json!({
            "id": "usd-coin",
            "symbol": "usdc",
            "name": "USDC",
            "image": {
                "thumb": "https://example.com/usdc-thumb.png",
                "small": "https://example.com/usdc-small.png",
                "large": "https://example.com/usdc-large.png"
            }
        });
        let meta = parse_coingecko_contract_response(&json).expect("parsed");
        assert_eq!(meta.name.as_deref(), Some("USDC"));
        assert_eq!(meta.symbol, "USDC");
        assert_eq!(
            meta.icon_url.as_deref(),
            Some("https://example.com/usdc-small.png")
        );
    }

    #[test]
    fn platform_and_llama_prefixes() {
        assert_eq!(llama_chain_prefix(ChainName::Avalanche), "avax");
        assert_eq!(coingecko_platform_id(ChainName::Polygon), "polygon-pos");
        assert_eq!(coingecko_platform_id(ChainName::Avalanche), "avalanche");
    }
}
