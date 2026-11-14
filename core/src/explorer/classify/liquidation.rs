//! Liquidation + Compound V3 BuyCollateral classification passes.
use std::collections::HashMap;

use alloy::primitives::Address;

use crate::explorer::interest_attr;
use crate::explorer::profit::DeltaLedger;
use crate::explorer::reject::RejectReason;
use crate::explorer::types::{
    Confidence, LiquidationFact, MevEvent, MevKind, OracleUpdateFact, PnlBasis, ReserveDataFact,
};

use super::drops::DropSink;
use super::BlockInput;

/// Block-scoped oracle/reserve facts already gathered by `classify_block`.
pub(super) struct LiqBlockCtx<'a> {
    pub block_oracle_owned: &'a [OracleUpdateFact],
    pub block_reserve_owned: &'a [ReserveDataFact],
}

/// Passes 1 + 1a: exact liquidations and BuyCollateral discount capture.
pub(super) fn classify_liquidations(
    input: &BlockInput,
    ctx: &LiqBlockCtx<'_>,
    drops: &mut DropSink,
) -> Vec<MevEvent> {
    let mut events: Vec<MevEvent> = Vec::new();
    let block_oracle_owned = ctx.block_oracle_owned;
    let block_reserve_owned = ctx.block_reserve_owned;
    // ── 1. Liquidation pass (exact, zero-heuristic) ─────────────────────
    // Mode A: flash-loan atomic liq when same tx has FlashLoanFact
    // + LiquidationFact → tag `flash_loan_liq` (plan P0.1 / P2.3 routing).
    for tx in &input.txs {
        if tx.liquidations.is_empty() {
            continue; // not-a-drop: tx has no liquidation event
        }
        let ledger = DeltaLedger::from_transfers(
            &tx.transfers,
            input.ctx.wrapped_native,
            (tx.from, tx.value),
        );
        let flash_funded = !tx.flashloans.is_empty();
        let primary_flash = tx.flashloans.first();
        for liq in &tx.liquidations {
            let searcher = if liq.liquidator.is_zero() {
                tx.from
            } else {
                liq.liquidator
            };
            // Transfer reconciliation: the liquidator's net positive
            // delta of the seized collateral must cover the event amount.
            // Compound V3 `Absorb` carries no per-asset seizure (collateral=0),
            // so it stays Exact with a TRANSFER_MISMATCH reason.
            let reconciled = !liq.collateral_asset.is_zero()
                && ledger.net(searcher, liq.collateral_asset) >= liq.collateral_amount;
            // The event still stands. The reject records that its seized
            // amount is not covered by transfers, which is why P&L is demoted.
            if !liq.collateral_asset.is_zero() && !reconciled {
                drops.anchor(
                    tx.tx_index,
                    RejectReason::LiquidationNotReconciled,
                    "transfer_net_below_seized_amount",
                );
            }
            let mut reasons: Vec<&str> = Vec::new();
            if !reconciled {
                reasons.push("TRANSFER_MISMATCH");
            }
            let mut tags: Vec<&str> = Vec::new();
            if flash_funded {
                tags.push("flash_loan_liq");
            }
            // Raw Morpho `badDebtAssets` stays as event data for a future live
            // risk model; it is no longer a strategy tag.
            let (flashloan_fee_wei, flashloan_fee_token) = if flash_funded {
                primary_flash
                    .map(|fl| (fl.fee.filter(|f| !f.is_zero()), Some(fl.token)))
                    .unwrap_or((None, None))
            } else {
                (None, None)
            };
            // P1.4: Chainlink poke in this or a prior tx, before the
            // liquidation, for a feed mapped to debt/collateral.
            let oracle_poke_block = oracle_poke_for_liq(
                liq,
                tx.tx_index,
                block_oracle_owned,
                &input.ctx.chainlink_feeds,
            );
            // P1.4 mode B: pre-poke divergence vs last stored answer (secondary).
            let oracle_pre_poke_divergence_bps = if oracle_poke_block {
                block_oracle_owned.iter().find_map(|o| {
                    if !oracle_precedes(o, tx.tx_index, liq.log_index) {
                        return None;
                    }
                    let asset = input.ctx.chainlink_feeds.get(&o.feed).copied();
                    let relevant = asset == Some(liq.debt_asset)
                        || asset == Some(liq.collateral_asset)
                        || input.ctx.chainlink_feeds.is_empty();
                    if !relevant {
                        return None;
                    }
                    let prior = input.prior_oracle_answers.get(&o.feed).copied()?;
                    interest_attr::oracle_pre_poke_divergence_bps(prior, o.answer)
                })
            } else {
                None
            };
            // P1.1: interest-accrual cause label (inferred; P&L stays O).
            let interest_evidence = interest_attr::interest_evidence(
                liq,
                block_reserve_owned,
                block_oracle_owned,
                &input.ctx.chainlink_feeds,
                Some(&input.interest_lookback),
            );
            let interest_accrued = interest_evidence.is_some();
            // P&L basis O: seized − repaid components (USD at persist); flash
            // premium recorded as F component when present.
            let mut details = serde_json::json!({
                "protocol": liq.protocol,
                "user": format!("{:#x}", liq.user),
                "collateral_asset": format!("{:#x}", liq.collateral_asset),
                "debt_asset": format!("{:#x}", liq.debt_asset),
                "collateral_amount": liq.collateral_amount.to_string(),
                "debt_to_cover": liq.debt_to_cover.to_string(),
                "reconciled": reconciled,
                "reasons": reasons,
                "tags": tags,
                "pnl_basis": PnlBasis::O.as_str(),
                "oracle_poke_block": oracle_poke_block,
                "interest_accrued": interest_accrued,
                "bad_debt_assets": liq.bad_debt_assets.to_string(),
                "pnl": {
                    "basis": PnlBasis::O.as_str(),
                    "collateral_amount": liq.collateral_amount.to_string(),
                    "debt_to_cover": liq.debt_to_cover.to_string(),
                },
            });
            if let Some(bps) = oracle_pre_poke_divergence_bps {
                details["oracle_pre_poke_divergence_bps"] = serde_json::json!(bps);
            }
            if let Some(evidence) = &interest_evidence {
                details["interest_evidence"] = serde_json::json!(evidence);
            }
            if let Some(fl) = primary_flash {
                details["flash_provider"] = serde_json::json!(fl.protocol);
                details["flash_provider_address"] =
                    serde_json::json!(format!("{:#x}", fl.provider));
                if let Some(fee) = fl.fee {
                    details["flash_premium"] = serde_json::json!(fee.to_string());
                    details["flash_premium_token"] = serde_json::json!(format!("{:#x}", fl.token));
                    details["pnl"]["flash_premium"] = serde_json::json!(fee.to_string());
                    details["pnl"]["flash_premium_basis"] = serde_json::json!(PnlBasis::F.as_str());
                }
                // P2.3: full flash routing list when multiple providers appear.
                if tx.flashloans.len() > 1 {
                    details["flash_providers"] = serde_json::json!(tx
                        .flashloans
                        .iter()
                        .map(|f| {
                            serde_json::json!({
                                "protocol": f.protocol,
                                "provider": format!("{:#x}", f.provider),
                                "premium": f.fee.map(|x| x.to_string()),
                                "token": format!("{:#x}", f.token),
                            })
                        })
                        .collect::<Vec<_>>());
                }
            }
            events.push(MevEvent {
                pools: vec![],
                // Compound V3 `Absorb` carries no per-asset seizure
                // (collateral=0): leave profit_token NULL so downstream
                // pricing does not special-case a zero address.
                profit_token: (!liq.collateral_asset.is_zero()).then_some(liq.collateral_asset),
                profit_amount: Some(liq.collateral_amount),
                gas_cost_wei: super::gas::tx_gas_cost_wei(tx),
                flashloan_fee_wei,
                flashloan_fee_token,
                details,
                // `LiquidationCall` carries no liquidator address; the tx
                // sender is the attribution target (zero fallback covered).
                ..MevEvent::for_tx(
                    input.block,
                    input.ts,
                    tx.tx_index,
                    tx.tx_hash,
                    MevKind::Liquidation,
                    searcher,
                    tx.to,
                )
            });
        }
    }

    // ── 1a. BuyCollateral discount capture (Compound V3, / P0.3) ────
    // Mode A: BuyCollateral in a block that also has (or just had) an Absorb
    // on the *same Comet market*. A different market's Absorb does not pair.
    {
        let absorb_markets: Vec<(u64, Address)> = input
            .txs
            .iter()
            .flat_map(|t| {
                t.liquidations
                    .iter()
                    .filter(|l| l.protocol == "compound_v3")
                    .map(move |l| (t.tx_index, l.emitter))
            })
            .collect();
        for tx in &input.txs {
            for buy in &tx.buy_collaterals {
                let paired = absorb_markets
                    .iter()
                    .any(|(idx, market)| *idx <= tx.tx_index && *market == buy.emitter);
                let buyer = if buy.buyer.is_zero() {
                    tx.from
                } else {
                    buy.buyer
                };
                // Compound V3 `BuyCollateral` does not carry the base token.
                // The buyer's payment transfer to the Comet does; infer it so
                // persist can value the discount instead of leaving PnL NULL.
                let base_token = tx
                    .transfers
                    .iter()
                    .find(|t| {
                        !t.token.is_zero()
                            && t.token != buy.collateral_asset
                            && t.to == buy.emitter
                            && (t.from == buyer || t.from == tx.from)
                            && t.amount >= buy.base_amount
                    })
                    .map(|t| t.token);
                let mut tags = vec!["buycollateral"];
                if !tx.flashloans.is_empty() {
                    tags.push("flash_loan_liq");
                }
                let mut reasons: Vec<&str> = Vec::new();
                if !paired {
                    reasons.push("ABSORB_UNPAIRED");
                }
                if base_token.is_none() && !buy.base_amount.is_zero() {
                    reasons.push("BASE_PAYMENT_UNRESOLVED");
                }
                let mut details = serde_json::json!({
                    "protocol": buy.protocol,
                    "buyer": format!("{:#x}", buy.buyer),
                    "collateral_asset": format!("{:#x}", buy.collateral_asset),
                    "debt_asset": format!("{:#x}", base_token.unwrap_or(Address::ZERO)),
                    "collateral_amount": buy.collateral_amount.to_string(),
                    "debt_to_cover": buy.base_amount.to_string(),
                    "base_amount": buy.base_amount.to_string(),
                    "absorb_paired": paired,
                    "tags": tags,
                    "reasons": reasons,
                    "pnl_basis": PnlBasis::O.as_str(),
                    "pnl": {
                        "basis": PnlBasis::O.as_str(),
                        "collateral_amount": buy.collateral_amount.to_string(),
                        "base_paid": buy.base_amount.to_string(),
                        "coins_basis": PnlBasis::R.as_str(),
                    },
                });
                if let Some(token) = base_token {
                    details["pnl"]["base_token"] = serde_json::json!(format!("{token:#x}"));
                }
                events.push(MevEvent {
                    pools: vec![],
                    profit_token: (!buy.collateral_asset.is_zero()).then_some(buy.collateral_asset),
                    profit_amount: Some(buy.collateral_amount),
                    gas_cost_wei: super::gas::tx_gas_cost_wei(tx),
                    confidence: if paired {
                        Confidence::Exact
                    } else {
                        Confidence::Inferred
                    },
                    details,
                    ..MevEvent::for_tx(
                        input.block,
                        input.ts,
                        tx.tx_index,
                        tx.tx_hash,
                        MevKind::Liquidation,
                        buyer,
                        tx.to,
                    )
                });
            }
        }
    }
    events
}

/// True when an oracle update lands before the liquidation in tx/log order.
fn oracle_precedes(update: &OracleUpdateFact, liq_tx_index: u64, liq_log_index: u64) -> bool {
    update.tx_index < liq_tx_index
        || (update.tx_index == liq_tx_index && update.log_index <= liq_log_index)
}

/// Chainlink poke before the liquidation for feeds mapped to its assets
/// (plan P1.4). A later same-block poke cannot explain the liquidation.
fn oracle_poke_for_liq(
    liq: &LiquidationFact,
    liq_tx_index: u64,
    updates: &[OracleUpdateFact],
    feed_to_asset: &HashMap<Address, Address>,
) -> bool {
    if feed_to_asset.is_empty() {
        return updates
            .iter()
            .any(|o| oracle_precedes(o, liq_tx_index, liq.log_index));
    }
    updates.iter().any(|o| {
        oracle_precedes(o, liq_tx_index, liq.log_index)
            && feed_to_asset
                .get(&o.feed)
                .is_some_and(|a| *a == liq.debt_asset || *a == liq.collateral_asset)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::classify::classify_block;
    use crate::explorer::test_fixtures::{block, liquidation, tx, ATK, USDC};

    /// Compound V3 `Absorb` carries no per-asset seizure (collateral = 0).
    /// The event is still persisted (with TRANSFER_MISMATCH), but its
    /// `profit_token` must be NULL — never `Some(0x0)`, which forces every
    /// downstream pricing path to special-case the zero address.
    #[test]
    fn zero_collateral_absorb_has_no_profit_token() {
        let mut t = tx(0, ATK, true, vec![], vec![]);
        t.liquidations
            .push(liquidation("compound_v3", ATK, Address::ZERO, USDC, 0, 100));
        let out = classify_block(&block(vec![t]));
        let liq = out
            .events
            .iter()
            .find(|e| e.kind == MevKind::Liquidation)
            .expect("zero-collateral absorb must still be persisted");
        assert_eq!(
            liq.profit_token, None,
            "zero collateral asset must yield profit_token NULL, not Some(0x0)"
        );
    }
}
