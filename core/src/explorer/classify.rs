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
                // `LiquidationCall` carries no liquidator address; the tx
                // sender is the attribution target (zero fallback covered).
                searcher: if liq.liquidator.is_zero() { tx.from } else { liq.liquidator },
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

    // ── 4b. Sandwich classification: front-run → victim → back-run ─────
    let mut sandwiches = classify_sandwiches(input);
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

/// One attacker's sandwich walk over a pool: front-run opens, victims
/// (third-party same-direction swaps in block order), and the closing back-run.
#[derive(Debug, Default)]
struct SandwichWalk {
    victims_seen: usize,
    last_victim: Option<(u64, B256, U256)>,
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

/// Classic realized-sandwich detection (plan §3/§8.1 pass 4): one attacker
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
            if s.token_in.is_zero() || s.token_out.is_zero() {
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
            for (tx_idx, from, leg) in &pool_swaps {
                if *from == attacker {
                    if front.is_none() {
                        front = Some(leg); // position open
                    } else if walk.victims_seen >= 1 {
                        let Some(f) = front else {
                            continue;
                        };
                        if opposite_dir(f, leg) {
                            // First opposite-direction attacker swap after a
                            // victim closes the sandwich.
out.push(fold_sandwich(
                            input,
                            pool,
                            attacker,
                            f,
                            leg,
                            &walk,
                        ));
                        break;
                        }
                    }
                    // Same-direction or pre-victim opposite swaps keep the
                    // position open; only the first back-run closes it.
                } else if let Some(f) = front {
                    if same_dir(f, leg) {
                        // Third-party swap in the same direction: the victim.
                        walk.victims_seen += 1;
                        walk.last_victim = Some((*tx_idx, leg.tx_hash, leg.amount_in));
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
    // (the token both legs trade against), plan §8.2.
    let profit = back.amount_out.saturating_sub(front.amount_in);
    let gas_cost_wei = input
        .txs
        .iter()
        .filter(|t| t.tx_index == back.tx_index)
        .map(|t| {
            U256::from(t.gas_used)
                .saturating_mul(U256::from((t.effective_gas_price_gwei * 1e9) as u128))
        })
        .next()
        .unwrap_or(U256::ZERO);
    MevEvent {
        block: input.block,
        ts: input.ts,
        tx_index: front.tx_index, // bundle anchor = front-run
        tx_hash: front.tx_hash,
        kind: MevKind::Sandwich,
        searcher: attacker,
        contract: None,
        pools: vec![pool],
        profit_token: Some(front.token_in),
        profit_amount: Some(profit),
        profit_usd: None,
        gas_cost_wei,
        confidence: Confidence::Exact,
        victim_hashes: walk
            .last_victim
            .map(|(_, h, _)| vec![h])
            .unwrap_or_default(),
        victim_swap_size: walk.last_victim.map(|(_, _, sz)| sz),
        details: serde_json::json!({
            "pool": format!("{pool:#x}"),
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
            "victims_seen": walk.victims_seen,
        }),
    }
}

/// JIT pairing: same pool, same owner, same tick range, Mint before Burn,
/// same block (plan §8.1 pass 5).
fn classify_jit(input: &BlockInput) -> Vec<MevEvent> {
    let mints: Vec<JitFact> = input
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::{Amm, LiquidationFact, MevKind};
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
        }
    }

    fn transfer(log_idx: u64, token: Address, from: Address, to: Address, amt: u64) -> TransferFact {
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
            txs,
        }
    }

    fn event_of<'a>(events: &'a [MevEvent], kind: MevKind) -> &'a MevEvent {
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
    fn non_cycle_profitable_swap_is_unknown() {
        let swaps = vec![swap(POOL_A, USDC, TOKA, 100, 200)];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::Unknown);
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(ev.profit_token, Some(TOKA));
        assert_eq!(ev.profit_amount, Some(U256::from(200)));
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
        };
        let mut t0 = tx(0, ATK, true, vec![], vec![]);
        t0.jit = vec![mint];
        let mut t1 = tx(1, VICTIM, true, vec![], vec![]);
        t1.jit = vec![burn];
        let input = block(vec![t0, t1]);
        let ev = classify_kind(&input, MevKind::Jit);
        assert_eq!(ev.searcher, ATK);
        assert_eq!(ev.tx_index, 0); // anchored to the mint tx
        // burn tx index embedded in details
        assert_eq!(ev.details["burn_tx_index"], serde_json::json!(1));
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
        let t0 = tx(0, ATK, true, vec![front], vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ]);
        let t1 = tx(1, VICTIM, true, vec![victim], vec![
            transfer(0, USDC, VICTIM, POOL_A, 200),
            transfer(1, TOKA, POOL_A, VICTIM, 350),
        ]);
        let t2 = tx(2, ATK, true, vec![back], vec![
            transfer(0, TOKA, ATK, POOL_A, 350),
            transfer(1, USDC, POOL_A, ATK, 195),
        ]);
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
    }

    #[test]
    fn plain_two_leg_exchange_is_not_sandwich() {
        // Attacker buys then sells with no third-party victim in between:
        // a normal round-trip, not a sandwich.
        let t0 = tx(0, ATK, true, vec![swap(POOL_A, USDC, TOKA, 100, 200)], vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ]);
        let t1 = tx(1, ATK, true, vec![swap(POOL_A, TOKA, USDC, 200, 95)], vec![
            transfer(0, TOKA, ATK, POOL_A, 200),
            transfer(1, USDC, POOL_A, ATK, 95),
        ]);
        let input = block(vec![t0, t1]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Sandwich));
    }

    #[test]
    fn victim_before_front_is_not_sandwich() {
        // The victim swap precedes the attacker's front-run: the attacker did
        // not open a position yet, so this is not a sandwich.
        let t0 = tx(0, VICTIM, true, vec![swap(POOL_A, USDC, TOKA, 200, 350)], vec![
            transfer(0, USDC, VICTIM, POOL_A, 200),
            transfer(1, TOKA, POOL_A, VICTIM, 350),
        ]);
        let t1 = tx(1, ATK, true, vec![swap(POOL_A, USDC, TOKA, 100, 200)], vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ]);
        let t2 = tx(2, ATK, true, vec![swap(POOL_A, TOKA, USDC, 200, 195)], vec![
            transfer(0, TOKA, ATK, POOL_A, 200),
            transfer(1, USDC, POOL_A, ATK, 195),
        ]);
        let input = block(vec![t0, t1, t2]);
        assert!(!kinds(&classify_block(&input)).contains(&MevKind::Sandwich));
    }

    #[test]
    fn jit_arb_upgraded_when_mint_tx_also_arbs() {
        // tx0 has a JIT mint AND an atomic-arb cycle.
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
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
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
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
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
}
