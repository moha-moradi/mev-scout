//! Persist-time PnL / pricing annotation helpers for explorer store.
use alloy::primitives::{Address, U256};

use crate::explorer::types::MevEvent;

/// Liquidation P&L (Phase 1.3): gross seized collateral value minus repaid
/// debt value, both priced at persist time. Cross-asset liquidations are an
/// approximation (liquidation bonus / market-sale slippage) and are marked
/// `inferred` with `LIQ_BONUS_APPROX`; missing prices fall back to the
/// collateral display amount with `MULTI_ASSET_PRICING`.
pub(super) fn liquidation_pnl(
    ev: &MevEvent,
    token_prices: &std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd>,
) -> (Option<f64>, &'static str, String) {
    use crate::explorer::types::LiquidationDetails;

    let liq = LiquidationDetails::from_details(&ev.details);
    let price_usd = |asset: Address, amount: U256| -> Option<f64> {
        let p = token_prices.get(&asset)?;
        Some(crate::explorer::pricing::token_amount_to_usd(amount, p))
    };
    let (collateral_usd, debt_usd, cross_asset) = match liq {
        Some(d) => (
            price_usd(d.collateral_asset, d.collateral_amount),
            price_usd(d.debt_asset, d.debt_to_cover),
            d.collateral_asset != d.debt_asset
                && !d.collateral_asset.is_zero()
                && !d.debt_asset.is_zero(),
        ),
        None => (None, None, true),
    };

    let mut details = ev.details.clone();
    let mut reasons = reason_list(&details);
    details["profit_usd_method"] = serde_json::json!("collateral_usd_minus_debt_usd");
    match (collateral_usd, debt_usd) {
        (Some(c), Some(d)) => {
            if cross_asset {
                reasons.push("LIQ_BONUS_APPROX".to_string());
                details["reasons"] = serde_json::json!(reasons);
                (Some(c - d), "inferred", details.to_string())
            } else {
                details["reasons"] = serde_json::json!(reasons);
                (Some(c - d), "exact", details.to_string())
            }
        }
        _ => {
            reasons.push("MULTI_ASSET_PRICING".to_string());
            details["profit_usd_fallback"] = serde_json::json!("collateral_amount");
            details["reasons"] = serde_json::json!(reasons);
            (None, "inferred", details.to_string())
        }
    }
}

/// Record why a persisted USD figure is incomplete or capped, and mark it
/// approximate. Reasons stack (`MULTI_ASSET_PRICING`, `NATIVE_UNPRICED`,
/// `pricing_clamped`) instead of replacing one another.
pub(super) fn note_pricing_issue(details_json: &mut String, reason: &str) {
    let mut d: serde_json::Value =
        serde_json::from_str(details_json).unwrap_or(serde_json::json!({}));
    let mut reasons = reason_list(&d);
    if !reasons.iter().any(|r| r == reason) {
        reasons.push(reason.to_string());
    }
    if let Some(map) = d.as_object_mut() {
        map.insert("reasons".into(), serde_json::json!(reasons));
        map.insert("usd_approximate".into(), serde_json::json!(true));
        if reason == "pricing_clamped" {
            map.insert("pricing_clamped".into(), serde_json::json!(true));
        }
    }
    *details_json = d.to_string();
}

/// Existing `reasons` array from event details (empty when absent).
pub(super) fn reason_list(details: &serde_json::Value) -> Vec<String> {
    details
        .get("reasons")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

