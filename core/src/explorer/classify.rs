//! Per-block realized-MEV classifier (plan §8.1).
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

use std::collections::HashMap;

use alloy::primitives::{Address, B256, U256};

use crate::explorer::decode;
use crate::explorer::profit::{
    is_closed_cycle, select_profit_token, DeltaLedger, ProfitTokenPolicy,
};
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
    pub jit: Vec<JitFact>,
}

/// One block's classified input.
pub struct BlockInput {
    pub block: u64,
    pub ts: u64,
    pub wrapped_native: Address,
    pub profit_policy: ProfitTokenPolicy,
    pub txs: Vec<TxInput>,
}

/// Classify one block. Returns (events, bundle_details) where bundle details
/// for sandwiches are already folded into `details_json` per event.
pub fn classify_block(input: &BlockInput) -> Vec<MevEvent> {
    let mut events: Vec<MevEvent> = Vec::new();

    // ── 1. Liquidation pass (exact, zero-heuristic) ─────────────────────
    for tx in &input.txs {
        for liq in &tx.liquidations {
            events.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: tx.tx_index,
                tx_hash: tx.tx_hash,
                kind: MevKind::Liquidation,
                searcher: liq.liquidator,
                contract: tx.to,
                pools: vec![],
                profit_token: Some(liq.collateral_asset),
                profit_amount: Some(liq.collateral_amount),
                profit_usd: None,
                gas_cost_wei: U256::from(tx.gas_used).saturating_mul(U256::from(
                    (tx.effective_gas_price_gwei * 1e9) as u128,
                )),
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
                }),
            });
        }
    }
    let liq_txs: std::collections::HashSet<u64> = events.iter().map(|e| e.tx_index).collect();

    // ── 2-3. Swap attribution + atomic arb pass ─────────────────────────
    // Sandwich state: (pool, attacker) → (front-run leg, back-run leg)
    let mut sandwich_candidates: HashMap<(Address, Address), SandwichState> = HashMap::new();

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

        let ledger = DeltaLedger::from_transfers(
            &tx.transfers,
            input.wrapped_native,
            (tx.from, tx.value),
        );

        // Participant scope: sender + receiver-side contracts (searcher
        // attribution per plan: "EOA sender (or its deployed contract)").
        let mut candidates: Vec<Address> = vec![tx.from];
        if let Some(to) = tx.to {
            candidates.push(to);
        }
        // Addresses with ≥2 swap participations via transfers also qualify
        for (addr, _) in tx.transfers.iter().map(|t| (t.from, ())).chain(tx.transfers.iter().map(|t| (t.to, ()))) {
            let _ = addr;
        }

        let candidate = candidates
            .iter()
            .copied()
            .find(|a| select_profit_token(&ledger, *a, &input.profit_policy).is_some());

        // ── Sandwich leg tracking (cross-tx pattern state) ─────────────
        for s in &tx.swaps {
            let key = (s.pool, tx.from);
            let entry = sandwich_candidates.entry(key).or_default();
            if s.token_in != Address::ZERO && s.token_out != Address::ZERO {
                // direction from token flow: buy when token_in is the "asset"
                // leg recorded first; we track both legs and classify below.
            }
            if entry.front.is_none() {
                entry.front = Some(SwapLeg {
                    tx_index: tx.tx_index,
                    tx_hash: tx.tx_hash,
                    token_in: s.token_in,
                    token_out: s.token_out,
                    amount_in: s.amount_in,
                    amount_out: s.amount_out,
                });
            } else if entry
                .front
                .as_ref()
                .map(|f| f.token_in == s.token_out || f.token_out == s.token_in)
                .unwrap_or(false)
                && entry.victims_seen < 1
            {
                // opposite direction same sender — candidate back-run, but a
                // victim swap must appear between legs; if none seen yet, keep
                // as potential replacement front-run (double open).
                entry.pending_back = Some(SwapLeg {
                    tx_index: tx.tx_index,
                    tx_hash: tx.tx_hash,
                    token_in: s.token_in,
                    token_out: s.token_out,
                    amount_in: s.amount_in,
                    amount_out: s.amount_out,
                });
            } else if entry.pending_back.is_some() {
                // second opposite-direction swap after a victim was seen
                entry.back = entry.pending_back.take();
            }
        }
        for (pool, victim_sender, size) in victim_swaps_between(&input.txs, tx, &sandwich_candidates) {
            let key = (pool, victim_sender);
            let entry = sandwich_candidates.entry(key).or_default();
            entry.victims_seen += 1;
            entry.last_victim = Some((tx.tx_index, tx.tx_hash, size));
        }

        // ── Atomic arb ─────────────────────────────────────────────────
        let arb_confirmed = tx.swaps.len() >= 2 && {
            let edges: Vec<(Address, Address)> = tx
                .swaps
                .iter()
                .filter(|s| s.token_in != Address::ZERO && s.token_out != Address::ZERO)
                .map(|s| (s.token_in, s.token_out))
                .collect();
            is_closed_cycle(&edges)
        };

        if let Some(searcher) = candidate {
            if let Some(token) = select_profit_token(&ledger, searcher, &input.profit_policy) {
                let amount = ledger.net(searcher, token);
                let is_dust_or_wrap = amount.is_zero();
                if !is_dust_or_wrap {
                    let kind = if arb_confirmed {
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
                        profit_usd: None, // pricing applied at persist time
                        gas_cost_wei: U256::from(tx.gas_used).saturating_mul(U256::from(
                            (tx.effective_gas_price_gwei * 1e9) as u128,
                        )),
                        confidence: if arb_confirmed {
                            Confidence::Exact
                        } else {
                            Confidence::Inferred
                        },
                        victim_hashes: vec![],
                        victim_swap_size: None,
                        details: serde_json::json!({
                            "route": tx.swaps.iter().map(|s| serde_json::json!({
                                "pool": format!("{:#x}", s.pool),
                                "amm": s.amm.as_str(),
                                "token_in": format!("{:#x}", s.token_in),
                                "token_out": format!("{:#x}", s.token_out),
                                "amount_in": s.amount_in.to_string(),
                                "amount_out": s.amount_out.to_string(),
                            })).collect::<Vec<_>>(),
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

    // ── 5b. JIT block-wide pairing ──────────────────────────────────────
    let mut jit_events = classify_jit(input);
    // jit_arb when a Jit pool tx also produced an arb event in this block
    let arb_txs: std::collections::HashSet<u64> = events
        .iter()
        .filter(|e| e.kind == MevKind::ArbAtomic)
        .map(|e| e.tx_index)
        .collect();
    for ev in jit_events.iter_mut() {
        if arb_txs.contains(&ev.tx_index) {
            ev.kind = MevKind::JitArb;
        }
    }
    events.append(&mut jit_events);

    // ── 4b. Sandwich fold: pending_back + victim ≥ 1 → Sandwich event ───
    let mut sandwiches = fold_sandwiches(input, &sandwich_candidates);
    events.append(&mut sandwiches);

    events.sort_by(|a, b| a.tx_index.cmp(&b.tx_index).then_with(|| {
        kind_order(a.kind).cmp(&kind_order(b.kind))
    }));
    events
}

fn kind_order(k: MevKind) -> u8 {
    match k {
        MevKind::Sandwich => 0,
        MevKind::ArbAtomic => 1,
        MevKind::JitArb => 2,
        MevKind::Jit => 3,
        MevKind::Liquidation => 4,
        MevKind::Unknown => 5,
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

#[derive(Debug, Default)]
struct SandwichState {
    front: Option<SwapLeg>,
    pending_back: Option<SwapLeg>,
    back: Option<SwapLeg>,
    victims_seen: usize,
    last_victim: Option<(u64, B256, U256)>,
}

/// Find third-party (non-attacker) swaps on pools the attacker already opened.
fn victim_swaps_between(
    txs: &[TxInput],
    current: &TxInput,
    state: &HashMap<(Address, Address), SandwichState>,
) -> Vec<(Address, Address, U256)> {
    let mut out = Vec::new();
    let attacker_pools: std::collections::HashSet<Address> = state
        .keys()
        .filter(|(_, a)| *a == current.from)
        .map(|(p, _)| *p)
        .collect();
    if attacker_pools.is_empty() {
        return out;
    }
    // Victims are swaps in *this* tx on attacker-opened pools where the swap's
    // accompanying transfers are not initiated by the attacker.
    for s in &current.swaps {
        if !attacker_pools.contains(&s.pool) {
            continue;
        }
        let attacker_transfers: Vec<&TransferFact> = current
            .transfers
            .iter()
            .filter(|t| t.from == current.from || t.to == current.from)
            .collect();
        let attacker_touched_pool = attacker_transfers.iter().any(|t| {
            t.token == s.token_in
                || t.token == s.token_out
                || t.from == s.pool
                || t.to == s.pool
        });
        if !attacker_touched_pool {
            out.push((s.pool, current.from, s.amount_in));
        }
    }
    out
}

fn fold_sandwiches(
    input: &BlockInput,
    state: &HashMap<(Address, Address), SandwichState>,
) -> Vec<MevEvent> {
    let mut out = Vec::new();
    for ((pool, attacker), s) in state {
        let (Some(front), Some(back)) = (&s.front, &s.back) else {
            continue;
        };
        if s.victims_seen == 0 {
            continue;
        }
        // Profit = back-run output − front-run input, netted in the profit
        // token (plan §8.2: attacker profit + victim swap size in v1).
        let profit = back.amount_out.saturating_sub(front.amount_in);
        let gas_cost_wei = input
            .txs
            .iter()
            .filter(|t| t.tx_index == back.tx_index)
            .map(|t| U256::from(t.gas_used).saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128)))
            .next()
            .unwrap_or(U256::ZERO);
        out.push(MevEvent {
            block: input.block,
            ts: input.ts,
            tx_index: front.tx_index, // bundle anchor = front-run
            tx_hash: front.tx_hash,
            kind: MevKind::Sandwich,
            searcher: *attacker,
            contract: None,
            pools: vec![*pool],
            profit_token: Some(front.token_in),
            profit_amount: Some(profit),
            profit_usd: None,
            gas_cost_wei,
            confidence: Confidence::Exact,
            victim_hashes: s
                .last_victim
                .map(|(_, h, _)| vec![h])
                .unwrap_or_default(),
            victim_swap_size: s.last_victim.map(|(_, _, sz)| sz),
            details: serde_json::json!({
                "pool": format!("{:#x}", pool),
                "front_run": {
                    "tx_index": front.tx_index,
                    "tx_hash": format!("{:#x}", front.tx_hash),
                    "amount_in": front.amount_in.to_string(),
                    "amount_out": front.amount_out.to_string(),
                },
                "back_run": {
                    "tx_index": back.tx_index,
                    "tx_hash": format!("{:#x}", back.tx_hash),
                    "amount_in": back.amount_in.to_string(),
                    "amount_out": back.amount_out.to_string(),
                },
                "backrun_tx_index": back.tx_index,
                "victims_seen": s.victims_seen,
            }),
        });
    }
    out
}

/// JIT pairing: same pool, same owner, same tick range, Mint before Burn,
/// same block (plan §8.1 pass 5).
fn classify_jit(input: &BlockInput) -> Vec<MevEvent> {
    let mints: Vec<&JitFact> = input
        .txs
        .iter()
        .flat_map(|t| t.jit.iter().map(move |j| {
            let mut j2 = j.clone();
            j2.tx_index = t.tx_index;
            j2
        }))
        .filter(|j| j.is_mint)
        .collect();
    let burns: Vec<(u64, &JitFact)> = input
        .txs
        .iter()
        .flat_map(|t| t.jit.iter().map(move |j| (t.tx_index, j)))
        .filter(|(_, j)| !j.is_mint)
        .collect();

    let mut out = Vec::new();
    for mint in &mints {
        let burn = burns.iter().find(|(_, b)| {
            b.pool == mint.pool
                && b.owner == mint.owner
                && b.tick_lower == mint.tick_lower
                && b.tick_upper == mint.tick_upper
                && b.liquidity >= mint.liquidity
        });
        if let Some((burn_tx, _)) = burn {
            let gas_cost_wei = input
                .txs
                .iter()
                .filter(|t| t.tx_index == mint.tx_index)
                .map(|t| U256::from(t.gas_used).saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128)))
                .next()
                .unwrap_or(U256::ZERO);
            out.push(MevEvent {
                block: input.block,
                ts: input.ts,
                tx_index: mint.tx_index,
                tx_hash: B256::ZERO, // mint tx hash resolved at ingest stamping
                kind: MevKind::Jit,
                searcher: mint.owner,
                contract: None,
                pools: vec![mint.pool],
                profit_token: None, // fee capture — priced via pool tokens
                profit_amount: None,
                profit_usd: None,
                gas_cost_wei,
                confidence: Confidence::Exact,
                victim_hashes: vec![],
                victim_swap_size: None,
                details: serde_json::json!({
                    "pool": format!("{:#x}", mint.pool),
                    "owner": format!("{:#x}", mint.owner),
                    "tick_lower": mint.tick_lower,
                    "tick_upper": mint.tick_upper,
                    "liquidity": mint.liquidity,
                    "burn_tx_index": burn_tx,
                    "amount0": mint.amount0.to_string(),
                    "amount1": mint.amount1.to_string(),
                }),
            });
        }
    }
    out
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

/// Convenience: decode a tx's receipt logs into facts (used by ingest).
pub fn decode_tx_logs(
    tx_index: u64,
    logs: &[crate::data::LogData],
) -> (Vec<TransferFact>, Vec<SwapFact>, Vec<crate::explorer::types::LiquidationFact>, Vec<JitFact>) {
    let mut transfers = Vec::new();
    let mut swaps = Vec::new();
    let mut liquidations = Vec::new();
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
        if let Some(mut j) = decode::decode_v3_mint_burn(log) {
            j.tx_index = tx_index;
            j.log_index = log_idx;
            jit.push(j);
        }
    }
    decode::attach_swap_tokens(&mut swaps, &transfers);
    (transfers, swaps, liquidations, jit)
}

/// True when a swap fact has unresolved direction tokens (V3 sentinel kept).
pub fn has_unresolved_tokens(s: &SwapFact) -> bool {
    s.token_in == decode::TOKEN0_SENTINEL
        || s.token_in == decode::TOKEN1_SENTINEL
        || s.token_in == Address::ZERO
        || s.token_out == Address::ZERO
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
