//! USD pricing for the explorer.
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

/// Chain-aware CoinGecko asset id for the native token.
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

/// DefiLlama coins-API chain prefix.
fn llama_chain_prefix(chain: crate::types::ChainName) -> &'static str {
    crate::cache::llama_chain_prefix(chain)
}

/// Convert a raw token amount to USD given a price and decimals.
pub fn token_amount_to_usd(amount: U256, price: &TokenUsd) -> f64 {
    let scalar = 10_f64.powi(price.decimals as i32);
    let units = u256_to_f64(amount) / scalar;
    units * price.usd
}

/// Why a token's USD is only approximate (Phase 2.4): fee-on-transfer or
/// rebase tokens distort recorded amounts, so their deltas are flagged.
pub fn approximate_token(token: &Address) -> Option<&'static str> {
    if crate::pool::state::pool_types::is_fee_on_transfer_token(token) {
        return Some("FOT");
    }
    if crate::pool::state::pool_types::is_rebase_token(token) {
        return Some("REBASE");
    }
    None
}

/// A USD quote for a raw token amount.
///
/// `clamped` is set when the realized-rate fallback exceeded the USD actually
/// observed on the route's trusted legs and was capped there. A residual
/// token cannot be worth more than the route it rode on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RealizedUsd {
    pub usd: f64,
    pub clamped: bool,
}

/// USD value of `amount` of `token`, preferring the external price and
/// falling back to the on-chain realized rate implied by the event's route
/// legs (Phase 2.4): a leg trading `token` against a priced counterpart
/// prices `token` at the rate the searcher actually realized.
///
/// Only legs whose `token_source` is `registry` or `transfer` are trusted.
/// A missing key (legacy rows) or `proximity` is a guess and is skipped.
/// Among trusted legs, the one with the largest priced notional wins, so a
/// dust hop cannot set the rate. The resulting fallback is capped at the
/// summed USD of those trusted priced legs. Non-finite results are `None`.
///
/// `details` is the event's `route`-bearing `details` JSON. Returns `None`
/// when neither an external price nor a usable trusted route leg exists.
pub fn amount_usd_realized(
    token: Address,
    amount: U256,
    token_prices: &HashMap<Address, TokenUsd>,
    details: &serde_json::Value,
) -> Option<RealizedUsd> {
    if let Some(p) = token_prices.get(&token) {
        let usd = token_amount_to_usd(amount, p);
        return finite_quote(usd, false);
    }
    if amount.is_zero() {
        return None;
    }
    let route = details.get("route")?.as_array()?;
    let route_usd = trusted_route_usd(route, token_prices);
    let mut best: Option<(f64, f64)> = None; // (leg notional, quote)
    for leg in route.iter().filter(|leg| leg_is_trusted(leg)) {
        let token_in = match leg.get("token_in").and_then(as_address) {
            Some(a) => a,
            None => continue,
        };
        let token_out = match leg.get("token_out").and_then(as_address) {
            Some(a) => a,
            None => continue,
        };
        let amount_in = match leg.get("amount_in").and_then(as_u256) {
            Some(a) => a,
            None => continue,
        };
        let amount_out = match leg.get("amount_out").and_then(as_u256) {
            Some(a) => a,
            None => continue,
        };
        // Sold `amount_in` of `token` for a priced counterpart.
        if token_in == token && !amount_in.is_zero() {
            if let Some(p) = token_prices.get(&token_out) {
                consider_leg(
                    &mut best,
                    token_amount_to_usd(amount_out, p),
                    u256_to_f64(amount),
                    u256_to_f64(amount_in),
                );
            }
        }
        // Spent a priced counterpart to obtain `amount_out` of `token`.
        if token_out == token && !amount_out.is_zero() {
            if let Some(p) = token_prices.get(&token_in) {
                consider_leg(
                    &mut best,
                    token_amount_to_usd(amount_in, p),
                    u256_to_f64(amount),
                    u256_to_f64(amount_out),
                );
            }
        }
    }
    let (_, quote) = best?;
    if !route_usd.is_finite() || route_usd <= 0.0 {
        return None;
    }
    if quote > route_usd {
        finite_quote(route_usd, true)
    } else {
        finite_quote(quote, false)
    }
}

fn finite_quote(usd: f64, clamped: bool) -> Option<RealizedUsd> {
    usd.is_finite().then_some(RealizedUsd { usd, clamped })
}

/// Largest priced notional wins. The quote is `leg_usd * amount / same_side`;
/// both raw amounts are the token being priced, so decimals cancel.
fn consider_leg(best: &mut Option<(f64, f64)>, leg_usd: f64, amount: f64, same_side: f64) {
    if !leg_usd.is_finite() || leg_usd <= 0.0 || !amount.is_finite() || same_side <= 0.0 {
        return;
    }
    let quote = leg_usd * amount / same_side;
    if !quote.is_finite() || quote < 0.0 {
        return;
    }
    match best {
        Some((prev, _)) if *prev >= leg_usd => {}
        _ => *best = Some((leg_usd, quote)),
    }
}

fn leg_is_trusted(leg: &serde_json::Value) -> bool {
    matches!(
        leg.get("token_source").and_then(|v| v.as_str()),
        Some("registry" | "transfer")
    )
}

/// Sum of USD observed on trusted legs. One priced side per leg so a swap is
/// not counted twice when both tokens have an external price.
fn trusted_route_usd(
    route: &[serde_json::Value],
    token_prices: &HashMap<Address, TokenUsd>,
) -> f64 {
    let mut total = 0.0f64;
    for leg in route.iter().filter(|leg| leg_is_trusted(leg)) {
        let Some(usd) = observed_leg_usd(leg, token_prices) else {
            continue;
        };
        total += usd;
    }
    total
}

fn observed_leg_usd(
    leg: &serde_json::Value,
    token_prices: &HashMap<Address, TokenUsd>,
) -> Option<f64> {
    let priced = |token: &serde_json::Value, amount: &serde_json::Value| -> Option<f64> {
        let token = as_address(token)?;
        let amount = as_u256(amount)?;
        let p = token_prices.get(&token)?;
        let usd = token_amount_to_usd(amount, p);
        (usd.is_finite() && usd > 0.0).then_some(usd)
    };
    priced(leg.get("token_in")?, leg.get("amount_in")?)
        .or_else(|| priced(leg.get("token_out")?, leg.get("amount_out")?))
}

fn as_address(v: &serde_json::Value) -> Option<Address> {
    v.as_str().and_then(|s| s.parse::<Address>().ok())
}

fn as_u256(v: &serde_json::Value) -> Option<U256> {
    v.as_str().and_then(|s| s.parse::<U256>().ok())
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
        .user_agent("mev-scout/0.1 (+https://github.com/local/mev-scout)")
        .build()?;
    let resp: HashMap<String, CoingeckoSimplePrice> = client.get(&url).send().await?.json().await?;
    resp.get(id)
        .and_then(|v| v.usd)
        .ok_or_else(|| anyhow::anyhow!("coingecko: no usd price for {id}"))
}

/// Live native price via DefiLlama current endpoint (CoinGecko fallback).
pub async fn fetch_native_price_llama(
    chain: crate::types::ChainName,
    wrapped_native: Address,
) -> anyhow::Result<f64> {
    if wrapped_native.is_zero() {
        anyhow::bail!("no wrapped-native for llama native price");
    }
    let prefix = llama_chain_prefix(chain);
    let key = format!("{prefix}:{wrapped_native:#x}");
    let url = format!("https://coins.llama.fi/prices/current/{}", urlencode(&key));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("mev-scout/0.1 (+https://github.com/local/mev-scout)")
        .build()?;
    let resp: LlamaResponse = client.get(&url).send().await?.json().await?;
    resp.coins
        .values()
        .next()
        .map(|c| c.price)
        .ok_or_else(|| anyhow::anyhow!("llama: no price for {key}"))
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
        .user_agent("mev-scout/0.1 (+https://github.com/local/mev-scout)")
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
    fn realized_rate_prices_unpriced_profit_token() {
        let token = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let mut prices = HashMap::new();
        prices.insert(
            usdc,
            TokenUsd {
                usd: 1.0,
                decimals: 6,
            },
        );
        let details = serde_json::json!({
            "route": [{
                "pool": "0x0000000000000000000000000000000000000000",
                "token_in": format!("{token:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "registry",
                "amount_in": "1000000",
                "amount_out": "2000000",
            }]
        });
        // 1 token sold for 2 USDC → realized rate 2.0 USD/token; 0.5 token ⇒ 1.0.
        let usd = amount_usd_realized(token, U256::from(500_000u64), &prices, &details)
            .unwrap()
            .usd;
        assert!((usd - 1.0).abs() < 1e-6, "got {usd}");
        // Cross-decimal: 1e18 raw of an 18-decimal token sold for 2 USDC (6 dec).
        // The ratio is same-token raw / raw, so decimals cancel → 0.5 token = $1.
        let details_xd = serde_json::json!({
            "route": [{
                "pool": "0x0000000000000000000000000000000000000000",
                "token_in": format!("{token:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "transfer",
                "amount_in": "1000000000000000000",
                "amount_out": "2000000",
            }]
        });
        let usd_xd = amount_usd_realized(
            token,
            U256::from(500_000_000_000_000_000u64),
            &prices,
            &details_xd,
        )
        .unwrap()
        .usd;
        assert!((usd_xd - 1.0).abs() < 1e-6, "cross-decimal got {usd_xd}");
        // Route leg where the profit token is the OUTPUT of a priced spend.
        let details2 = serde_json::json!({
            "route": [{
                "pool": "0x0000000000000000000000000000000000000000",
                "token_in": format!("{usdc:#x}"),
                "token_out": format!("{token:#x}"),
                "token_source": "registry",
                "amount_in": "1000000",
                "amount_out": "500000",
            }]
        });
        let usd2 = amount_usd_realized(token, U256::from(250_000u64), &prices, &details2)
            .unwrap()
            .usd;
        assert!((usd2 - 0.5).abs() < 1e-6, "got {usd2}");
        // External price always wins over the route-derived rate.
        prices.insert(
            token,
            TokenUsd {
                usd: 9.0,
                decimals: 6,
            },
        );
        let usd3 = amount_usd_realized(token, U256::from(1_000_000u64), &prices, &details)
            .unwrap()
            .usd;
        assert!((usd3 - 9.0).abs() < 1e-6, "got {usd3}");
        // No priced counterpart and no route → None.
        let other = address!("0b00000000000000000000000000000000000000");
        assert!(amount_usd_realized(other, U256::from(1u64), &prices, &details).is_none());
    }

    /// Block 26059586 shape: an 18-decimal residual priced off a proximity leg
    /// whose `amount_in` is a 6-decimal quantity. The old first-match formula
    /// returned `3000 * (1.31558265676348437e17 / 1e6) ~= 394674797029045.31`.
    #[test]
    #[allow(clippy::excessive_precision)]
    fn proximity_leg_does_not_blow_up_cross_decimal_residual() {
        let token = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let mut prices = HashMap::new();
        prices.insert(
            usdc,
            TokenUsd {
                usd: 1.0,
                decimals: 6,
            },
        );
        // amount_in of 1 raw against ~1.3156e17 of a 6-decimal priced token:
        // out_usd * 3000 / 1 == 3000 * (1.3156e17 / 1e6).
        let amount_out = "131558265676348437";
        let old: f64 = 3000.0 * (131_558_265_676_348_437.0 / 1e6);
        let expected: f64 = 394_674_797_029_045.31;
        assert!((old - expected).abs() < 1.0, "blowup shape drifted: {old}");
        for source in [None, Some("proximity")] {
            let mut leg = serde_json::json!({
                "pool": "0x0000000000000000000000000000000000000000",
                "token_in": format!("{token:#x}"),
                "token_out": format!("{usdc:#x}"),
                "amount_in": "1",
                "amount_out": amount_out,
            });
            if let Some(src) = source {
                leg["token_source"] = serde_json::json!(src);
            }
            let details = serde_json::json!({ "route": [leg] });
            assert!(
                amount_usd_realized(token, U256::from(3000u64), &prices, &details).is_none(),
                "source {source:?} must not reproduce {old}"
            );
        }
    }

    #[test]
    fn trusted_legs_best_match_keeps_largest_notional() {
        let token = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let mut prices = HashMap::new();
        prices.insert(
            usdc,
            TokenUsd {
                usd: 1.0,
                decimals: 6,
            },
        );
        // First leg is dust ($1 notional → $0.50 on a 500_000 residual).
        // Second leg is $100 notional → $50. Largest notional wins.
        let details = serde_json::json!({
            "route": [
                {
                    "token_in": format!("{token:#x}"),
                    "token_out": format!("{usdc:#x}"),
                    "token_source": "registry",
                    "amount_in": "1000000",
                    "amount_out": "1000000",
                },
                {
                    "token_in": format!("{token:#x}"),
                    "token_out": format!("{usdc:#x}"),
                    "token_source": "transfer",
                    "amount_in": "1000000",
                    "amount_out": "100000000",
                }
            ]
        });
        let q = amount_usd_realized(token, U256::from(500_000u64), &prices, &details).unwrap();
        assert!(!q.clamped);
        assert!((q.usd - 50.0).abs() < 1e-6, "got {}", q.usd);
    }

    #[test]
    fn residual_quote_clamped_to_trusted_route_usd() {
        let token = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4000000000000000000000000000000000000000");
        let mut prices = HashMap::new();
        prices.insert(
            usdc,
            TokenUsd {
                usd: 1.0,
                decimals: 6,
            },
        );
        // 1 raw sold for $1 USDC, residual of 1_000_000 raw would be $1e6.
        // Cap is the route notional ($1).
        let details = serde_json::json!({
            "route": [{
                "token_in": format!("{token:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "registry",
                "amount_in": "1",
                "amount_out": "1000000",
            }]
        });
        let q = amount_usd_realized(token, U256::from(1_000_000u64), &prices, &details).unwrap();
        assert!(q.clamped);
        assert!((q.usd - 1.0).abs() < 1e-6, "got {}", q.usd);

        prices.insert(
            token,
            TokenUsd {
                usd: f64::INFINITY,
                decimals: 18,
            },
        );
        assert!(
            amount_usd_realized(token, U256::from(1u64), &prices, &details).is_none(),
            "non-finite external price is rejected"
        );
    }

    #[test]
    fn fot_and_rebase_tokens_flag_approximate() {
        // USDT (0xdac1…) is in the bundled fee_on_transfer registry.
        let usdt = address!("dac17f958d2ee523a2206206994597c13d831ec7");
        assert_eq!(approximate_token(&usdt), Some("FOT"));
        // stETH (0xae7a…) is in the rebase registry.
        let steth = address!("ae7ab96520de3a18e5e111b5eaab095312d7fe84");
        assert_eq!(approximate_token(&steth), Some("REBASE"));
        assert_eq!(
            approximate_token(&address!("4000000000000000000000000000000000000000")),
            None
        );
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
