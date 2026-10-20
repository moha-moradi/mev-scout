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

/// Apply Compound-V2-fork underlying rewrite when the collateral exchange rate
/// is known (or mark `QI_EXCHANGE_RATE_MISSING` when it is not).
///
/// `collateral_rate`: `Some(rate)` converts seizeTokens; `None` with a cToken
/// collateral leaves USD null. Debt repay amount is already underlying — only
/// the debt address is remapped.
pub fn apply_c_token_liq_conversion(
    ev: &mut MevEvent,
    underlyings: &HashMap<Address, Address>,
    collateral_rate: Option<U256>,
) {
    if ev.kind != MevKind::Liquidation {
        return;
    }
    let Some(liq) = crate::explorer::types::LiquidationDetails::from_details(&ev.details) else {
        return;
    };
    let debt_is_qi = underlyings.contains_key(&liq.debt_asset);
    let coll_is_qi = underlyings.contains_key(&liq.collateral_asset);
    if !debt_is_qi && !coll_is_qi {
        return;
    }

    let mut details = ev.details.clone();
    if debt_is_qi {
        details["qi_debt_asset"] = serde_json::json!(format!("{:#x}", liq.debt_asset));
        details["qi_debt_to_cover"] = serde_json::json!(liq.debt_to_cover.to_string());
        let new_debt = underlyings[&liq.debt_asset];
        details["debt_asset"] = serde_json::json!(format!("{:#x}", new_debt));
    }
    if coll_is_qi {
        details["qi_collateral_asset"] = serde_json::json!(format!("{:#x}", liq.collateral_asset));
        details["qi_collateral_amount"] = serde_json::json!(liq.collateral_amount.to_string());
        let Some(rate) = collateral_rate else {
            push_reason(&mut details, "QI_EXCHANGE_RATE_MISSING");
            ev.profit_token = None;
            ev.profit_amount = None;
            ev.profit_usd = None;
            ev.details = details;
            return;
        };
        let new_coll = underlyings[&liq.collateral_asset];
        let new_coll_amt = qi_tokens_to_underlying(liq.collateral_amount, rate);
        details["collateral_asset"] = serde_json::json!(format!("{:#x}", new_coll));
        details["collateral_amount"] = serde_json::json!(new_coll_amt.to_string());
        details["qi_exchange_rate_wad"] = serde_json::json!(rate.to_string());
        if let Some(pnl) = details.get_mut("pnl").and_then(|v| v.as_object_mut()) {
            pnl.insert(
                "collateral_amount".into(),
                serde_json::json!(new_coll_amt.to_string()),
            );
        }
        ev.profit_token = Some(new_coll);
        ev.profit_amount = Some(new_coll_amt);
    } else if let Some(tok) = ev.profit_token {
        // Debt-only remap: keep profit on whatever collateral was already set.
        let _ = tok;
    }

    if let Some(pnl) = details.get_mut("pnl").and_then(|v| v.as_object_mut()) {
        pnl.insert(
            "debt_to_cover".into(),
            serde_json::json!(liq.debt_to_cover.to_string()),
        );
    }
    // If only debt remapped, refresh profit_token from details collateral.
    if !coll_is_qi {
        if let Some(c) = details
            .get("collateral_asset")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Address>().ok())
        {
            ev.profit_token = Some(c);
        }
        if let Some(a) = details
            .get("collateral_amount")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<U256>().ok())
        {
            ev.profit_amount = Some(a);
        }
    }
    ev.details = details;
}

/// Rewrite Compound-V2-fork liquidation events so PnL prices underlyings,
/// never cToken/qiToken/vToken addresses.
pub async fn convert_c_token_liquidation_events(
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
        let coll_is_qi = underlyings.contains_key(&liq.collateral_asset);
        let debt_is_qi = underlyings.contains_key(&liq.debt_asset);
        if !coll_is_qi && !debt_is_qi {
            continue;
        }
        // None only when collateral is a cToken and the rate call failed / was
        // cached miss. Debt-only remaps pass Some(_) that apply ignores.
        let collateral_rate = if coll_is_qi {
            fetch_exchange_rate_stored(rpc, liq.collateral_asset, block, ts, cache).await
        } else {
            Some(U256::ZERO)
        };
        apply_c_token_liq_conversion(ev, underlyings, collateral_rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use crate::explorer::types::{Confidence, MevEvent, MevKind};

    fn liq_event(collateral: Address, debt: Address, seize: u64, repay: u64) -> MevEvent {
        MevEvent {
            block: 1,
            ts: 1,
            tx_index: 0,
            tx_hash: alloy::primitives::B256::ZERO,
            kind: MevKind::Liquidation,
            searcher: Address::ZERO,
            contract: None,
            pools: vec![],
            profit_token: Some(collateral),
            profit_amount: Some(U256::from(seize)),
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({
                "protocol": "benqi",
                "collateral_asset": format!("{collateral:#x}"),
                "debt_asset": format!("{debt:#x}"),
                "collateral_amount": seize.to_string(),
                "debt_to_cover": repay.to_string(),
                "reasons": [],
                "pnl": {
                    "basis": "O",
                    "collateral_amount": seize.to_string(),
                    "debt_to_cover": repay.to_string(),
                },
            }),
        }
    }

    #[test]
    fn qi_tokens_to_underlying_scales_by_wad() {
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
    fn apply_converts_seize_and_remaps_debt() {
        let qi_avax = address!("5c0401e81bc07ca70fad469b451682c0d747ef1c");
        let wavax = address!("B31f66AA3C1e785363F0875A1B74E27b85FD66c7");
        let qi_btc = address!("89a415b3d20098e6a6c8f7a59001c67bd3129821");
        let btc_b = address!("152b9d0FdC40C096757F570A51E494bd4b943E50");
        let mut map = HashMap::new();
        map.insert(qi_avax, wavax);
        map.insert(qi_btc, btc_b);

        let mut ev = liq_event(qi_avax, qi_btc, 100, 50);
        let rate = U256::from(2u64) * U256::from(10u64.pow(18));
        apply_c_token_liq_conversion(&mut ev, &map, Some(rate));

        assert_eq!(ev.profit_token, Some(wavax));
        assert_eq!(ev.profit_amount, Some(U256::from(200u64)));
        let liq = crate::explorer::types::LiquidationDetails::from_details(&ev.details).unwrap();
        assert_eq!(liq.collateral_asset, wavax);
        assert_eq!(liq.collateral_amount, U256::from(200u64));
        assert_eq!(liq.debt_asset, btc_b);
        assert_eq!(liq.debt_to_cover, U256::from(50u64)); // repay already underlying
        assert_eq!(
            ev.details["qi_collateral_asset"].as_str().unwrap().to_lowercase(),
            format!("{qi_avax:#x}")
        );
    }

    #[test]
    fn apply_missing_rate_marks_reason_and_clears_profit() {
        let qi_avax = address!("5c0401e81bc07ca70fad469b451682c0d747ef1c");
        let wavax = address!("B31f66AA3C1e785363F0875A1B74E27b85FD66c7");
        let mut map = HashMap::new();
        map.insert(qi_avax, wavax);

        let mut ev = liq_event(qi_avax, wavax, 100, 1);
        apply_c_token_liq_conversion(&mut ev, &map, None);
        assert!(ev.profit_token.is_none());
        assert!(ev.profit_amount.is_none());
        let reasons = ev.details["reasons"].as_array().unwrap();
        assert!(reasons.iter().any(|r| r.as_str() == Some("QI_EXCHANGE_RATE_MISSING")));
        // qi amounts preserved for forensics
        assert!(ev.details.get("qi_collateral_asset").is_some());
    }

    /// Alias present but underlying map empty for that cToken → conversion
    /// no-ops (caller must leave USD null via rate-miss path, not invent USD).
    #[test]
    fn apply_skips_when_collateral_not_in_underlyings_map() {
        let qi_avax = address!("5c0401e81bc07ca70fad469b451682c0d747ef1c");
        let wavax = address!("B31f66AA3C1e785363F0875A1B74E27b85FD66c7");
        let map = HashMap::new(); // empty — aliased market not pinned
        let mut ev = liq_event(qi_avax, wavax, 100, 1);
        apply_c_token_liq_conversion(&mut ev, &map, Some(U256::from(10u64.pow(18))));
        // Unchanged: still prices qi address (store layer skips / nulls).
        assert_eq!(ev.profit_token, Some(qi_avax));
        assert_eq!(ev.profit_amount, Some(U256::from(100u64)));
        assert!(ev.details.get("qi_collateral_asset").is_none());
    }

    /// Cross-market: debt and collateral both cTokens (isolated) → both remap.
    #[test]
    fn apply_cross_market_both_isolated_remaps() {
        let qi_coq = address!("0ebfebd41e1ea83be5e911cdcd2730a0ccee344d");
        let coq = address!("420FcA0121DC28039145009570975747295F2329");
        let qi_joe = address!("4036cb0d6bf6b5f17aa4e05191f86d4b1655b0d9");
        let joe = address!("6e84a6216eA6dACC71eE8E6b0a5B7322EEbC0fDd");
        let mut map = HashMap::new();
        map.insert(qi_coq, coq);
        map.insert(qi_joe, joe);

        let mut ev = liq_event(qi_coq, qi_joe, 50, 10);
        let rate = U256::from(3u64) * U256::from(10u64.pow(18));
        apply_c_token_liq_conversion(&mut ev, &map, Some(rate));

        let liq = crate::explorer::types::LiquidationDetails::from_details(&ev.details).unwrap();
        assert_eq!(liq.collateral_asset, coq);
        assert_eq!(liq.collateral_amount, U256::from(150u64));
        assert_eq!(liq.debt_asset, joe);
        assert_eq!(liq.debt_to_cover, U256::from(10u64));
        assert_eq!(ev.profit_token, Some(coq));
        assert_eq!(ev.profit_amount, Some(U256::from(150u64)));
    }
}
