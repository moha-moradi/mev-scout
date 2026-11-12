//! Narrow, fail-closed V3 accounting. Detection is not proof of economic ownership.
//! Only an EOA-owned position, funded by that EOA and paid back to it, is supported.
//! Contract/NFT-manager owners are deliberately unavailable, not treated as searchers.
use super::{
    classify::BlockInput,
    pricing::TokenUsd,
    types::{JitFact, MevEvent, MevKind, TransferFact},
};
use alloy::primitives::{address, keccak256, Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const MAX_HISTORY_BLOCKS: u64 = 1000;
pub const MAX_FACTS: usize = 20_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpSwap {
    pub log_index: u64,
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpTx {
    pub tx_hash: B256,
    pub tx_index: u64,
    pub from: Address,
    pub to: Option<Address>,
    pub facts: Vec<JitFact>,
    pub transfers: Vec<TransferFact>,
    /// Swaps in this receipt. Required to reconcile a same-transaction cycle;
    /// unrelated swap tokens still fail the transfer check.
    pub swaps: Vec<LpSwap>,
    pub gas: Option<Gas>,
    pub other_activity: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickEvidence {
    pub tx_index: u64,
    pub log_index: u64,
    pub pool: Address,
    pub tick: Option<i32>,
}
/// A bounded receipt-derived journal. Empty blocks are persisted too: absence of
/// LP events is useful only if the entire canonical receipt set was present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpBlock {
    pub block_number: u64,
    pub hash: B256,
    pub parent_hash: Option<B256>,
    pub ts: u64,
    pub complete: bool,
    pub txs: Vec<LpTx>,
    pub swaps: Vec<TickEvidence>,
}
impl LpBlock {
    pub fn from_input(
        input: &BlockInput,
        header: &crate::data::BlockData,
        txs: &[crate::data::TxData],
        receipts: &[crate::data::ReceiptData],
    ) -> Self {
        let mut sorted: Vec<_> = receipts.iter().collect();
        sorted.sort_by_key(|r| r.tx_index);
        let mut cumulative = 0u64;
        let mut hashes = HashSet::new();
        let mut complete = receipts.len() == txs.len() && header.number == input.block;
        for (index, r) in sorted.iter().enumerate() {
            cumulative = match cumulative.checked_add(r.gas_used) {
                Some(n) => n,
                None => {
                    complete = false;
                    u64::MAX
                }
            };
            complete &= r.tx_index == index as u64
                && r.block_hash == Some(header.hash)
                && hashes.insert(r.tx_hash)
                && r.cumulative_gas_used == cumulative
                && txs
                    .iter()
                    .any(|t| t.index == r.tx_index && t.hash == r.tx_hash);
        }
        complete &= cumulative == header.gas_used;
        let mut block = Self {
            block_number: input.block,
            hash: header.hash,
            parent_hash: None,
            ts: input.ts,
            complete,
            txs: vec![],
            swaps: vec![],
        };
        for t in &input.txs {
            block.swaps.extend(t.swaps.iter().map(|s| TickEvidence {
                tx_index: t.tx_index,
                log_index: s.log_index,
                pool: s.pool,
                tick: s.tick,
            }));
            if t.jit.is_empty() {
                continue;
            }
            let receipt = receipts
                .iter()
                .find(|r| r.tx_index == t.tx_index && r.tx_hash == t.tx_hash);
            let gas = receipt.and_then(|r| {
                r.effective_gas_price.map(|p| Gas {
                    tx_hash: t.tx_hash,
                    ts: input.ts,
                    gas_used: r.gas_used.to_string(),
                    effective_gas_price_wei: p.to_string(),
                    gas_cost_wei: (U256::from(r.gas_used) * U256::from(p)).to_string(),
                })
            });
            block.txs.push(LpTx {
                tx_hash: t.tx_hash,
                tx_index: t.tx_index,
                from: t.from,
                to: t.to,
                facts: t.jit.clone(),
                transfers: t.transfers.clone(),
                swaps: t
                    .swaps
                    .iter()
                    .map(|s| LpSwap {
                        log_index: s.log_index,
                        pool: s.pool,
                        token_in: s.token_in,
                        token_out: s.token_out,
                        amount_in: s.amount_in,
                        amount_out: s.amount_out,
                    })
                    .collect(),
                gas,
                // Swaps are part of a same-transaction JIT cycle. They are
                // validated by log order and exact transfer reconciliation in
                // `reconcile`; financing/price/value activity is still fatal.
                other_activity: !t.flashloans.is_empty()
                    || !t.liquidations.is_empty()
                    || !t.value.is_zero(),
            });
        }
        block.txs.sort_by_key(|t| t.tx_index);
        for t in &mut block.txs {
            t.facts.sort_by_key(|f| f.log_index);
            t.swaps.sort_by_key(|s| s.log_index);
        }
        if block.swaps.len()
            + block
                .txs
                .iter()
                .map(|t| t.facts.len() + t.transfers.len())
                .sum::<usize>()
            > MAX_FACTS
        {
            block.complete = false;
            block.txs.clear();
            block.swaps.clear();
        }
        block
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub kind: String,
    pub block_number: u64,
    pub ts: u64,
    pub tx_index: u64,
    /// Receipt-local, never the block-global Ethereum logIndex.
    pub log_index: u64,
    pub tx_hash: B256,
    pub amount0: String,
    pub amount1: String,
    pub liquidity: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gas {
    pub tx_hash: B256,
    pub ts: u64,
    pub gas_used: String,
    pub effective_gas_price_wei: String,
    pub gas_cost_wei: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Valuation {
    pub gross_usd: Option<f64>,
    pub gas_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub paid_fees_usd: Option<f64>,
    pub pricing_complete: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub token: Address,
    pub hour: u64,
    pub usd: f64,
    pub decimals: u32,
    pub source: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionBaseline {
    pub block_hash: B256,
    pub block_number: u64,
    pub liquidity: String,
    pub tokens_owed0: String,
    pub tokens_owed1: String,
}
impl PositionBaseline {
    pub fn decode(data: &[u8], block_hash: B256, block_number: u64) -> Option<Self> {
        // positions(bytes32) -> uint128,uint256,uint256,uint128,uint128
        if data.len() != 160
            || [0, 96, 128]
                .iter()
                .any(|i| data[*i..*i + 16].iter().any(|b| *b != 0))
        {
            return None;
        }
        Some(Self {
            block_hash,
            block_number,
            liquidity: U256::from_be_slice(&data[..32]).to_string(),
            tokens_owed0: U256::from_be_slice(&data[96..128]).to_string(),
            tokens_owed1: U256::from_be_slice(&data[128..160]).to_string(),
        })
    }
    fn empty(&self) -> bool {
        self.liquidity == "0" && self.tokens_owed0 == "0" && self.tokens_owed1 == "0"
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JitAccounting {
    pub version: u32,
    pub status: String,
    pub method: String,
    pub reasons: Vec<String>,
    pub pool: Address,
    pub owner: Address,
    pub beneficiary: Option<Address>,
    pub token0: Address,
    pub token1: Address,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub mint_amount0: Option<String>,
    pub mint_amount1: Option<String>,
    pub burn_amount0: Option<String>,
    pub burn_amount1: Option<String>,
    pub collect_amount0: Option<String>,
    pub collect_amount1: Option<String>,
    pub fee_amount0: Option<String>,
    pub fee_amount1: Option<String>,
    pub cashflow_amount0: Option<String>,
    pub cashflow_amount1: Option<String>,
    pub timeline: Vec<Timeline>,
    pub gas: Vec<Gas>,
    pub baselines: Vec<PositionBaseline>,
    pub quotes: Vec<Quote>,
    pub valuation: Valuation,
    pub usd_approximate: bool,
}
impl JitAccounting {
    pub fn unavailable(ev: &MevEvent, tokens: (Address, Address), reason: &str) -> Self {
        Self {
            version: 1,
            status: "unavailable".into(),
            method: "v3_settled_cashflows".into(),
            reasons: if reason.is_empty() {
                vec![]
            } else {
                vec![reason.into()]
            },
            pool: ev.pools.first().copied().unwrap_or_default(),
            owner: ev.details["owner"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(ev.searcher),
            beneficiary: None,
            token0: tokens.0,
            token1: tokens.1,
            tick_lower: ev.details["tick_lower"].as_i64().unwrap_or_default() as i32,
            tick_upper: ev.details["tick_upper"].as_i64().unwrap_or_default() as i32,
            mint_amount0: None,
            mint_amount1: None,
            burn_amount0: None,
            burn_amount1: None,
            collect_amount0: None,
            collect_amount1: None,
            fee_amount0: None,
            fee_amount1: None,
            cashflow_amount0: None,
            cashflow_amount1: None,
            timeline: vec![],
            gas: vec![],
            baselines: vec![],
            quotes: vec![],
            valuation: Valuation::default(),
            usd_approximate: true,
        }
    }
    pub fn reject(&mut self, reason: &str) {
        self.status = "unavailable".into();
        if !self.reasons.iter().any(|r| r == reason) {
            self.reasons.push(reason.into());
        }
        self.valuation = Valuation::default();
    }
    pub fn from_details(details: &serde_json::Value) -> Option<Self> {
        let a: Self = serde_json::from_value(details.get("jit_accounting")?.clone()).ok()?;
        (a.version == 1 && a.method == "v3_settled_cashflows").then_some(a)
    }
}

pub fn position_key(f: &JitFact) -> (Address, Address, i32, i32) {
    (f.pool, f.owner, f.tick_lower, f.tick_upper)
}
fn matches(a: &JitAccounting, f: &JitFact) -> bool {
    (a.pool, a.owner, a.tick_lower, a.tick_upper) == position_key(f)
}
fn signed_difference(a: U256, b: U256) -> String {
    if a >= b {
        (a - b).to_string()
    } else {
        format!("-{}", b - a)
    }
}

/// Reconcile only actual receipts. Pool fee tiers, full-pool volume and endpoint
/// liquidity never enter this formula. A Burn increases owed principal; it pays nothing.
pub fn reconcile(
    a: &mut JitAccounting,
    blocks: &[LpBlock],
    opening: (u64, u64, u64),
    closing: (u64, u64, u64),
    window: u64,
) {
    if closing.0.saturating_sub(opening.0) > window.min(MAX_HISTORY_BLOCKS) {
        a.reject("outside_jit_window");
    }
    let history: Vec<_> = blocks
        .iter()
        .filter(|b| b.block_number >= opening.0 && b.block_number <= closing.0)
        .collect();
    if history.len() as u64 != closing.0.saturating_sub(opening.0) + 1
        || history.iter().any(|b| !b.complete)
        || history.windows(2).any(|p| {
            p[1].block_number != p[0].block_number + 1 || p[1].parent_hash != Some(p[0].hash)
        })
    {
        a.reject("receipt_gap_or_hash_mismatch");
    }
    let facts: Vec<_> = history
        .iter()
        .flat_map(|b| {
            b.txs
                .iter()
                .flat_map(move |t| t.facts.iter().map(move |f| (*b, t, f)))
        })
        .filter(|(_, _, f)| matches(a, f))
        .collect();
    for (b, t, f) in &facts {
        a.timeline.push(Timeline {
            kind: if f.is_mint {
                "mint"
            } else if f.is_collect {
                "collect"
            } else {
                "burn"
            }
            .into(),
            block_number: b.block_number,
            ts: b.ts,
            tx_index: t.tx_index,
            log_index: f.log_index,
            tx_hash: t.tx_hash,
            amount0: f.amount0.to_string(),
            amount1: f.amount1.to_string(),
            liquidity: f.liquidity.to_string(),
        });
    }
    a.timeline
        .sort_by_key(|t| (t.block_number, t.tx_index, t.log_index));
    let mints: Vec<_> = facts.iter().filter(|(_, _, f)| f.is_mint).collect();
    let burns: Vec<_> = facts
        .iter()
        .filter(|(_, _, f)| !f.is_mint && !f.is_collect)
        .collect();
    let collects: Vec<_> = facts.iter().filter(|(_, _, f)| f.is_collect).collect();
    if mints.len() != 1 {
        a.reject("additions_overlap_or_reopen");
    }
    if burns.len() != 1 {
        a.reject("partial_or_multiple_burns");
    }
    if collects.len() != 1 {
        a.reject("missing_partial_or_deferred_collect");
    }
    let (Some(m), Some(b), Some(c)) = (mints.first(), burns.first(), collects.first()) else {
        return;
    };
    let order = |x: &(&LpBlock, &LpTx, &JitFact)| (x.0.block_number, x.1.tx_index, x.2.log_index);
    if order(m) != opening
        || order(b) != closing
        || order(m) >= order(b)
        || order(b) >= order(c)
        || b.0.block_number != c.0.block_number
        || b.1.tx_hash != c.1.tx_hash
    {
        a.reject("out_of_order_or_deferred_collect");
    }
    if m.2.bin_amm || b.2.bin_amm {
        a.reject("unsupported_lb_accounting");
    }
    if m.2.liquidity == 0 || m.2.liquidity != b.2.liquidity {
        a.reject("partial_burn");
    }
    // The position key must belong to the operation itself: either the EOA
    // sender (direct/routed mint) or the transaction target contract that
    // mints, burns and collects the same key. Known shared position managers
    // are never assumed to be a searcher.
    let operates = |tx: &LpTx| tx.from == a.owner || tx.to == Some(a.owner);
    let mint_sender_ok =
        m.2.sender == Some(a.owner) || (m.1.from == a.owner && m.2.sender == m.1.to);
    if shared_position_manager(a.owner)
        || !operates(m.1)
        || !operates(b.1)
        || !operates(c.1)
        || c.2.recipient != Some(a.owner)
        || !mint_sender_ok
    {
        a.reject("shared_manager_or_unprovable_beneficiary");
    } else {
        a.beneficiary = Some(a.owner);
    }
    if a.baselines.len() != 2
        || !a.baselines.iter().all(PositionBaseline::empty)
        || history.first().and_then(|h| h.parent_hash) != a.baselines.first().map(|p| p.block_hash)
        || history.last().map(|h| h.hash) != a.baselines.last().map(|p| p.block_hash)
        || a.baselines
            .first()
            .is_some_and(|p| p.block_number + 1 != opening.0)
        || a.baselines
            .last()
            .is_some_and(|p| p.block_number != closing.0)
    {
        a.reject("archive_baseline_or_settlement_unavailable");
    }
    // Positive tick evidence is a detection heuristic, not a proof of every
    // crossed tick. V3 upper tick is exclusive; only swaps strictly in lifetime.
    if !history.iter().any(|h| {
        h.swaps.iter().any(|s| {
            let o = (h.block_number, s.tx_index, s.log_index);
            o > opening
                && o < closing
                && s.pool == a.pool
                && s.tick
                    .is_some_and(|k| k >= a.tick_lower && k < a.tick_upper)
        })
    }) {
        a.reject("no_in_lifetime_tick_evidence");
    }
    a.mint_amount0 = Some(m.2.amount0.to_string());
    a.mint_amount1 = Some(m.2.amount1.to_string());
    a.burn_amount0 = Some(b.2.amount0.to_string());
    a.burn_amount1 = Some(b.2.amount1.to_string());
    a.collect_amount0 = Some(c.2.amount0.to_string());
    a.collect_amount1 = Some(c.2.amount1.to_string());
    if c.2.amount0 < b.2.amount0 || c.2.amount1 < b.2.amount1 {
        a.reject("collect_below_burn_principal");
    } else {
        a.fee_amount0 = Some((c.2.amount0 - b.2.amount0).to_string());
        a.fee_amount1 = Some((c.2.amount1 - b.2.amount1).to_string());
    }
    a.cashflow_amount0 = Some(signed_difference(c.2.amount0, m.2.amount0));
    a.cashflow_amount1 = Some(signed_difference(c.2.amount1, m.2.amount1));
    let mut seen = HashSet::new();
    let (token0, token1) = (a.token0, a.token1);
    let token_index = move |token: Address| {
        if token == token0 {
            Some(0)
        } else if token == token1 {
            Some(1)
        } else {
            None
        }
    };
    let mint_edge = (m.1.tx_index, m.2.log_index);
    let burn_edge = (b.1.tx_index, b.2.log_index);
    for (_, tx, _) in [**m, **b, **c] {
        if !seen.insert(tx.tx_hash) {
            continue;
        }
        if tx.other_activity || tx.facts.iter().any(|f| !matches(a, f)) {
            a.reject("shared_transaction_or_financing");
        }
        if let Some(g) = &tx.gas {
            a.gas.push(g.clone());
        } else {
            a.reject("exact_receipt_gas_unavailable");
        }
        let open_tx = tx.tx_hash == m.1.tx_hash;
        let close_tx = tx.tx_hash == b.1.tx_hash;
        // A same-transaction cycle's validating swap sits strictly between
        // its Mint and Burn. An opening tx may keep trading after Mint; a
        // closing tx may only trade before Burn.
        let in_swap_window = |log_index: u64| {
            let edge = (tx.tx_index, log_index);
            (open_tx && edge > mint_edge || !open_tx) && (close_tx && edge < burn_edge || !close_tx)
        };
        let mut swap_in = [U256::ZERO; 2];
        let mut swap_out = [U256::ZERO; 2];
        for s in &tx.swaps {
            if s.pool != a.pool || !in_swap_window(s.log_index) {
                a.reject("unrelated_swap_in_owned_transaction");
                continue;
            }
            let (Some(i), Some(o)) = (token_index(s.token_in), token_index(s.token_out)) else {
                a.reject("unattributed_or_nonstandard_transfer");
                continue;
            };
            match (
                swap_in[i].checked_add(s.amount_in),
                swap_out[o].checked_add(s.amount_out),
            ) {
                (Some(x), Some(y)) => {
                    swap_in[i] = x;
                    swap_out[o] = y;
                }
                _ => a.reject("transfer_overflow"),
            }
        }
        let mut ins = [U256::ZERO; 2];
        let mut outs = [U256::ZERO; 2];
        let mut seen_swap_in = [U256::ZERO; 2];
        let mut seen_swap_out = [U256::ZERO; 2];
        for tr in &tx.transfers {
            let token = token_index(tr.token);
            if let Some(i) = token {
                if tr.from == a.owner && tr.to == a.pool && open_tx && tr.log_index < m.2.log_index
                {
                    match ins[i].checked_add(tr.amount) {
                        Some(n) => ins[i] = n,
                        None => a.reject("transfer_overflow"),
                    }
                } else if tr.from == a.pool
                    && tr.to == a.owner
                    && close_tx
                    && tr.log_index > b.2.log_index
                    && tr.log_index < c.2.log_index
                {
                    match outs[i].checked_add(tr.amount) {
                        Some(n) => outs[i] = n,
                        None => a.reject("transfer_overflow"),
                    }
                } else if in_swap_window(tr.log_index) && tr.to == a.pool {
                    match seen_swap_in[i].checked_add(tr.amount) {
                        Some(n) => seen_swap_in[i] = n,
                        None => a.reject("transfer_overflow"),
                    }
                } else if in_swap_window(tr.log_index) && tr.from == a.pool {
                    match seen_swap_out[i].checked_add(tr.amount) {
                        Some(n) => seen_swap_out[i] = n,
                        None => a.reject("transfer_overflow"),
                    }
                } else {
                    a.reject("unattributed_or_nonstandard_transfer");
                }
            } else if !tr.amount.is_zero() {
                a.reject("unattributed_or_nonstandard_transfer");
            }
        }
        let expected_in = if open_tx {
            [m.2.amount0, m.2.amount1]
        } else {
            [U256::ZERO; 2]
        };
        let expected_out = if close_tx {
            [c.2.amount0, c.2.amount1]
        } else {
            [U256::ZERO; 2]
        };
        if ins != expected_in
            || outs != expected_out
            || seen_swap_in != swap_in
            || seen_swap_out != swap_out
        {
            a.reject("token_transfer_reconciliation_failed");
        }
    }
    if a.reasons.is_empty() {
        a.status = "verified".into();
    }
}

/// Same evaluator for initial annotation and repricing. Quotes are keyed by
/// each cashflow's own hour, not the closing block. Missing components clear
/// only the applicable derived totals; no stale or partial strategy profit.
pub fn evaluate(a: &JitAccounting) -> Valuation {
    if a.version != 1 || a.status != "verified" || a.method != "v3_settled_cashflows" {
        return Valuation::default();
    }
    let value = |token: Address, ts: u64, amount: &str| -> Option<f64> {
        let raw = amount.parse::<U256>().ok()?;
        if raw.is_zero() {
            return Some(0.0);
        }
        let q = a
            .quotes
            .iter()
            .find(|q| q.token == token && q.hour == ts / 3600)?;
        if !q.usd.is_finite() || q.usd <= 0.0 || q.decimals > 77 {
            return None;
        }
        let usd = super::pricing::token_amount_to_usd(
            raw,
            &TokenUsd {
                usd: q.usd,
                decimals: q.decimals,
            },
        );
        usd.is_finite().then_some(usd)
    };
    let mut gross = Some(0.0);
    for t in &a.timeline {
        let sign = match t.kind.as_str() {
            "mint" => -1.0,
            "collect" => 1.0,
            _ => continue,
        };
        gross = gross
            .zip(value(a.token0, t.ts, &t.amount0))
            .zip(value(a.token1, t.ts, &t.amount1))
            .map(|((g, x), y)| g + sign * (x + y));
    }
    let mut gas = Some(0.0);
    let mut seen = HashSet::new();
    for g in &a.gas {
        if seen.insert(g.tx_hash) {
            gas = gas
                .zip(value(Address::ZERO, g.ts, &g.gas_cost_wei))
                .map(|(a, b)| a + b);
        }
    }
    let fees = a
        .timeline
        .iter()
        .find(|t| t.kind == "collect")
        .and_then(|t| {
            Some(
                value(a.token0, t.ts, a.fee_amount0.as_deref()?)?
                    + value(a.token1, t.ts, a.fee_amount1.as_deref()?)?,
            )
        });
    let finite = |n: Option<f64>| n.filter(|v| v.is_finite());
    let gross = finite(gross);
    let gas = finite(gas);
    let fees = finite(fees);
    let net = finite(gross.zip(gas).map(|(g, c)| g - c));
    Valuation {
        gross_usd: gross,
        gas_usd: gas,
        paid_fees_usd: fees,
        net_profit_usd: net,
        pricing_complete: gross.is_some() && gas.is_some() && fees.is_some(),
    }
}

/// Conservative token semantics allowlist. Unknown tokens are not inferred to
/// be standard from a Transfer topic or a successful decimals() call.
pub fn standard_token(chain: u64, token: Address) -> bool {
    if super::pricing::approximate_token(&token).is_some() {
        return false;
    }
    let tokens: &[Address] = match chain {
        1 => &[
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            address!("dAC17F958D2ee523a2206206994597C13D831ec7"),
            address!("6B175474E89094C44Da98b954EedeAC495271d0F"),
            address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
        ],
        43114 => &[
            address!("B31f66AA3C1e785363F0875A1B74E27b85FD66c7"),
            address!("B97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E"),
            address!("9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7"),
        ],
        _ => &[],
    };
    tokens.contains(&token)
}
/// Canonical shared position managers. Their keys fund many users, so an
/// event-level owner is not an attributable searcher.
fn shared_position_manager(owner: Address) -> bool {
    owner == address!("C36442b4a4522E871399CD717aBDD847Ab11FE88")
}
fn canonical_factory(chain: u64) -> Option<Address> {
    match chain {
        1 => Some(address!("1F98431c8aD98523631AE4a59f267346ea31F984")),
        43114 => Some(address!("740b1c1de25031C31FF4fC9A62f554A55cdC1baD")),
        _ => None,
    }
}
fn calldata(signature: &str, args: &[u8]) -> Bytes {
    let mut v = keccak256(signature)[..4].to_vec();
    v.extend_from_slice(args);
    v.into()
}
fn position_call(owner: Address, lo: i32, hi: i32) -> Bytes {
    let mut packed = owner.as_slice().to_vec();
    packed.extend_from_slice(&lo.to_be_bytes()[1..]);
    packed.extend_from_slice(&hi.to_be_bytes()[1..]);
    calldata("positions(bytes32)", keccak256(packed).as_slice())
}

/// Per-block bounded cache including failures, keyed by block hash + call.
/// A hard deadline also caps provider retry time. Ordinary indexing continues.
pub struct ArchiveBudget<'a> {
    rpc: &'a crate::rpc::RpcClient,
    cache: HashMap<(B256, Address, Bytes), Option<Bytes>>,
    remaining: usize,
    deadline: tokio::time::Instant,
}
impl<'a> ArchiveBudget<'a> {
    pub fn new(rpc: &'a crate::rpc::RpcClient) -> Self {
        Self {
            rpc,
            cache: HashMap::new(),
            remaining: 24,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(3),
        }
    }
    async fn call(&mut self, hash: B256, to: Address, data: Bytes) -> Option<Bytes> {
        let key = (hash, to, data.clone());
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        if self.remaining == 0 || tokio::time::Instant::now() >= self.deadline {
            return None;
        }
        self.remaining -= 1;
        let deadline = self
            .deadline
            .min(tokio::time::Instant::now() + std::time::Duration::from_millis(700));
        let ret = tokio::time::timeout_at(deadline, self.rpc.call_at_hash(to, data, hash))
            .await
            .ok()
            .and_then(Result::ok);
        self.cache.insert(key, ret.clone());
        ret
    }
    async fn prove(&mut self, a: &mut JitAccounting, chain: u64, first: &LpBlock, last: &LpBlock) {
        let Some(factory) = canonical_factory(chain) else {
            a.reject("untrusted_v3_deployment");
            return;
        };
        // Trusted registry/deployment semantics, not a matching event signature:
        // ask the canonical factory for the registry token pair and the pool fee.
        let fee = self.call(last.hash, a.pool, calldata("fee()", &[])).await;
        let Some(fee) = fee.filter(|b| b.len() == 32 && b[..29].iter().all(|b| *b == 0)) else {
            a.reject("archive_or_pool_metadata_unavailable");
            return;
        };
        let mut args = vec![0u8; 96];
        args[12..32].copy_from_slice(a.token0.as_slice());
        args[44..64].copy_from_slice(a.token1.as_slice());
        args[64..].copy_from_slice(&fee);
        let registered = self
            .call(
                last.hash,
                factory,
                calldata("getPool(address,address,uint24)", &args),
            )
            .await;
        if !registered.is_some_and(|r| {
            r.len() == 32
                && r[..12].iter().all(|b| *b == 0)
                && Address::from_slice(&r[12..]) == a.pool
        }) {
            a.reject("untrusted_v3_registry_pool");
            return;
        }
        let Some(parent) = first.parent_hash else {
            a.reject("opening_parent_hash_unavailable");
            return;
        };
        let call = position_call(a.owner, a.tick_lower, a.tick_upper);
        for (hash, number) in [
            (parent, first.block_number.saturating_sub(1)),
            (last.hash, last.block_number),
        ] {
            let baseline = self
                .call(hash, a.pool, call.clone())
                .await
                .and_then(|b| PositionBaseline::decode(&b, hash, number));
            if let Some(b) = baseline {
                a.baselines.push(b);
            } else {
                a.reject("archive_positions_unavailable");
            }
        }
    }
}

pub async fn enrich(
    events: &mut [MevEvent],
    blocks: &[LpBlock],
    pools: &HashMap<Address, (Address, Address)>,
    chain: u64,
    window: u64,
    budget: &mut ArchiveBudget<'_>,
) {
    let Some(current) = blocks.last() else {
        return;
    };
    let mut usage: HashMap<B256, usize> = HashMap::new();
    let mut enriched = Vec::new();
    for (idx, ev) in events
        .iter_mut()
        .enumerate()
        .filter(|(_, e)| e.kind == MevKind::Jit)
    {
        let pool = ev.pools.first().copied().unwrap_or_default();
        let mut a =
            JitAccounting::unavailable(ev, pools.get(&pool).copied().unwrap_or_default(), "");
        if ev.details["bin_amm"].as_bool() == Some(true) {
            a.reject("unsupported_lb_accounting");
        }
        if !standard_token(chain, a.token0) || !standard_token(chain, a.token1) {
            a.reject("nonstandard_or_unsupported_tokens");
        }
        let open_block = ev.details["opened_block"]
            .as_u64()
            .unwrap_or(current.block_number);
        let mint_tx = ev.details["opening_tx_index"].as_u64();
        let mint_log = ev.details["opening_log_index"].as_u64();
        let first = blocks.iter().find(|b| b.block_number == open_block);
        let m = first.and_then(|b| {
            b.txs
                .iter()
                .flat_map(|t| t.facts.iter().map(move |f| (t, f)))
                .find(|(t, f)| {
                    f.is_mint
                        && matches(&a, f)
                        && mint_tx.is_none_or(|n| n == t.tx_index)
                        && mint_log.is_none_or(|n| n == f.log_index)
                })
        });
        let burn_tx = ev.details["burn_tx_index"].as_u64().unwrap_or(ev.tx_index);
        let burn_log = ev.details["burn_log_index"].as_u64();
        let burn = current
            .txs
            .iter()
            .flat_map(|t| t.facts.iter().map(move |f| (t, f)))
            .find(|(t, f)| {
                !f.is_mint
                    && !f.is_collect
                    && matches(&a, f)
                    && t.tx_index == burn_tx
                    && burn_log.is_none_or(|n| n == f.log_index)
            });
        if let (Some(first), Some((mt, m)), Some((bt, b))) = (first, m, burn) {
            ev.details["opening_tx_hash"] = serde_json::json!(mt.tx_hash);
            ev.details["opening_tx_index"] = serde_json::json!(mt.tx_index);
            ev.details["opening_log_index"] = serde_json::json!(m.log_index);
            if a.reasons.is_empty() {
                budget.prove(&mut a, chain, first, current).await;
            }
            reconcile(
                &mut a,
                blocks,
                (first.block_number, mt.tx_index, m.log_index),
                (current.block_number, bt.tx_index, b.log_index),
                window,
            );
            let mut owned = HashSet::new();
            owned.insert(mt.tx_hash);
            owned.insert(bt.tx_hash);
            for hash in owned {
                *usage.entry(hash).or_default() += 1;
            }
        } else {
            a.reject("incomplete_opening_or_closing_history");
        }
        enriched.push((idx, a));
    }
    for (idx, mut a) in enriched {
        if a.gas
            .iter()
            .any(|g| usage.get(&g.tx_hash).copied().unwrap_or_default() > 1)
        {
            a.reject("shared_transaction_cost_allocation");
        }
        let ev = &mut events[idx];
        ev.profit_tokens.clear();
        ev.profit_token = None;
        ev.profit_amount = None;
        ev.profit_usd = None;
        ev.gas_cost_wei = U256::ZERO;
        ev.details["jit_accounting"] = serde_json::to_value(a).unwrap_or_else(|e| {
            tracing::warn!("JIT accounting serialization failed: {e}");
            serde_json::Value::Null
        });
        ev.details["usd_approximate"] = serde_json::json!(true);
    }
}

/// Discover every timestamp before warming, including native gas. The native
/// marker maps to wrapped-native historical quotes at the ingestion boundary.
pub fn quote_requests(a: &JitAccounting) -> BTreeMap<u64, HashSet<Address>> {
    let mut requests: BTreeMap<u64, HashSet<Address>> = BTreeMap::new();
    for t in &a.timeline {
        if t.kind == "mint" || t.kind == "collect" {
            requests
                .entry(t.ts / 3600)
                .or_default()
                .extend([a.token0, a.token1]);
        }
    }
    for g in &a.gas {
        requests
            .entry(g.ts / 3600)
            .or_default()
            .insert(Address::ZERO);
    }
    requests
}

#[cfg(test)]
mod accounting_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]
    use super::*;
    use crate::data::{BlockData, ReceiptData, TxData};
    use crate::explorer::classify::{BlockContext, BlockInput, TxInput};
    use crate::explorer::types::{Amm, LegSource, SwapFact, TransferFact};
    use alloy::primitives::address;

    const OWNER: Address = address!("1111111111111111111111111111111111111111");
    const ROUTER: Address = address!("2222222222222222222222222222222222222222");
    const POOL: Address = address!("3333333333333333333333333333333333333333");
    /// `(block, tx_index, log_index)` for an opening/closing position edge.
    type PosRef = (u64, u64, u64);
    // Avalanche allowlisted tokens: USDC.e and USDT.e.
    const USD0: Address = address!("B97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E");
    const USD1: Address = address!("9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7");

    fn fact(
        is_mint: bool,
        is_collect: bool,
        tx_index: u64,
        log_index: u64,
        liquidity: u128,
        amount0: u64,
        amount1: u64,
    ) -> JitFact {
        JitFact {
            tx_index,
            log_index,
            pool: POOL,
            owner: OWNER,
            tick_lower: -100,
            tick_upper: 100,
            is_mint,
            is_collect,
            sender: is_mint.then_some(ROUTER),
            recipient: is_collect.then_some(OWNER),
            liquidity,
            amount0: U256::from(amount0),
            amount1: U256::from(amount1),
            bin_amm: false,
        }
    }

    fn transfer(
        tx_index: u64,
        log_index: u64,
        token: Address,
        from: Address,
        to: Address,
        amount: u64,
    ) -> TransferFact {
        TransferFact {
            tx_index,
            log_index,
            token,
            from,
            to,
            amount: U256::from(amount),
        }
    }

    fn gas(tx_hash: B256, ts: u64, cost: u64) -> Gas {
        Gas {
            tx_hash,
            ts,
            gas_used: "1".into(),
            effective_gas_price_wei: "1".into(),
            gas_cost_wei: cost.to_string(),
        }
    }

    fn base() -> JitAccounting {
        JitAccounting {
            version: 1,
            status: "unavailable".into(),
            method: "v3_settled_cashflows".into(),
            reasons: vec![],
            pool: POOL,
            owner: OWNER,
            beneficiary: None,
            token0: USD0,
            token1: USD1,
            tick_lower: -100,
            tick_upper: 100,
            mint_amount0: None,
            mint_amount1: None,
            burn_amount0: None,
            burn_amount1: None,
            collect_amount0: None,
            collect_amount1: None,
            fee_amount0: None,
            fee_amount1: None,
            cashflow_amount0: None,
            cashflow_amount1: None,
            timeline: vec![],
            gas: vec![],
            baselines: vec![],
            quotes: vec![],
            valuation: Valuation::default(),
            usd_approximate: true,
        }
    }

    fn block(number: u64, seed: u8, parent: Option<B256>, ts: u64, txs: Vec<LpTx>) -> LpBlock {
        LpBlock {
            block_number: number,
            hash: B256::repeat_byte(seed),
            parent_hash: parent,
            ts,
            complete: true,
            txs,
            swaps: vec![],
        }
    }

    fn tx(
        hash: B256,
        index: u64,
        facts: Vec<JitFact>,
        transfers: Vec<TransferFact>,
        gas: Option<Gas>,
    ) -> LpTx {
        LpTx {
            tx_hash: hash,
            tx_index: index,
            from: OWNER,
            to: Some(ROUTER),
            facts,
            transfers,
            swaps: vec![],
            gas,
            other_activity: false,
        }
    }

    fn happy_blocks() -> (Vec<LpBlock>, PosRef, PosRef) {
        let mint_tx = B256::repeat_byte(0x31);
        let close_tx = B256::repeat_byte(0x32);
        let open = block(
            10,
            0x10,
            Some(B256::repeat_byte(0x09)),
            1_000,
            vec![tx(
                mint_tx,
                1,
                vec![fact(true, false, 1, 5, 50, 1_000, 2_000)],
                vec![
                    transfer(1, 4, USD0, OWNER, POOL, 1_000),
                    transfer(1, 4, USD1, OWNER, POOL, 2_000),
                ],
                Some(gas(mint_tx, 1_000, 100)),
            )],
        );
        let middle = block(11, 0x11, Some(open.hash), 2_000, vec![]);
        let close = block(
            12,
            0x12,
            Some(middle.hash),
            9_000,
            vec![tx(
                close_tx,
                3,
                vec![
                    fact(false, false, 3, 2, 50, 1_000, 2_000),
                    fact(false, true, 3, 6, 0, 1_100, 2_200),
                ],
                vec![
                    transfer(3, 5, USD0, POOL, OWNER, 1_100),
                    transfer(3, 5, USD1, POOL, OWNER, 2_200),
                ],
                Some(gas(close_tx, 9_000, 50)),
            )],
        );
        let mut history = vec![open, middle, close];
        history[1].swaps.push(TickEvidence {
            tx_index: 0,
            log_index: 1,
            pool: POOL,
            tick: Some(0),
        });
        (history, (10, 1, 5), (12, 3, 2))
    }

    fn with_baselines(a: &mut JitAccounting, history: &[LpBlock]) {
        let first = history.first().unwrap();
        let last = history.last().unwrap();
        a.baselines = vec![
            PositionBaseline {
                block_hash: first.parent_hash.unwrap(),
                block_number: first.block_number - 1,
                liquidity: "0".into(),
                tokens_owed0: "0".into(),
                tokens_owed1: "0".into(),
            },
            PositionBaseline {
                block_hash: last.hash,
                block_number: last.block_number,
                liquidity: "0".into(),
                tokens_owed0: "0".into(),
                tokens_owed1: "0".into(),
            },
        ];
    }

    #[test]
    fn settled_cycle_reconciles_principal_and_fees() {
        let (history, opening, closing) = happy_blocks();
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "verified", "reasons={:?}", a.reasons);
        assert_eq!(a.fee_amount0.as_deref(), Some("100"));
        assert_eq!(a.fee_amount1.as_deref(), Some("200"));
        assert_eq!(a.cashflow_amount0.as_deref(), Some("100"));
        assert_eq!(a.cashflow_amount1.as_deref(), Some("200"));
        assert_eq!(a.gas.len(), 2, "one gas entry per owned tx, not per fact");
    }

    #[test]
    fn collect_below_burn_principal_is_unavailable_not_zero() {
        let (mut history, opening, closing) = happy_blocks();
        history[2].txs[0].facts[1].amount0 = U256::from(900);
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "unavailable");
        assert!(a
            .reasons
            .iter()
            .any(|r| r == "collect_below_burn_principal"));
        assert_eq!(a.fee_amount0, None);
    }

    #[test]
    fn partial_burn_is_unavailable() {
        let (mut history, opening, closing) = happy_blocks();
        history[2].txs[0].facts[0].liquidity = 25;
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "unavailable");
        assert!(a.reasons.iter().any(|r| r == "partial_burn"));
    }

    #[test]
    fn shared_manager_owner_is_unavailable() {
        let (mut history, opening, closing) = happy_blocks();
        history[0].txs[0].from = ROUTER;
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "unavailable");
        assert!(a
            .reasons
            .iter()
            .any(|r| r == "shared_manager_or_unprovable_beneficiary"));
    }

    #[test]
    fn lb_bin_cycle_is_unavailable() {
        let (mut history, opening, closing) = happy_blocks();
        history[0].txs[0].facts[0].bin_amm = true;
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "unavailable");
        assert!(a.reasons.iter().any(|r| r == "unsupported_lb_accounting"));
    }

    #[test]
    fn missing_in_lifetime_swap_is_unavailable() {
        let (mut history, opening, closing) = happy_blocks();
        history[1].swaps.clear();
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "unavailable");
        assert!(a
            .reasons
            .iter()
            .any(|r| r == "no_in_lifetime_tick_evidence"));
    }

    /// Only swaps strictly between the opening Mint and closing Burn, on the
    /// JIT pool, and inside the half-open tick range prove the position ever
    /// saw traffic. Endpoints and after-close swaps must not count.
    #[test]
    fn only_strictly_in_lifetime_in_range_swaps_count_as_evidence() {
        let cases: &[(usize, u64, u64, u64, i32, &str)] = &[
            (1, 11, 0, 1, 0, "strictly inside"),
            (1, 11, 0, 1, 100, "upper tick endpoint"),
            (0, 10, 1, 5, 0, "opening endpoint"),
            (2, 12, 3, 2, 0, "closing endpoint"),
            (2, 12, 3, 3, 0, "after closing"),
        ];
        for (block_pos, block_no, tx_index, log_index, tick, name) in cases {
            let (mut history, opening, closing) = happy_blocks();
            history[1].swaps.clear();
            history[*block_pos].swaps.push(TickEvidence {
                tx_index: *tx_index,
                log_index: *log_index,
                pool: POOL,
                tick: Some(*tick),
            });
            let mut a = base();
            with_baselines(&mut a, &history);
            reconcile(&mut a, &history, opening, closing, 1_000);
            assert_eq!(*block_no, history[*block_pos].block_number, "{name}");
            if *name == "strictly inside" {
                assert_eq!(a.status, "verified", "{name}: reasons={:?}", a.reasons);
            } else {
                assert_eq!(a.status, "unavailable", "{name}");
                assert!(
                    a.reasons
                        .iter()
                        .any(|r| r == "no_in_lifetime_tick_evidence"),
                    "{name}: reasons={:?}",
                    a.reasons
                );
            }
        }
    }

    /// A Mint, Burn and Collect in one transaction is still a settled cycle:
    /// receipt order `mint < burn < collect` is enough, and gas is counted once.
    #[test]
    fn same_transaction_cycle_reconciles() {
        let hash = B256::repeat_byte(0x41);
        let open = block(
            20,
            0x20,
            Some(B256::repeat_byte(0x1f)),
            1_000,
            vec![tx(
                hash,
                2,
                vec![
                    fact(true, false, 2, 1, 50, 1_000, 2_000),
                    fact(false, false, 2, 3, 50, 1_000, 2_000),
                    fact(false, true, 2, 5, 0, 1_100, 2_200),
                ],
                vec![
                    transfer(2, 0, USD0, OWNER, POOL, 1_000),
                    transfer(2, 0, USD1, OWNER, POOL, 2_000),
                    transfer(2, 4, USD0, POOL, OWNER, 1_100),
                    transfer(2, 4, USD1, POOL, OWNER, 2_200),
                ],
                Some(gas(hash, 1_000, 100)),
            )],
        );
        let mut history = vec![open];
        history[0].swaps.push(TickEvidence {
            tx_index: 2,
            log_index: 2,
            pool: POOL,
            tick: Some(0),
        });
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, (20, 2, 1), (20, 2, 3), 1_000);
        assert_eq!(a.status, "verified", "reasons={:?}", a.reasons);
        assert_eq!(a.fee_amount0.as_deref(), Some("100"));
        assert_eq!(a.fee_amount1.as_deref(), Some("200"));
        assert_eq!(a.gas.len(), 1, "one receipt, one gas entry");
    }

    /// The production path must accept a cycle whose validating swap is in the
    /// same receipt: `LpBlock::from_input` records the swap and transfer legs
    /// between Mint and Burn instead of rejecting the transaction as shared.
    #[test]
    fn same_tx_cycle_from_receipt_input_reconciles() {
        let block_hash = B256::repeat_byte(0xaa);
        let parent = B256::repeat_byte(0x99);
        let tx_hash = B256::repeat_byte(0xbb);
        let mut mint = fact(true, false, 0, 1, 50, 1_000, 2_000);
        mint.sender = Some(OWNER);
        let tx_input = TxInput {
            tx_index: 0,
            tx_hash,
            from: OWNER,
            to: Some(POOL),
            success: true,
            gas_used: 100,
            effective_gas_price_gwei: 1.0,
            value: U256::ZERO,
            transfers: vec![
                transfer(0, 0, USD0, OWNER, POOL, 1_000),
                transfer(0, 0, USD1, OWNER, POOL, 2_000),
                transfer(0, 2, USD0, OWNER, POOL, 10),
                transfer(0, 3, USD1, POOL, OWNER, 20),
                transfer(0, 5, USD0, POOL, OWNER, 1_100),
                transfer(0, 5, USD1, POOL, OWNER, 2_200),
            ],
            swaps: vec![SwapFact {
                tx_index: 0,
                log_index: 3,
                pool: POOL,
                amm: Amm::V3,
                token_in: USD0,
                token_out: USD1,
                amount_in: U256::from(10u64),
                amount_out: U256::from(20u64),
                token_source: LegSource::Registry,
                tick: Some(0),
                owner: Some(OWNER),
            }],
            jit: vec![
                mint,
                fact(false, false, 0, 4, 50, 1_000, 2_000),
                fact(false, true, 0, 6, 0, 1_100, 2_200),
            ],
            ..Default::default()
        };
        let input = BlockInput {
            block: 30,
            ts: 1_000,
            ctx: BlockContext::for_tests(),
            open_positions: vec![],
            interest_lookback: Default::default(),
            prior_oracle_answers: Default::default(),
            savax_exchange_rate_wad: None,
            tip_sinks: Default::default(),
            txs: vec![tx_input],
        };
        let header = BlockData {
            number: 30,
            hash: block_hash,
            timestamp: 1_000,
            base_fee_per_gas: Some(1),
            gas_limit: 30_000_000,
            gas_used: 100,
            coinbase: Address::ZERO,
            difficulty: U256::ZERO,
            mix_hash: B256::ZERO,
        };
        let tx = TxData {
            hash: tx_hash,
            index: 0,
            tx_type: 2,
            from: OWNER,
            to: Some(POOL),
            input: Bytes::new(),
            value: U256::ZERO,
            gas_limit: 100,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: Some(1),
            gas_price: None,
            nonce: 0,
            access_list: vec![],
            authorization_list: vec![],
        };
        let receipt = ReceiptData {
            block_hash: Some(block_hash),
            tx_hash,
            tx_index: 0,
            status: true,
            gas_used: 100,
            cumulative_gas_used: 100,
            effective_gas_price: Some(1),
            logs: vec![],
            contract_address: None,
        };
        let mut journal = LpBlock::from_input(&input, &header, &[tx], &[receipt]);
        assert!(journal.complete);
        assert_eq!(journal.txs[0].swaps.len(), 1);
        journal.parent_hash = Some(parent);
        let mut a = base();
        with_baselines(&mut a, std::slice::from_ref(&journal));
        reconcile(
            &mut a,
            std::slice::from_ref(&journal),
            (30, 0, 1),
            (30, 0, 4),
            1_000,
        );
        assert_eq!(a.status, "verified", "reasons={:?}", a.reasons);
        assert_eq!(a.fee_amount0.as_deref(), Some("100"));
        assert_eq!(a.fee_amount1.as_deref(), Some("200"));
        assert_eq!(a.gas.len(), 1);
    }

    /// Fees come only from this position's own Collect. Another LP's activity
    /// in the same pool must not change the JIT's reconciled fee or gas.
    #[test]
    fn competitor_lp_activity_does_not_inflate_fees() {
        let (mut history, opening, closing) = happy_blocks();
        let mut competitor = fact(true, false, 2, 1, 40, 9_999, 9_999);
        competitor.owner = ROUTER;
        history[1].txs.push(tx(
            B256::repeat_byte(0x51),
            2,
            vec![competitor],
            vec![],
            None,
        ));
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "verified", "reasons={:?}", a.reasons);
        assert_eq!(a.fee_amount0.as_deref(), Some("100"));
        assert_eq!(a.fee_amount1.as_deref(), Some("200"));
        assert_eq!(a.gas.len(), 2, "competitor gas is not attributed");
    }

    /// A real loss on both tokens stays `verified` and reports negative
    /// cashflows instead of being zeroed or dropped.
    #[test]
    fn two_token_loss_is_reconciled_not_hidden() {
        let (mut history, opening, closing) = happy_blocks();
        history[2].txs[0].facts[0].amount0 = U256::from(500);
        history[2].txs[0].facts[0].amount1 = U256::from(1_000);
        history[2].txs[0].facts[1].amount0 = U256::from(600);
        history[2].txs[0].facts[1].amount1 = U256::from(1_100);
        history[2].txs[0].transfers[0].amount = U256::from(600);
        history[2].txs[0].transfers[1].amount = U256::from(1_100);
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "verified", "reasons={:?}", a.reasons);
        assert_eq!(a.cashflow_amount0.as_deref(), Some("-400"));
        assert_eq!(a.cashflow_amount1.as_deref(), Some("-900"));
        assert_eq!(a.fee_amount0.as_deref(), Some("100"));
        assert_eq!(a.fee_amount1.as_deref(), Some("100"));
    }

    /// Missing archive proof (a baseline) is unavailable, never assumed empty.
    #[test]
    fn missing_archive_baseline_is_unavailable() {
        let (history, opening, closing) = happy_blocks();
        let mut a = base();
        with_baselines(&mut a, &history);
        a.baselines.pop();
        reconcile(&mut a, &history, opening, closing, 1_000);
        assert_eq!(a.status, "unavailable");
        assert!(a
            .reasons
            .iter()
            .any(|r| r == "archive_baseline_or_settlement_unavailable"));
    }

    fn verified_for_pricing() -> JitAccounting {
        let (history, opening, closing) = happy_blocks();
        let mut a = base();
        with_baselines(&mut a, &history);
        reconcile(&mut a, &history, opening, closing, 1_000);
        a.status = "verified".into();
        a
    }

    #[test]
    fn valuation_uses_each_cashflows_own_hour_and_counts_gas_once() {
        let mut a = verified_for_pricing();
        let h_mint = 1_000u64 / 3600;
        let h_close = 9_000u64 / 3600;
        a.quotes = vec![
            Quote {
                token: USD0,
                hour: h_mint,
                usd: 1.0,
                decimals: 6,
                source: "t".into(),
            },
            Quote {
                token: USD1,
                hour: h_mint,
                usd: 1.0,
                decimals: 6,
                source: "t".into(),
            },
            // The closing hour is priced at 2x.
            Quote {
                token: USD0,
                hour: h_close,
                usd: 2.0,
                decimals: 6,
                source: "t".into(),
            },
            Quote {
                token: USD1,
                hour: h_close,
                usd: 2.0,
                decimals: 6,
                source: "t".into(),
            },
            Quote {
                token: Address::ZERO,
                hour: h_mint,
                usd: 1.0,
                decimals: 18,
                source: "t".into(),
            },
            Quote {
                token: Address::ZERO,
                hour: h_close,
                usd: 1.0,
                decimals: 18,
                source: "t".into(),
            },
        ];
        let v = evaluate(&a);
        let expected_gross = -3_000.0 / 1e6 + 6_600.0 / 1e6;
        assert!(
            (v.gross_usd.unwrap() - expected_gross).abs() < 1e-12,
            "{:?}",
            v.gross_usd
        );
        // Two owned txs: 100 wei + 50 wei at $1 per 1e18 wei.
        let expected_gas = 150.0 / 1e18;
        assert!(
            (v.gas_usd.unwrap() - expected_gas).abs() < 1e-24,
            "{:?}",
            v.gas_usd
        );
        assert!(v.pricing_complete);
    }

    #[test]
    fn missing_component_price_makes_totals_null() {
        let mut a = verified_for_pricing();
        a.quotes.clear();
        let h = 1_000u64 / 3600;
        a.quotes.push(Quote {
            token: USD0,
            hour: h,
            usd: 1.0,
            decimals: 6,
            source: "t".into(),
        });
        let v = evaluate(&a);
        assert_eq!(v.gross_usd, None, "a partial sum is not gross");
        assert_eq!(v.net_profit_usd, None);
        assert!(!v.pricing_complete);
    }
}
