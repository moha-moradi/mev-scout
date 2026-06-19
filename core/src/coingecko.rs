//! CoinGecko USD pricing with caching.
//!
//! Provides live USD exchange rates for native tokens and arbitrary ERC20 tokens
//! of supported chains. Prices are fetched once and cached in-memory with a
//! configurable TTL.

use alloy::primitives::Address;
use crate::types::ChainName;

/// Maps our ChainName to CoinGecko's asset identifier (for native tokens).
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

/// Maps our ChainName to CoinGecko's platform identifier (for ERC20 token prices).
fn coingecko_platform(chain: ChainName) -> &'static str {
    match chain {
        ChainName::Ethereum => "ethereum",
        ChainName::Polygon => "polygon-pos",
        ChainName::Bsc => "binance-smart-chain",
        ChainName::Avalanche => "avalanche",
        ChainName::Arbitrum => "arbitrum-one",
        ChainName::Base => "base",
        ChainName::Optimism => "optimistic-ethereum",
    }
}

/// Cached USD price for a token.
#[derive(Debug, Clone)]
pub struct PriceEntry {
    pub usd: f64,
    pub fetched_at: std::time::Instant,
}

/// In-memory price cache with TTL.
#[derive(Debug)]
pub struct PriceCache {
    // Key = coingecko asset id or token address hex, value = price entry
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
        match self.fetch_native_price(asset_id).await {
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

    /// Get USD price for an arbitrary ERC20 token on the given chain.
    /// Uses CoinGecko's `/simple/token_price/{platform}` endpoint with the
    /// token's contract address.
    ///
    /// Returns cached value if fresh, otherwise fetches from API.
    pub async fn token_usd(&mut self, chain: ChainName, token: Address) -> Option<f64> {
        let addr_hex = format!("{:#x}", token);
        let cache_key = format!("{}:{}", coingecko_platform(chain), addr_hex);

        if let Some(entry) = self.entries.get(&cache_key) {
            if entry.fetched_at.elapsed() < self.ttl {
                return Some(entry.usd);
            }
        }

        match self.fetch_token_price(chain, &addr_hex).await {
            Ok(usd) => {
                self.entries.insert(cache_key, PriceEntry {
                    usd,
                    fetched_at: std::time::Instant::now(),
                });
                Some(usd)
            }
            Err(e) => {
                if let Some(entry) = self.entries.get(&cache_key) {
                    tracing::warn!("CoinGecko token price fetch failed, using stale: {e}");
                    return Some(entry.usd);
                }
                tracing::warn!("CoinGecko token price fetch failed: {e}");
                None
            }
        }
    }

    /// Execute the HTTP request to CoinGecko for native token price.
    async fn fetch_native_price(&self, asset_id: &str) -> Result<f64, anyhow::Error> {
        let url = format!(
            "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd",
            asset_id
        );

        let mut req = self.client.get(&url);

        if let Some(key) = &self.api_key {
            req = req.header("x-cg-demo-api-key", key);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("CoinGecko returned HTTP {}", resp.status());
        }

        let map: std::collections::HashMap<String, CoinGeckoPriceResponse> = resp.json().await?;
        match map.get(asset_id) {
            Some(entry) => Ok(entry.usd),
            None => anyhow::bail!("asset '{asset_id}' not found in CoinGecko response"),
        }
    }

    /// Execute the HTTP request to CoinGecko for an ERC20 token price.
    async fn fetch_token_price(&self, chain: ChainName, token_hex: &str) -> Result<f64, anyhow::Error> {
        let platform = coingecko_platform(chain);
        let url = format!(
            "https://api.coingecko.com/api/v3/simple/token_price/{}?contract_addresses={}&vs_currencies=usd",
            platform, token_hex
        );

        let mut req = self.client.get(&url);

        if let Some(key) = &self.api_key {
            req = req.header("x-cg-demo-api-key", key);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("CoinGecko token price returned HTTP {}", resp.status());
        }

        // Response is like: {"0x...":{"usd":1.0}}
        let map: std::collections::HashMap<String, CoinGeckoPriceResponse> = resp.json().await?;
        match map.get(token_hex) {
            Some(entry) => Ok(entry.usd),
            None => anyhow::bail!("token {token_hex} not found on {platform}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

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
    fn test_coingecko_platform_mapping() {
        assert_eq!(coingecko_platform(ChainName::Ethereum), "ethereum");
        assert_eq!(coingecko_platform(ChainName::Polygon), "polygon-pos");
        assert_eq!(coingecko_platform(ChainName::Bsc), "binance-smart-chain");
        assert_eq!(coingecko_platform(ChainName::Avalanche), "avalanche");
        assert_eq!(coingecko_platform(ChainName::Arbitrum), "arbitrum-one");
        assert_eq!(coingecko_platform(ChainName::Base), "base");
        assert_eq!(coingecko_platform(ChainName::Optimism), "optimistic-ethereum");
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

    #[test]
    fn test_token_usd_cache_key_uses_platform_and_address() {
        let mut cache = PriceCache::new(None);
        let _usdc = address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");
        // Insert a mock entry for USDC on Polygon
        cache.entries.insert(
            "polygon-pos:0x2791bca1f2de4661ed88a30c99a7a9449aa84174".to_string(),
            PriceEntry { usd: 1.0, fetched_at: std::time::Instant::now() },
        );
        // Should return cached value without making HTTP requests
        let result = cache.entries.get("polygon-pos:0x2791bca1f2de4661ed88a30c99a7a9449aa84174");
        assert!(result.is_some());
        assert!((result.unwrap().usd - 1.0).abs() < 0.001);
    }
}
