//! Per-block realized-MEV classifier.
//!
//! Input: one settled block (header + ordered txs with receipts, tx senders)
//! decoded into swap/transfer/liquidation/JIT facts. Output: `Vec<MevEvent>`
//! plus sandwich bundles folded into events (victim hashes + leg details).
//!
//! Pass order (first match wins, disjoint):
//! 1. Liquidation pass — exact event match on configured lending pools.
//! 2. Swap attribution — per-tx per-address net token deltas.
//! 3. Atomic arb pass — ≥2 swaps in one tx + closed cycle + positive net
//!    delta of a single profit token.
//! 4. Sandwich pass — same sender, same pool, opposite directions, with a
//!    third-party swap between front-run and back-run (tx-index ordered).
//! 5. JIT pass — V3 Mint+Burn same position same block; jit_arb when the
//!    same tx also closed an arb cycle. Leftover profitable patterns →
//!    `unknown` (inferred).

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, B256, U256};

use crate::explorer::decode;
use crate::explorer::profit::{
    has_closed_cycle, net_flash_loan, select_profit_token, DeltaLedger, ProfitTokenPolicy,
};
use crate::explorer::store::OpenPosition;
use crate::explorer::types::{Confidence, JitFact, MevEvent, MevKind, SwapFact, TransferFact};

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
    pub jit: Vec<JitFact>,
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
    pub txs: Vec<TxInput>,
}

/// Classify one block. Returns (events, bundle_details) where bundle details
/// for sandwiches are already folded into `details_json` per event.
pub fn classify_block(input: &BlockInput) -> Vec<MevEvent> {
    let mut events: Vec<MevEvent> = Vec::new();

    // ── 1. Liquidation pass (exact, zero-heuristic) ─────────────────────
    for tx in &input.txs {
        if tx.liquidations.is_empty() {
            continue;
        }
        let ledger =
            DeltaLedger::from_transfers(&tx.transfers, input.wrapped_native, (tx.from, tx.value));
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
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Exact,
                victim_hashes: vec![],
                victim_swap_size: None,
                details: serde_json::json!({
                    "protocol": liq.protocol,
                    "user": format!("{:#x}", liq.user),
                    "collateral_asset": format!("{:#x}", liq.collateral_asset),
                    "debt_asset": format!("{:#x}", liq.debt_asset),
                    "collateral_amount": liq.collateral_amount.to_string(),
                    "debt_to_cover": liq.debt_to_cover.to_string(),
                    "reconciled": reconciled,
                    "reasons": reasons,
                }),
            });
        }
    }
    let liq_txs: std::collections::HashSet<u64> = events.iter().map(|e| e.tx_index).collect();

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
                            "route": tx.swaps.iter().map(|s| serde_json::json!({
                                "pool": format!("{:#x}", s.pool),
                                "amm": s.amm.as_str(),
                                "token_in": format!("{:#x}", s.token_in),
                                "token_out": format!("{:#x}", s.token_out),
                                "amount_in": s.amount_in.to_string(),
                                "amount_out": s.amount_out.to_string(),
                            })).collect::<Vec<_>>(),
                            "arb_meta": arb_route_meta(&tx.swaps, flashloan_fee_wei.is_some()),
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
        MevKind::Unknown => 7,
    }
}

#[derive(Debug, Clone, Default)]
struct SwapLeg {
    tx_index: u64,
    tx_hash: B256,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out: U256,
}

/// One attacker's sandwich walk over a pool: front-run opens, victims
/// (third-party same-direction swaps in block order), and the closing back-run.
#[derive(Debug, Default)]
struct SandwichWalk {
    /// Third-party same-direction swaps seen while the position was open.
    victims: Vec<SwapLeg>,
}

/// True when `a` and `b` swap the same token pair in the same direction.
fn same_dir(a: &SwapLeg, b: &SwapLeg) -> bool {
    !a.token_in.is_zero()
        && !a.token_out.is_zero()
        && a.token_in == b.token_in
        && a.token_out == b.token_out
}

/// True when `a` and `b` swap the same token pair in opposite directions.
fn opposite_dir(a: &SwapLeg, b: &SwapLeg) -> bool {
    !a.token_in.is_zero()
        && !a.token_out.is_zero()
        && a.token_in == b.token_out
        && a.token_out == b.token_in
}

/// Classic realized-sandwich detection (pass 4): one attacker
/// opens a position on a pool (front-run), a **third-party** same-direction
/// swap (the victim) lands between it and the attacker's closing opposite-
/// direction swap (back-run). Victims are any EOA, not just one bundled in
/// the attacker's own tx, so the classic three-distinct-EOA pattern matches.
///
/// Walk per (pool, attacker): track the attacker's first swap on the pool as
/// the front-run; every other-sender swap in the same direction while the
/// position is open is a victim; the first attacker swap in the opposite
/// direction after ≥1 victim is the back-run.
fn classify_sandwiches(input: &BlockInput) -> Vec<MevEvent> {
    // Flatten every successful tx's resolved-direction swaps in block order.
    let mut by_pool: HashMap<Address, Vec<(u64, Address, SwapLeg)>> = HashMap::new();
    for tx in &input.txs {
        if !tx.success {
            continue;
        }
        for s in &tx.swaps {
            if has_unresolved_tokens(s) {
                continue; // unresolved direction cannot anchor a leg
            }
            by_pool.entry(s.pool).or_default().push((
                tx.tx_index,
                tx.from,
                SwapLeg {
                    tx_index: tx.tx_index,
                    tx_hash: tx.tx_hash,
                    token_in: s.token_in,
                    token_out: s.token_out,
                    amount_in: s.amount_in,
                    amount_out: s.amount_out,
                },
            ));
        }
    }

    let mut out = Vec::new();
    for (pool, pool_swaps) in by_pool {
        // Candidate attackers on the pool (block order preserved in the
        // outer loop; per-pool walks are internally tx-index ordered).
        let mut attackers: Vec<Address> = pool_swaps.iter().map(|(_, f, _)| *f).collect();
        attackers.sort();
        attackers.dedup();

        for attacker in attackers {
            let mut front: Option<&SwapLeg> = None;
            let mut walk = SandwichWalk::default();
            for (_, from, leg) in &pool_swaps {
                if *from == attacker {
                    if front.is_none() {
                        front = Some(leg); // position open
                    } else if !walk.victims.is_empty() {
                        let Some(f) = front else {
                            continue;
                        };
                        if opposite_dir(f, leg) {
                            // First opposite-direction attacker swap after a
                            // victim closes the sandwich.
                            out.push(fold_sandwich(input, pool, attacker, f, leg, &walk));
                            break;
                        }
                    }
                    // Same-direction or pre-victim opposite swaps keep the
                    // position open; only the first back-run closes it.
                } else if let Some(f) = front {
                    if same_dir(f, leg) {
                        // Third-party swap in the same direction: the victim.
                        walk.victims.push(leg.clone());
                    }
                }
            }
        }
    }
    out
}

fn fold_sandwich(
    input: &BlockInput,
    pool: Address,
    attacker: Address,
    front: &SwapLeg,
    back: &SwapLeg,
    walk: &SandwichWalk,
) -> MevEvent {
    // Profit = back-run output − front-run input, netted in the profit token
    // (the token both legs trade against).
    let profit = back.amount_out.saturating_sub(front.amount_in);
    // Sum gas across the front-run and back-run txs (Phase 1.4).
    let mut gas_cost_wei = U256::ZERO;
    let mut seen = std::collections::HashSet::new();
    let mut front_to: Option<Address> = None;
    let mut back_to: Option<Address> = None;
    for t in &input.txs {
        if t.tx_index == front.tx_index {
            front_to = t.to;
        }
        if t.tx_index == back.tx_index {
            back_to = t.to;
        }
        if (t.tx_index == front.tx_index || t.tx_index == back.tx_index) && seen.insert(t.tx_index)
        {
            gas_cost_wei = gas_cost_wei.saturating_add(
                U256::from(t.gas_used)
                    .saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128)),
            );
        }
    }
    // Contract-mediated legs (logs-only): either sandwich leg targets a
    // contract (`tx.to`). Store-backed labeled-searcher enrichment is out of
    // scope here — classify stays store-free.
    let contract_mediated = front_to.is_some() || back_to.is_some();
    let contract = front_to.or(back_to);

    // Logs-only victim degradation vs the front-run execution price
    // (`amount_out/amount_in`). Evidence only — never a classification gate.
    let price = |l: &SwapLeg| -> Option<f64> {
        if l.amount_in.is_zero() {
            return None;
        }
        Some(
            crate::explorer::pricing::u256_to_f64(l.amount_out)
                / crate::explorer::pricing::u256_to_f64(l.amount_in),
        )
    };
    let front_price = price(front);
    let victim_price = walk.victims.last().and_then(price);
    let degradation_pct = match (front_price, victim_price) {
        (Some(f), Some(v)) if f > 0.0 => Some((f - v) / f * 100.0),
        _ => None,
    };

    let reverse_backrun = opposite_dir(front, back);
    let mut reasons: Vec<&str> = Vec::new();
    if reverse_backrun {
        reasons.push("REVERSE_DIRECTION");
    }
    reasons.push("SAME_SEARCHER");
    if degradation_pct.is_some_and(|d| d > 0.0) {
        reasons.push("VICTIM_EXECUTION_DEGRADED");
    }
    if contract_mediated {
        reasons.push("CONTRACT_MEDIATED");
    }

    MevEvent {
        block: input.block,
        ts: input.ts,
        tx_index: front.tx_index, // bundle anchor = front-run
        tx_hash: front.tx_hash,
        kind: MevKind::Sandwich,
        searcher: attacker,
        contract,
        pools: vec![pool],
        profit_token: Some(front.token_in),
        profit_amount: Some(profit),
        profit_tokens: vec![],
        profit_usd: None,
        gas_cost_wei,
        flashloan_fee_wei: None,
        flashloan_fee_token: None,
        confidence: Confidence::Exact,
        victim_hashes: walk.victims.iter().map(|v| v.tx_hash).collect(),
        victim_swap_size: walk.victims.last().map(|v| v.amount_in),
        details: serde_json::json!({
            "mode": "realized",
            "pool": format!("{pool:#x}"),
            "front_run": {
                "tx_index": front.tx_index,
                "tx_hash": format!("{:#x}", front.tx_hash),
                "amount_in": front.amount_in.to_string(),
                "amount_out": front.amount_out.to_string(),
                "to": front_to.map(|a| format!("{a:#x}")),
            },
            "back_run": {
                "tx_index": back.tx_index,
                "tx_hash": format!("{:#x}", back.tx_hash),
                "amount_in": back.amount_in.to_string(),
                "amount_out": back.amount_out.to_string(),
                "to": back_to.map(|a| format!("{a:#x}")),
            },
            "backrun_tx_index": back.tx_index,
            "victims_seen": walk.victims.len(),
            "victim_hashes": walk
                .victims
                .iter()
                .map(|v| format!("{:#x}", v.tx_hash))
                .collect::<Vec<_>>(),
            "victim_swap_sizes": walk
                .victims
                .iter()
                .map(|v| v.amount_in.to_string())
                .collect::<Vec<_>>(),
            "evidence": {
                "same_pool": true,
                "direction_match": true,
                "reverse_backrun": reverse_backrun,
                "same_searcher": true,
                "contract_mediated": contract_mediated,
            },
            "victim_execution": {
                "front_price": front_price,
                "victim_price": victim_price,
                "degradation_pct": degradation_pct,
            },
            "reasons": reasons,
        }),
    }
}

/// Max tx-index distance that still counts as "adjacent" for causal
/// backrun/frontrun passes (§24 / §13). Blocks are sorted; a window of 8
/// keeps the pair causal without spanning unrelated traffic.
const CAUSAL_WINDOW_TXS: u64 = 8;

/// Execution price of a leg: output per input (same-direction comparison only).
fn exec_price(leg: &SwapLeg) -> Option<f64> {
    if leg.amount_in.is_zero() {
        return None;
    }
    Some(
        crate::explorer::pricing::u256_to_f64(leg.amount_out)
            / crate::explorer::pricing::u256_to_f64(leg.amount_in),
    )
}

/// True when `b`'s execution price is measurably better than `r`'s, both
/// trading the same direction on the same pool. `min_pct` is the minimum
/// relative improvement that counts as material (guards float noise).
fn price_better_by(b: &SwapLeg, r: &SwapLeg, min_pct: f64) -> bool {
    match (exec_price(b), exec_price(r)) {
        (Some(pb), Some(pr)) if pr > 0.0 => (pb - pr) / pr * 100.0 > min_pct,
        _ => false,
    }
}

/// True when `v`'s execution price is measurably worse than `f`'s (both same
/// direction): the observable victim-execution degradation.
fn price_worse_by(v: &SwapLeg, f: &SwapLeg, min_pct: f64) -> bool {
    match (exec_price(v), exec_price(f)) {
        (Some(pv), Some(pf)) if pf > 0.0 => (pf - pv) / pf * 100.0 > min_pct,
        _ => false,
    }
}

/// Flatten every successful tx's direction-resolved swaps in block order,
/// grouped per pool. Used by the causal backrun/frontrun passes.
fn pool_legs(input: &BlockInput) -> HashMap<Address, Vec<(u64, Address, SwapLeg)>> {
    let mut by_pool: HashMap<Address, Vec<(u64, Address, SwapLeg)>> = HashMap::new();
    for tx in &input.txs {
        if !tx.success {
            continue;
        }
        for s in &tx.swaps {
            if has_unresolved_tokens(s) {
                continue; // unresolved direction cannot anchor a leg
            }
            by_pool.entry(s.pool).or_default().push((
                tx.tx_index,
                tx.from,
                SwapLeg {
                    tx_index: tx.tx_index,
                    tx_hash: tx.tx_hash,
                    token_in: s.token_in,
                    token_out: s.token_out,
                    amount_in: s.amount_in,
                    amount_out: s.amount_out,
                },
            ));
        }
    }
    for legs in by_pool.values_mut() {
        legs.sort_by_key(|(i, _, _)| *i);
    }
    by_pool
}

/// Logs-only closed-cycle profit of a tx, mirroring the arb pass: the sender
/// (or a qualified participant) nets a positive delta in a priority token.
fn tx_cycle_profit(input: &BlockInput, tx: &TxInput) -> Option<(Address, Address, U256)> {
    let ledger =
        DeltaLedger::from_transfers(&tx.transfers, input.wrapped_native, (tx.from, tx.value));
    let mut candidates: Vec<Address> = vec![tx.from];
    if let Some(to) = tx.to {
        candidates.push(to);
    }
    let mut participation: HashMap<Address, usize> = HashMap::new();
    for t in &tx.transfers {
        *participation.entry(t.from).or_insert(0) += 1;
        *participation.entry(t.to).or_insert(0) += 1;
    }
    for (addr, count) in participation {
        if count >= 2 && !candidates.contains(&addr) {
            candidates.push(addr);
        }
    }
    let resolved: Vec<(Address, Address, _)> = tx
        .swaps
        .iter()
        .filter(|s| {
            !has_unresolved_tokens(s)
                && s.token_in != decode::TOKEN0_SENTINEL
                && s.token_in != decode::TOKEN1_SENTINEL
        })
        .map(|s| (s.token_in, s.token_out, s.pool))
        .collect();
    if resolved.len() < 2
        || !has_closed_cycle(&resolved)
        || !flow_attributable(&tx.swaps, &candidates)
    {
        return None; // "closed cycle; else rejected" (§24) + flow ownership (§7.1)
    }
    for searcher in candidates {
        if let Some(token) = select_profit_token(&ledger, searcher, &input.profit_policy) {
            let amount = ledger.net(searcher, token);
            if !amount.is_zero() {
                return Some((searcher, token, amount));
            }
        }
    }
    None
}

/// Phase 3.1: logs-only causal backrun. A market-moving tx `A` (large notional,
/// different sender) on pool `P` is followed, within the adjacency window, by a
/// tx `B` whose sender profitably closes a directed cycle whose leg on `P`
/// trades *opposite* A's direction at a better execution price than the pool
/// offered pre-move. Requiring pool overlap + direction-of-benefit + closed
/// cycle; nothing else qualifies (§24). Never Exact — this is a logs-only
/// proxy, not REVM `profit(B|after)`.
fn classify_backruns(input: &BlockInput, consumed: &HashSet<u64>) -> Vec<MevEvent> {
    let by_pool = pool_legs(input);
    let txs_by_index: HashMap<u64, &TxInput> = input.txs.iter().map(|t| (t.tx_index, t)).collect();
    let mut out: Vec<MevEvent> = Vec::new();
    // The backrun leg can only anchor the backrun tx once **per pool**; a tx
    // can swap on several pools, so pool must be part of the claim key.
    let mut claimed: HashSet<(Address, u64)> = HashSet::new();

    for (pool, legs) in by_pool {
        for (bi, (btx, bfrom, bleg)) in legs.iter().enumerate() {
            if consumed.contains(btx) || !claimed.insert((pool, *btx)) {
                continue;
            }
            let Some(b_tx) = txs_by_index.get(btx).copied() else {
                continue;
            };
            if b_tx.from != *bfrom {
                continue;
            }
            let Some(_) = tx_cycle_profit(input, b_tx) else {
                continue; // must close a profitable cycle
            };
            // Nearest prior leg on the same pool in the opposite direction,
            // within the adjacency window, from a different sender.
            let mut move_: Option<(&(u64, Address, SwapLeg), &SwapLeg)> = None;
            for (ai, (atx, afrom, aleg)) in legs.iter().enumerate().take(bi).rev() {
                if *atx >= *btx || *btx - *atx > CAUSAL_WINDOW_TXS {
                    continue;
                }
                if *afrom == *bfrom {
                    continue;
                }
                if !opposite_dir(aleg, bleg) {
                    continue;
                }
                // Large-notional market move: A at least as large as B.
                if aleg.amount_in < bleg.amount_in {
                    continue;
                }
                // Pre-move price reference: nearest earlier same-direction leg.
                let ref_ = legs[..ai]
                    .iter()
                    .rev()
                    .find(|(_, f, l)| *f != *afrom && same_dir(l, bleg));
                if let Some((_, _, ref_leg)) = ref_ {
                    if price_better_by(bleg, ref_leg, 0.5) {
                        move_ = Some((&legs[ai], ref_leg));
                        break;
                    }
                }
            }
            let Some(a) = move_ else {
                continue;
            };
            let atx = a.0 .0;
            let afrom = a.0 .1;
            let aleg = &a.0 .2;
            let a_hash = a.0 .2.tx_hash;
            let (btoken, bamount) = {
                let (_searcher, token, amount) = tx_cycle_profit(input, b_tx).unwrap();
                (token, amount)
            };
            out.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: *btx,
                tx_hash: b_tx.tx_hash,
                kind: MevKind::Backrun,
                searcher: *bfrom,
                contract: None,
                pools: vec![pool],
                profit_token: Some(btoken),
                profit_amount: Some(bamount),
                profit_tokens: vec![],
                profit_usd: None,
                gas_cost_wei: U256::from(b_tx.gas_used)
                    .saturating_mul(U256::from((b_tx.effective_gas_price_gwei * 1e9) as u128)),
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Inferred,
                victim_hashes: vec![a_hash],
                victim_swap_size: None,
                details: serde_json::json!({
                    "mode": "realized",
                    "tier": "inferred",
                    "pool": format!("{pool:#x}"),
                    "source_tx_index": atx,
                    "source_sender": format!("{afrom:#x}"),
                    "move": {
                        "tx_index": atx,
                        "direction": format!("{:#x}->{:#x}", aleg.token_in, aleg.token_out),
                        "amount_in": aleg.amount_in.to_string(),
                        "amount_out": aleg.amount_out.to_string(),
                    },
                    "evidence": {
                        "STATE_DELTA_MATCH": true,
                        "DIRECTION_OF_BENEFIT": opposite_dir(aleg, bleg),
                        "PROFIT_VERIFIED": true,
                        "exec_better_than_pre_move": price_better_by(bleg, a.1, 0.5),
                        "cycle": true,
                        "supporting_tx_hashes": [format!("{a_hash:#x}")],
                    },
                    "ledger": {
                        "searcher": format!("{bfrom:#x}"),
                        "profit_token": format!("{btoken:#x}"),
                        "profit_amount": bamount.to_string(),
                    },
                    "reasons": ["STATE_DELTA_MATCH", "PROFIT_VERIFIED", "DIRECT_TOKEN_CYCLE"],
                }),
            });
        }
    }
    out
}

/// Phase 3.2: logs-only causal frontrun. A searcher tx `F` moves pool `P`
/// (swap measurable from amounts); a third-party tx `V` immediately after
/// swaps the same direction at a degraded execution price vs `F`; and `F`'s
/// sender then closes the position with a profitable opposite leg on `P`
/// (PROFIT_VERIFIED). Sandwich-consumed front/back txs are excluded by
/// `consumed` (§13 / kind priority).
fn classify_frontruns(input: &BlockInput, consumed: &HashSet<u64>) -> Vec<MevEvent> {
    let by_pool = pool_legs(input);
    let txs_by_index: HashMap<u64, &TxInput> = input.txs.iter().map(|t| (t.tx_index, t)).collect();
    // Global, tx-ordered leg index so the searcher's profitable opposite close
    // can land on a different pool than the victim's (§13: "subsequent
    // profitable opposite leg" — no same-pool requirement). A same-pool close
    // after a third-party victim is, by construction, also a commodity
    // sandwich; the sandwich pass owns that case (front/back anchors land in
    // `consumed` via kind priority).
    let mut all: Vec<(u64, Address, SwapLeg)> = by_pool
        .values()
        .flat_map(|legs| legs.iter().map(|(tx, from, leg)| (*tx, *from, leg.clone())))
        .collect();
    all.sort_by_key(|(tx, _, _)| *tx);
    let mut out: Vec<MevEvent> = Vec::new();
    let mut claimed: HashSet<(Address, u64, u64)> = HashSet::new();

    for (pool, legs) in by_pool {
        for (fi, (ftx, ffrom, fleg)) in legs.iter().enumerate() {
            if consumed.contains(ftx) {
                continue;
            }
            // Victim: next third-party same-direction swap, adjacent window.
            let mut victim_: Option<(u64, Address, &SwapLeg)> = None;
            for (vt, vfrom, vleg) in legs.iter().skip(fi + 1) {
                if *vt - *ftx == 0 || *vt - *ftx > CAUSAL_WINDOW_TXS {
                    continue;
                }
                if *vfrom == *ffrom {
                    continue;
                }
                if !same_dir(vleg, fleg) {
                    continue;
                }
                if price_worse_by(vleg, fleg, 0.5) {
                    victim_ = Some((*vt, *vfrom, vleg));
                    break;
                }
            }
            let Some((vtx, vfrom, vleg)) = victim_ else {
                continue;
            };
            if claimed.contains(&(pool, *ftx, vtx)) {
                continue;
            }
            // Searcher's profitable opposite leg after the victim
            // (PROFIT_VERIFIED), on any pool the searcher trades.
            let close = all.iter().find(|(ctx, cfrom, cleg)| {
                *ctx >= vtx
                    && *cfrom == *ffrom
                    && opposite_dir(fleg, cleg)
                    && cleg.amount_out > fleg.amount_in
            });
            let Some((_, _, cleg)) = close else {
                continue;
            };
            let profit = cleg.amount_out.saturating_sub(fleg.amount_in);
            if profit.is_zero() || !claimed.insert((pool, *ftx, vtx)) {
                continue;
            }
            let ftx_data = txs_by_index.get(ftx).copied();
            let gas_cost_wei = ftx_data.map_or(U256::ZERO, |t| {
                U256::from(t.gas_used)
                    .saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128))
            });
            out.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: *ftx,
                tx_hash: fleg.tx_hash,
                kind: MevKind::Frontrun,
                searcher: *ffrom,
                contract: None,
                pools: vec![pool],
                profit_token: Some(fleg.token_in),
                profit_amount: Some(profit),
                profit_tokens: vec![],
                profit_usd: None,
                gas_cost_wei,
                flashloan_fee_wei: None,
                flashloan_fee_token: None,
                confidence: Confidence::Inferred,
                victim_hashes: vec![vleg.tx_hash],
                victim_swap_size: Some(vleg.amount_in),
                details: serde_json::json!({
                    "mode": "realized",
                    "tier": "inferred",
                    "pool": format!("{pool:#x}"),
                    "victim_tx_index": vtx,
                    "front_run": {
                        "tx_index": ftx,
                        "tx_hash": format!("{:#x}", fleg.tx_hash),
                        "amount_in": fleg.amount_in.to_string(),
                        "amount_out": fleg.amount_out.to_string(),
                    },
                    "victim": {
                        "tx_index": vtx,
                        "sender": format!("{vfrom:#x}"),
                        "amount_in": vleg.amount_in.to_string(),
                        "amount_out": vleg.amount_out.to_string(),
                    },
                    "evidence": {
                        "STATE_DELTA_MATCH": true,
                        "VICTIM_EXECUTION_DEGRADED": true,
                        "PROFIT_VERIFIED": true,
                        "supporting_tx_hashes": [format!("{:#x}", vleg.tx_hash)],
                    },
                    "reasons": ["STATE_DELTA_MATCH", "VICTIM_EXECUTION_DEGRADED", "PROFIT_VERIFIED"],
                }),
            });
        }
    }
    out
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

/// JIT pairing: same pool, same owner, same tick range, Mint before Burn.
///
/// Phase 1.5: pairs an in-block Mint+Burn *or* a prior-block open position
/// (loaded from `jit_open_positions`) with an in-block exact-liquidity Burn.
/// A candidate is only emitted when the block contains an in-window swap in
/// that pool whose tick lies inside the range (tick-overlap validation).
fn classify_jit(input: &BlockInput) -> Vec<MevEvent> {
    let mints: Vec<JitFact> = input
        .txs
        .iter()
        .flat_map(|t| {
            t.jit.iter().map(move |j| {
                let mut j2 = j.clone();
                j2.tx_index = t.tx_index;
                j2
            })
        })
        .filter(|j| j.is_mint)
        .collect();
    let burns: Vec<(u64, &JitFact)> = input
        .txs
        .iter()
        .flat_map(|t| t.jit.iter().map(move |j| (t.tx_index, j)))
        .filter(|(_, j)| !j.is_mint)
        .collect();

    let mut out = Vec::new();

    // Same-block Mint → Burn.
    for mint in &mints {
        let burn = burns.iter().find(|(_, b)| {
            b.pool == mint.pool
                && b.owner == mint.owner
                && b.tick_lower == mint.tick_lower
                && b.tick_upper == mint.tick_upper
                && b.liquidity >= mint.liquidity
        });
        if let Some((burn_tx, _)) = burn {
            if let Some(ev) = build_jit_event(
                input,
                mint.pool,
                mint.owner,
                mint.tick_lower,
                mint.tick_upper,
                mint.tx_index,
                mint.liquidity,
                mint.amount0,
                mint.amount1,
                *burn_tx,
                input.block,
                mint.bin_amm,
            ) {
                out.push(ev);
            }
        }
    }

    // Cross-block: a prior-block open position closed by an in-block Burn.
    for pos in &input.open_positions {
        let burn = burns.iter().find(|(_, b)| {
            b.pool == pos.pool
                && b.owner == pos.owner
                && b.tick_lower == pos.tick_lower
                && b.tick_upper == pos.tick_upper
                && b.liquidity >= pos.liquidity
        });
        if let Some((burn_tx, b)) = burn {
            // Anchor to the burn tx when the opening tx is not in this block.
            if let Some(ev) = build_jit_event(
                input,
                pos.pool,
                pos.owner,
                pos.tick_lower,
                pos.tick_upper,
                *burn_tx,
                pos.liquidity,
                U256::ZERO,
                U256::ZERO,
                *burn_tx,
                pos.opened_block,
                b.bin_amm,
            ) {
                out.push(ev);
            }
        }
    }

    out
}

/// Build one JIT event, or `None` when no in-window swap validates the range.
#[allow(clippy::too_many_arguments)]
fn build_jit_event(
    input: &BlockInput,
    pool: Address,
    owner: Address,
    tick_lower: i32,
    tick_upper: i32,
    mint_tx: u64,
    liquidity: u128,
    amount0: U256,
    amount1: U256,
    burn_tx: u64,
    opened_block: u64,
    bin_amm: bool,
) -> Option<MevEvent> {
    // V3: require an in-range tick. LB: any same-pool swap (Swap has no tick).
    let in_range = |s: &SwapFact| {
        if s.pool != pool {
            return false;
        }
        if bin_amm {
            return true;
        }
        s.tick.is_some_and(|k| k >= tick_lower && k <= tick_upper)
    };
    if !input.txs.iter().flat_map(|t| &t.swaps).any(in_range) {
        return None;
    }

    let gas_cost_wei = input
        .txs
        .iter()
        .filter(|t| t.tx_index == mint_tx)
        .map(|t| {
            U256::from(t.gas_used)
                .saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128))
        })
        .next()
        .unwrap_or(U256::ZERO);

    // Documented estimate: in-range swap volume × pool fee (30 bps default).
    // V3 fees are not in logs and principal amounts are not fees, so this is
    // explicitly inferred (`Confidence::Exact` remains on the detection).
    const POOL_FEE_BPS: u32 = 30;
    let mut by_token: std::collections::BTreeMap<String, U256> = std::collections::BTreeMap::new();
    for t in &input.txs {
        for s in &t.swaps {
            if in_range(s) {
                let fee =
                    s.amount_in.saturating_mul(U256::from(POOL_FEE_BPS)) / U256::from(10_000u32);
                *by_token.entry(format!("{:#x}", s.token_in)).or_default() += fee;
            }
        }
    }
    let fees_estimated: serde_json::Map<String, serde_json::Value> = by_token
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.to_string())))
        .collect();

    let range_key = if bin_amm { "bin" } else { "tick" };
    Some(MevEvent {
        block: input.block,
        ts: input.ts,
        tx_index: mint_tx,
        tx_hash: B256::ZERO, // mint tx hash resolved at ingest stamping
        kind: MevKind::Jit,
        searcher: owner,
        contract: None,
        pools: vec![pool],
        profit_token: None, // fee capture — priced via pool tokens
        profit_amount: None,
        profit_tokens: vec![],
        profit_usd: None,
        gas_cost_wei,
        flashloan_fee_wei: None,
        flashloan_fee_token: None,
        confidence: Confidence::Exact,
        victim_hashes: vec![],
        victim_swap_size: None,
        details: serde_json::json!({
            "pool": format!("{:#x}", pool),
            "owner": format!("{:#x}", owner),
            "tick_lower": tick_lower,
            "tick_upper": tick_upper,
            "liquidity": liquidity,
            "burn_tx_index": burn_tx,
            "opened_block": opened_block,
            "held_blocks": input.block.saturating_sub(opened_block),
            "amount0": amount0.to_string(),
            "amount1": amount1.to_string(),
            "bin_amm": bin_amm,
            "mode": "realized",
            "reasons": if bin_amm {
                serde_json::json!(["SHORT_LP_LIFETIME", "SAME_POOL_SWAP_DURING_LB_POSITION"])
            } else {
                serde_json::json!(["SHORT_LP_LIFETIME", "OVERLAPPING_TICK_RANGE"])
            },
            "fees_estimated": {
                "method": "in_range_volume_x_pool_fee",
                "pool_fee_bps": POOL_FEE_BPS,
                "confidence": "inferred",
                "range": range_key,
                "by_token": fees_estimated,
            },
        }),
    })
}

/// Stamp tx hashes onto JIT events (mint tx lookup happens at ingest where
/// hashes are available in the same loop as decoding).
pub fn stamp_jit_tx_hashes(events: &mut [MevEvent], tx_hashes: &HashMap<u64, B256>) {
    for ev in events.iter_mut() {
        if ev.kind == MevKind::Jit && ev.tx_hash == B256::ZERO {
            if let Some(h) = tx_hashes.get(&ev.tx_index) {
                ev.tx_hash = *h;
            }
        }
    }
}

/// Decoded facts for one transaction: (transfers, swaps, liquidations,
/// flash loans, JIT).
pub type TxFacts = (
    Vec<TransferFact>,
    Vec<SwapFact>,
    Vec<crate::explorer::types::LiquidationFact>,
    Vec<crate::explorer::types::FlashLoanFact>,
    Vec<JitFact>,
);

/// Convenience: decode a tx's receipt logs into facts (used by ingest).
/// `pool_tokens` = pool → (token0, token1) registry for swap direction (1.1).
pub fn decode_tx_logs(
    tx_index: u64,
    logs: &[crate::data::LogData],
    pool_tokens: &HashMap<Address, (Address, Address)>,
) -> TxFacts {
    let mut transfers = Vec::new();
    let mut swaps = Vec::new();
    let mut liquidations = Vec::new();
    let mut flashloans = Vec::new();
    let mut jit = Vec::new();
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
            liquidations.push(l);
        }
        if let Some(mut f) = decode::decode_flash_loan(log) {
            f.tx_index = tx_index;
            f.log_index = log_idx;
            flashloans.push(f);
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
    }
    decode::attach_swap_tokens(&mut swaps, &transfers, pool_tokens);
    // Phase 1.6: drop aggregator edges already covered by DEX swap edges.
    decode::dedup_aggregator_facts(&mut swaps);
    (transfers, swaps, liquidations, flashloans, jit)
}

/// True when a swap fact has unresolved direction tokens (V3 sentinel kept).
pub fn has_unresolved_tokens(s: &SwapFact) -> bool {
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
fn flow_attributable(swaps: &[SwapFact], candidates: &[Address]) -> bool {
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
pub fn filter_unresolved(events: Vec<MevEvent>) -> Vec<MevEvent> {
    events
        .into_iter()
        .filter(|e| {
            if e.kind != MevKind::Unknown {
                return true;
            }
            e.profit_amount.map(|a| !a.is_zero()).unwrap_or(false)
        })
        .collect()
}

/// Mark whether any swap in the event had V3 sentinel direction (audit info).
pub fn route_uses_sentinel(swaps: &[SwapFact]) -> bool {
    swaps.iter().any(has_unresolved_tokens)
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
    use crate::explorer::types::{Amm, FlashLoanFact, LiquidationFact, MevKind};
    use alloy::primitives::{address, b256, U256};

    const ATK: Address = address!("1000000000000000000000000000000000000001");
    const VICTIM: Address = address!("2000000000000000000000000000000000000002");
    const POOL_A: Address = address!("3000000000000000000000000000000000000003");
    const POOL_B: Address = address!("3000000000000000000000000000000000000004");
    const USDC: Address = address!("4000000000000000000000000000000000000005");
    const TOKA: Address = address!("5000000000000000000000000000000000000006");
    const WNATIVE: Address = address!("6000000000000000000000000000000000000007");

    fn swap(pool: Address, tin: Address, tout: Address, ain: u64, aout: u64) -> SwapFact {
        SwapFact {
            tx_index: 0,
            log_index: 0,
            pool,
            amm: Amm::V2,
            token_in: tin,
            token_out: tout,
            amount_in: U256::from(ain),
            amount_out: U256::from(aout),
            tick: None,
            owner: None,
        }
    }

    fn swap_owned(
        owner: Address,
        pool: Address,
        tin: Address,
        tout: Address,
        ain: u64,
        aout: u64,
    ) -> SwapFact {
        SwapFact {
            owner: Some(owner),
            ..swap(pool, tin, tout, ain, aout)
        }
    }

    fn swap_at_tick(
        pool: Address,
        tin: Address,
        tout: Address,
        ain: u64,
        aout: u64,
        tick: i32,
    ) -> SwapFact {
        let mut s = swap(pool, tin, tout, ain, aout);
        s.amm = Amm::V3;
        s.tick = Some(tick);
        s
    }

    fn transfer(
        log_idx: u64,
        token: Address,
        from: Address,
        to: Address,
        amt: u64,
    ) -> TransferFact {
        TransferFact {
            tx_index: 0,
            log_index: log_idx,
            token,
            from,
            to,
            amount: U256::from(amt),
        }
    }

    fn tx(
        idx: u64,
        from: Address,
        success: bool,
        swaps: Vec<SwapFact>,
        transfers: Vec<TransferFact>,
    ) -> TxInput {
        TxInput {
            tx_index: idx,
            tx_hash: B256::repeat_byte(idx as u8),
            from,
            to: None,
            success,
            gas_used: 100_000,
            effective_gas_price_gwei: 30.0,
            value: U256::ZERO,
            transfers,
            swaps,
            liquidations: vec![],
            flashloans: vec![],
            jit: vec![],
        }
    }

    fn block(txs: Vec<TxInput>) -> BlockInput {
        BlockInput {
            block: 12345,
            ts: 1_700_000_000,
            wrapped_native: WNATIVE,
            profit_policy: ProfitTokenPolicy {
                priority: vec![USDC],
                wrapped_native: WNATIVE,
                weth: WNATIVE,
            },
            arb_likely_parity: true,
            open_positions: vec![],
            txs,
        }
    }

    fn event_of(events: &[MevEvent], kind: MevKind) -> &MevEvent {
        events.iter().find(|e| e.kind == kind).unwrap_or_else(|| {
            panic!(
                "expected a {:?} event; got {:?}",
                kind,
                events.iter().map(|e| e.kind).collect::<Vec<_>>()
            )
        })
    }

    fn kinds(events: &[MevEvent]) -> Vec<MevKind> {
        let mut v: Vec<MevKind> = events.iter().map(|e| e.kind).collect();
        v.sort_by_key(|k| k.as_str());
        v
    }

    fn classify_kind(input: &BlockInput, kind: MevKind) -> MevEvent {
        event_of(&classify_block(input), kind).clone()
    }

    #[test]
    fn atomic_arb_two_pool_closed_cycle() {
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap(POOL_B, TOKA, USDC, 200, 110),
        ];
        // attacker pays 100 USDC on pool A, sells the 200 TOKA on pool B for 110 USDC.
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        assert_eq!(ev.searcher, ATK);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(10)));
        assert_eq!(ev.confidence, Confidence::Exact);
        assert_eq!(ev.pools, vec![POOL_A, POOL_B]);
        assert_eq!(ev.tx_index, 0);
    }

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
        assert_eq!(ev.confidence, Confidence::Inferred, "cycle mixes unrelated owners");
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
        assert_eq!(ev.confidence, Confidence::Inferred, "flow owner is not a candidate");
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
    fn two_entity_cycle_does_not_emit() {
        // ATK ↔ pool round trip is only a 2-entity cycle; with parity off the
        // profitable residual is not MEV and must not be staged as Unknown.
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
            "2-entity cycle must not emit, got {events:?}"
        );
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
    fn flash_loan_principal_is_netted_when_repay_missing() {
        let provider = address!("7000000000000000000000000000000000000007");
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 1000, 1000),
            swap(POOL_B, TOKA, USDC, 1000, 1010),
        ];
        let transfers = vec![
            transfer(0, USDC, provider, ATK, 1000), // borrow (repay leg absent)
            transfer(1, USDC, ATK, POOL_A, 1000),
            transfer(2, TOKA, POOL_A, ATK, 1000),
            transfer(3, TOKA, ATK, POOL_B, 1000),
            transfer(4, USDC, POOL_B, ATK, 1010),
        ];
        let mut t = tx(0, ATK, true, swaps, transfers);
        t.flashloans = vec![FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: ATK,
            token: USDC,
            amount: U256::from(1000),
            fee: Some(U256::from(5)),
            recipient: ATK,
            provider,
        }];
        let ev = classify_kind(&block(vec![t]), MevKind::ArbAtomic);
        // 1010 sold − 1000 bought − 5 fee = 5; the 1000 principal is stripped.
        assert_eq!(ev.profit_amount, Some(U256::from(5)));
        assert_eq!(ev.flashloan_fee_wei, Some(U256::from(5)));
    }

    #[test]
    fn flash_loan_repay_present_keeps_profit() {
        let provider = address!("7000000000000000000000000000000000000007");
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 1000, 1000),
            swap(POOL_B, TOKA, USDC, 1000, 1010),
        ];
        let transfers = vec![
            transfer(0, USDC, provider, ATK, 1000), // borrow
            transfer(1, USDC, ATK, POOL_A, 1000),
            transfer(2, TOKA, POOL_A, ATK, 1000),
            transfer(3, TOKA, ATK, POOL_B, 1000),
            transfer(4, USDC, POOL_B, ATK, 1010),
            transfer(5, USDC, ATK, provider, 1005), // repay principal + fee
        ];
        let mut t = tx(0, ATK, true, swaps, transfers);
        t.flashloans = vec![FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: ATK,
            token: USDC,
            amount: U256::from(1000),
            fee: Some(U256::from(5)),
            recipient: ATK,
            provider,
        }];
        let ev = classify_kind(&block(vec![t]), MevKind::ArbAtomic);
        // Repay already nets the principal; don't subtract it twice.
        assert_eq!(ev.profit_amount, Some(U256::from(5)));
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
    fn causal_backrun_detected_after_market_move() {
        // tx0: pre-move reference — TOKA sold on POOL_A at ~1.1 USDC/TOKA.
        let r = tx(
            0,
            VICTIM,
            true,
            vec![swap(POOL_A, TOKA, USDC, 100, 110)],
            vec![
                transfer(0, TOKA, VICTIM, POOL_A, 100),
                transfer(1, USDC, POOL_A, VICTIM, 110),
            ],
        );
        // tx1: market move — large USDC→TOKA buy on POOL_A (TOKA price up).
        let a = tx(
            1,
            MARKET,
            true,
            vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
            vec![
                transfer(0, USDC, MARKET, POOL_A, 1000),
                transfer(1, TOKA, POOL_A, MARKET, 100),
            ],
        );
        // tx2: backrunner sells TOKA (1.2 vs pre-move 1.1) and re-buys on
        // POOL_B, closing a cycle — a different sender, adjacent to the move.
        let b = tx(
            2,
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
        );
        let input = block(vec![r, a, b]);
        let events = classify_block(&input);
        let ev = event_of(&events, MevKind::Backrun);
        assert_eq!(ev.searcher, ATK);
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(ev.tx_index, 2);
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
        // Precedence: the profitable closed-cycle tx keeps Backrun, so the
        // ArbAtomic claim on tx2 is superseded (no double-counted P&L).
        assert_eq!(
            events.iter().filter(|e| e.kind == MevKind::Backrun).count(),
            1
        );
        assert!(!events
            .iter()
            .any(|e| e.kind == MevKind::ArbAtomic && e.tx_index == 2));
        assert_eq!(ev.victim_hashes, vec![B256::repeat_byte(1)]);
    }

    #[test]
    fn backrun_rejected_without_pre_move_reference() {
        // Only the market move + the would-be backrun; without a pre-move
        // reference price, causality can't be proven (§24).
        let a = tx(
            0,
            MARKET,
            true,
            vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
            vec![
                transfer(0, USDC, MARKET, POOL_A, 1000),
                transfer(1, TOKA, POOL_A, MARKET, 100),
            ],
        );
        let b = tx(
            1,
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
        );
        let input = block(vec![a, b]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Backrun));
    }

    #[test]
    fn backrun_rejected_when_move_and_backrun_same_sender() {
        let r = tx(
            0,
            VICTIM,
            true,
            vec![swap(POOL_A, TOKA, USDC, 100, 110)],
            vec![
                transfer(0, TOKA, VICTIM, POOL_A, 100),
                transfer(1, USDC, POOL_A, VICTIM, 110),
            ],
        );
        // Market move by ATK (= backrunner's sender): not a causal backrun.
        let a = tx(
            1,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 500, 100)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 500),
                transfer(1, TOKA, POOL_A, ATK, 100),
            ],
        );
        let b = tx(
            2,
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
        );
        let input = block(vec![r, a, b]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Backrun));
    }

    #[test]
    fn causal_frontrun_detected_with_cross_pool_close() {
        // tx0: searcher moves POOL_A (USDC→TOKA at 2.0).
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
        // tx1: victim same-direction, degraded (1.5 vs 2.0).
        let v = tx(
            1,
            VICTIM,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 150)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 100),
                transfer(1, TOKA, POOL_A, VICTIM, 150),
            ],
        );
        // tx2: searcher's profitable close on POOL_B (not a same-pool
        // sandwich back-run, so the sandwich pass cannot claim it).
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
        let input = block(vec![f, v, c]);
        let v_hash = B256::repeat_byte(1);
        let events = classify_block(&input);
        let ev = event_of(&events, MevKind::Frontrun);
        assert_eq!(ev.searcher, ATK);
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(ev.tx_index, 0);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(150)));
        assert_eq!(ev.victim_hashes, vec![v_hash]);
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
        // Cross-pool close: no same-pool sandwich structure exists.
        assert!(!kinds(&events).contains(&MevKind::Sandwich));
    }

    #[test]
    fn frontrun_rejected_without_measurable_degradation() {
        // tx1 executes at the same price as tx0: no degradation ⇒ not a
        // frontrun (explicit §13 negative).
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
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 100),
                transfer(1, TOKA, POOL_A, VICTIM, 200),
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
        let input = block(vec![f, v, c]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Frontrun));
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
    fn liquidation_event_detected_exact() {
        let liq = LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            user: VICTIM,
            liquidator: ATK,
            collateral_asset: USDC,
            debt_asset: WNATIVE,
            collateral_amount: U256::from(500),
            debt_to_cover: U256::from(300),
        };
        let mut t = tx(0, ATK, true, vec![], vec![]);
        t.liquidations = vec![liq];
        let input = block(vec![t]);
        let ev = classify_kind(&input, MevKind::Liquidation);
        assert_eq!(ev.searcher, ATK);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(500)));
        assert_eq!(ev.confidence, Confidence::Exact);
    }

    #[test]
    fn liquidation_transfer_reconciled() {
        let liq = LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            user: VICTIM,
            liquidator: ATK,
            collateral_asset: USDC,
            debt_asset: WNATIVE,
            collateral_amount: U256::from(500),
            debt_to_cover: U256::from(300),
        };
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 500)],
        );
        t.liquidations = vec![liq];
        let ev = classify_kind(&block(vec![t]), MevKind::Liquidation);
        assert_eq!(ev.details["reconciled"], serde_json::json!(true));
        assert_eq!(ev.details["reasons"], serde_json::json!([]));
    }

    #[test]
    fn liquidation_mismatch_records_reason() {
        // No collateral transfer captured (e.g. aToken transfer or Absorb):
        // the event is still Exact but flagged TRANSFER_MISMATCH.
        let liq = LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "compound_v3",
            user: VICTIM,
            liquidator: ATK,
            collateral_asset: Address::ZERO,
            debt_asset: Address::ZERO,
            collateral_amount: U256::ZERO,
            debt_to_cover: U256::from(300),
        };
        let mut t = tx(0, ATK, true, vec![], vec![]);
        t.liquidations = vec![liq];
        let ev = classify_kind(&block(vec![t]), MevKind::Liquidation);
        assert_eq!(ev.confidence, Confidence::Exact);
        assert_eq!(ev.details["reconciled"], serde_json::json!(false));
        assert_eq!(
            ev.details["reasons"],
            serde_json::json!(["TRANSFER_MISMATCH"])
        );
    }

    #[test]
    fn liquidation_uses_tx_sender_when_liquidator_zero() {
        let liq = LiquidationFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            user: VICTIM,
            liquidator: Address::ZERO,
            collateral_asset: USDC,
            debt_asset: WNATIVE,
            collateral_amount: U256::from(500),
            debt_to_cover: U256::from(300),
        };
        let mut t = tx(0, ATK, true, vec![], vec![]);
        t.liquidations = vec![liq];
        let input = block(vec![t]);
        let ev = classify_kind(&input, MevKind::Liquidation);
        assert_eq!(ev.searcher, ATK);
    }

    #[test]
    fn jit_mint_burn_paired_same_block() {
        let mint = JitFact {
            tx_index: 0,
            log_index: 0,
            pool: POOL_A,
            owner: ATK,
            tick_lower: -100,
            tick_upper: 100,
            is_mint: true,
            liquidity: 1000,
            amount0: U256::from(5),
            amount1: U256::from(5),
            bin_amm: false,
        };
        let burn = JitFact {
            tx_index: 1,
            log_index: 0,
            pool: POOL_A,
            owner: ATK,
            tick_lower: -100,
            tick_upper: 100,
            is_mint: false,
            liquidity: 1000,
            amount0: U256::from(5),
            amount1: U256::from(5),
            bin_amm: false,
        };
        let mut t0 = tx(
            0,
            ATK,
            true,
            vec![swap_at_tick(POOL_A, USDC, TOKA, 100, 200, 0)],
            vec![],
        );
        t0.jit = vec![mint];
        let mut t1 = tx(1, VICTIM, true, vec![], vec![]);
        t1.jit = vec![burn];
        let input = block(vec![t0, t1]);
        let ev = classify_kind(&input, MevKind::Jit);
        assert_eq!(ev.searcher, ATK);
        assert_eq!(ev.tx_index, 0); // anchored to the mint tx
                                    // burn tx index embedded in details
        assert_eq!(ev.details["burn_tx_index"], serde_json::json!(1));
        assert_eq!(ev.confidence, Confidence::Exact);
        assert_eq!(
            ev.details["fees_estimated"]["confidence"],
            serde_json::json!("inferred")
        );
    }

    #[test]
    fn jit_cross_block_open_position_closed_by_burn() {
        let burn = JitFact {
            tx_index: 0,
            log_index: 0,
            pool: POOL_A,
            owner: ATK,
            tick_lower: -100,
            tick_upper: 100,
            is_mint: false,
            liquidity: 1000,
            amount0: U256::from(5),
            amount1: U256::from(5),
            bin_amm: false,
        };
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
        let mint = JitFact {
            tx_index: 0,
            log_index: 0,
            pool: POOL_A,
            owner: ATK,
            tick_lower: -100,
            tick_upper: 100,
            is_mint: true,
            liquidity: 1000,
            amount0: U256::from(5),
            amount1: U256::from(5),
            bin_amm: false,
        };
        let burn = JitFact {
            tx_index: 1,
            log_index: 0,
            pool: POOL_A,
            owner: ATK,
            tick_lower: -100,
            tick_upper: 100,
            is_mint: false,
            liquidity: 1000,
            amount0: U256::from(5),
            amount1: U256::from(5),
            bin_amm: false,
        };
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
    fn classic_three_eoa_sandwich_detected() {
        // The canonical realized sandwich: a separate victim EOA buys between
        // the attacker's front-run buy and back-run sell on the same pool.
        // front: USDC->TOKA buy by ATK (tx0), victim: USDC->TOKA buy by VICTIM
        // (tx1), back: TOKA->USDC sell by ATK (tx2).
        let front = swap(POOL_A, USDC, TOKA, 100, 200);
        let victim = swap(POOL_A, USDC, TOKA, 200, 350);
        let back = swap(POOL_A, TOKA, USDC, 350, 195);
        let t0 = tx(
            0,
            ATK,
            true,
            vec![front],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        );
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
        let t2 = tx(
            2,
            ATK,
            true,
            vec![back],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 350),
                transfer(1, USDC, POOL_A, ATK, 195),
            ],
        );
        let input = block(vec![t0, t1, t2]);
        let ev = classify_kind(&input, MevKind::Sandwich);
        assert_eq!(ev.searcher, ATK);
        assert_eq!(ev.pools, vec![POOL_A]);
        assert_eq!(ev.tx_index, 0); // anchored to front-run
        assert_eq!(ev.profit_token, Some(USDC));
        // profit = back-run amount_out (195) - front-run amount_in (100)
        assert_eq!(ev.profit_amount, Some(U256::from(95)));
        assert_eq!(ev.confidence, Confidence::Exact);
        // victim attached: separate EOA tx (tx1), size = victim's amount_in
        assert_eq!(ev.victim_swap_size, Some(U256::from(200)));
        // Phase 1.4 evidence: both victim legs recorded with sizes, and the
        // front+back gas is summed (2 txs × 100_000 × 30 gwei).
        assert_eq!(ev.victim_hashes, vec![B256::repeat_byte(1)]);
        assert_eq!(ev.details["victims_seen"], serde_json::json!(1));
        assert_eq!(ev.details["victim_swap_sizes"], serde_json::json!(["200"]));
        assert_eq!(
            ev.details["evidence"]["reverse_backrun"],
            serde_json::json!(true)
        );
        assert_eq!(
            ev.details["reasons"],
            serde_json::json!([
                "REVERSE_DIRECTION",
                "SAME_SEARCHER",
                "VICTIM_EXECUTION_DEGRADED"
            ])
        );
        // front price 200/100 = 2.0, victim price 350/200 = 1.75 → 12.5%
        assert_eq!(
            ev.details["victim_execution"]["degradation_pct"],
            serde_json::json!(12.5)
        );
        assert_eq!(ev.gas_cost_wei, U256::from(6_000_000_000_000_000u64));
        assert_eq!(
            ev.details["evidence"]["contract_mediated"],
            serde_json::json!(false)
        );
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
            tx_index: 0,
            log_index: 9,
            pool: POOL_A,
            owner: ATK,
            tick_lower: -100,
            tick_upper: 100,
            is_mint: true,
            liquidity: 1000,
            amount0: U256::from(5),
            amount1: U256::from(5),
            bin_amm: false,
        };
        let burn = JitFact {
            tx_index: 1,
            log_index: 0,
            pool: POOL_A,
            owner: ATK,
            tick_lower: -100,
            tick_upper: 100,
            is_mint: false,
            liquidity: 1000,
            amount0: U256::from(5),
            amount1: U256::from(5),
            bin_amm: false,
        };
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
    fn lb_bin_jit_pairs_without_tick_on_swap() {
        // LB swaps carry no tick; bin_amm JIT must still fire when a same-pool
        // swap sits between deposit and withdraw.
        let mint = JitFact {
            tx_index: 0,
            log_index: 0,
            pool: POOL_A,
            owner: ATK,
            tick_lower: 8000,
            tick_upper: 8010,
            is_mint: true,
            liquidity: 2,
            amount0: U256::from(10),
            amount1: U256::from(5),
            bin_amm: true,
        };
        let burn = JitFact {
            tx_index: 2,
            log_index: 0,
            pool: POOL_A,
            owner: ATK,
            tick_lower: 8000,
            tick_upper: 8010,
            is_mint: false,
            liquidity: 2,
            amount0: U256::from(10),
            amount1: U256::from(5),
            bin_amm: true,
        };
        let mut t0 = tx(0, ATK, true, vec![], vec![]);
        t0.jit = vec![mint];
        // Same-pool swap with tick=None (LB).
        let t1 = tx(
            1,
            MARKET,
            true,
            vec![swap(POOL_A, USDC, TOKA, 1000, 100)],
            vec![],
        );
        let mut t2 = tx(2, ATK, true, vec![], vec![]);
        t2.jit = vec![burn];
        let events = classify_block(&block(vec![t0, t1, t2]));
        assert!(
            kinds(&events).contains(&MevKind::Jit),
            "expected LB JIT; got {:?}",
            kinds(&events)
        );
        let ev = event_of(&events, MevKind::Jit);
        assert_eq!(ev.details["bin_amm"], serde_json::json!(true));
        assert_eq!(ev.details["mode"], serde_json::json!("realized"));
    }
}
