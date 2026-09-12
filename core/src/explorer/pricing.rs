//! USD pricing for the explorer (plan §8.5).
//!
//! - **Live**: CoinGecko simple/price (chain-aware asset ids), cached hourly
//!   in the explorer `prices` table.
//! - **Backfill**: DefiLlama coins API hourly close (batch-friendly, no key).
//! - **Long-tail fallback**: tokens without a stable leg are reported in
//!   token units and excluded from USD aggregates (flagged, not guessed).
//!
//! Conversion helpers are decimals-aware; the store persists raw amounts and
//! applies USD at insert time via `TokenUsd` records.

use std::collections::HashMap;

use alloy::primitives::{Address, U256};
use serde::Deserialize;

/// A USD price observation for a token, with its decimals for conversion.
#[derive(Debug, Clone, Copy)]
pub struct TokenUsd {
    pub usd: f64,
    pub decimals: u32,
}

/// Chain-aware CoinGecko asset id for the native token (plan §8.5).
pub fn native_asset_id(chain: crate::types::ChainName) -> &'static str {
    match chain {
        crate::types::ChainName::Polygon => "matic-network",
        crate::types::ChainName::Avalanche => "avalanche-2",
        crate::types::ChainName::Bsc => "binancecoin",
        crate::types::ChainName::Ethereum
        | crate::types::ChainName::Arbitrum
        | crate::types::ChainName::Base
        | crate::types::ChainName::Optimism => "ethereum",
    }
}

/// DefiLlama coins-API chain prefix (plan §8.5).
fn llama_chain_prefix(chain: crate::types::ChainName) -> &'static str {
    match chain {
        crate::types::ChainName::Polygon => "polygon",
        crate::types::ChainName::Avalanche => "avax",
        crate::types::ChainName::Bsc => "bsc",
        crate::types::ChainName::Ethereum => "ethereum",
        crate::types::ChainName::Arbitrum => "arbitrum",
        crate::types::ChainName::Base => "base",
        crate::types::ChainName::Optimism => "optimism",
    }
}

/// Convert a raw token amount to USD given a price and decimals.
pub fn token_amount_to_usd(amount: U256, price: &TokenUsd) -> f64 {
    let scalar = 10_f64.powi(price.decimals as i32);
    let units = u256_to_f64(amount) / scalar;
    units * price.usd
}

/// Convert wei (18-decimals native) to USD.
pub fn wei_to_usd(wei: U256, native_usd: f64) -> f64 {
    u256_to_f64(wei) / 1e18 * native_usd
}

/// U256 → f64 (precision loss acceptable at report layer).
pub fn u256_to_f64(v: U256) -> f64 {
    let limbs = v.as_limbs();
    (limbs[0] as f64)
        + (limbs[1] as f64) * 2f64.powi(64)
        + (limbs[2] as f64) * 2f64.powi(128)
        + (limbs[3] as f64) * 2f64.powi(192)
}

/// Hour bucket for the price cache (UTC).
pub fn hour_bucket(ts: u64) -> u64 {
    ts / 3600
}

#[derive(Deserialize)]
struct CoingeckoSimplePrice {
    #[serde(default)]
    usd: Option<f64>,
}

/// Fetch the native token USD price from CoinGecko (live path).
pub async fn fetch_native_price_coingecko(chain: crate::types::ChainName) -> anyhow::Result<f64> {
    let id = native_asset_id(chain);
    let url = format!("https://api.coingecko.com/api/v3/simple/price?ids={id}&vs_currencies=usd");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp: HashMap<String, CoingeckoSimplePrice> = client.get(&url).send().await?.json().await?;
    resp.get(id)
        .and_then(|v| v.usd)
        .ok_or_else(|| anyhow::anyhow!("coingecko: no usd price for {id}"))
}

#[derive(Deserialize)]
struct LlamaResponse {
    #[serde(default)]
    coins: HashMap<String, LlamaCoin>,
}

#[derive(Deserialize)]
struct LlamaCoin {
    price: f64,
    #[serde(default)]
    decimals: Option<u32>,
}

/// Fetch historical token prices from DefiLlama (backfill path).
///
/// `tokens` = addresses to price at `timestamp`. Returns a map of token →
/// (price, decimals-when-known). DefiLlama's `decimals` field is only present
/// on some endpoints; unknown decimals are returned as `None` and callers
/// must resolve decimals locally (token cache) before USD conversion.
pub async fn fetch_prices_llama(
    chain: crate::types::ChainName,
    timestamp: u64,
    tokens: &[Address],
) -> anyhow::Result<HashMap<Address, (f64, Option<u32>)>> {
    if tokens.is_empty() {
        return Ok(HashMap::new());
    }
    let prefix = llama_chain_prefix(chain);
    let assets: Vec<String> = tokens.iter().map(|t| format!("{prefix}:{t:#x}")).collect();
    let url = format!(
        "https://coins.llama.fi/prices/historical/{}?{}",
        timestamp,
        assets
            .iter()
            .map(|a| format!("coins={}", urlencode(a)))
            .collect::<Vec<_>>()
            .join("&")
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp: LlamaResponse = client.get(&url).send().await?.json().await?;

    let mut out = HashMap::new();
    for (key, coin) in resp.coins {
        if let Some(addr_str) = key.split(':').next_back() {
            if let Ok(addr) = addr_str.parse::<Address>() {
                out.insert(addr, (coin.price, coin.decimals));
            }
        }
    }
    Ok(out)
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn conversion_decimals_aware() {
        let usdc = TokenUsd {
            usd: 1.0,
            decimals: 6,
        };
        assert!((token_amount_to_usd(U256::from(1_000_000u64), &usdc) - 1.0).abs() < 1e-9);
        let wpol = TokenUsd {
            usd: 0.5,
            decimals: 18,
        };
        assert!(
            (token_amount_to_usd(U256::from(2_000_000_000_000_000_000u64), &wpol) - 1.0).abs()
                < 1e-9
        );
        assert!((wei_to_usd(U256::from(1_000_000_000_000_000_000u64), 2.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn native_ids_chain_aware() {
        assert_eq!(
            native_asset_id(crate::types::ChainName::Polygon),
            "matic-network"
        );
        assert_eq!(
            native_asset_id(crate::types::ChainName::Avalanche),
            "avalanche-2"
        );
    }

    #[test]
    fn llama_prefix() {
        assert_eq!(
            llama_chain_prefix(crate::types::ChainName::Polygon),
            "polygon"
        );
        assert_eq!(
            llama_chain_prefix(crate::types::ChainName::Avalanche),
            "avax"
        );
    }

    #[test]
    fn hour_bucketing() {
        assert_eq!(hour_bucket(3_600), 1);
        assert_eq!(hour_bucket(3_599), 0);
    }

    #[test]
    fn u256_conversion() {
        assert!((u256_to_f64(U256::from(123u64)) - 123.0).abs() < 1e-9);
        let big = U256::from(1u64) << 100;
        assert!((u256_to_f64(big) - (1u128 << 100) as f64).abs() < 1e6);
        let _ = address!("0x0000000000000000000000000000000000000001");
    }
}
