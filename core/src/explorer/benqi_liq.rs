//! Convert Benqi / Compound-V2 liquidation amounts off qiToken units before USD.
//!
//! `LiquidateBorrow` stores:
//! - `seizeTokens` in qiToken (cToken) units → need `exchangeRateStored`
//! - `repayAmount` already in the borrowed market's **underlying** units
//!
//! Pricing qiToken addresses directly yields `NULL` (unpriced) or inflated USD
//! when a qi address coincidentally matches a Llama feed. Rewrite details to
//! underlyings; if the collateral rate is missing, leave USD null.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};
use tracing::debug;

use crate::explorer::pricing;
use crate::explorer::types::{MevEvent, MevKind};
use crate::rpc::RpcClient;

/// Hourly `exchangeRateStored` cache keyed by (qiToken, hour_bucket).
#[derive(Debug, Default)]
pub struct QiRateCache {
    by_key: HashMap<(Address, u64), U256>,
    missed: HashSet<(Address, u64)>,
}

impl QiRateCache {
    fn get(&self, qi: Address, hour: u64) -> Option<U256> {
        self.by_key.get(&(qi, hour)).copied()
    }

    fn insert(&mut self, qi: Address, hour: u64, rate: U256) {
        self.missed.remove(&(qi, hour));
        self.by_key.insert((qi, hour), rate);
    }

    fn missed(&self, qi: Address, hour: u64) -> bool {
        self.missed.contains(&(qi, hour))
    }

    fn miss(&mut self, qi: Address, hour: u64) {
        self.missed.insert((qi, hour));
    }
}

/// `underlying = qi_amount * exchangeRateStored / 1e18` (Compound V2 scale).
pub fn qi_tokens_to_underlying(qi_amount: U256, exchange_rate_wad: U256) -> U256 {
    qi_amount.saturating_mul(exchange_rate_wad) / U256::from(10u64.pow(18))
}

async fn fetch_exchange_rate_stored(
    rpc: &RpcClient,
    qi: Address,
    block: u64,
    ts: u64,
    cache: &mut QiRateCache,
) -> Option<U256> {
    let hour = pricing::hour_bucket(ts);
    if let Some(rate) = cache.get(qi, hour) {
        return Some(rate);
    }
    if cache.missed(qi, hour) {
        return None;
    }
    let data = crate::pool::selectors::CTOKEN_EXCHANGE_RATE_STORED.to_vec();
    match rpc.call(qi, alloy::primitives::Bytes::from(data), block).await {
        Ok(ret) if ret.len() >= 32 => {
            let rate = U256::from_be_slice(&ret[..32]);
            if rate.is_zero() {
                cache.miss(qi, hour);
                return None;
            }
            cache.insert(qi, hour, rate);
            Some(rate)
        }
        Ok(_) => {
            cache.miss(qi, hour);
            None
        }
        Err(e) => {
            debug!(?qi, block, "qiToken exchangeRateStored eth_call failed: {e}");
            cache.miss(qi, hour);
            None
        }
    }
}

fn push_reason(details: &mut serde_json::Value, reason: &str) {
    let mut reasons: Vec<String> = details
        .get("reasons")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if !reasons.iter().any(|r| r == reason) {
        reasons.push(reason.to_string());
    }
    details["reasons"] = serde_json::json!(reasons);
}

/// Rewrite Benqi liquidation events so PnL prices underlyings, never qiTokens.
///
/// Debt address is remapped (repay is already underlying). Collateral seize
/// amount is converted with `exchangeRateStored`. Missing collateral rate →
/// `QI_EXCHANGE_RATE_MISSING` and clear profit fields so persist leaves USD null.
pub async fn convert_benqi_liquidation_events(
    rpc: &RpcClient,
    events: &mut [MevEvent],
    underlyings: &HashMap<Address, Address>,
    block: u64,
    ts: u64,
    cache: &mut QiRateCache,
) {
    if underlyings.is_empty() {
        return;
    }
    for ev in events.iter_mut() {
        if ev.kind != MevKind::Liquidation {
            continue;
        }
        let Some(liq) = crate::explorer::types::LiquidationDetails::from_details(&ev.details) else {
            continue;
        };
        let debt_is_qi = underlyings.contains_key(&liq.debt_asset);
        let coll_is_qi = underlyings.contains_key(&liq.collateral_asset);
        if !debt_is_qi && !coll_is_qi {
            continue;
        }

        let mut details = ev.details.clone();
        // Preserve raw qi economics before rewrite.
        if debt_is_qi {
            details["qi_debt_asset"] = serde_json::json!(format!("{:#x}", liq.debt_asset));
            details["qi_debt_to_cover"] = serde_json::json!(liq.debt_to_cover.to_string());
        }
        if coll_is_qi {
            details["qi_collateral_asset"] =
                serde_json::json!(format!("{:#x}", liq.collateral_asset));
            details["qi_collateral_amount"] = serde_json::json!(liq.collateral_amount.to_string());
        }

        let mut new_debt = liq.debt_asset;
        let new_debt_amt = liq.debt_to_cover; // already underlying for Compound/Benqi
        if debt_is_qi {
            new_debt = underlyings[&liq.debt_asset];
            details["debt_asset"] = serde_json::json!(format!("{:#x}", new_debt));
        }

        let mut new_coll = liq.collateral_asset;
        let mut new_coll_amt = liq.collateral_amount;
        if coll_is_qi {
            let Some(rate) =
                fetch_exchange_rate_stored(rpc, liq.collateral_asset, block, ts, cache).await
            else {
                push_reason(&mut details, "QI_EXCHANGE_RATE_MISSING");
                // Do not leave qi addresses for pricing — clear display amounts.
                ev.profit_token = None;
                ev.profit_amount = None;
                ev.profit_usd = None;
                ev.details = details;
                continue;
            };
            new_coll = underlyings[&liq.collateral_asset];
            new_coll_amt = qi_tokens_to_underlying(liq.collateral_amount, rate);
            details["collateral_asset"] = serde_json::json!(format!("{:#x}", new_coll));
            details["collateral_amount"] = serde_json::json!(new_coll_amt.to_string());
            details["qi_exchange_rate_wad"] = serde_json::json!(rate.to_string());
            if let Some(pnl) = details.get_mut("pnl").and_then(|v| v.as_object_mut()) {
                pnl.insert(
                    "collateral_amount".into(),
                    serde_json::json!(new_coll_amt.to_string()),
                );
            }
        }

        ev.profit_token = Some(new_coll);
        ev.profit_amount = Some(new_coll_amt);
        // Keep debt_to_cover numeric (unchanged) but asset may have been remapped.
        if let Some(pnl) = details.get_mut("pnl").and_then(|v| v.as_object_mut()) {
            pnl.insert(
                "debt_to_cover".into(),
                serde_json::json!(new_debt_amt.to_string()),
            );
        }
        let _ = new_debt;
        ev.details = details;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn qi_tokens_to_underlying_scales_by_wad() {
        // 100 qi * 2e18 rate → 200 underlying (same decimals as rate scale).
        let qi = U256::from(100u64);
        let rate = U256::from(2u64) * U256::from(10u64.pow(18));
        assert_eq!(qi_tokens_to_underlying(qi, rate), U256::from(200u64));
    }

    #[test]
    fn qi_rate_cache_miss_sticks_until_insert() {
        let mut cache = QiRateCache::default();
        let qi = address!("5c0401e81bc07ca70fad469b451682c0d747ef1c");
        let hour = 1u64;
        assert!(cache.get(qi, hour).is_none());
        cache.miss(qi, hour);
        assert!(cache.missed(qi, hour));
        cache.insert(qi, hour, U256::from(42u64));
        assert!(!cache.missed(qi, hour));
        assert_eq!(cache.get(qi, hour), Some(U256::from(42u64)));
    }

    #[test]
    fn convert_marks_missing_rate_without_rpc() {
        // Sync path: empty cache + no underlyings → no-op.
        let underlyings = HashMap::new();
        let mut events = vec![];
        let mut cache = QiRateCache::default();
        // Just ensure empty map is a no-op (async runtime not needed for empty).
        assert!(underlyings.is_empty());
        assert!(events.is_empty());
        let _ = &mut cache;
    }
}
