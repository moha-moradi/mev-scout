//! CoinGecko USD pricing with caching.
//!
//! Provides live USD exchange rates for native tokens and arbitrary ERC20 tokens
//! of supported chains. Prices are fetched once and cached in-memory with a
//! configurable TTL.

use std::future::Future;

use alloy::primitives::Address;
use crate::pool::state::PoolManager;
use crate::types::{ChainName, PriceOracleMode};

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
    entries: tokio::sync::Mutex<std::collections::HashMap<String, PriceEntry>>,
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
            entries: tokio::sync::Mutex::new(std::collections::HashMap::new()),
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

    /// Generic check-cache → fetch → store → fallback-to-stale helper.
    async fn get_or_fetch<F, Fut>(&self, key: &str, fetch_fn: F) -> Option<f64>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<f64>>,
    {
        {
            let entries = self.entries.lock().await;
            if let Some(entry) = entries.get(key) {
                if entry.fetched_at.elapsed() < self.ttl {
                    return Some(entry.usd);
                }
            }
        }

        match fetch_fn().await {
            Ok(usd) => {
                let mut entries = self.entries.lock().await;
                entries.insert(key.to_string(), PriceEntry {
                    usd,
                    fetched_at: std::time::Instant::now(),
                });
                Some(usd)
            }
            Err(e) => {
                let entries = self.entries.lock().await;
                if let Some(entry) = entries.get(key) {
                    tracing::warn!("CoinGecko fetch failed, using stale price: {e}");
                    return Some(entry.usd);
                }
                tracing::warn!("CoinGecko fetch failed and no cached price: {e}");
                None
            }
        }
    }

    /// Execute a CoinGecko HTTP GET, parse JSON, and extract the price for
    /// the given lookup key.
    async fn execute_price_request(&self, url: &str, lookup_key: &str) -> anyhow::Result<f64> {
        let mut req = self.client.get(url);
        if let Some(key) = &self.api_key {
            req = req.header("x-cg-demo-api-key", key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("coinGecko returned HTTP {}", resp.status());
        }
        let map: std::collections::HashMap<String, CoinGeckoPriceResponse> = resp.json().await?;
        match map.get(lookup_key) {
            Some(entry) => Ok(entry.usd),
            None => anyhow::bail!("'{lookup_key}' not found in coinGecko response"),
        }
    }

    /// Get USD price for a chain's native token.
    pub async fn usd_price(&self, chain: ChainName) -> Option<f64> {
        let asset_id = coingecko_asset_id(chain);
        let url = format!(
            "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd",
            asset_id
        );
        self.get_or_fetch(asset_id, || async {
            self.execute_price_request(&url, asset_id).await
        }).await
    }

    /// Get the native token USD price according to the configured oracle mode.
    pub async fn resolve_native_price(
        &self,
        mode: PriceOracleMode,
        chain: ChainName,
        pm: &PoolManager,
        block: u64,
    ) -> Option<f64> {
        match mode {
            PriceOracleMode::CoinGeckoOnly => self.usd_price(chain).await,
            PriceOracleMode::OnChain => self.resolve_onchain_price(pm, block).await,
            PriceOracleMode::Hybrid => {
                let cg_price = self.usd_price(chain).await;
                let onchain_price = self.resolve_onchain_price(pm, block).await;
                match (cg_price, onchain_price) {
                    (Some(cg), Some(oc)) => {
                        let divergence = (cg - oc).abs() / cg;
                        if divergence > 0.05 {
                            tracing::warn!(
                                "PriceOracle Hybrid: CoinGecko={cg:.4} vs OnChain={oc:.4} ({:.1}% divergence >5%)",
                                divergence * 100.0
                            );
                        }
                        Some((cg + oc) / 2.0)
                    }
                    (Some(cg), None) => Some(cg),
                    (None, Some(oc)) => Some(oc),
                    (None, None) => None,
                }
            }
        }
    }

    /// Derive native token price from the pool manager's on-chain state.
    async fn resolve_onchain_price(&self, pm: &PoolManager, block: u64) -> Option<f64> {
        let cache_key = format!("onchain:{}", block);
        {
            let entries = self.entries.lock().await;
            if let Some(entry) = entries.get(&cache_key) {
                if entry.fetched_at.elapsed() < self.ttl {
                    return Some(entry.usd);
                }
            }
        }
        let stable_tokens = vec![
            alloy::primitives::address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            alloy::primitives::address!("dAC17F958D2ee523a2206206994597C13D831ec7"),
            alloy::primitives::address!("6B175474E89094C44Da98b954EedeAC495271d0F"),
        ];
        let price = pm.onchain_native_price(&stable_tokens);
        if let Some(p) = price {
            let mut entries = self.entries.lock().await;
            entries.insert(cache_key, PriceEntry {
                usd: p,
                fetched_at: std::time::Instant::now(),
            });
        }
        price
    }

    /// Get USD price for an arbitrary ERC20 token on the given chain.
    pub async fn token_usd(&self, chain: ChainName, token: Address) -> Option<f64> {
        let addr_hex = format!("{:#x}", token);
        let cache_key = format!("{}:{}", coingecko_platform(chain), addr_hex);
        let url = format!(
            "https://api.coingecko.com/api/v3/simple/token_price/{}?contract_addresses={}&vs_currencies=usd",
            coingecko_platform(chain), addr_hex
        );
        self.get_or_fetch(&cache_key, || async {
            self.execute_price_request(&url, &addr_hex).await
        }).await
    }
}
