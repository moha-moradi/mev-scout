//! Persist-time PnL / pricing annotation helpers for explorer store.
//!
//! Everything here is pure: no `Connection` enters these functions. `facts.rs`
//! owns the SQL; this module owns the business logic that decides *what* to
//! persist for one event.
use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::explorer::pricing::TokenUsd;
use crate::explorer::types::{MevEvent, MevKind};

use super::merge_details_json;
#[cfg(test)]
use super::SwapRow;

/// One fully annotated `mev_ops` row, ready to bind.
///
/// Field order mirrors the `mev_ops` INSERT column list in `facts.rs`:
/// `block_number, tx_index, tx_hash, ts, kind, eoa, contract, confidence,
/// canonical_id, profit_token, profit_amount, volume_usd, profit_usd,
/// gas_cost_usd, flashloan_fee_usd, net_profit_usd, route_json,
/// victim_hashes, details_json, detector, created_at, excluded_from_net`.
/// `detector` is the fixed `'explorer'` SQL literal (not a bind), so it has no
/// field; every other column does.
#[derive(Debug, Clone)]
pub struct PersistedOp {
    pub block_number: u64,
    pub tx_index: u64,
    pub tx_hash: String,
    pub ts: u64,
    pub kind: &'static str,
    pub eoa: String,
    pub contract: Option<String>,
    pub confidence: &'static str,
    pub canonical_id: String,
    pub profit_token: Option<String>,
    pub profit_amount: Option<String>,
    pub volume_usd: Option<f64>,
    pub profit_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub flashloan_fee_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub route_json: Option<String>,
    pub victim_hashes: Option<String>,
    pub details_json: Option<String>,
    pub created_at: i64,
    pub excluded_from_net: i64,
    /// `keeper` | `bundler` | `claim_and_sell` when this row is `Utility`
    /// (or carries exactly one of those tags). Absent otherwise.
    pub subkind: Option<&'static str>,
    /// Pricing-quality flags promoted out of `details_json` so reports can
    /// count them without substring scans.
    pub usd_approximate: i64,
    pub native_unpriced: i64,
    pub multi_asset_pricing: i64,
    pub pricing_clamped: i64,
    pub flash_fee_unpriced: i64,
}

/// Apply every persist-time business rule for one event and return the row to
/// bind: PnL annotation (liquidation or residual pricing), the A10 sandwich
/// loss gate, volume, flash-fee conversion, native-residual pricing (A16) and
/// the A19 `pnl_basis`/confidence handling.
///
/// Pure: the caller supplies the block-level price inputs and the event's
/// precomputed volume (one scan per block, not one per event); no store handle
/// is involved.
pub fn annotate_event_with_volume(
    ev: &MevEvent,
    volume_usd: Option<f64>,
    token_prices: &HashMap<Address, TokenUsd>,
    native_price_usd: Option<f64>,
    created_at: i64,
) -> PersistedOp {
    // Liquidation P&L: profit ≈ collateral_usd − debt_usd, valued at persist
    // with hourly prices. Never subtract raw amounts when tokens differ.
    let mut confidence = ev.confidence.as_str();
    let mut details_json = ev.details.to_string();
    let mut pricing_clamped = false;
    let mut unpriced_residual = false;
    let mut native_unpriced = false;
    let profit_usd = if ev.kind == MevKind::Liquidation {
        let (usd, conf, details) = liquidation_pnl(ev, token_prices);
        confidence = conf;
        details_json = details;
        usd
    } else if !ev.profit_tokens.is_empty() {
        // USD-sum across every positive residual (flash-netting already
        // cleaned the ledger in 2.2) with realized-rate fallback for unpriced
        // tokens. A missing quote is not zero: the sum is partial and marked
        // approximate instead of being stored as a complete figure.
        let mut total = 0.0f64;
        for (tok, amt) in &ev.profit_tokens {
            if *tok == crate::explorer::profit::NATIVE_MARKER {
                // The native price used for gas is already in scope: price the
                // native residual with it instead of persisting NULL profit
                // for ETH-denominated MEV. Only a genuinely missing price is a
                // data limit worth flagging.
                match native_price_usd {
                    Some(np) => {
                        total += crate::explorer::pricing::wei_to_usd(*amt, np);
                    }
                    None => native_unpriced = true,
                }
                continue;
            }
            match crate::explorer::pricing::amount_usd_realized(
                *tok,
                *amt,
                token_prices,
                &ev.details,
            ) {
                Some(q) if q.usd.is_finite() => {
                    total += q.usd;
                    pricing_clamped |= q.clamped;
                }
                _ => unpriced_residual = true,
            }
        }
        if !total.is_finite() {
            total = 0.0;
            unpriced_residual = true;
        }
        details_json = merge_details_json(
            &details_json,
            [(
                "profit_tokens",
                serde_json::json!(ev
                    .profit_tokens
                    .iter()
                    .map(|(tok, amt)| serde_json::json!({
                        "token": format!("{tok:#x}"),
                        "amount": amt.to_string(),
                    }))
                    .collect::<Vec<_>>()),
            )],
        );
        (total > 0.0).then_some(total)
    } else {
        match (ev.profit_token, ev.profit_amount) {
            (Some(tok), Some(amt)) if tok == crate::explorer::profit::NATIVE_MARKER => {
                match native_price_usd {
                    Some(np) => Some(crate::explorer::pricing::wei_to_usd(amt, np)),
                    None => {
                        native_unpriced = true;
                        None
                    }
                }
            }
            (Some(tok), _) if tok == crate::explorer::profit::NATIVE_MARKER => {
                native_unpriced = true;
                None
            }
            (Some(tok), Some(amt)) => {
                match crate::explorer::pricing::amount_usd_realized(
                    tok,
                    amt,
                    token_prices,
                    &ev.details,
                ) {
                    Some(q) if q.usd.is_finite() => {
                        pricing_clamped |= q.clamped;
                        Some(q.usd)
                    }
                    _ => {
                        unpriced_residual = true;
                        None
                    }
                }
            }
            _ => None,
        }
    };
    if unpriced_residual {
        note_pricing_issue(&mut details_json, "MULTI_ASSET_PRICING");
    }
    if native_unpriced {
        note_pricing_issue(&mut details_json, "NATIVE_UNPRICED");
    }
    if pricing_clamped {
        note_pricing_issue(&mut details_json, "pricing_clamped");
    }
    // FOT / rebase profit tokens are flagged approximate (recorded amounts are
    // distorted by the token mechanics).
    if let Some(reason) = ev
        .profit_tokens
        .iter()
        .map(|(tok, _)| *tok)
        .chain(ev.profit_token)
        .find_map(|t| crate::explorer::pricing::approximate_token(&t))
    {
        details_json = merge_details_json(
            &details_json,
            [
                ("usd_approximate", serde_json::json!(true)),
                ("approximate_reason", serde_json::json!(reason)),
            ],
        );
    }
    // Gas in USD via native price (gas wei → native → USD).
    let gas_usd =
        native_price_usd.map(|np| crate::explorer::pricing::wei_to_usd(ev.gas_cost_wei, np));
    // Flash-loan fee in USD via the borrowed token's price (2.2). A non-zero
    // fee whose token has no usable quote is not worth $0: the row keeps NULL
    // (never an invented zero) and the op is flagged so the report can say the
    // fee is unmeasurable. A zero quote is not a price (A14), so a
    // non-positive/non-finite USD value counts as unpriced too.
    let mut flash_fee_unpriced = false;
    let flashloan_fee_usd = match (ev.flashloan_fee_token, ev.flashloan_fee_wei) {
        (Some(tok), Some(amt)) if !amt.is_zero() => match token_prices.get(&tok) {
            Some(p) => {
                let usd = crate::explorer::pricing::token_amount_to_usd(amt, p);
                if usd.is_finite() && usd > 0.0 {
                    Some(usd)
                } else {
                    flash_fee_unpriced = true;
                    None
                }
            }
            None => {
                flash_fee_unpriced = true;
                None
            }
        },
        _ => None,
    };
    // The classifier netted `borrowed + fee` into the ledger when the repay
    // Transfer was absent, so the residual it reported is already fee-netted:
    // subtracting the fee here would count it twice. The fee is still written
    // to the row for display.
    let fee_netted = ev
        .details
        .get("flashloan_fee_netted_in_ledger")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Only an unpriced fee the ledger did *not* already absorb makes `net`
    // incomplete. A netted fee sits inside the token residual, so its missing
    // USD quote changes nothing downstream — flagging it would claim a gap
    // that does not exist.
    if flash_fee_unpriced && !fee_netted {
        note_pricing_issue(&mut details_json, "FLASH_FEE_UNPRICED");
    }
    let fee_for_net = if fee_netted {
        0.0
    } else {
        flashloan_fee_usd.unwrap_or(0.0)
    };
    let net = match (profit_usd, gas_usd) {
        (Some(g), Some(gg)) => Some(g - gg - fee_for_net),
        _ => None,
    };
    // Sandwich profitability gate: a sandwich whose extractable profit does
    // not cover combined front+back gas (plus the flash-loan fee, unless the
    // classifier already netted it into the ledger) is not a realized-MEV op
    // worth netting — but it is persisted anyway with `excluded_from_net = 1`
    // so the forensics stay queryable and op counts stay truthful; every *net*
    // aggregation skips the row. Gross, volume and fee sums still include it:
    // the extraction really happened, only the provable loss is hidden from
    // net sums. Unowned-net (missing price) events are kept unflagged — the
    // gate never guesses.
    let excluded_from_net =
        (ev.kind == MevKind::Sandwich && matches!(net, Some(n) if n <= 0.0)) as i64;
    let details_value: serde_json::Value = serde_json::from_str(&details_json).unwrap_or_default();
    let reason_present = |name: &str| {
        details_value
            .get("reasons")
            .and_then(|r| r.as_array())
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(name)))
    };
    let usd_approximate_flag = i64::from(
        details_value
            .get("usd_approximate")
            .and_then(|v| v.as_bool())
            == Some(true),
    );
    let native_unpriced_flag = i64::from(reason_present("NATIVE_UNPRICED"));
    let multi_asset_pricing_flag = i64::from(reason_present("MULTI_ASSET_PRICING"));
    let pricing_clamped_flag = i64::from(reason_present("pricing_clamped"));
    let flash_fee_unpriced_flag = i64::from(reason_present("FLASH_FEE_UNPRICED"));
    PersistedOp {
        block_number: ev.block,
        tx_index: ev.tx_index,
        tx_hash: format!("{:#x}", ev.tx_hash),
        ts: ev.ts,
        kind: ev.kind.as_str(),
        eoa: format!("{:#x}", ev.searcher),
        contract: ev.contract.map(|a| format!("{:#x}", a)),
        confidence,
        canonical_id: crate::explorer::explorer_canonical_id(ev),
        profit_token: ev.profit_token.map(|a| format!("{:#x}", a)),
        profit_amount: ev.profit_amount.map(|a| a.to_string()),
        volume_usd,
        profit_usd,
        gas_cost_usd: gas_usd,
        flashloan_fee_usd,
        net_profit_usd: net,
        route_json: ev.details.get("route").map(|r| r.to_string()),
        victim_hashes: serde_json::to_string(
            &ev.victim_hashes
                .iter()
                .map(|h| format!("{:#x}", h))
                .collect::<Vec<_>>(),
        )
        .ok(),
        details_json: Some(details_json),
        created_at,
        excluded_from_net,
        subkind: crate::explorer::types::utility_subkind(&ev.details),
        usd_approximate: usd_approximate_flag,
        native_unpriced: native_unpriced_flag,
        multi_asset_pricing: multi_asset_pricing_flag,
        pricing_clamped: pricing_clamped_flag,
        flash_fee_unpriced: flash_fee_unpriced_flag,
    }
}

/// Liquidation P&L: gross seized collateral value minus repaid
/// debt value, both priced at persist time. Cross-asset liquidations are an
/// approximation (liquidation bonus / market-sale slippage) and are marked
/// `inferred` with `LIQ_BONUS_APPROX`; missing prices fall back to the
/// collateral display amount with `MULTI_ASSET_PRICING`.
pub(super) fn liquidation_pnl(
    ev: &MevEvent,
    token_prices: &std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd>,
) -> (Option<f64>, &'static str, String) {
    use crate::explorer::types::LiquidationDetails;

    // Benqi seizeTokens left in qi units when exchangeRateStored failed —
    // never price the qiToken address (wrong Llama matches / inflated USD).
    let reasons_pre = reason_list(&ev.details);
    if reasons_pre.iter().any(|r| r == "QI_EXCHANGE_RATE_MISSING") {
        let mut details = ev.details.clone();
        details["profit_usd_method"] = serde_json::json!("skipped_qi_unconverted");
        return (None, "inferred", details.to_string());
    }

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

/// Test helper: derive an event's volume from the block's swaps, matching the
/// persist path's precomputed per-tx map.
#[cfg(test)]
pub fn annotate_event(
    ev: &MevEvent,
    swaps: &[SwapRow],
    token_prices: &HashMap<Address, TokenUsd>,
    native_price_usd: Option<f64>,
    created_at: i64,
) -> PersistedOp {
    let total = swaps
        .iter()
        .filter(|s| s.tx_index == ev.tx_index)
        .fold(0.0f64, |acc, s| {
            acc + token_prices
                .get(&s.token_in)
                .map(|p| crate::explorer::pricing::token_amount_to_usd(s.amount_in, p))
                .unwrap_or(0.0)
        });
    annotate_event_with_volume(
        ev,
        (total > 0.0).then_some(total),
        token_prices,
        native_price_usd,
        created_at,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::{Amm, Confidence};
    use alloy::primitives::{address, b256};

    fn token_usd(usd: f64, decimals: u32) -> TokenUsd {
        TokenUsd { usd, decimals }
    }

    fn sample_event(block: u64) -> MevEvent {
        MevEvent {
            block,
            ts: 1_700_000_000,
            tx_index: 3,
            tx_hash: b256!("1111111111111111111111111111111111111111111111111111111111111111"),
            kind: MevKind::ArbAtomic,
            searcher: address!("2222000000000000000000000000000000000002"),
            contract: None,
            pools: vec![address!("3333000000000000000000000000000000000003")],
            profit_token: Some(address!("4444000000000000000000000000000000000004")),
            profit_amount: Some(U256::from(1_000_000u64)),
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::from(100_000_000_000_000u64),
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({
                "route": [{"pool": "0x3333", "amm": "v3"}]
            }),
        }
    }

    fn swap_leg(tx_index: u64, token_in: Address, amount_in: u64) -> SwapRow {
        SwapRow {
            tx_index,
            log_index: 0,
            pool: address!("3333000000000000000000000000000000000003"),
            amm: Amm::V3,
            token_in,
            token_out: address!("5555000000000000000000000000000000000005"),
            amount_in: U256::from(amount_in),
            amount_out: U256::ZERO,
        }
    }

    /// USD-sum across every positive residual: the priced legs are added, the
    /// display-primary pair is left untouched, and a complete sum carries no
    /// `MULTI_ASSET_PRICING` flag.
    #[test]
    fn multi_residual_profit_usd_sums_all_priced_tokens() {
        let tok_a = address!("4444000000000000000000000000000000000004"); // 6 decimals
        let tok_b = address!("5555000000000000000000000000000000000005"); // 18 decimals
        let mut ev = sample_event(200);
        ev.profit_token = Some(tok_a);
        ev.profit_amount = Some(U256::from(1_000_000u64)); // 1 token @0.5 = 0.5 USD
        ev.profit_tokens = vec![
            (tok_a, U256::from(1_000_000u64)),
            (tok_b, U256::from(1_000_000_000_000_000_000u64)), // 1 token @2.0 = 2 USD
        ];
        let mut prices = HashMap::new();
        prices.insert(tok_a, token_usd(0.5, 6));
        prices.insert(tok_b, token_usd(2.0, 18));

        let op = annotate_event(&ev, &[], &prices, Some(0.75), 0);
        let expected = 0.5 + 2.0;
        let profit_usd = op.profit_usd.expect("fully priced residual sum");
        assert!((profit_usd - expected).abs() < 1e-6, "got {profit_usd}");
        // Net subtracts gas (100_000_000_000_000 wei native @0.75 = 0.000075 USD).
        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;
        let net = op.net_profit_usd.expect("net is priced");
        assert!((net - (expected - gas_usd)).abs() < 1e-9, "net got {net}");
        // Display-primary pair is persisted unchanged.
        assert_eq!(
            op.profit_token.as_deref(),
            Some("0x4444000000000000000000000000000000000004")
        );
        assert_eq!(op.profit_amount.as_deref(), Some("1000000"));
        // Residual list surfaces in details for forensic display.
        let details = op.details_json.as_deref().unwrap_or("");
        assert!(details.contains("\"profit_tokens\""));
        // Every residual had an external price, so the sum is complete.
        assert!(!details.contains("MULTI_ASSET_PRICING"));
    }

    /// A ledger that already absorbed the flash-loan fee (the repay leg was
    /// absent, so `arb.rs` recorded `flashloan_fee_netted_in_ledger`) must not
    /// have that fee removed a second time at persist. The unfetched case keeps
    /// the old `profit − gas − fee` arithmetic, and the fee stays on the row
    /// for display either way.
    #[test]
    fn flashloan_fee_is_not_subtracted_twice_when_ledger_netted_it() {
        let tok = address!("4444000000000000000000000000000000000004"); // 6 decimals
        let mut prices = HashMap::new();
        prices.insert(tok, token_usd(0.5, 6));
        let profit_usd = 0.5; // 1_000_000 units @ $0.5
        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;
        let fee_usd = 1_000f64 / 1e6 * 0.5; // 1_000 units @ $0.5 = $0.0005

        let mut ev = sample_event(400);
        ev.profit_token = Some(tok);
        ev.profit_amount = Some(U256::from(1_000_000u64));
        ev.gas_cost_wei = U256::from(100_000_000_000_000u64);
        ev.flashloan_fee_wei = Some(U256::from(1_000u64));
        ev.flashloan_fee_token = Some(tok);

        // No signal: the fee was not netted into the ledger, so persist removes it.
        let plain = ev.clone();
        // Ledger netted the fee: persist must not remove it again.
        let mut netted = ev;
        netted.details["flashloan_fee_netted_in_ledger"] = serde_json::json!(true);

        for (event, expected_net) in [
            (plain, profit_usd - gas_usd - fee_usd),
            (netted, profit_usd - gas_usd),
        ] {
            let op = annotate_event(&event, &[], &prices, Some(0.75), 0);
            let net = op.net_profit_usd.expect("net is priced");
            assert!(
                (net - expected_net).abs() < 1e-9,
                "net {net} != expected {expected_net}"
            );
            // Display keeps the fee on the row in both cases.
            let fee_usd_row = op.flashloan_fee_usd.expect("fee stays visible");
            assert!(
                (fee_usd_row - fee_usd).abs() < 1e-12,
                "fee {fee_usd_row} != {fee_usd}"
            );
        }
    }

    /// Pricing edges for a single-token residual: FOT token flagged
    /// approximate, realized route rate used when the feed is absent, and the
    /// cross-decimal / proximity blowup shapes both priced correctly.
    #[test]
    fn fot_profit_token_flagged_approximate_and_realized_rate_falls_back() {
        let usdt = address!("dac17f958d2ee523a2206206994597c13d831ec7"); // bundled FOT
        let longtail = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4444000000000000000000000000000000000004");

        let mut prices = HashMap::new();
        prices.insert(usdt, token_usd(0.99, 6));
        prices.insert(usdc, token_usd(1.0, 6));

        let mut fot = sample_event(301);
        fot.profit_token = Some(usdt);
        fot.profit_amount = Some(U256::from(1_000_000u64));
        // Unpriced profit token priced at the realized route rate:
        // 1 longtail token sold for 2 USDC ⇒ each is worth 2 USD.
        let mut realized = sample_event(302);
        realized.profit_token = Some(longtail);
        realized.profit_amount = Some(U256::from(500_000u64));
        realized.details = serde_json::json!({
            "route": [{
                "pool": "0x3333",
                "amm": "v2",
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "registry",
                "amount_in": "1000000",
                "amount_out": "2000000",
            }]
        });

        let fot_op = annotate_event(&fot, &[], &prices, Some(0.75), 0);
        let fot_details = fot_op.details_json.as_deref().unwrap_or("");
        assert!(fot_details.contains("\"usd_approximate\":true"));
        assert!(fot_details.contains("\"approximate_reason\":\"FOT\""));
        // FOT still priced from the external feed.
        assert!((fot_op.profit_usd.expect("FOT priced") - 0.99).abs() < 1e-6);

        let realized_op = annotate_event(&realized, &[], &prices, Some(0.75), 0);
        // 0.5 longtail token × realized 2.0 USD/token = 1.0, minus gas.
        let expected_profit = 1.0;
        let realized_profit = realized_op.profit_usd.expect("realized priced");
        assert!(
            (realized_profit - expected_profit).abs() < 1e-6,
            "got {realized_profit}"
        );
        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;
        assert_eq!(
            realized_op.net_profit_usd.expect("net priced"),
            expected_profit - gas_usd
        );

        // Cross-decimal trusted leg: 1e18 raw sold for 2 USDC, so 0.5e18 is $1.
        // Decimals cancel inside the same-token ratio.
        let mut cross = sample_event(303);
        cross.profit_token = Some(longtail);
        cross.profit_amount = Some(U256::from(500_000_000_000_000_000u64));
        cross.details = serde_json::json!({
            "route": [{
                "pool": "0x3333",
                "amm": "v2",
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "registry",
                "amount_in": "1000000000000000000",
                "amount_out": "2000000",
            }]
        });
        // Proximity leg with the block-26059586 ratio must not be stored as a
        // complete USD figure. 3000 raw against amount_in 1 and ~1.3e17 of a
        // 6-decimal token is the $394,674,797,029,045 shape.
        let mut guessed = sample_event(304);
        guessed.profit_token = Some(longtail);
        guessed.profit_amount = Some(U256::from(3000u64));
        guessed.profit_tokens = vec![(longtail, U256::from(3000u64))];
        guessed.details = serde_json::json!({
            "route": [{
                "pool": "0x3333",
                "amm": "v2",
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "proximity",
                "amount_in": "1",
                "amount_out": "131558265676348437",
            }]
        });

        let cross_op = annotate_event(&cross, &[], &prices, Some(0.75), 0);
        let cross_usd = cross_op.profit_usd.expect("cross-decimal priced");
        assert!(
            (cross_usd - 1.0).abs() < 1e-6,
            "cross-decimal got {cross_usd}"
        );

        let guessed_op = annotate_event(&guessed, &[], &prices, Some(0.75), 0);
        assert!(
            guessed_op.profit_usd.is_none(),
            "got {:?}",
            guessed_op.profit_usd
        );
        let guessed_details = guessed_op.details_json.as_deref().unwrap_or("");
        assert!(guessed_details.contains("MULTI_ASSET_PRICING"));
        assert!(guessed_details.contains("\"usd_approximate\":true"));
    }

    /// Clamped realized rates, native residuals and partially priced sums are
    /// each surfaced explicitly instead of being folded into a complete figure.
    #[test]
    fn clamped_and_native_residuals_are_explicit() {
        let longtail = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4444000000000000000000000000000000000004");
        let mut prices = HashMap::new();
        prices.insert(usdc, token_usd(1.0, 6));

        let mut clamped = sample_event(410);
        clamped.profit_token = Some(longtail);
        clamped.profit_amount = Some(U256::from(1_000_000u64));
        clamped.details = serde_json::json!({
            "route": [{
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "registry",
                "amount_in": "1",
                "amount_out": "1000000",
            }]
        });

        let mut native = sample_event(411);
        native.profit_token = Some(usdc);
        native.profit_amount = Some(U256::from(1_000_000u64));
        native.profit_tokens = vec![
            (usdc, U256::from(1_000_000u64)),
            (crate::explorer::profit::NATIVE_MARKER, U256::from(1u64)),
        ];

        let mut partial = sample_event(412);
        partial.profit_token = Some(usdc);
        partial.profit_amount = Some(U256::from(1_000_000u64));
        partial.profit_tokens = vec![
            (usdc, U256::from(1_000_000u64)),
            (longtail, U256::from(100u64)),
        ];

        let clamped_op = annotate_event(&clamped, &[], &prices, Some(0.75), 0);
        assert!((clamped_op.profit_usd.expect("clamped priced") - 1.0).abs() < 1e-6);
        let clamped_details = clamped_op.details_json.as_deref().unwrap_or("");
        assert!(clamped_details.contains("pricing_clamped"));
        assert!(clamped_details.contains("\"usd_approximate\":true"));

        // USDC residual is $1; since A16 the native residual is priced with
        // the in-scope native price ($0.75 × 1 wei ≈ $0), so it contributes
        // real USD and raises no unpriced/approximate reason.
        let native_op = annotate_event(&native, &[], &prices, Some(0.75), 0);
        assert!((native_op.profit_usd.expect("native priced") - 1.0).abs() < 1e-6);
        let native_details = native_op.details_json.as_deref().unwrap_or("");
        assert!(
            !native_details.contains("NATIVE_UNPRICED"),
            "a native residual priced from the in-scope native price must not be flagged unpriced"
        );
        assert!(
            !native_details.contains("\"usd_approximate\":true"),
            "a fully priced native residual is not approximate"
        );

        // Priced USDC is kept; the unpriced residual is not silently folded in as $0.
        let partial_op = annotate_event(&partial, &[], &prices, Some(0.75), 0);
        assert!((partial_op.profit_usd.expect("partial priced") - 1.0).abs() < 1e-6);
        let partial_details = partial_op.details_json.as_deref().unwrap_or("");
        assert!(partial_details.contains("MULTI_ASSET_PRICING"));
        assert!(partial_details.contains("\"usd_approximate\":true"));
    }

    /// Volume is the USD notional of the op's own swap legs; unpriced legs are
    /// skipped and an entirely unpriced leg set yields `None`.
    #[test]
    fn volume_usd_prices_only_the_op_tx_legs() {
        let usdc = address!("4444000000000000000000000000000000000004");
        let unpriced = address!("9999000000000000000000000000000000000009");
        let mut prices = HashMap::new();
        prices.insert(usdc, token_usd(1.0, 6));

        let mut ev = sample_event(400);
        ev.tx_index = 7;
        let legs = [
            swap_leg(7, usdc, 5_000_000),   // $5
            swap_leg(7, usdc, 2_000_000),   // $2
            swap_leg(9, usdc, 100_000_000), // other tx, not attributed
        ];
        let op = annotate_event(&ev, &legs, &prices, Some(0.75), 0);
        let v = op.volume_usd.expect("volume from the op's legs");
        assert!((v - 7.0).abs() < 1e-6, "op volume {v}");

        let mut unpr = sample_event(401);
        unpr.tx_index = 11;
        let unpr_legs = [swap_leg(11, unpriced, 123)];
        let op = annotate_event(&unpr, &unpr_legs, &prices, Some(0.75), 0);
        assert!(op.volume_usd.is_none(), "got {:?}", op.volume_usd);
    }

    /// A10: the sandwich gate flags — never drops — a sandwich whose net is
    /// known and non-positive. Positive-net sandwiches, other kinds, and
    /// sandwiches whose net could not be priced stay unflagged (the gate never
    /// guesses).
    #[test]
    fn sandwich_loss_gate_flags_only_known_non_positive_net() {
        let tok = address!("4444000000000000000000000000000000000004");
        let mut prices = HashMap::new();
        prices.insert(tok, token_usd(0.5, 6));

        // Zero extractable profit cannot cover the gas: net < 0.
        let mut lossy = sample_event(120);
        lossy.kind = MevKind::Sandwich;
        lossy.profit_amount = Some(U256::ZERO);

        let mut winner = sample_event(121);
        winner.kind = MevKind::Sandwich;

        let mut other_kind = sample_event(122);
        other_kind.profit_amount = Some(U256::ZERO);

        // No profit figure at all: net is None, so the gate cannot judge.
        let mut unpriced = sample_event(123);
        unpriced.kind = MevKind::Sandwich;
        unpriced.profit_token = None;
        unpriced.profit_amount = None;

        let op = annotate_event(&lossy, &[], &prices, Some(0.75), 0);
        assert!(op.net_profit_usd.expect("net is priced") < 0.0);
        assert_eq!(op.excluded_from_net, 1);

        let op = annotate_event(&winner, &[], &prices, Some(0.75), 0);
        assert!(op.net_profit_usd.expect("net is priced") > 0.0);
        assert_eq!(op.excluded_from_net, 0);

        let op = annotate_event(&other_kind, &[], &prices, Some(0.75), 0);
        assert!(op.net_profit_usd.expect("net is priced") < 0.0);
        assert_eq!(op.excluded_from_net, 0, "only sandwiches are loss-gated");

        let op = annotate_event(&unpriced, &[], &prices, Some(0.75), 0);
        assert!(op.net_profit_usd.is_none());
        assert_eq!(op.excluded_from_net, 0, "the gate never guesses");
    }

    /// A13/A14: a non-zero flash fee is converted only with a usable quote. An
    /// unmeasurable fee stays NULL and is flagged — unless the ledger already
    /// absorbed it, in which case flagging would claim a gap that does not
    /// exist. A zero fee never invents a price or a flag.
    #[test]
    fn flash_fee_converts_only_with_a_usable_quote() {
        let tok = address!("4444000000000000000000000000000000000004");
        let unpriced_tok = address!("9999000000000000000000000000000000000009");
        let mut prices = HashMap::new();
        prices.insert(tok, token_usd(0.5, 6));
        // A zero quote is not a price (A14): it must count as unpriced.
        prices.insert(unpriced_tok, token_usd(0.0, 6));

        let with_fee = |fee_token: Address, fee: u64, netted: bool| {
            let mut ev = sample_event(500);
            ev.profit_token = Some(tok);
            ev.profit_amount = Some(U256::from(1_000_000u64));
            ev.flashloan_fee_token = Some(fee_token);
            ev.flashloan_fee_wei = Some(U256::from(fee));
            if netted {
                ev.details["flashloan_fee_netted_in_ledger"] = serde_json::json!(true);
            }
            ev
        };
        let profit = 0.5;
        let gas = 100_000_000_000_000f64 / 1e18 * 0.75;
        let fee = 1_000f64 / 1e6 * 0.5;

        // Priced, un-netted: converted, subtracted once, no flag.
        let op = annotate_event(&with_fee(tok, 1_000, false), &[], &prices, Some(0.75), 0);
        assert!((op.flashloan_fee_usd.expect("priced fee") - fee).abs() < 1e-12);
        assert!((op.net_profit_usd.expect("net priced") - (profit - gas - fee)).abs() < 1e-9);
        assert!(!op
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("FLASH_FEE_UNPRICED"));

        // Unpriced token: NULL fee + flag, and no invented zero in net.
        let op = annotate_event(
            &with_fee(unpriced_tok, 1_000, false),
            &[],
            &prices,
            Some(0.75),
            0,
        );
        assert!(op.flashloan_fee_usd.is_none());
        assert!((op.net_profit_usd.expect("net priced") - (profit - gas)).abs() < 1e-9);
        let details = op.details_json.as_deref().unwrap_or("");
        assert!(details.contains("FLASH_FEE_UNPRICED"));
        assert!(details.contains("\"usd_approximate\":true"));

        // The ledger already netted the unpriced fee: no gap, no flag.
        let op = annotate_event(
            &with_fee(unpriced_tok, 1_000, true),
            &[],
            &prices,
            Some(0.75),
            0,
        );
        assert!(op.flashloan_fee_usd.is_none());
        assert!((op.net_profit_usd.expect("net priced") - (profit - gas)).abs() < 1e-9);
        assert!(!op
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("FLASH_FEE_UNPRICED"));

        // Zero fee: nothing to convert, no flag.
        let op = annotate_event(&with_fee(tok, 0, false), &[], &prices, Some(0.75), 0);
        assert!(op.flashloan_fee_usd.is_none());
        assert!((op.net_profit_usd.expect("net priced") - (profit - gas)).abs() < 1e-9);
        assert!(!op
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("FLASH_FEE_UNPRICED"));
    }

    /// A19: the classifier-normalized confidence and the `details.pnl_basis`
    /// marker are persisted unchanged for non-liquidations; a liquidation's
    /// confidence is re-derived from the pricing evidence computed at persist
    /// (cross-asset collateral/debt is an approximation) while the basis stays
    /// on the row.
    #[test]
    fn a19_basis_and_confidence_are_persisted() {
        let tok = address!("4444000000000000000000000000000000000004");
        let mut prices = HashMap::new();
        prices.insert(tok, token_usd(0.5, 6));

        let mut gross = sample_event(600);
        gross.confidence = Confidence::Inferred;
        gross.details["pnl_basis"] = serde_json::json!("gross_amount_diff");

        let mut ledger = sample_event(601);
        ledger.details["pnl_basis"] = serde_json::json!("R");

        let gross_op = annotate_event(&gross, &[], &prices, Some(0.75), 0);
        assert_eq!(gross_op.confidence, "inferred");
        assert!(gross_op
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"pnl_basis\":\"gross_amount_diff\""));

        let ledger_op = annotate_event(&ledger, &[], &prices, Some(0.75), 0);
        assert_eq!(ledger_op.confidence, "exact");
        assert!(ledger_op
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"pnl_basis\":\"R\""));

        let collateral = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let debt = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let mut liq_prices = HashMap::new();
        liq_prices.insert(collateral, token_usd(2.0, 0));
        liq_prices.insert(debt, token_usd(1.0, 0));
        let mut liq = sample_event(602);
        liq.kind = MevKind::Liquidation;
        liq.confidence = Confidence::Exact;
        liq.profit_token = Some(collateral);
        liq.profit_amount = Some(U256::from(500u64));
        liq.details = serde_json::json!({
            "collateral_asset": format!("{collateral:#x}"),
            "debt_asset": format!("{debt:#x}"),
            "collateral_amount": "500",
            "debt_to_cover": "300",
            "reconciled": true,
            "reasons": [],
            "pnl_basis": "O",
        });

        let liq_op = annotate_event(&liq, &[], &liq_prices, Some(0.75), 0);
        assert_eq!(liq_op.confidence, "inferred");
        assert!(liq_op
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"pnl_basis\":\"O\""));
    }
}
