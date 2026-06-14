//! CoinGecko USD pricing with caching.
//!
//! Provides live USD exchange rates for native tokens of supported chains.
//! Prices are fetched once and cached in-memory with a configurable TTL.

use crate::types::ChainName;

/// Maps our ChainName to CoinGecko's asset identifier.
fn coingecko_asset_id(chain: ChainName) -> &'static str {
    match chain {
        ChainName::Polygon => "matic-network",
        ChainName::Ethereum => "ethereum",
        ChainName::Bsc => "binancecoin",
        ChainName::Avalanche => "avalanche-2",
        ChainName::Arbitrum => "ethereum",
        ChainName::Base => "ethereum",
        ChainName::Optimism => "ethereum",
    }
}

/// Cached USD price for a chain's native token.
#[derive(Debug, Clone)]
pub struct PriceEntry {
    pub usd: f64,
    pub fetched_at: std::time::Instant,
}

/// In-memory price cache with TTL.
#[derive(Debug)]
pub struct PriceCache {
    // Key = coingecko asset id, value = price entry
    entries: std::collections::HashMap<String, PriceEntry>,
    ttl: std::time::Duration,
    api_key: Option<String>,
    client: reqwest::Client,
}

/// Response shape from CoinGecko `/simple/price`.
#[derive(serde::Deserialize)]
struct CoinGeckoPriceResponse {
    #[serde(default)]
    usd: f64,
}

impl PriceCache {
    /// Create a new price cache with the given optional API key.
    ///
    /// Free tier (no API key) works but has rate limits of 10-30 req/min.
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            ttl: std::time::Duration::from_secs(300),
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// Set a custom TTL for cached prices.
    pub fn with_ttl(mut self, ttl: std::time::Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Get USD price for a chain's native token.
    /// Returns cached value if fresh, otherwise fetches from API.
    pub async fn usd_price(&mut self, chain: ChainName) -> Option<f64> {
        let asset_id = coingecko_asset_id(chain);

        // Check cache
        if let Some(entry) = self.entries.get(asset_id) {
            if entry.fetched_at.elapsed() < self.ttl {
                return Some(entry.usd);
            }
        }

        // Fetch from API
        match self.fetch_price(asset_id).await {
            Ok(usd) => {
                self.entries.insert(asset_id.to_string(), PriceEntry {
                    usd,
                    fetched_at: std::time::Instant::now(),
                });
                Some(usd)
            }
            Err(e) => {
                // Fall back to stale cache if available
                if let Some(entry) = self.entries.get(asset_id) {
                    tracing::warn!("CoinGecko fetch failed, using stale price: {e}");
                    return Some(entry.usd);
                }
                tracing::warn!("CoinGecko fetch failed and no cached price: {e}");
                None
            }
        }
    }

    /// Execute the HTTP request to CoinGecko.
    async fn fetch_price(&self, asset_id: &str) -> Result<f64, anyhow::Error> {
        let url = format!(
            "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd",
            asset_id
        );

        let mut req = self.client.get(&url);

        // Add API key header if provided (CoinGecko Pro/Demo tier)
        if let Some(key) = &self.api_key {
            req = req.header("x-cg-demo-api-key", key);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("CoinGecko returned HTTP {}", resp.status());
        }

        // Response is like: {"ethereum":{"usd":3500.0}}
        let map: std::collections::HashMap<String, CoinGeckoPriceResponse> = resp.json().await?;
        match map.get(asset_id) {
            Some(entry) => Ok(entry.usd),
            None => anyhow::bail!("asset '{asset_id}' not found in CoinGecko response"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coingecko_asset_id_mapping() {
        assert_eq!(coingecko_asset_id(ChainName::Polygon), "matic-network");
        assert_eq!(coingecko_asset_id(ChainName::Ethereum), "ethereum");
        assert_eq!(coingecko_asset_id(ChainName::Bsc), "binancecoin");
        assert_eq!(coingecko_asset_id(ChainName::Avalanche), "avalanche-2");
        assert_eq!(coingecko_asset_id(ChainName::Arbitrum), "ethereum");
        assert_eq!(coingecko_asset_id(ChainName::Base), "ethereum");
        assert_eq!(coingecko_asset_id(ChainName::Optimism), "ethereum");
    }

    #[test]
    fn test_price_cache_starts_empty() {
        let cache = PriceCache::new(None);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn test_price_cache_with_ttl() {
        let cache = PriceCache::new(None).with_ttl(std::time::Duration::from_secs(60));
        assert_eq!(cache.ttl.as_secs(), 60);
    }
}
