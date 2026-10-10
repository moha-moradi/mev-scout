//! Liquidation + Compound V3 BuyCollateral classification passes.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::interest_attr;
use crate::explorer::profit::DeltaLedger;
use crate::explorer::types::{
    Confidence, MevEvent, MevKind, OracleUpdateFact, PnlBasis, ReserveDataFact,
};

use super::BlockInput;

/// Block-scoped oracle/reserve facts already gathered by `classify_block`.
pub(super) struct LiqBlockCtx<'a> {
    pub oracle_feeds_in_block: &'a HashSet<Address>,
    pub block_oracle_owned: &'a [OracleUpdateFact],
    pub block_reserve_owned: &'a [ReserveDataFact],
}

/// Passes 1 + 1a: exact liquidations and BuyCollateral discount capture.
pub(super) fn classify_liquidations(input: &BlockInput, ctx: &LiqBlockCtx<'_>) -> Vec<MevEvent> {
    let mut events: Vec<MevEvent> = Vec::new();
    let oracle_feeds_in_block = ctx.oracle_feeds_in_block;
    let block_oracle_owned = ctx.block_oracle_owned;
    let block_reserve_owned = ctx.block_reserve_owned;
    // ── 1. Liquidation pass (exact, zero-heuristic) ─────────────────────
    // Mode A (§17.8.4): flash-loan atomic liq when same tx has FlashLoanFact
    // + LiquidationFact → tag `flash_loan_liq` (plan P0.1 / P2.3 routing).
    for tx in &input.txs {
        if tx.liquidations.is_empty() {
            continue;
        }
        let ledger =
            DeltaLedger::from_transfers(&tx.transfers, input.wrapped_native, (tx.from, tx.value));
        let flash_funded = !tx.flashloans.is_empty();
        let primary_flash = tx.flashloans.first();
        for liq in &tx.liquidations {
            let searcher = if liq.liquidator.is_zero() {
                tx.from
            } else {
                liq.liquidator
            };
            // Transfer reconciliation (Phase 1.3): the liquidator's net positive
            // delta of the seized collateral must cover the event amount.
            // Compound V3 `Absorb` carries no per-asset seizure (collateral=0),
            // so it stays Exact with a TRANSFER_MISMATCH reason.
            let reconciled = !liq.collateral_asset.is_zero()
                && ledger.net(searcher, liq.collateral_asset) >= liq.collateral_amount;
            let mut reasons: Vec<&str> = Vec::new();
            if !reconciled {
                reasons.push("TRANSFER_MISMATCH");
            }
            let mut tags: Vec<&str> = Vec::new();
            if flash_funded {
                tags.push("flash_loan_liq");
            }
            // P3.14 — bad-debt / near-insolvent attribution (Morpho badDebtAssets).
            // Mode-B proxy: nonzero badDebtAssets ⇒ post-liq position is insolvent
            // (HF collapsed); declared correlational in fixtures.
            let bad_debt = !liq.bad_debt_assets.is_zero();
            if bad_debt {
                tags.push("bad_debt_liq");
            }
            let post_liq_insolvent = bad_debt;
            let (flashloan_fee_wei, flashloan_fee_token) = if flash_funded {
                primary_flash
                    .map(|fl| (fl.fee.filter(|f| !f.is_zero()), Some(fl.token)))
                    .unwrap_or((None, None))
            } else {
                (None, None)
            };
            // P1.4: co-block Chainlink poke for a feed mapped to debt/collateral.
            let oracle_poke_block = oracle_poke_for_liq(
                liq.debt_asset,
                liq.collateral_asset,
                oracle_feeds_in_block,
                &input.chainlink_feeds,
            );
            // P1.4 mode B: pre-poke divergence vs last stored answer (secondary).
            let oracle_pre_poke_divergence_bps = if oracle_poke_block {
                block_oracle_owned.iter().find_map(|o| {
                    let asset = input.chainlink_feeds.get(&o.feed).copied();
                    let relevant = asset == Some(liq.debt_asset)
                        || asset == Some(liq.collateral_asset)
                        || input.chainlink_feeds.is_empty();
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
            let interest_accrued = interest_attr::interest_accrued(
                liq,
                block_reserve_owned,
                block_oracle_owned,
                &input.chainlink_feeds,
                Some(&input.interest_lookback),
            );
            // P&L basis O: seized − repaid components (USD at persist); flash
            // premium recorded as F component when present (§0.1).
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
                "bad_debt": bad_debt,
                "bad_debt_assets": liq.bad_debt_assets.to_string(),
                "post_liq_insolvent": post_liq_insolvent,
                "pnl": {
                    "basis": PnlBasis::O.as_str(),
                    "collateral_amount": liq.collateral_amount.to_string(),
                    "debt_to_cover": liq.debt_to_cover.to_string(),
                },
            });
            if let Some(bps) = oracle_pre_poke_divergence_bps {
                details["oracle_pre_poke_divergence_bps"] = serde_json::json!(bps);
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
                block: input.block,
                ts: input.ts,
                tx_index: tx.tx_index,
                tx_hash: tx.tx_hash,
                kind: MevKind::Liquidation,
                // `LiquidationCall` carries no liquidator address; the tx
                // sender is the attribution target (zero fallback covered).
                searcher,
                contract: tx.to,
                pools: vec![],
                profit_token: Some(liq.collateral_asset),
                profit_amount: Some(liq.collateral_amount),
                profit_tokens: vec![],
                profit_usd: None,
                gas_cost_wei: U256::from(tx.gas_used)
                    .saturating_mul(U256::from((tx.effective_gas_price_gwei * 1e9) as u128)),
                flashloan_fee_wei,
                flashloan_fee_token,
                confidence: Confidence::Exact,
                victim_hashes: vec![],
                victim_swap_size: None,
                details,
            });
        }
    }

    // ── 1a. BuyCollateral discount capture (Compound V3, §26 / P0.3) ────
    // Mode A: BuyCollateral in a block that also has (or just had) Absorb.
    // Same-tx or prior-tx Absorb in this block pairs the discount capture.
    {
        let absorb_txs: HashSet<u64> = input
            .txs
            .iter()
            .filter(|t| t.liquidations.iter().any(|l| l.protocol == "compound_v3"))
            .map(|t| t.tx_index)
            .collect();
        for tx in &input.txs {
            for buy in &tx.buy_collaterals {
                let paired = absorb_txs.iter().any(|&idx| idx <= tx.tx_index);
                let mut tags = vec!["buycollateral"];
                if !tx.flashloans.is_empty() {
                    tags.push("flash_loan_liq");
                }
                let mut details = serde_json::json!({
                    "protocol": buy.protocol,
                    "buyer": format!("{:#x}", buy.buyer),
                    "collateral_asset": format!("{:#x}", buy.collateral_asset),
                    "debt_asset": format!("{:#x}", Address::ZERO),
                    "collateral_amount": buy.collateral_amount.to_string(),
                    "debt_to_cover": buy.base_amount.to_string(),
                    "base_amount": buy.base_amount.to_string(),
                    "absorb_paired": paired,
                    "tags": tags,
                    "pnl_basis": PnlBasis::O.as_str(),
                    "pnl": {
                        "basis": PnlBasis::O.as_str(),
                        "collateral_amount": buy.collateral_amount.to_string(),
                        "base_paid": buy.base_amount.to_string(),
                        "coins_basis": PnlBasis::R.as_str(),
                    },
                });
                if !paired {
                    details["reasons"] = serde_json::json!(["ABSORB_UNPAIRED"]);
                }
                events.push(MevEvent {
                    block: input.block,
                    ts: input.ts,
                    tx_index: tx.tx_index,
                    tx_hash: tx.tx_hash,
                    kind: MevKind::Liquidation,
                    searcher: if buy.buyer.is_zero() {
                        tx.from
                    } else {
                        buy.buyer
                    },
                    contract: tx.to,
                    pools: vec![],
                    profit_token: Some(buy.collateral_asset),
                    profit_amount: Some(buy.collateral_amount),
                    profit_tokens: vec![],
                    profit_usd: None,
                    gas_cost_wei: U256::from(tx.gas_used)
                        .saturating_mul(U256::from((tx.effective_gas_price_gwei * 1e9) as u128)),
                    flashloan_fee_wei: None,
                    flashloan_fee_token: None,
                    confidence: if paired {
                        Confidence::Exact
                    } else {
                        Confidence::Inferred
                    },
                    victim_hashes: vec![],
                    victim_swap_size: None,
                    details,
                });
            }
        }
    }
    events
}

/// Co-block Chainlink poke for feeds mapped to the liquidation's assets
/// (plan P1.4). When the feed map is empty, any in-block AnswerUpdated counts
/// as a poke (declared correlational in fixtures).
fn oracle_poke_for_liq(
    debt: Address,
    collateral: Address,
    feeds_in_block: &HashSet<Address>,
    feed_to_asset: &HashMap<Address, Address>,
) -> bool {
    if feeds_in_block.is_empty() {
        return false;
    }
    if feed_to_asset.is_empty() {
        return true;
    }
    feed_to_asset.iter().any(|(feed, asset)| {
        feeds_in_block.contains(feed) && (*asset == debt || *asset == collateral)
    })
}
