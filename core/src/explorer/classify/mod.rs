//! Per-block realized-MEV classifier.
//!
//! Input: one settled block (header + ordered txs with receipts, tx senders)
//! decoded into swap/transfer/liquidation/JIT facts. Output: `Vec<MevEvent>`
//! plus sandwich bundles folded into events (victim hashes + leg details).
//!
//! Pass order (first match wins, disjoint):
//! 1. Liquidation pass — exact event match on configured lending pools.
//! 2. Skim pass — V2-like pair outbound Transfers with no Swap/Mint/Burn/Sync.
//! 3. Swap attribution — per-tx per-address net token deltas.
//! 4. Atomic arb pass — ≥2 swaps in one tx + closed cycle + positive net
//!    delta of a single profit token.
//! 5. Sandwich pass — same sender, same pool, opposite directions, with a
//!    third-party swap between front-run and back-run (tx-index ordered).
//! 6. JIT pass — V3 Mint+Burn same position same block; jit_arb when the
//!    same tx also closed an arb cycle. Leftover profitable patterns →
//!    `unknown` (inferred).

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, B256, U256};

use crate::explorer::decode;
use crate::explorer::interest_attr;
use crate::explorer::profit::{
    has_closed_cycle, net_flash_loan, select_profit_token, DeltaLedger, ProfitTokenPolicy,
};
use crate::explorer::store::OpenPosition;
use crate::explorer::strategy_tags::{self, ArbTagOpts};
use crate::explorer::types::{
    BuyCollateralFact, Confidence, EpochRewardFact, GmxEventFact, JitFact, KeeperFact, MevEvent,
    MevKind, OracleUpdateFact, PnlBasis, RateCacheFact, ReserveDataFact, SwapFact, TransferFact,
    UserOpFact, V2PairOpFact,
};

mod skim;
mod legs;
mod sandwich;
mod causal;
mod jit;

use causal::{classify_backruns, classify_frontruns};
use jit::classify_jit;
use sandwich::classify_sandwiches;
use skim::classify_skims;

pub(crate) use jit::stamp_jit_tx_hashes;

/// Raw per-tx input the classifier consumes (decoded by `ingest`).
#[derive(Debug, Clone, Default)]
pub struct TxInput {
    pub tx_index: u64,
    pub tx_hash: B256,
    pub from: Address,
    pub to: Option<Address>,
    pub success: bool,
    pub gas_used: u64,
    pub effective_gas_price_gwei: f64,
    pub value: U256,
    pub transfers: Vec<TransferFact>,
    pub swaps: Vec<SwapFact>,
    pub liquidations: Vec<crate::explorer::types::LiquidationFact>,
    pub flashloans: Vec<crate::explorer::types::FlashLoanFact>,
    /// Compound V3 `BuyCollateral` facts (discount capture after Absorb).
    pub buy_collaterals: Vec<BuyCollateralFact>,
    pub jit: Vec<JitFact>,
    /// UniV2 Sync/Mint/Burn logs — exclusion signal for skim.
    pub v2_pair_ops: Vec<V2PairOpFact>,
    /// Chainlink `AnswerUpdated` facts (plan P1.4).
    pub oracle_updates: Vec<OracleUpdateFact>,
    /// Aave `ReserveDataUpdated` facts (plan P1.1).
    pub reserve_updates: Vec<ReserveDataFact>,
    /// Balancer `TokenRateCacheUpdated` facts (plan P3.2).
    pub rate_cache_updates: Vec<RateCacheFact>,
    /// Gelato / Chainlink Automation executions (plan P1.5).
    pub keepers: Vec<KeeperFact>,
    /// ve(3,3) NotifyReward facts (plan P3.15).
    pub epoch_rewards: Vec<EpochRewardFact>,
    /// GMX V2 EventEmitter ADL/liq facts (plan P3.7).
    pub gmx_events: Vec<GmxEventFact>,
    /// ERC-4337 UserOperationEvent facts (plan P3.11).
    pub user_ops: Vec<UserOpFact>,
}

/// One block's classified input.
pub struct BlockInput {
    pub block: u64,
    pub ts: u64,
    pub wrapped_native: Address,
    pub profit_policy: ProfitTokenPolicy,
    /// Mevlive-parity fallback (Phase 1.2): when true, a profitable residual
    /// that is not a closed cycle is still labeled `arb_atomic`/`inferred`.
    /// Default true until the Phase-0 window gate passes.
    pub arb_likely_parity: bool,
    /// Cross-block JIT open Mint positions (Phase 1.5), loaded from the store
    /// window for the current block.
    pub open_positions: Vec<OpenPosition>,
    /// Registry addresses that expose UniV2-style `skim()` (V2/Solidly/Camelot).
    pub v2_like_pools: HashSet<Address>,
    /// Chainlink aggregator → underlying asset (plan P1.1 / P1.4).
    pub chainlink_feeds: HashMap<Address, Address>,
    /// Benqi sAVAX (plan P3.16); `None` disables the tag.
    pub savax: Option<Address>,
    /// Pharaoh / Blackhole pools eligible for epoch-transition tagging (P3.15).
    pub epoch_venue_pools: HashSet<Address>,
    /// Optional GMX EventEmitter allowlist; empty = accept any matching name hash.
    pub gmx_event_emitters: HashSet<Address>,
    /// Mode-B interest lookback (plan P1.1); empty = same-block heuristic only.
    pub interest_lookback: interest_attr::InterestLookback,
    /// Prior Chainlink answers keyed by feed (plan P1.4 mode B).
    pub prior_oracle_answers: HashMap<Address, i128>,
    /// Benqi sAVAX `getPooledAvaxByShares(1e18)` at this block (plan P3.16).
    pub savax_exchange_rate_wad: Option<U256>,
    pub txs: Vec<TxInput>,
}

/// Classify one block. Returns (events, bundle_details) where bundle details
/// for sandwiches are already folded into `details_json` per event.
pub fn classify_block(input: &BlockInput) -> Vec<MevEvent> {
    let mut events: Vec<MevEvent> = Vec::new();

    // Block-wide oracle / reserve facts (P1.1 / P1.4 co-block checks).
    let block_oracle_updates: Vec<&OracleUpdateFact> = input
        .txs
        .iter()
        .flat_map(|t| t.oracle_updates.iter())
        .collect();
    let block_reserve_updates: Vec<&ReserveDataFact> = input
        .txs
        .iter()
        .flat_map(|t| t.reserve_updates.iter())
        .collect();
    let block_oracle_owned: Vec<OracleUpdateFact> =
        block_oracle_updates.iter().map(|o| (*o).clone()).collect();
    let block_reserve_owned: Vec<ReserveDataFact> =
        block_reserve_updates.iter().map(|r| (*r).clone()).collect();
    let oracle_feeds_in_block: HashSet<Address> =
        block_oracle_owned.iter().map(|o| o.feed).collect();
    let epoch_notify = input.txs.iter().any(|t| !t.epoch_rewards.is_empty());
    let epoch_signal = epoch_notify || strategy_tags::near_epoch_boundary(input.ts);
    let gmx_adl_signal = input.txs.iter().any(|t| {
        t.gmx_events.iter().any(|g| {
            (input.gmx_event_emitters.is_empty() || input.gmx_event_emitters.contains(&g.emitter))
                && (g.kind == "adl" || g.kind == "impact" || g.kind == "liquidation")
        })
    });
    let epoch_venue_list: Vec<Address> = input.epoch_venue_pools.iter().copied().collect();
    let block_rate_cache: Vec<RateCacheFact> = input
        .txs
        .iter()
        .flat_map(|t| t.rate_cache_updates.iter().cloned())
        .collect();
    let arb_tag_opts = ArbTagOpts {
        wrapped_native: input.wrapped_native,
        savax: input.savax,
        savax_exchange_rate_wad: input.savax_exchange_rate_wad,
        epoch_signal,
        epoch_venue_pools: &epoch_venue_list,
        gmx_adl_signal,
        rate_cache_updates: &block_rate_cache,
    };

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
                &oracle_feeds_in_block,
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
                &block_reserve_owned,
                &block_oracle_owned,
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
    let liq_txs: std::collections::HashSet<u64> = events
        .iter()
        .filter(|e| e.kind == MevKind::Liquidation)
        .map(|e| e.tx_index)
        .collect();

    // ── 1b. Skim pass (exact, UniV2 pair outbound without Swap/Mint/Burn/Sync)
    events.extend(classify_skims(input));

    // ── 2-3. Swap attribution + atomic arb pass ─────────────────────────
    for tx in &input.txs {
        if !tx.success {
            continue;
        }
        if liq_txs.contains(&tx.tx_index) {
            // Liquidation anchor tx — still scan for arb in the same tx
            // (liquidator often swaps seized collateral), but do not double-
            // classify the tx as pure arb unless deltas confirm it.
        }
        if tx.swaps.is_empty() {
            continue;
        }

        let mut ledger =
            DeltaLedger::from_transfers(&tx.transfers, input.wrapped_native, (tx.from, tx.value));

        // Phase 2.2: net flash-loan borrow/repay before computing residuals so
        // borrowed principal never counts as profit. Only net when the repay
        // leg is *absent* from the Transfer stream (provider callback internal
        // repayments); when the repay Transfer to the provider is present the
        // ledger already nets it, so subtracting again would erase real profit.
        let mut flashloan_fee_wei: Option<U256> = None;
        let mut flashloan_fee_token: Option<Address> = None;
        for fl in &tx.flashloans {
            if fl.token.is_zero() || fl.amount.is_zero() {
                continue;
            }
            let fee = fl.fee.unwrap_or(U256::ZERO);
            let borrower = if fl.initiator.is_zero() {
                tx.from
            } else {
                fl.initiator
            };
            let repaid = tx.transfers.iter().any(|t| {
                t.token == fl.token
                    && t.amount >= fl.amount
                    && (t.from == borrower || t.from == tx.from)
                    && (t.to == fl.provider || t.to == fl.recipient)
            });
            if !repaid {
                net_flash_loan(&mut ledger, borrower, fl.token, fl.amount, fee);
                if let Some(to) = tx.to {
                    if to != borrower {
                        net_flash_loan(&mut ledger, to, fl.token, fl.amount, fee);
                    }
                }
            }
            if !fee.is_zero() && flashloan_fee_wei.is_none_or(|f| fee > f) {
                flashloan_fee_wei = Some(fee);
                flashloan_fee_token = Some(fl.token);
            }
        }

        // Participant scope: sender + receiver-side contracts (searcher
        // attribution: "EOA sender (or its deployed contract)").
        let mut candidates: Vec<Address> = vec![tx.from];
        if let Some(to) = tx.to {
            candidates.push(to);
        }
        // Addresses with ≥2 swap participations via transfers also qualify.
        let mut participation: std::collections::HashMap<Address, usize> =
            std::collections::HashMap::new();
        for t in &tx.transfers {
            *participation.entry(t.from).or_insert(0) += 1;
            *participation.entry(t.to).or_insert(0) += 1;
        }
        let qualified: Vec<Address> = participation
            .into_iter()
            .filter(|(addr, count)| *count >= 2 && !candidates.contains(addr))
            .map(|(addr, _)| addr)
            .collect();
        candidates.extend(qualified);

        let candidate = candidates
            .iter()
            .copied()
            .find(|a| select_profit_token(&ledger, *a, &input.profit_policy).is_some());

        // ── Atomic arb ─────────────────────────────────────────────────
        // Resolved directional edges (token_in, token_out, pool).
        let resolved_edges: Vec<(Address, Address, Address)> = tx
            .swaps
            .iter()
            .filter(|s| {
                !has_unresolved_tokens(s)
                    && s.token_in != decode::TOKEN0_SENTINEL
                    && s.token_in != decode::TOKEN1_SENTINEL
            })
            .map(|s| (s.token_in, s.token_out, s.pool))
            .collect();
        // Exact only on a closed directed cycle spanning ≥2 pools (§8.1)
        // attributable to a single flow owner among the searcher's route (§7.1).
        let arb_confirmed = resolved_edges.len() >= 2
            && has_closed_cycle(&resolved_edges)
            && flow_attributable(&tx.swaps, &candidates);
        // Mevlive-parity fallback: single-hop / unresolved profitable residuals
        // are labeled arb (mevlive Type=Arbitrage) while the parity flag holds.
        let arb_likely = arb_confirmed || (!tx.swaps.is_empty() && input.arb_likely_parity);

        if let Some(searcher) = candidate {
            if let Some(token) = select_profit_token(&ledger, searcher, &input.profit_policy) {
                let amount = ledger.net(searcher, token);
                let is_dust_or_wrap = amount.is_zero();
                if !is_dust_or_wrap {
                    // Spec §7.1 / §8.1: only emit when we have a confirmed cycle
                    // (or mevlive parity catch-all), or a ≥3-entity transfer
                    // cycle that Stage 3b may upgrade. Plain single-hop swaps
                    // with a positive residual are NOT MEV — do not emit them
                    // as Unknown (that polluted the live feed as fake arb/$).
                    let transfer_cycle = has_transfer_cycle(searcher, &tx.transfers);
                    if !arb_likely && !transfer_cycle {
                        continue;
                    }
                    // Phase 2.3: emit every positive residual so persist can
                    // USD-sum across them; the selected pair stays display-primary.
                    let profit_tokens: Vec<(Address, U256)> = ledger
                        .positive_tokens(searcher)
                        .into_iter()
                        .map(|t| (t, ledger.net(searcher, t)))
                        .filter(|(_, amt)| *amt > U256::ZERO)
                        .collect();
                    let kind = if arb_likely {
                        MevKind::ArbAtomic
                    } else {
                        MevKind::Unknown
                    };
                    // P0.4: flash-swap / flash-loan flag when principal was borrowed.
                    let flash_arb = !tx.flashloans.is_empty() || flashloan_fee_wei.is_some();
                    // P1.3: multi-hop arb whose route includes a non-blue-chip token.
                    let long_tail =
                        arb_likely && is_long_tail_arb(&tx.swaps, &input.profit_policy.priority);
                    let mut tags: Vec<&str> = Vec::new();
                    if flash_arb && arb_likely {
                        tags.push("flash_arb");
                    }
                    if long_tail {
                        tags.push("long_tail");
                    }
                    if arb_likely {
                        tags.extend(strategy_tags::arb_strategy_tags(&tx.swaps, &arb_tag_opts));
                    }
                    events.push(MevEvent {
                        block: input.block,
                        ts: input.ts,
                        tx_index: tx.tx_index,
                        tx_hash: tx.tx_hash,
                        kind,
                        searcher,
                        contract: if searcher == tx.from { None } else { tx.to },
                        pools: tx.swaps.iter().map(|s| s.pool).collect(),
                        profit_token: Some(token),
                        profit_amount: Some(amount),
                        profit_tokens,
                        profit_usd: None, // pricing applied at persist time
                        gas_cost_wei: U256::from(tx.gas_used).saturating_mul(U256::from(
                            (tx.effective_gas_price_gwei * 1e9) as u128,
                        )),
                        flashloan_fee_wei,
                        flashloan_fee_token,
                        confidence: if arb_confirmed {
                            Confidence::Exact
                        } else {
                            Confidence::Inferred
                        },
                        victim_hashes: vec![],
                        victim_swap_size: None,
                        details: serde_json::json!({
                            "mode": "realized",
                            "tags": tags,
                            "pnl_basis": PnlBasis::R.as_str(),
                            "pnl": {
                                "basis": PnlBasis::R.as_str(),
                                "profit_amount": amount.to_string(),
                                "profit_token": format!("{:#x}", token),
                            },
                            "route": tx.swaps.iter().map(|s| serde_json::json!({
                                "pool": format!("{:#x}", s.pool),
                                "amm": s.amm.as_str(),
                                "token_in": format!("{:#x}", s.token_in),
                                "token_out": format!("{:#x}", s.token_out),
                                "token_source": s.token_source.as_str(),
                                "amount_in": s.amount_in.to_string(),
                                "amount_out": s.amount_out.to_string(),
                            })).collect::<Vec<_>>(),
                            "arb_meta": arb_route_meta(&tx.swaps, flash_arb),
                        }),
                    });
                    continue;
                }
            }
        }

        // ── 5. JIT pass (Mint+Burn same position, same block) ──────────
        // Handled block-wide below (needs cross-tx pairing); per-tx we only
        // mark minted positions.
    }

    // ── 4b. Keeper / automation execution (plan P1.5 / §21) ─────────────
    // Mode A: Gelato ExecSuccess / Chainlink UpkeepPerformed|LogTriggered.
    // Emitted as Unknown + tags so MevKind stays stable (§8.1); P&L basis F.
    for tx in &input.txs {
        for keeper in &tx.keepers {
            let fee = keeper.fee.filter(|f| !f.is_zero());
            let mut details = serde_json::json!({
                "protocol": keeper.protocol,
                "tags": ["keeper", "automation"],
                "pnl_basis": PnlBasis::F.as_str(),
                "emitter": format!("{:#x}", keeper.emitter),
                "pnl": {
                    "basis": PnlBasis::F.as_str(),
                },
            });
            if let Some(f) = fee {
                details["fee"] = serde_json::json!(f.to_string());
                details["pnl"]["fee"] = serde_json::json!(f.to_string());
            }
            if let Some(tok) = keeper.fee_token {
                details["fee_token"] = serde_json::json!(format!("{:#x}", tok));
                details["pnl"]["fee_token"] = serde_json::json!(format!("{:#x}", tok));
            }
            events.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: tx.tx_index,
                tx_hash: tx.tx_hash,
                kind: MevKind::Unknown,
                searcher: tx.from,
                contract: tx.to,
                pools: vec![],
                profit_token: keeper.fee_token,
                profit_amount: fee,
                profit_tokens: vec![],
                profit_usd: None,
                gas_cost_wei: U256::from(tx.gas_used)
                    .saturating_mul(U256::from((tx.effective_gas_price_gwei * 1e9) as u128)),
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Exact,
                victim_hashes: vec![],
                victim_swap_size: None,
                details,
            });
        }
    }

    // ── 4c. Airdrop claim-and-sell (plan P3.13 / §7.3) ──────────────────
    // Mode A: Transfer from zero + same-tx sell swap; held claims are ignored.
    // When the tx already produced ArbAtomic, just attach the tag (no double count).
    for tx in &input.txs {
        if !tx.success {
            continue;
        }
        let Some(token) = strategy_tags::claim_and_sell_token(&tx.transfers, &tx.swaps) else {
            continue;
        };
        if let Some(ev) = events
            .iter_mut()
            .find(|e| e.tx_index == tx.tx_index && e.kind == MevKind::ArbAtomic)
        {
            append_tag(&mut ev.details, "claim_and_sell");
            append_tag(&mut ev.details, "airdrop");
            ev.details["claimed_token"] = serde_json::json!(format!("{:#x}", token));
            continue;
        }
        let ledger =
            DeltaLedger::from_transfers(&tx.transfers, input.wrapped_native, (tx.from, tx.value));
        let Some(pt) = select_profit_token(&ledger, tx.from, &input.profit_policy) else {
            continue;
        };
        let pa = ledger.net(tx.from, pt);
        if pa.is_zero() {
            continue;
        }
        events.push(MevEvent {
            block: input.block,
            ts: input.ts,
            tx_index: tx.tx_index,
            tx_hash: tx.tx_hash,
            kind: MevKind::Unknown,
            searcher: tx.from,
            contract: tx.to,
            pools: tx.swaps.iter().map(|s| s.pool).collect(),
            profit_token: Some(pt),
            profit_amount: Some(pa),
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::from(tx.gas_used)
                .saturating_mul(U256::from((tx.effective_gas_price_gwei * 1e9) as u128)),
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({
                "protocol": "claim_and_sell",
                "tags": ["claim_and_sell", "airdrop"],
                "claimed_token": format!("{:#x}", token),
                "pnl_basis": PnlBasis::R.as_str(),
                "pnl": {
                    "basis": PnlBasis::R.as_str(),
                    "profit_amount": pa.to_string(),
                    "profit_token": format!("{:#x}", pt),
                },
            }),
        });
    }

    // ── 4d. ERC-4337 bundler executions (plan P3.11 / §7.4) ─────────────
    for tx in &input.txs {
        for uo in &tx.user_ops {
            if !uo.success {
                continue;
            }
            events.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: tx.tx_index,
                tx_hash: tx.tx_hash,
                kind: MevKind::Unknown,
                searcher: tx.from,
                contract: Some(uo.entry_point),
                pools: vec![],
                profit_token: Some(input.wrapped_native),
                profit_amount: Some(uo.actual_gas_cost),
                profit_tokens: vec![],
                profit_usd: None,
                gas_cost_wei: U256::from(tx.gas_used)
                    .saturating_mul(U256::from((tx.effective_gas_price_gwei * 1e9) as u128)),
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Exact,
                victim_hashes: vec![],
                victim_swap_size: None,
                details: serde_json::json!({
                    "protocol": "erc4337",
                    "tags": ["bundler", "erc4337"],
                    "sender": format!("{:#x}", uo.sender),
                    "paymaster": format!("{:#x}", uo.paymaster),
                    "pnl_basis": PnlBasis::R.as_str(),
                    "pnl": {
                        "basis": PnlBasis::R.as_str(),
                        "actual_gas_cost": uo.actual_gas_cost.to_string(),
                    },
                }),
            });
        }
    }

    // ── 3b. Transfer-graph cycle upgrade (Phase 1.6) ────────────────────
    // Unknown events whose tx shows a closed ≥3-entity transfer cycle rooted
    // at the searcher (searcher → X → … → searcher) upgrade to ArbAtomic.
    // This recovers aggregator/routing flows whose swap logs are not
    // decodable; it is independent of the `arb_likely_parity` heuristic and
    // stays Inferred — the upgrade never inflates Exact arb (§1.6).
    {
        let txs_by_index: HashMap<u64, &TxInput> =
            input.txs.iter().map(|t| (t.tx_index, t)).collect();
        for ev in events.iter_mut() {
            if ev.kind != MevKind::Unknown {
                continue;
            }
            // Keeper / bundler / claim-and-sell stay on their declared basis;
            // do not promote them via the transfer-cycle arb heuristic.
            if ev
                .details
                .get("tags")
                .and_then(|t| t.as_array())
                .is_some_and(|a| {
                    a.iter().any(|x| {
                        matches!(
                            x.as_str(),
                            Some("keeper") | Some("bundler") | Some("claim_and_sell")
                        )
                    })
                })
            {
                continue;
            }
            let Some(tx) = txs_by_index.get(&ev.tx_index).copied() else {
                continue;
            };
            if has_transfer_cycle(ev.searcher, &tx.transfers) {
                ev.kind = MevKind::ArbAtomic;
                ev.details["mode"] = serde_json::json!("realized");
                ev.details["reasons"] = serde_json::json!(["TRANSFER_CYCLE"]);
                ev.details["evidence"] = serde_json::json!({
                    "transfer_cycle": true,
                    "note": "closed >=3-entity transfer cycle with positive ledger delta",
                });
                ev.details["arb_meta"] = arb_route_meta(&tx.swaps, !tx.flashloans.is_empty());
                if let Some(meta) = ev.details.get_mut("arb_meta") {
                    meta["arb_shape"] = serde_json::json!("transfer_cycle");
                }
            }
        }
    }

    // ── 5b. JIT block-wide pairing ──────────────────────────────────────
    let mut jit_events = classify_jit(input);
    // jit_arb when a JIT tx also produced an arb in this block. The standalone
    // ArbAtomic is suppressed so USD/P&L never double-counts the same flow.
    //
    // ⚠️ LOAD-BEARING: this is a kind *upgrade*, not a standalone pass, and it is
    // independent of the live detectors. The `Strategy::JitArb` variant and the
    // `JitArbDetector` were removed from the execution path
    // (`docs/plan_prune_strategies.md` §5) because that detector had no honest
    // P&L model — but this branch must survive. Dropping either half corrupts
    // every USD/P&L figure the explorer emits: keeping `JitArb` without the
    // `retain` below double-counts the flow as `Jit` *and* `ArbAtomic`.
    let arb_txs: std::collections::HashSet<u64> = events
        .iter()
        .filter(|e| e.kind == MevKind::ArbAtomic)
        .map(|e| e.tx_index)
        .collect();
    let mut jit_arb_txs: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for ev in jit_events.iter_mut() {
        if arb_txs.contains(&ev.tx_index) {
            ev.kind = MevKind::JitArb;
            ev.details["components"] = serde_json::json!(["JIT", "Arb"]);
            jit_arb_txs.insert(ev.tx_index);
        }
    }
    if !jit_arb_txs.is_empty() {
        events.retain(|e| !(e.kind == MevKind::ArbAtomic && jit_arb_txs.contains(&e.tx_index)));
    }
    events.append(&mut jit_events);

    // ── 4b. Sandwich classification: front-run → victim → back-run ─────
    let mut sandwiches = classify_sandwiches(input);

    // ── 3c. Causal backrun / frontrun (Phase 3) ─────────────────────────
    // Sandwich-consumed fronts/backs are excluded (kind priority): a sandwich
    // anchors on its front-run tx and closes on the back-run tx; neither may
    // be re-claimed as a standalone frontrun/backrun.
    let mut consumed: HashSet<u64> = HashSet::new();
    for s in &sandwiches {
        consumed.insert(s.tx_index);
        if let Some(b) = s.details.get("backrun_tx_index").and_then(|v| v.as_u64()) {
            consumed.insert(b);
        }
    }
    let mut frontruns = classify_frontruns(input, &consumed);
    let frontrun_consumed: HashSet<u64> = frontruns.iter().map(|e| e.tx_index).collect();
    let mut back_consumed = consumed;
    back_consumed.extend(frontrun_consumed);
    let mut backruns = classify_backruns(input, &back_consumed);
    // Kind precedence on the same claimed tx: Frontrun > Backrun > ArbAtomic >
    // Unknown. A causal claim lifts a profitable closed-cycle tx out of the arb
    // pass; the arb/unknown event on that tx is superseded so P&L is never
    // double-counted (Frontrun/Backrun win per the precedence list).
    let mut superseder: HashMap<u64, MevKind> = HashMap::new();
    for e in frontruns.iter().chain(backruns.iter()) {
        superseder.entry(e.tx_index).or_insert(e.kind);
    }
    events.retain(|e| {
        !(matches!(e.kind, MevKind::ArbAtomic | MevKind::Unknown)
            && superseder.contains_key(&e.tx_index))
    });
    events.append(&mut frontruns);
    events.append(&mut backruns);
    events.append(&mut sandwiches);

    events.sort_by(|a, b| {
        a.tx_index
            .cmp(&b.tx_index)
            .then_with(|| kind_order(a.kind).cmp(&kind_order(b.kind)))
    });
    events
}

fn kind_order(k: MevKind) -> u8 {
    match k {
        MevKind::Sandwich => 0,
        MevKind::Frontrun => 1,
        MevKind::Backrun => 2,
        MevKind::ArbAtomic => 3,
        MevKind::JitArb => 4,
        MevKind::Jit => 5,
        MevKind::Liquidation => 6,
        MevKind::Skim => 7,
        MevKind::Unknown => 8,
    }
}
















/// Derive route geometry metadata for an atomic arb (spec §10 / §11.4).
/// Does not invent new `MevKind`s — annotation only.
fn arb_route_meta(swaps: &[SwapFact], flashloan_funded: bool) -> serde_json::Value {
    let hop_count = swaps.len();
    let mut pools: Vec<Address> = swaps.iter().map(|s| s.pool).collect();
    pools.sort();
    pools.dedup();
    let mut amms: Vec<&str> = swaps.iter().map(|s| s.amm.as_str()).collect();
    amms.sort();
    amms.dedup();
    let mut tokens: Vec<Address> = Vec::new();
    for s in swaps {
        if !has_unresolved_tokens(s) {
            tokens.push(s.token_in);
            tokens.push(s.token_out);
        }
    }
    tokens.sort();
    tokens.dedup();
    let unique_tokens = tokens.len();
    let arb_shape = if hop_count == 0 {
        "transfer_cycle"
    } else if hop_count == 2 || unique_tokens == 2 {
        "two_pool"
    } else if hop_count == 3 || unique_tokens == 3 {
        "triangular"
    } else {
        "multi_hop"
    };
    serde_json::json!({
        "hop_count": hop_count,
        "pool_count": pools.len(),
        "dex_count": amms.len(),
        "is_cross_dex": amms.len() > 1,
        "flashloan_funded": flashloan_funded,
        "arb_shape": arb_shape,
        "unique_tokens": unique_tokens,
    })
}




/// Multi-hop arb whose route includes a non-blue-chip token (plan P1.3 / §2.2).
///
/// Blue-chip allowlist = profit-token priority (stables + wrapped native).
/// Requires ≥2 swap legs so single-hop dust is never labeled long-tail.
fn is_long_tail_arb(swaps: &[SwapFact], blue_chips: &[Address]) -> bool {
    if swaps.len() < 2 {
        return false;
    }
    let blue: HashSet<Address> = blue_chips.iter().copied().collect();
    swaps.iter().any(|s| {
        (!s.token_in.is_zero() && !blue.contains(&s.token_in))
            || (!s.token_out.is_zero() && !blue.contains(&s.token_out))
    })
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

fn append_tag(details: &mut serde_json::Value, tag: &str) {
    let arr = details
        .as_object_mut()
        .map(|o| o.entry("tags").or_insert_with(|| serde_json::json!([])));
    if let Some(serde_json::Value::Array(tags)) = arr {
        if !tags.iter().any(|t| t.as_str() == Some(tag)) {
            tags.push(serde_json::json!(tag));
        }
    }
}

/// Decoded facts for one transaction.
pub type TxFacts = (
    Vec<TransferFact>,
    Vec<SwapFact>,
    Vec<crate::explorer::types::LiquidationFact>,
    Vec<crate::explorer::types::FlashLoanFact>,
    Vec<BuyCollateralFact>,
    Vec<JitFact>,
    Vec<V2PairOpFact>,
    Vec<OracleUpdateFact>,
    Vec<ReserveDataFact>,
    Vec<RateCacheFact>,
    Vec<KeeperFact>,
    Vec<EpochRewardFact>,
    Vec<GmxEventFact>,
    Vec<UserOpFact>,
);

/// Decode a tx's receipt logs into facts (used by ingest / scenarios).
/// `pool_tokens` = pool → (token0, token1) registry for swap direction (1.1).
/// `liquidation_protocol_aliases` remaps shared-topic0 emitters (Spark, Benqi).
pub(crate) fn decode_tx_logs_with_aliases(
    tx_index: u64,
    logs: &[crate::data::LogData],
    pool_tokens: &HashMap<Address, (Address, Address)>,
    liquidation_protocol_aliases: &HashMap<Address, &'static str>,
) -> TxFacts {
    let mut transfers = Vec::new();
    let mut swaps = Vec::new();
    let mut liquidations = Vec::new();
    let mut flashloans = Vec::new();
    let mut buy_collaterals = Vec::new();
    let mut jit = Vec::new();
    let mut v2_pair_ops = Vec::new();
    let mut oracle_updates = Vec::new();
    let mut reserve_updates = Vec::new();
    let mut rate_cache_updates = Vec::new();
    let mut keepers = Vec::new();
    let mut epoch_rewards = Vec::new();
    let mut gmx_events = Vec::new();
    let mut user_ops = Vec::new();
    for (log_idx, log) in logs.iter().enumerate() {
        let log_idx = log_idx as u64;
        if let Some(mut t) = decode::decode_transfer(log) {
            t.tx_index = tx_index;
            t.log_index = log_idx;
            transfers.push(t);
        }
        if let Some((amm, mut s)) = decode::decode_swap(log) {
            s.tx_index = tx_index;
            s.log_index = log_idx;
            s.amm = amm;
            swaps.push(s);
        }
        if let Some(mut l) = decode::decode_liquidation(log) {
            l.tx_index = tx_index;
            l.log_index = log_idx;
            decode::remap_liquidation_protocol(&mut l, liquidation_protocol_aliases);
            liquidations.push(l);
        }
        if let Some(mut f) = decode::decode_flash_loan(log) {
            f.tx_index = tx_index;
            f.log_index = log_idx;
            flashloans.push(f);
        }
        if let Some(mut b) = decode::decode_buy_collateral(log) {
            b.tx_index = tx_index;
            b.log_index = log_idx;
            buy_collaterals.push(b);
        }
        if let Some(mut j) = decode::decode_v3_mint_burn(log) {
            j.tx_index = tx_index;
            j.log_index = log_idx;
            jit.push(j);
        } else if let Some(mut j) = decode::decode_lb_bins_liquidity(log) {
            j.tx_index = tx_index;
            j.log_index = log_idx;
            jit.push(j);
        }
        if let Some(mut op) = decode::decode_v2_pair_op(log) {
            op.tx_index = tx_index;
            op.log_index = log_idx;
            v2_pair_ops.push(op);
        }
        if let Some(mut o) = decode::decode_oracle_update(log) {
            o.tx_index = tx_index;
            o.log_index = log_idx;
            oracle_updates.push(o);
        }
        if let Some(mut r) = decode::decode_reserve_data(log) {
            r.tx_index = tx_index;
            r.log_index = log_idx;
            reserve_updates.push(r);
        }
        if let Some(mut rc) = decode::decode_rate_cache_update(log) {
            rc.tx_index = tx_index;
            rc.log_index = log_idx;
            rate_cache_updates.push(rc);
        }
        if let Some(mut k) = decode::decode_keeper(log) {
            k.tx_index = tx_index;
            k.log_index = log_idx;
            keepers.push(k);
        }
        if let Some(mut e) = decode::decode_epoch_reward(log) {
            e.tx_index = tx_index;
            e.log_index = log_idx;
            epoch_rewards.push(e);
        }
        if let Some(mut g) = decode::decode_gmx_event(log) {
            g.tx_index = tx_index;
            g.log_index = log_idx;
            gmx_events.push(g);
        }
        if let Some(mut u) = decode::decode_user_op(log) {
            u.tx_index = tx_index;
            u.log_index = log_idx;
            user_ops.push(u);
        }
    }
    decode::attach_swap_tokens(&mut swaps, &transfers, pool_tokens);
    // Phase 1.6: drop aggregator edges already covered by DEX swap edges.
    decode::dedup_aggregator_facts(&mut swaps);
    (
        transfers,
        swaps,
        liquidations,
        flashloans,
        buy_collaterals,
        jit,
        v2_pair_ops,
        oracle_updates,
        reserve_updates,
        rate_cache_updates,
        keepers,
        epoch_rewards,
        gmx_events,
        user_ops,
    )
}

/// True when a swap fact has unresolved direction tokens (V3 sentinel kept).
pub(crate) fn has_unresolved_tokens(s: &SwapFact) -> bool {
    s.token_in == decode::TOKEN0_SENTINEL
        || s.token_in == decode::TOKEN1_SENTINEL
        || s.token_in == Address::ZERO
        || s.token_out == Address::ZERO
}

/// Phase 1.2 flow ownership (§7.1/§8.1): a closed cycle must be attributable
/// to one searcher's route. Among direction-resolved swap legs, the distinct
/// funder-owners must collapse to a single actor, and that actor must be one
/// of the tx's searcher candidates. Fully-unattributed legs (no inbound
/// transfer observable) cannot disprove ownership and pass (preserves recall
/// for flash-mint / internal-balance flows); an unrelated actor's legs always
/// fail.
pub(super) fn flow_attributable(swaps: &[SwapFact], candidates: &[Address]) -> bool {
    let mut owner: Option<Address> = None;
    for s in swaps {
        if has_unresolved_tokens(s) {
            continue;
        }
        let Some(o) = s.owner else {
            continue;
        };
        match owner {
            Some(cur) if cur != o => return false,
            _ => owner = Some(o),
        }
    }
    match owner {
        None => true, // nothing to disprove
        Some(o) => candidates.contains(&o),
    }
}

/// Suppress `unknown` events whose swaps are entirely unresolved-direction
/// or whose searcher candidate netted zero after netting — dedup guard for
/// noisy blocks.
pub(crate) fn filter_unresolved(events: Vec<MevEvent>) -> Vec<MevEvent> {
    events
        .into_iter()
        .filter(|e| {
            if e.kind != MevKind::Unknown {
                return true;
            }
            // Keeper / bundler / claim-and-sell are Exact fingerprints even
            // when profit_amount is zero/absent — keep them (P1.5 / P3.11/13).
            if e.details
                .get("tags")
                .and_then(|t| t.as_array())
                .is_some_and(|a| {
                    a.iter().any(|x| {
                        matches!(
                            x.as_str(),
                            Some("keeper") | Some("bundler") | Some("claim_and_sell")
                        )
                    })
                })
            {
                return true;
            }
            e.profit_amount.map(|a| !a.is_zero()).unwrap_or(false)
        })
        .collect()
}

/// True when the tx's ERC-20 transfer graph contains a closed ≥3-entity cycle
/// rooted at `searcher` (searcher → X → … → searcher with ≥2 distinct
/// intermediate addresses). Zero-address nodes (wrapped-native mint/burn) are
/// excluded. Evidence for the Phase 1.6 Unknown→ArbAtomic upgrade.
fn has_transfer_cycle(searcher: Address, transfers: &[TransferFact]) -> bool {
    let mut adj: HashMap<Address, Vec<Address>> = HashMap::new();
    for t in transfers {
        if t.amount.is_zero() || t.from.is_zero() || t.to.is_zero() || t.from == t.to {
            continue;
        }
        adj.entry(t.from).or_default().push(t.to);
    }
    let Some(outs) = adj.get(&searcher).cloned() else {
        return false;
    };
    for n in outs {
        if n == searcher {
            continue;
        }
        let mut stack = vec![(n, vec![n])];
        while let Some((node, path)) = stack.pop() {
            let Some(nexts) = adj.get(&node) else {
                continue;
            };
            for &m in nexts {
                if m == searcher {
                    // Back to the searcher through ≥2 distinct intermediaries.
                    if path.len() >= 2 {
                        return true;
                    }
                } else if m != node && path.len() < 6 && !path.contains(&m) {
                    let mut p = path.clone();
                    p.push(m);
                    stack.push((m, p));
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::store::OpenPosition;
    use crate::explorer::test_fixtures::{
        block, block_with_v2, classify_kind, event_of, flash_loan, jit, kinds, liquidation, swap,
        swap_at_tick, swap_owned, transfer, tx, ATK, POOL_A, POOL_B, TOKA, USDC, VICTIM, WNATIVE,
    };
    use crate::explorer::types::{Confidence, JitFact, MevKind, V2PairOpFact, V2PairOpKind};
    use alloy::primitives::{address, b256, U256};


    #[test]
    fn arb_cycle_single_flow_owner_is_exact() {
        // Both legs funded by the searcher itself → attributable to its route
        // (§7.1): cycle stays Exact.
        let swaps = vec![
            swap_owned(ATK, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(ATK, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        assert_eq!(ev.confidence, Confidence::Exact);
    }

    #[test]
    fn arb_cycle_mixed_flow_owner_is_not_exact() {
        // The second leg of the closed cycle is funded by an unrelated actor
        // inside the same tx — the cycle mixes routes and must NOT be Exact
        // (§7.1/§8.1). The parity fallback still labels it arb/Inferred.
        const OUTSIDER: Address = address!("7000000000000000000000000000000000000011");
        let swaps = vec![
            swap_owned(ATK, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(OUTSIDER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        assert_eq!(
            ev.confidence,
            Confidence::Inferred,
            "cycle mixes unrelated owners"
        );
        assert_eq!(ev.searcher, ATK);
    }

    #[test]
    fn arb_cycle_owned_only_by_non_candidate_is_not_exact() {
        // A closed cycle whose legs are all funded by an address that is not a
        // searcher candidate is not attributable to the route → not Exact.
        const OUTSIDER: Address = address!("7000000000000000000000000000000000000012");
        let swaps = vec![
            swap_owned(OUTSIDER, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(OUTSIDER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        assert_eq!(
            ev.confidence,
            Confidence::Inferred,
            "flow owner is not a candidate"
        );
    }

    #[test]
    fn multi_residual_arb_captures_every_positive_token() {
        const TOKB: Address = address!("7000000000000000000000000000000000000009");
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap(POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
            transfer(4, TOKB, VICTIM, ATK, 500),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        // Display-primary stays the priority token; every positive residual is
        // listed for persist-time USD summation (2.3), zero-nets excluded.
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(10)));
        let mut toks = ev.profit_tokens.clone();
        toks.sort();
        assert_eq!(toks, vec![(USDC, U256::from(10)), (TOKB, U256::from(500))]);
    }

    #[test]
    fn arb_route_json_records_token_source() {
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap(POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        let route = ev.details["route"].as_array().expect("route");
        assert!(route.len() >= 2);
        assert!(route
            .iter()
            .all(|leg| leg["token_source"].as_str() == Some("registry")));
    }

    #[test]
    fn non_cycle_profitable_swap_is_arb_likely() {
        let swaps = vec![swap(POOL_A, USDC, TOKA, 100, 200)];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        // Single-hop profitable residuals are labeled arb (not Unknown) for
        // live-feed parity with tip explorers (mevlive Type=Arbitrage).
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(ev.profit_token, Some(TOKA));
        assert_eq!(ev.profit_amount, Some(U256::from(200)));
    }

    #[test]
    fn non_cycle_with_parity_off_emits_nothing() {
        // Spec §7.1: a single-hop profitable residual is not MEV. With parity
        // off we must not emit Unknown (that used to flood the live feed).
        let swaps = vec![swap(POOL_A, USDC, TOKA, 100, 200)];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ];
        let mut input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        input.arb_likely_parity = false;
        let events = classify_block(&input);
        assert!(
            events.is_empty(),
            "expected no events for plain swap, got {events:?}"
        );
    }

    #[test]
    fn transfer_cycle_upgrades_unknown_to_arb() {
        // Aggregator-routed flow: swap tokens don't close a cycle (USDC→TOKA→
        // WNATIVE is acyclic), but the transfer graph closes ATK → router →
        // POOL_A → ATK — a ≥3-entity cycle. With parity off the residual would
        // be Unknown; the transfer cycle upgrades it to ArbAtomic (Inferred).
        let router = address!("7000000000000000000000000000000000000007");
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap(POOL_B, TOKA, WNATIVE, 200, 60),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, router, 100),
            transfer(1, USDC, router, POOL_A, 100),
            transfer(2, TOKA, POOL_A, ATK, 200),
        ];
        let mut input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        input.arb_likely_parity = false;
        let events = classify_block(&input);
        let ev = event_of(&events, MevKind::ArbAtomic);
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(ev.details["reasons"], serde_json::json!(["TRANSFER_CYCLE"]));
        assert_eq!(
            ev.details["evidence"]["transfer_cycle"],
            serde_json::json!(true)
        );
        assert_eq!(ev.profit_amount, Some(U256::from(200))); // TOKA payout
    }

    #[test]
    fn interleaved_cycle_is_exact_arb() {
        // Three-hop cycle emitted out of chain order: A:USDC->TOKA,
        // A:WNATIVE->USDC, B:TOKA->WNATIVE. The graph walk still closes it.
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap(POOL_A, WNATIVE, USDC, 50, 110),
            swap(POOL_B, TOKA, WNATIVE, 200, 60),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, WNATIVE, ATK, POOL_A, 50),
            transfer(3, USDC, POOL_A, ATK, 110),
            transfer(4, TOKA, ATK, POOL_B, 200),
            transfer(5, WNATIVE, POOL_B, ATK, 60),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        assert_eq!(ev.confidence, Confidence::Exact);
        assert_eq!(ev.profit_token, Some(USDC));
    }

    #[test]
    fn flash_loan_netting_depends_on_repay_leg() {
        let provider = address!("7000000000000000000000000000000000000007");
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 1000, 1000),
            swap(POOL_B, TOKA, USDC, 1000, 1010),
        ];
        let borrow_legs = vec![
            transfer(0, USDC, provider, ATK, 1000),
            transfer(1, USDC, ATK, POOL_A, 1000),
            transfer(2, TOKA, POOL_A, ATK, 1000),
            transfer(3, TOKA, ATK, POOL_B, 1000),
            transfer(4, USDC, POOL_B, ATK, 1010),
        ];
        // Missing repay: principal is stripped so only the 5 fee remains.
        // Present repay (principal + fee): the transfer stream already nets
        // the principal, and it must not be subtracted a second time.
        for with_repay in [false, true] {
            let mut transfers = borrow_legs.clone();
            if with_repay {
                transfers.push(transfer(5, USDC, ATK, provider, 1005));
            }
            let mut t = tx(0, ATK, true, swaps.clone(), transfers);
            t.flashloans = vec![flash_loan(provider)];
            let ev = classify_kind(&block(vec![t]), MevKind::ArbAtomic);
            assert_eq!(ev.profit_amount, Some(U256::from(5)), "repay={with_repay}");
            assert_eq!(ev.flashloan_fee_wei, Some(U256::from(5)));
        }
    }

    #[test]
    fn zero_netting_produces_no_event() {
        // Wrap-pair noise nets the wrapped-native delta to zero, so the
        // attacker holds no positive residual on any token -> no event.
        let swaps = vec![swap(POOL_A, WNATIVE, TOKA, 100, 200)];
        let zero_wrap = vec![
            transfer(0, WNATIVE, Address::ZERO, ATK, 1000), // wrapped mint (wrap noise)
            transfer(1, WNATIVE, ATK, Address::ZERO, 1000), // wrapped burn (wrap noise)
        ];
        // no TOKA transfer leg -> attacker nets nothing (wrap filtered).
        let input = block(vec![tx(0, ATK, true, swaps, zero_wrap)]);
        assert!(classify_block(&input).is_empty());
    }

    #[test]
    fn failed_tx_is_skipped() {
        let swaps = vec![swap(POOL_A, USDC, TOKA, 100, 200)];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ];
        let mut t = tx(0, ATK, true, swaps, transfers);
        t.success = false; // revert: swaps must not be classified
        let input = block(vec![t]);
        assert!(classify_block(&input).is_empty());
    }

    const MARKET: Address = address!("8000000000000000000000000000000000000008");

    #[test]
    fn causal_backrun_and_frontrun_cases() {
        // (include pre-move reference, mover, USDC sold into the move, expect a backrun)
        let backruns = [
            (
                "after a different sender moves the market",
                true,
                MARKET,
                1000u64,
                true,
            ),
            ("without a pre-move reference", false, MARKET, 1000, false),
            (
                "when the move and the backrun share a sender",
                true,
                ATK,
                500,
                false,
            ),
        ];
        for (name, with_reference, mover, move_in, expect) in backruns {
            let mut txs = Vec::new();
            if with_reference {
                txs.push(tx(
                    0,
                    VICTIM,
                    true,
                    vec![swap(POOL_A, TOKA, USDC, 100, 110)],
                    vec![
                        transfer(0, TOKA, VICTIM, POOL_A, 100),
                        transfer(1, USDC, POOL_A, VICTIM, 110),
                    ],
                ));
            }
            let move_idx = txs.len() as u64;
            txs.push(tx(
                move_idx,
                mover,
                true,
                vec![swap(POOL_A, USDC, TOKA, move_in, 100)],
                vec![
                    transfer(0, USDC, mover, POOL_A, move_in),
                    transfer(1, TOKA, POOL_A, mover, 100),
                ],
            ));
            let back_idx = txs.len() as u64;
            txs.push(tx(
                back_idx,
                ATK,
                true,
                vec![
                    swap(POOL_A, TOKA, USDC, 100, 120),
                    swap(POOL_B, USDC, TOKA, 120, 130),
                ],
                vec![
                    transfer(0, TOKA, ATK, POOL_A, 100),
                    transfer(1, USDC, POOL_A, ATK, 120),
                    transfer(2, USDC, ATK, POOL_B, 120),
                    transfer(3, TOKA, POOL_B, ATK, 130),
                ],
            ));
            let events = classify_block(&block(txs));
            let found = kinds(&events).contains(&MevKind::Backrun);
            assert_eq!(found, expect, "{name}");
            if !expect {
                continue;
            }
            let ev = event_of(&events, MevKind::Backrun);
            assert_eq!(ev.searcher, ATK);
            assert_eq!(ev.confidence, Confidence::Inferred);
            assert_eq!(ev.tx_index, back_idx);
            let reasons: Vec<&str> = ev.details["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert!(reasons.contains(&"STATE_DELTA_MATCH"));
            assert!(reasons.contains(&"DIRECT_TOKEN_CYCLE"));
            assert_eq!(
                ev.details["evidence"]["DIRECTION_OF_BENEFIT"],
                serde_json::json!(true)
            );
            assert_eq!(ev.details["tier"], serde_json::json!("inferred"));
            assert_eq!(ev.details["mode"], serde_json::json!("realized"));
            assert_eq!(
                events.iter().filter(|e| e.kind == MevKind::Backrun).count(),
                1
            );
            assert!(!events
                .iter()
                .any(|e| e.kind == MevKind::ArbAtomic && e.tx_index == back_idx));
            assert_eq!(ev.victim_hashes, vec![B256::repeat_byte(move_idx as u8)]);
        }

        // Victim output on the same pool: 150 is degraded vs the front-run's
        // 200; 200 is the same price and must not be a frontrun.
        for (name, victim_out, expect) in [
            ("cross-pool close after a degraded victim", 150u64, true),
            ("no measurable degradation", 200, false),
        ] {
            let f = tx(
                0,
                ATK,
                true,
                vec![swap(POOL_A, USDC, TOKA, 100, 200)],
                vec![
                    transfer(0, USDC, ATK, POOL_A, 100),
                    transfer(1, TOKA, POOL_A, ATK, 200),
                ],
            );
            let v = tx(
                1,
                VICTIM,
                true,
                vec![swap(POOL_A, USDC, TOKA, 100, victim_out)],
                vec![
                    transfer(0, USDC, VICTIM, POOL_A, 100),
                    transfer(1, TOKA, POOL_A, VICTIM, victim_out),
                ],
            );
            let c = tx(
                2,
                ATK,
                true,
                vec![swap(POOL_B, TOKA, USDC, 200, 250)],
                vec![
                    transfer(0, TOKA, ATK, POOL_B, 200),
                    transfer(1, USDC, POOL_B, ATK, 250),
                ],
            );
            let events = classify_block(&block(vec![f, v, c]));
            let found = kinds(&events).contains(&MevKind::Frontrun);
            assert_eq!(found, expect, "{name}");
            if !expect {
                continue;
            }
            let ev = event_of(&events, MevKind::Frontrun);
            assert_eq!(ev.searcher, ATK);
            assert_eq!(ev.confidence, Confidence::Inferred);
            assert_eq!(ev.tx_index, 0);
            assert_eq!(ev.profit_token, Some(USDC));
            assert_eq!(ev.profit_amount, Some(U256::from(150)));
            assert_eq!(ev.victim_hashes, vec![B256::repeat_byte(1)]);
            let reasons: Vec<&str> = ev.details["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap())
                .collect();
            assert!(reasons.contains(&"STATE_DELTA_MATCH"));
            assert!(reasons.contains(&"VICTIM_EXECUTION_DEGRADED"));
            assert!(reasons.contains(&"PROFIT_VERIFIED"));
            assert_eq!(ev.details["tier"], serde_json::json!("inferred"));
            assert_eq!(ev.details["victim_tx_index"], serde_json::json!(1));
            assert!(!kinds(&events).contains(&MevKind::Sandwich));
        }
    }
    #[test]
    fn sandwich_consumed_legs_not_reclaimed_by_phase3() {
        // The classic three-EOA sandwich: front (tx0), victim (tx1), back
        // (tx2) on one pool. Sandwich owns the anchors (kind priority); the
        // causal passes must not re-emit frontrun/backrun over the same legs.
        let t0 = tx(
            0,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        );
        let t1 = tx(
            1,
            VICTIM,
            true,
            vec![swap(POOL_A, USDC, TOKA, 200, 350)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 200),
                transfer(1, TOKA, POOL_A, VICTIM, 350),
            ],
        );
        let t2 = tx(
            2,
            ATK,
            true,
            vec![swap(POOL_A, TOKA, USDC, 350, 195)],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 350),
                transfer(1, USDC, POOL_A, ATK, 195),
            ],
        );
        let input = block(vec![t0, t1, t2]);
        let kinds = kinds(&classify_block(&input));
        assert!(kinds.contains(&MevKind::Sandwich));
        assert!(!kinds.contains(&MevKind::Frontrun));
        assert!(!kinds.contains(&MevKind::Backrun));
    }

    #[test]
    fn liquidation_outcomes() {
        struct Case {
            protocol: &'static str,
            liquidator: Address,
            collateral: Address,
            debt: Address,
            collateral_amount: u64,
            debt_to_cover: u64,
            with_collateral_transfer: bool,
            searcher: Option<Address>,
            profit_token: Option<Address>,
            profit_amount: Option<u64>,
            confidence: Option<Confidence>,
            reconciled: Option<bool>,
            reasons: Option<&'static [&'static str]>,
        }
        let cases = [
            Case {
                protocol: "aave_v3",
                liquidator: ATK,
                collateral: USDC,
                debt: WNATIVE,
                collateral_amount: 500,
                debt_to_cover: 300,
                with_collateral_transfer: false,
                searcher: Some(ATK),
                profit_token: Some(USDC),
                profit_amount: Some(500),
                confidence: Some(Confidence::Exact),
                reconciled: None,
                reasons: None,
            },
            Case {
                protocol: "aave_v3",
                liquidator: ATK,
                collateral: USDC,
                debt: WNATIVE,
                collateral_amount: 500,
                debt_to_cover: 300,
                with_collateral_transfer: true,
                searcher: None,
                profit_token: None,
                profit_amount: None,
                confidence: None,
                reconciled: Some(true),
                reasons: Some(&[]),
            },
            Case {
                protocol: "compound_v3",
                liquidator: ATK,
                collateral: Address::ZERO,
                debt: Address::ZERO,
                collateral_amount: 0,
                debt_to_cover: 300,
                with_collateral_transfer: false,
                searcher: None,
                profit_token: None,
                profit_amount: None,
                confidence: Some(Confidence::Exact),
                reconciled: Some(false),
                reasons: Some(&["TRANSFER_MISMATCH"]),
            },
            Case {
                protocol: "aave_v3",
                liquidator: Address::ZERO,
                collateral: USDC,
                debt: WNATIVE,
                collateral_amount: 500,
                debt_to_cover: 300,
                with_collateral_transfer: false,
                searcher: Some(ATK),
                profit_token: None,
                profit_amount: None,
                confidence: None,
                reconciled: None,
                reasons: None,
            },
        ];
        for c in cases {
            let transfers = if c.with_collateral_transfer {
                vec![transfer(0, USDC, POOL_A, ATK, 500)]
            } else {
                vec![]
            };
            let mut t = tx(0, ATK, true, vec![], transfers);
            t.liquidations = vec![liquidation(
                c.protocol,
                c.liquidator,
                c.collateral,
                c.debt,
                c.collateral_amount,
                c.debt_to_cover,
            )];
            let ev = classify_kind(&block(vec![t]), MevKind::Liquidation);
            if let Some(searcher) = c.searcher {
                assert_eq!(ev.searcher, searcher, "{}", c.protocol);
            }
            if let Some(token) = c.profit_token {
                assert_eq!(ev.profit_token, Some(token));
            }
            if let Some(amount) = c.profit_amount {
                assert_eq!(ev.profit_amount, Some(U256::from(amount)));
            }
            if let Some(confidence) = c.confidence {
                assert_eq!(ev.confidence, confidence);
            }
            if let Some(reconciled) = c.reconciled {
                assert_eq!(ev.details["reconciled"], serde_json::json!(reconciled));
            }
            if let Some(reasons) = c.reasons {
                assert_eq!(ev.details["reasons"], serde_json::json!(reasons));
            }
            assert_eq!(ev.details["pnl_basis"], serde_json::json!("O"));
        }
    }

    /// P0.1 positive: same tx has FlashLoanFact + LiquidationFact → tag

    /// P0.1 negative: flash loan without liquidation must not emit liquidation

    /// P0.3 positive: Absorb then BuyCollateral in the same block →
    /// `buycollateral` tag, pnl_basis O, Exact confidence.
    #[test]
    fn buycollateral_pairs_with_same_block_absorb() {
        let mut absorb_tx = tx(0, ATK, true, vec![], vec![]);
        absorb_tx.liquidations = vec![liquidation(
            "compound_v3",
            ATK,
            Address::ZERO,
            Address::ZERO,
            0,
            300,
        )];
        let mut buy_tx = tx(1, ATK, true, vec![], vec![]);
        buy_tx.buy_collaterals = vec![BuyCollateralFact {
            tx_index: 1,
            log_index: 0,
            protocol: "compound_v3",
            emitter: Address::ZERO,
            buyer: ATK,
            collateral_asset: USDC,
            base_amount: U256::from(90),
            collateral_amount: U256::from(100),
        }];
        let events = classify_block(&block(vec![absorb_tx, buy_tx]));
        let buy = events
            .iter()
            .find(|e| {
                e.kind == MevKind::Liquidation
                    && e.details
                        .get("tags")
                        .and_then(|t| t.as_array())
                        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("buycollateral")))
            })
            .expect("buycollateral event");
        assert_eq!(buy.confidence, Confidence::Exact);
        assert_eq!(buy.details["pnl_basis"], serde_json::json!("O"));
        assert_eq!(buy.details["absorb_paired"], serde_json::json!(true));
        assert_eq!(buy.details["pnl"]["base_paid"], serde_json::json!("90"));
        assert_eq!(buy.profit_amount, Some(U256::from(100)));
    }

    /// P0.3 negative: BuyCollateral without a prior Absorb is Inferred + unpaired.
    #[test]
    fn buycollateral_without_absorb_is_inferred() {
        let mut buy_tx = tx(0, ATK, true, vec![], vec![]);
        buy_tx.buy_collaterals = vec![BuyCollateralFact {
            tx_index: 0,
            log_index: 0,
            protocol: "compound_v3",
            emitter: Address::ZERO,
            buyer: ATK,
            collateral_asset: USDC,
            base_amount: U256::from(90),
            collateral_amount: U256::from(100),
        }];
        let ev = classify_kind(&block(vec![buy_tx]), MevKind::Liquidation);
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(ev.details["absorb_paired"], serde_json::json!(false));
        assert_eq!(
            ev.details["reasons"],
            serde_json::json!(["ABSORB_UNPAIRED"])
        );
    }

    /// P0.4 positive: ArbAtomic funded by a flash loan → `flash_arb` tag,
    /// pnl_basis R net of the in-tx premium (borrow +1000, arb +10 gross,
    /// repay −1005 → realized 5). Fixture models the full loop: borrow
    /// inflow, swap legs, repay leg (same convention as


    /// P0.2 end-to-end: a Benqi qiToken `LiquidateBorrow` receipt log decodes
    /// through the emitter alias registry to `details.protocol = "benqi"` and
    /// classifies with the Compound-family P&L basis `O` (§0.1) carrying the
    /// exact event amounts. Negative: the identical log without aliases stays

















    #[test]
    fn jit_cross_block_open_position_closed_by_burn() {
        let burn = jit(0, false);
        let mut t0 = tx(
            0,
            VICTIM,
            true,
            vec![swap_at_tick(POOL_A, USDC, TOKA, 100, 200, 0)],
            vec![],
        );
        t0.jit = vec![burn];
        let mut input = block(vec![t0]);
        input.block = 2000;
        input.open_positions = vec![OpenPosition {
            pool: POOL_A,
            owner: ATK,
            tick_lower: -100,
            tick_upper: 100,
            opened_block: 1500,
            liquidity: 1000,
        }];
        let ev = classify_kind(&input, MevKind::Jit);
        assert_eq!(ev.searcher, ATK);
        assert_eq!(ev.tx_index, 0); // anchored to the in-block burn tx
        assert_eq!(ev.details["opened_block"], serde_json::json!(1500));
        assert_eq!(ev.details["held_blocks"], serde_json::json!(500));
        assert_eq!(
            ev.details["reasons"],
            serde_json::json!(["SHORT_LP_LIFETIME", "OVERLAPPING_TICK_RANGE"])
        );
    }

    #[test]
    fn jit_requires_tick_overlap() {
        // Mint+Burn around tick 0..0, but the only swap lands at tick 200:
        // the range captured nothing, so it is ordinary LP activity, not JIT.
        let mint = jit(0, true);
        let burn = jit(1, false);
        let mut t0 = tx(
            0,
            ATK,
            true,
            vec![swap_at_tick(POOL_A, USDC, TOKA, 100, 200, 200)],
            vec![],
        );
        t0.jit = vec![mint];
        let mut t1 = tx(1, VICTIM, true, vec![], vec![]);
        t1.jit = vec![burn];
        assert!(!kinds(&classify_block(&block(vec![t0, t1]))).contains(&MevKind::Jit));
    }


    #[test]
    fn sandwich_contract_mediated_legs_tagged() {
        let router = address!("9000000000000000000000000000000000000009");
        let front = swap(POOL_A, USDC, TOKA, 100, 200);
        let victim = swap(POOL_A, USDC, TOKA, 200, 350);
        let back = swap(POOL_A, TOKA, USDC, 350, 195);
        let mut t0 = tx(
            0,
            ATK,
            true,
            vec![front],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        );
        t0.to = Some(router);
        let t1 = tx(
            1,
            VICTIM,
            true,
            vec![victim],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 200),
                transfer(1, TOKA, POOL_A, VICTIM, 350),
            ],
        );
        let mut t2 = tx(
            2,
            ATK,
            true,
            vec![back],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 350),
                transfer(1, USDC, POOL_A, ATK, 195),
            ],
        );
        t2.to = Some(router);
        let ev = classify_kind(&block(vec![t0, t1, t2]), MevKind::Sandwich);
        assert_eq!(ev.contract, Some(router));
        assert_eq!(
            ev.details["evidence"]["contract_mediated"],
            serde_json::json!(true)
        );
        let reasons: Vec<&str> = ev.details["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(reasons.contains(&"CONTRACT_MEDIATED"));
    }

    #[test]
    fn plain_two_leg_exchange_is_not_sandwich() {
        // Attacker buys then sells with no third-party victim in between:
        // a normal round-trip, not a sandwich.
        let t0 = tx(
            0,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        );
        let t1 = tx(
            1,
            ATK,
            true,
            vec![swap(POOL_A, TOKA, USDC, 200, 95)],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 200),
                transfer(1, USDC, POOL_A, ATK, 95),
            ],
        );
        let input = block(vec![t0, t1]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Sandwich));
    }

    #[test]
    fn victim_before_front_is_not_sandwich() {
        // The victim swap precedes the attacker's front-run: the attacker did
        // not open a position yet, so this is not a sandwich.
        let t0 = tx(
            0,
            VICTIM,
            true,
            vec![swap(POOL_A, USDC, TOKA, 200, 350)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 200),
                transfer(1, TOKA, POOL_A, VICTIM, 350),
            ],
        );
        let t1 = tx(
            1,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        );
        let t2 = tx(
            2,
            ATK,
            true,
            vec![swap(POOL_A, TOKA, USDC, 200, 195)],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 200),
                transfer(1, USDC, POOL_A, ATK, 195),
            ],
        );
        let input = block(vec![t0, t1, t2]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Sandwich));
    }

    #[test]
    fn jit_arb_upgraded_when_mint_tx_also_arbs() {
        // tx0 has a JIT mint AND an atomic-arb cycle.
        let swaps = vec![
            swap_at_tick(POOL_A, USDC, TOKA, 100, 200, 0),
            swap(POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let mint = JitFact {
            log_index: 9,
            ..jit(0, true)
        };
        let burn = jit(1, false);
        let mut t0 = tx(0, ATK, true, swaps, transfers);
        t0.jit = vec![mint];
        let mut t1 = tx(1, VICTIM, true, vec![], vec![]);
        t1.jit = vec![burn];
        let input = block(vec![t0, t1]);
        let events = classify_block(&input);
        assert!(kinds(&events).contains(&MevKind::JitArb));
        let ev = event_of(&events, MevKind::JitArb);
        assert_eq!(ev.tx_index, 0);
    }

    #[test]
    fn empty_block_yields_no_events() {
        let input = block(vec![tx(0, ATK, true, vec![], vec![])]);
        assert!(classify_block(&input).is_empty());
    }

    #[test]
    fn unresolved_unknown_filter_keeps_only_positive() {
        let keep = MevEvent {
            block: 1,
            ts: 1,
            tx_index: 0,
            tx_hash: B256::ZERO,
            kind: MevKind::Unknown,
            searcher: ATK,
            contract: None,
            pools: vec![],
            profit_token: Some(TOKA),
            profit_amount: Some(U256::from(10)),
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: Confidence::Inferred,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({}),
        };
        let mut drop = keep.clone();
        drop.profit_amount = Some(U256::ZERO);
        let out = filter_unresolved(vec![keep.clone(), drop]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].profit_amount, Some(U256::from(10)));
    }

    #[test]
    fn stamp_jit_tx_hashes_fills_zero_hashes() {
        let mut ev = MevEvent {
            block: 1,
            ts: 1,
            tx_index: 0,
            tx_hash: B256::ZERO,
            kind: MevKind::Jit,
            searcher: ATK,
            contract: None,
            pools: vec![],
            profit_token: None,
            profit_amount: None,
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({}),
        };
        let h = b256!("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        let mut map = HashMap::new();
        map.insert(0u64, h);
        stamp_jit_tx_hashes(std::slice::from_mut(&mut ev), &map);
        assert_eq!(ev.tx_hash, h);
        // Non-JIT / already-set hashes untouched.
        let mut arb = ev;
        arb.kind = MevKind::ArbAtomic;
        let h0 = B256::ZERO;
        arb.tx_hash = h0;
        let mut map2 = map.clone();
        map2.insert(0, B256::repeat_byte(0xAA));
        stamp_jit_tx_hashes(std::slice::from_mut(&mut arb), &map2);
        assert_eq!(arb.tx_hash, h0);
    }



    #[test]
    fn skim_dual_token_profit_tokens() {
        let t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![
                transfer(0, USDC, POOL_A, ATK, 10),
                transfer(1, TOKA, POOL_A, ATK, 20),
            ],
        );
        let input = block_with_v2(vec![t], &[POOL_A]);
        let ev = classify_kind(&input, MevKind::Skim);
        assert_eq!(ev.profit_tokens.len(), 2);
        assert!(ev
            .profit_tokens
            .iter()
            .any(|(tok, amt)| *tok == USDC && *amt == U256::from(10)));
        assert!(ev
            .profit_tokens
            .iter()
            .any(|(tok, amt)| *tok == TOKA && *amt == U256::from(20)));
        // Priority prefers USDC as primary display token.
        assert_eq!(ev.profit_token, Some(USDC));
    }

    #[test]
    fn skim_excluded_when_same_pair_has_swap() {
        let t = tx(
            0,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        );
        let input = block_with_v2(vec![t], &[POOL_A]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Skim));
    }

    #[test]
    fn skim_excluded_when_same_pair_has_sync() {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        t.v2_pair_ops = vec![V2PairOpFact {
            tx_index: 0,
            log_index: 1,
            pool: POOL_A,
            kind: V2PairOpKind::Sync,
        }];
        let input = block_with_v2(vec![t], &[POOL_A]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Skim));
    }

    #[test]
    fn skim_ignored_for_non_registry_address() {
        let t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        // Empty v2_like set → no skim.
        assert!(!kinds(&classify_block(&block(vec![t]))).contains(&MevKind::Skim));
    }

    #[test]
    fn skim_coexists_with_unrelated_arb_in_same_block() {
        let skim_tx = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        let arb_tx = tx(
            1,
            VICTIM,
            true,
            vec![
                swap_owned(VICTIM, POOL_A, USDC, TOKA, 100, 200),
                swap_owned(VICTIM, POOL_B, TOKA, USDC, 200, 110),
            ],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 100),
                transfer(1, TOKA, POOL_A, VICTIM, 200),
                transfer(2, TOKA, VICTIM, POOL_B, 200),
                transfer(3, USDC, POOL_B, VICTIM, 110),
            ],
        );
        // Only POOL_A is skim-eligible; arb on A+B still classifies separately.
        // Skim tx has no swap on POOL_A so skim fires; arb tx has swaps so no skim.
        let input = block_with_v2(vec![skim_tx, arb_tx], &[POOL_A]);
        let events = classify_block(&input);
        let ks = kinds(&events);
        assert!(ks.contains(&MevKind::Skim), "got {ks:?}");
        assert!(ks.contains(&MevKind::ArbAtomic), "got {ks:?}");
    }
}
