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
//!    same tx also closed an arb cycle.
//! 7. `Utility` covers exact-fingerprint operational executions (keeper,
//!    airdrop claim-and-sell, ERC-4337 bundler). No residual-inference pass
//!    exists; a future one gets its own kind, never this bucket.
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, B256, U256};

use crate::explorer::decode;
use crate::explorer::interest_attr;
use crate::explorer::profit::ProfitTokenPolicy;
use crate::explorer::store::OpenPosition;
use crate::explorer::strategy_tags::{self, ArbTagOpts};
use crate::explorer::types::{
    AggregatorSignal, BuyCollateralFact, Confidence, EpochRewardFact, GmxEventFact, JitFact,
    KeeperFact, MevEvent, MevKind, OracleUpdateFact, PnlBasis, RateCacheFact, ReserveDataFact,
    SwapFact, TransferFact, UserOpFact, V2PairOpFact,
};

mod arb;
mod causal;
mod claims;
mod drops;
mod gas;
mod jit;
mod legs;
mod liquidation;
mod sandwich;
mod skim;

use arb::classify_arbs;
use causal::{classify_backruns, classify_frontruns};
use jit::classify_jit;
use liquidation::{classify_liquidations, LiqBlockCtx};
use sandwich::classify_sandwiches;
use skim::classify_skims;

pub(crate) use arb::flow_attributable;
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
    /// Aggregator/router intent signals (side-channel; survives swap dedup).
    pub aggregator_signals: Vec<AggregatorSignal>,
}

/// One block's classified input.
pub struct BlockInput {
    pub block: u64,
    pub ts: u64,
    pub wrapped_native: Address,
    pub profit_policy: ProfitTokenPolicy,
    /// Cross-block JIT open Mint positions, loaded from the store
    /// window for the current block.
    pub open_positions: Vec<OpenPosition>,
    /// Registry addresses that expose UniV2-style `skim` (V2/Solidly/Camelot).
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
    /// Addresses that must not be attributed as searcher profit (block
    /// coinbase + optional config tip sinks). Forwarded residual landing
    /// solely here is dropped so validator tips do not inflate PnL.
    pub tip_sinks: HashSet<Address>,
    pub txs: Vec<TxInput>,
}

/// Output of [`classify_block`]: realized events plus classify-side drops.
#[derive(Debug, Clone, Default)]
pub struct ClassifyOutput {
    pub events: Vec<MevEvent>,
    pub rejects: Vec<crate::explorer::RejectedCandidate>,
}

impl std::ops::Deref for ClassifyOutput {
    type Target = [MevEvent];
    fn deref(&self) -> &Self::Target {
        &self.events
    }
}

impl IntoIterator for ClassifyOutput {
    type Item = MevEvent;
    type IntoIter = std::vec::IntoIter<MevEvent>;
    fn into_iter(self) -> Self::IntoIter {
        self.events.into_iter()
    }
}

/// Classify one block. Sandwich bundle details are folded into `details_json`
/// per event. Dropped near-miss arb cycles land in [`ClassifyOutput::rejects`].
pub fn classify_block(input: &BlockInput) -> ClassifyOutput {
    let mut events: Vec<MevEvent> = Vec::new();
    let mut rejects: Vec<crate::explorer::RejectedCandidate> = Vec::new();

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
    let liq_ctx = LiqBlockCtx {
        oracle_feeds_in_block: &oracle_feeds_in_block,
        block_oracle_owned: &block_oracle_owned,
        block_reserve_owned: &block_reserve_owned,
    };
    let mut drops = drops::DropSink::new(input.block);
    events.extend(classify_liquidations(input, &liq_ctx, &mut drops));

    // ── 1b. Skim pass (exact, UniV2 pair outbound without Swap/Mint/Burn/Sync)
    events.extend(classify_skims(input, &mut drops));

    // ── 2-3. Swap attribution + atomic arb (+ keeper/claim/bundler/upgrade)
    let (arb_events, arb_rejects) = classify_arbs(input, &arb_tag_opts);
    events.extend(arb_events);
    rejects.extend(arb_rejects);

    // ── 5b. JIT block-wide pairing ──────────────────────────────────────
    let mut jit_events = classify_jit(input, &mut drops);
    // jit_arb when a JIT tx also produced an arb in this block. The standalone
    // ArbAtomic is suppressed so USD/P&L never double-counts the same flow.
    //

    // ⚠️ LOAD-BEARING: this is a kind *upgrade*, not a standalone pass, and it is
    // independent of the live detectors. The `Strategy::JitArb` variant and the
    // `JitArbDetector` were removed from the execution path because that
    // detector had no honest P&L model — but this branch must survive.
    // Dropping either half corrupts
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
    let mut sandwiches = classify_sandwiches(input, &mut drops);

    // ── 3c. Causal backrun / frontrun ─────────────────────────
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
    let mut frontruns = classify_frontruns(input, &consumed, &mut drops);
    let frontrun_consumed: HashSet<u64> = frontruns.iter().map(|e| e.tx_index).collect();
    let mut back_consumed = consumed;
    back_consumed.extend(frontrun_consumed);
    let mut backruns = classify_backruns(input, &back_consumed, &mut drops);
    // TxClaims is the authority for "one tx index, one P&L-bearing kind".
    // Passes still over-emit; sandwich back-run txs and frontrun close txs
    // are claimed even when the event itself is anchored on the other leg,
    // so an ArbAtomic on that leg cannot survive. `consumed` above stays as
    // an input prune. The Jit→JitArb upgrade above stays too: it is a kind
    // change, not a second event.
    let mut claims = claims::TxClaims::default();
    for ev in events
        .iter()
        .chain(frontruns.iter())
        .chain(backruns.iter())
        .chain(sandwiches.iter())
    {
        claims.claim(ev.tx_index, ev.kind);
    }
    for s in &sandwiches {
        if let Some(b) = s.details.get("backrun_tx_index").and_then(|v| v.as_u64()) {
            claims.claim(b, MevKind::Sandwich);
        }
    }
    for e in &frontruns {
        if let Some(c) = e.details.get("close_tx_index").and_then(|v| v.as_u64()) {
            claims.claim(c, MevKind::Frontrun);
        }
    }
    events.append(&mut frontruns);
    events.append(&mut backruns);
    events.append(&mut sandwiches);
    events.retain(|e| claims.is_claimed(e.tx_index) && claims.winner(e.tx_index) == Some(e.kind));

    events.sort_by(|a, b| {
        a.tx_index
            .cmp(&b.tx_index)
            .then_with(|| kind_precedence(a.kind).cmp(&kind_precedence(b.kind)))
    });
    normalize_confidence(&mut events);
    rejects.extend(drops.into_rejects());
    ClassifyOutput { events, rejects }
}

/// A19: `Confidence::Exact` means the P&L figure is a **realized ledger
/// residual** (`details.pnl_basis = "R"`) with a non-empty amount and no
/// recorded reconcile failure. Every other basis — gross amount diff, oracle /
/// seized collateral, fee/reward fields, gas-cost-as-profit — and any
/// amount-less event is demoted to `Inferred`. Detection is unchanged; only
/// the confidence claim moves.
pub(crate) fn normalize_confidence(events: &mut [MevEvent]) {
    for e in events {
        if e.confidence != Confidence::Exact {
            continue;
        }
        let basis_exact = e
            .details
            .get("pnl_basis")
            .and_then(|v| v.as_str())
            .is_some_and(|b| b == PnlBasis::R.as_str());
        let reconciled = !e
            .details
            .get("reasons")
            .and_then(|r| r.as_array())
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("TRANSFER_MISMATCH")));
        let has_pnl = e.profit_amount.is_some_and(|a| !a.is_zero()) || !e.profit_tokens.is_empty();
        if !(basis_exact && reconciled && has_pnl) {
            e.confidence = Confidence::Inferred;
        }
    }
}

/// Kind priority for [`claims::TxClaims`] (lower = higher priority).
///
/// Sandwich > Frontrun > Backrun > JitArb > Liquidation > Jit > Skim >
/// ArbAtomic > Utility. Encodes the A15 census decisions: the indexed
/// corpus had zero co-labeled `(block, tx_index)` pairs, so demoting
/// `ArbAtomic` under the three explicitly anchored kinds is a no-op on that
/// data and must not move goldens. A real collision keeps the explicit
/// anchor (liquidation / mint-burn / pair outflow) and drops the inferred cycle.
pub fn kind_precedence(k: MevKind) -> u8 {
    match k {
        MevKind::Sandwich => 0,
        MevKind::Frontrun => 1,
        MevKind::Backrun => 2,
        MevKind::JitArb => 3,
        MevKind::Liquidation => 4,
        MevKind::Jit => 5,
        MevKind::Skim => 6,
        MevKind::ArbAtomic => 7,
        MevKind::Utility => 8,
    }
}

pub(super) fn append_tag(details: &mut serde_json::Value, tag: &str) {
    let arr = details
        .as_object_mut()
        .map(|o| o.entry("tags").or_insert_with(|| serde_json::json!([])));
    if let Some(serde_json::Value::Array(tags)) = arr {
        if !tags.iter().any(|t| t.as_str() == Some(tag)) {
            tags.push(serde_json::json!(tag));
        }
    }
}

/// Decoded facts for one transaction (named fields — avoid fragile tuples).
#[derive(Debug, Clone, Default)]
pub struct DecodedTxFacts {
    pub transfers: Vec<TransferFact>,
    pub swaps: Vec<SwapFact>,
    pub liquidations: Vec<crate::explorer::types::LiquidationFact>,
    pub flashloans: Vec<crate::explorer::types::FlashLoanFact>,
    pub buy_collaterals: Vec<BuyCollateralFact>,
    pub jit: Vec<JitFact>,
    pub v2_pair_ops: Vec<V2PairOpFact>,
    pub oracle_updates: Vec<OracleUpdateFact>,
    pub reserve_updates: Vec<ReserveDataFact>,
    pub rate_cache_updates: Vec<RateCacheFact>,
    pub keepers: Vec<KeeperFact>,
    pub epoch_rewards: Vec<EpochRewardFact>,
    pub gmx_events: Vec<GmxEventFact>,
    pub user_ops: Vec<UserOpFact>,
    pub aggregator_signals: Vec<AggregatorSignal>,
}

fn push_swap(facts: &mut DecodedTxFacts, tx_index: u64, log_idx: u64, log: &crate::data::LogData) {
    if let Some((amm, mut s)) = decode::decode_swap(log) {
        s.tx_index = tx_index;
        s.log_index = log_idx;
        s.amm = amm;
        facts.swaps.push(s);
    }
}

/// Decode a tx's receipt logs into facts (used by ingest / scenarios).
/// `pool_tokens` = pool → (token0, token1) registry for swap direction (1.1).
/// `liquidation_protocol_aliases` remaps shared-topic0 emitters (Spark, Benqi).
pub(crate) fn decode_tx_logs_with_aliases(
    tx_index: u64,
    logs: &[crate::data::LogData],
    pool_tokens: &HashMap<Address, (Address, Address)>,
    liquidation_protocol_aliases: &HashMap<Address, &'static str>,
) -> DecodedTxFacts {
    let mut facts = DecodedTxFacts::default();
    for (log_idx, log) in logs.iter().enumerate() {
        let log_idx = log_idx as u64;
        if let Some(&topic0) = log.topics.first() {
            match decode::log_family(topic0) {
                Some(decode::LogFamily::Transfer) => {
                    if let Some(mut t) = decode::decode_transfer(log) {
                        t.tx_index = tx_index;
                        t.log_index = log_idx;
                        facts.transfers.push(t);
                    }
                }
                // Signal before the swap edge so brand survives dedup.
                Some(decode::LogFamily::Aggregator) => {
                    if let Some(mut sig) = decode::decode_aggregator_signal(log) {
                        sig.log_index = log_idx;
                        facts.aggregator_signals.push(sig);
                    }
                    push_swap(&mut facts, tx_index, log_idx, log);
                }
                Some(decode::LogFamily::Swap) => push_swap(&mut facts, tx_index, log_idx, log),
                Some(decode::LogFamily::Liquidation) => {
                    if let Some(mut l) = decode::decode_liquidation(log) {
                        l.tx_index = tx_index;
                        l.log_index = log_idx;
                        decode::remap_liquidation_protocol(&mut l, liquidation_protocol_aliases);
                        facts.liquidations.push(l);
                    }
                }
                Some(decode::LogFamily::FlashLoan) => {
                    if let Some(mut f) = decode::decode_flash_loan(log) {
                        f.tx_index = tx_index;
                        f.log_index = log_idx;
                        facts.flashloans.push(f);
                    }
                }
                Some(decode::LogFamily::BuyCollateral) => {
                    if let Some(mut b) = decode::decode_buy_collateral(log) {
                        b.tx_index = tx_index;
                        b.log_index = log_idx;
                        facts.buy_collaterals.push(b);
                    }
                }
                Some(decode::LogFamily::V3Liquidity) => {
                    if let Some(mut j) = decode::decode_v3_mint_burn(log) {
                        j.tx_index = tx_index;
                        j.log_index = log_idx;
                        facts.jit.push(j);
                    }
                }
                Some(decode::LogFamily::LbLiquidity) => {
                    if let Some(mut j) = decode::decode_lb_bins_liquidity(log) {
                        j.tx_index = tx_index;
                        j.log_index = log_idx;
                        facts.jit.push(j);
                    }
                }
                Some(decode::LogFamily::V2PairOp) => {
                    if let Some(mut op) = decode::decode_v2_pair_op(log) {
                        op.tx_index = tx_index;
                        op.log_index = log_idx;
                        facts.v2_pair_ops.push(op);
                    }
                }
                Some(decode::LogFamily::Oracle) => {
                    if let Some(mut o) = decode::decode_oracle_update(log) {
                        o.tx_index = tx_index;
                        o.log_index = log_idx;
                        facts.oracle_updates.push(o);
                    }
                }
                Some(decode::LogFamily::Reserve) => {
                    if let Some(mut r) = decode::decode_reserve_data(log) {
                        r.tx_index = tx_index;
                        r.log_index = log_idx;
                        facts.reserve_updates.push(r);
                    }
                }
                Some(decode::LogFamily::RateCache) => {
                    if let Some(mut rc) = decode::decode_rate_cache_update(log) {
                        rc.tx_index = tx_index;
                        rc.log_index = log_idx;
                        facts.rate_cache_updates.push(rc);
                    }
                }
                Some(decode::LogFamily::Keeper) => {
                    if let Some(mut k) = decode::decode_keeper(log) {
                        k.tx_index = tx_index;
                        k.log_index = log_idx;
                        facts.keepers.push(k);
                    }
                }
                Some(decode::LogFamily::EpochReward) => {
                    if let Some(mut e) = decode::decode_epoch_reward(log) {
                        e.tx_index = tx_index;
                        e.log_index = log_idx;
                        facts.epoch_rewards.push(e);
                    }
                }
                Some(decode::LogFamily::UserOp) => {
                    if let Some(mut u) = decode::decode_user_op(log) {
                        u.tx_index = tx_index;
                        u.log_index = log_idx;
                        facts.user_ops.push(u);
                    }
                }
                None => {}
            }
        }
        // GMX EventEmitter keys eventNameHash on topics[1]; topic0 is not stable.
        if let Some(mut g) = decode::decode_gmx_event(log) {
            g.tx_index = tx_index;
            g.log_index = log_idx;
            facts.gmx_events.push(g);
        }
    }
    decode::attach_swap_tokens(&mut facts.swaps, &facts.transfers, pool_tokens);
    // drop aggregator edges already covered by DEX swap edges.
    decode::dedup_aggregator_facts(&mut facts.swaps);
    facts
}

/// True when a swap fact has unresolved direction tokens (V3 sentinel kept).
pub(crate) fn has_unresolved_tokens(s: &SwapFact) -> bool {
    decode::is_unresolved_token(s.token_in) || decode::is_unresolved_token(s.token_out)
}

/// Drop a `Utility` event that is not one of the three fingerprints and has
/// no positive profit amount. The three families stay even at zero profit.
pub(crate) fn filter_unresolved(events: Vec<MevEvent>) -> Vec<MevEvent> {
    events
        .into_iter()
        .filter(|e| {
            if e.kind != MevKind::Utility {
                return true;
            }
            if crate::explorer::types::utility_subkind(&e.details).is_some() {
                return true;
            }
            e.profit_amount.map(|a| !a.is_zero()).unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::explorer::ingest::IngestConfig;
    use crate::explorer::store::OpenPosition;
    use crate::explorer::test_fixtures::{
        block, block_with_v2, classify_kind, event_of, flash_loan, jit, kinds, liquidation, swap,
        swap_at_tick, swap_owned, transfer, tx, tx_to, ATK, POOL_A, POOL_B, TOKA, USDC, VICTIM,
        WNATIVE,
    };
    use crate::explorer::types::{
        utility_subkind, Confidence, JitFact, KeeperFact, MevKind, UserOpFact, V2PairOpFact,
        V2PairOpKind,
    };
    use crate::types::ChainName;
    use alloy::primitives::{address, b256, Address, U256};

    /// A future pass that dumps an untagged event into `Utility` fails here
    /// instead of silently recreating the old mystery bucket.
    #[test]
    fn every_utility_event_has_exactly_one_subkind() {
        let mut keeper_tx = tx(0, ATK, true, vec![], vec![]);
        keeper_tx.keepers = vec![KeeperFact {
            tx_index: 0,
            log_index: 0,
            protocol: "chainlink",
            emitter: ATK,
            fee: None,
            fee_token: None,
        }];
        let mut bundler_tx = tx(1, ATK, true, vec![], vec![]);
        bundler_tx.user_ops = vec![UserOpFact {
            tx_index: 1,
            log_index: 0,
            entry_point: ATK,
            sender: VICTIM,
            paymaster: Address::ZERO,
            actual_gas_cost: U256::from(1u64),
            success: true,
        }];
        let out = classify_block(&block(vec![keeper_tx, bundler_tx]));
        let utilities: Vec<_> = out
            .events
            .iter()
            .filter(|e| e.kind == MevKind::Utility)
            .collect();
        assert_eq!(utilities.len(), 2, "keeper and bundler must both emit");
        for ev in utilities {
            let n = ev
                .details
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter(|t| {
                            matches!(
                                t.as_str(),
                                Some("keeper") | Some("bundler") | Some("claim_and_sell")
                            )
                        })
                        .count()
                })
                .unwrap_or(0);
            assert_eq!(n, 1, "exactly one utility family tag: {:?}", ev.details);
            assert!(
                matches!(
                    utility_subkind(&ev.details),
                    Some("keeper") | Some("bundler") | Some("claim_and_sell")
                ),
                "untagged Utility is the old mystery bucket: {:?}",
                ev.details
            );
        }
    }

    #[test]
    fn arb_cycle_single_flow_owner_is_exact() {
        // Both legs funded by the searcher itself → attributable to its route
        //: cycle stays Exact.
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
    fn arb_cycle_mixed_flow_owner_is_dropped() {
        // The second leg of the closed cycle is funded by an unrelated actor
        // inside the same tx — flow is not attributable → no ArbAtomic.
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
        let events = classify_block(&input);
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "cycle mixes unrelated owners, got {events:?}"
        );
    }

    #[test]
    fn arb_cycle_owned_only_by_non_candidate_is_dropped() {
        // A closed cycle whose legs are all funded by an address that is not a
        // searcher candidate is not attributable → no ArbAtomic.
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
        let events = classify_block(&input);
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "flow owner is not a candidate, got {events:?}"
        );
    }

    #[test]
    fn arb_cycle_pool_funded_intermediate_is_attributable() {
        // Intermediate token moves pool→pool (not through the searcher), so the
        // second leg's funder-owner is POOL_A — a pool of this very route. That
        // owner is neutral; the external owner (ATK) still attributes the
        // cycle. Regression: 5 mev.zone atomic arbs dropped by the old
        // "every owner must equal the searcher" rule.
        let swaps = vec![
            swap_owned(ATK, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(POOL_A, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, POOL_B, 200),
            transfer(2, USDC, POOL_B, ATK, 110),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        assert_eq!(ev.confidence, Confidence::Exact);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(10)));
    }

    #[test]
    fn arb_cycle_pool_and_outsider_owner_is_dropped() {
        // A neutral route-pool owner must not mask an unrelated external
        // owner: with owners {ATK, POOL_A (route pool), OUTSIDER}, the external
        // owners {ATK, OUTSIDER} do not collapse → no ArbAtomic.
        const OUTSIDER: Address = address!("7000000000000000000000000000000000000013");
        const POOL_C: Address = address!("300000000000000000000000000000000000000c");
        let swaps = vec![
            swap_owned(ATK, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(POOL_A, POOL_B, TOKA, WNATIVE, 200, 30),
            swap_owned(OUTSIDER, POOL_C, WNATIVE, USDC, 30, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, POOL_B, 200),
            transfer(2, WNATIVE, POOL_B, POOL_C, 30),
            transfer(3, USDC, POOL_C, ATK, 110),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let events = classify_block(&input);
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "an unrelated external owner must still reject the cycle, got {events:?}"
        );
    }

    #[test]
    fn arb_cycle_net_zero_searcher_forwards_residual_to_third_party_is_inferred() {
        // Searcher contract runs the cycle, nets zero, forwards residual to a
        // single non-swap third party → ArbAtomic Inferred + forwarded_profit.
        const SEARCHER: Address = address!("9000000000000000000000000000000000000009");
        const RECIPIENT: Address = address!("a00000000000000000000000000000000000000a");
        let swaps = vec![
            swap_owned(SEARCHER, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(SEARCHER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, SEARCHER, POOL_A, 100),
            transfer(1, TOKA, POOL_A, SEARCHER, 200),
            transfer(2, TOKA, SEARCHER, POOL_B, 200),
            transfer(3, USDC, POOL_B, SEARCHER, 110),
            transfer(4, USDC, SEARCHER, RECIPIENT, 10),
        ];
        let input = block(vec![tx_to(0, ATK, SEARCHER, true, swaps, transfers)]);
        let ev = classify_kind(&input, MevKind::ArbAtomic);
        assert_eq!(ev.searcher, RECIPIENT);
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(ev.profit_amount, Some(U256::from(10)));
        assert_eq!(ev.contract, Some(SEARCHER));
        let tags = ev.details["tags"].as_array().unwrap();
        assert!(tags.iter().any(|t| t.as_str() == Some("forwarded_profit")));
        let forwarded_from = format!("{SEARCHER:#x}");
        assert_eq!(
            ev.details["forwarded_from"].as_str(),
            Some(forwarded_from.as_str())
        );
    }

    #[test]
    fn arb_cycle_forwarded_to_tip_sink_is_dropped() {
        const SEARCHER: Address = address!("9000000000000000000000000000000000000009");
        const COINBASE: Address = address!("b00000000000000000000000000000000000000b");
        let swaps = vec![
            swap_owned(SEARCHER, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(SEARCHER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, SEARCHER, POOL_A, 100),
            transfer(1, TOKA, POOL_A, SEARCHER, 200),
            transfer(2, TOKA, SEARCHER, POOL_B, 200),
            transfer(3, USDC, POOL_B, SEARCHER, 110),
            transfer(4, USDC, SEARCHER, COINBASE, 10),
        ];
        let mut input = block(vec![tx_to(0, ATK, SEARCHER, true, swaps, transfers)]);
        input.tip_sinks.insert(COINBASE);
        let events = classify_block(&input);
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "tip-sink residual must not count as arb PnL, got {events:?}"
        );
    }

    #[test]
    fn configured_tip_sink_propagates_into_classifier_and_drops_residual() {
        // End-to-end wiring: an `[explorer] tip_sink_addresses` entry must reach
        // the `IngestConfig.tip_sink_addresses` field, merge with the per-block
        // coinbase exactly as `index_block_fetched` does, and cause a residual
        // forwarded to that *configured* sink to be dropped as `BuilderTip`
        // (never emitted as arb PnL).
        let sink = address!("0000000000000000000000000000000000000042");
        let config: Config = toml::from_str(
            &format!(
                "chain = \"polygon\"\n[chains.polygon]\nchain_id = 137\n[explorer]\ntip_sink_addresses = [\"{sink}\"]\n"
            ),
        )
        .expect("config with tip sink must parse");
        assert_eq!(config.explorer.tip_sink_addresses, vec![sink]);

        let (_, chain_cfg) =
            crate::config::validation::resolve_chain(&config).expect("resolve_chain for polygon");
        let mut ingest = IngestConfig::from_chain(ChainName::Polygon, &chain_cfg);
        // The exact field the job/backfill entry points assign.
        ingest.tip_sink_addresses = config.explorer.tip_sink_addresses.iter().cloned().collect();
        assert!(
            ingest.tip_sink_addresses.contains(&sink),
            "configured sink must land in IngestConfig.tip_sink_addresses"
        );

        const COINBASE: Address = address!("0000000000000000000000000000000000000084");
        let mut tip_sinks = ingest.tip_sink_addresses.clone();
        if !COINBASE.is_zero() {
            tip_sinks.insert(COINBASE);
        }
        assert!(
            tip_sinks.contains(&sink) && tip_sinks.contains(&COINBASE),
            "configured sink must coexist with coinbase in tip_sinks: {tip_sinks:?}"
        );

        // Searcher contract nets zero and forwards the residual to the configured
        // sink (not coinbase) → BuilderTip drop.
        const SEARCHER: Address = address!("00000000000000000000000000000000000000c8");
        let swaps = vec![
            swap_owned(SEARCHER, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(SEARCHER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, SEARCHER, POOL_A, 100),
            transfer(1, TOKA, POOL_A, SEARCHER, 200),
            transfer(2, TOKA, SEARCHER, POOL_B, 200),
            transfer(3, USDC, POOL_B, SEARCHER, 110),
            transfer(4, USDC, SEARCHER, sink, 10),
        ];
        let mut input = block(vec![tx_to(0, ATK, SEARCHER, true, swaps, transfers)]);
        input.tip_sinks = tip_sinks;
        let events = classify_block(&input);
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "configured tip-sink residual must not count as arb PnL, got {events:?}"
        );
    }

    #[test]
    fn arb_cycle_forwarded_to_two_recipients_is_dropped() {
        const SEARCHER: Address = address!("9000000000000000000000000000000000000009");
        const R1: Address = address!("a00000000000000000000000000000000000000a");
        const R2: Address = address!("a00000000000000000000000000000000000000b");
        let swaps = vec![
            swap_owned(SEARCHER, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(SEARCHER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, SEARCHER, POOL_A, 100),
            transfer(1, TOKA, POOL_A, SEARCHER, 200),
            transfer(2, TOKA, SEARCHER, POOL_B, 200),
            transfer(3, USDC, POOL_B, SEARCHER, 110),
            transfer(4, USDC, SEARCHER, R1, 6),
            transfer(5, USDC, SEARCHER, R2, 4),
        ];
        let input = block(vec![tx_to(0, ATK, SEARCHER, true, swaps, transfers)]);
        assert!(
            !kinds(&classify_block(&input)).contains(&MevKind::ArbAtomic),
            "ambiguous multi-recipient forward must drop"
        );
    }

    #[test]
    fn arb_cycle_forwarded_inbound_from_outsider_is_dropped() {
        const SEARCHER: Address = address!("9000000000000000000000000000000000000009");
        const RECIPIENT: Address = address!("a00000000000000000000000000000000000000a");
        const OUTSIDER: Address = address!("7000000000000000000000000000000000000011");
        let swaps = vec![
            swap_owned(SEARCHER, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(SEARCHER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, SEARCHER, POOL_A, 100),
            transfer(1, TOKA, POOL_A, SEARCHER, 200),
            transfer(2, TOKA, SEARCHER, POOL_B, 200),
            transfer(3, USDC, POOL_B, SEARCHER, 110),
            // Residual appears on recipient but funded by an outsider, not the
            // searcher/EOA — must not attribute.
            transfer(4, USDC, OUTSIDER, RECIPIENT, 10),
            transfer(5, USDC, SEARCHER, OUTSIDER, 10),
        ];
        let input = block(vec![tx_to(0, ATK, SEARCHER, true, swaps, transfers)]);
        assert!(
            !kinds(&classify_block(&input)).contains(&MevKind::ArbAtomic),
            "outsider-funded residual must drop"
        );
    }

    #[test]
    fn multi_residual_arb_keeps_anchor_only_in_profit_tokens() {
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
        // Anchor-only profit_tokens (USD must not inflate with unrelated inbound).
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(10)));
        assert_eq!(ev.profit_tokens, vec![(USDC, U256::from(10))]);
        let other = ev.details["other_residuals"].as_array().unwrap();
        assert!(other.iter().any(|r| {
            r["token"].as_str() == Some(&format!("{TOKB:#x}"))
                && r["amount"].as_str() == Some("500")
        }));
    }

    #[test]
    fn arb_cycle_in_walk_dust_deficit_still_emits() {
        // Anchor +10 USDC; leave 1 wei of TOKA stuck (dust) → still ArbAtomic.
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap(POOL_B, TOKA, USDC, 199, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
            transfer(2, TOKA, ATK, POOL_B, 199),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let ev = classify_kind(
            &block(vec![tx(0, ATK, true, swaps, transfers)]),
            MevKind::ArbAtomic,
        );
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(10)));
        assert_eq!(ev.profit_tokens, vec![(USDC, U256::from(10))]);
    }

    #[test]
    fn arb_cycle_material_in_walk_deficit_is_dropped() {
        // Spend more TOKA than received (inventory dump), far above 1 bps.
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 100),
            swap(POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 100),
            transfer(2, TOKA, ATK, POOL_B, 200),
            transfer(3, USDC, POOL_B, ATK, 110),
        ];
        let events = classify_block(&block(vec![tx(0, ATK, true, swaps, transfers)]));
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "material intermediate deficit must drop, got {events:?}"
        );
    }

    #[test]
    fn arb_cycle_eoa_funds_anchor_contract_holds_profit() {
        const SEARCHER: Address = address!("9000000000000000000000000000000000000009");
        let swaps = vec![
            swap_owned(SEARCHER, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(SEARCHER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, SEARCHER, 100),
            transfer(1, USDC, SEARCHER, POOL_A, 100),
            transfer(2, TOKA, POOL_A, SEARCHER, 200),
            transfer(3, TOKA, SEARCHER, POOL_B, 200),
            transfer(4, USDC, POOL_B, SEARCHER, 110),
        ];
        let ev = classify_kind(
            &block(vec![tx_to(0, ATK, SEARCHER, true, swaps, transfers)]),
            MevKind::ArbAtomic,
        );
        assert_eq!(ev.searcher, SEARCHER);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(10)));
    }

    #[test]
    fn arb_cycle_eoa_negative_outside_walk_is_dropped() {
        const SEARCHER: Address = address!("9000000000000000000000000000000000000009");
        const TOKB: Address = address!("7000000000000000000000000000000000000009");
        let swaps = vec![
            swap_owned(SEARCHER, POOL_A, USDC, TOKA, 100, 200),
            swap_owned(SEARCHER, POOL_B, TOKA, USDC, 200, 110),
        ];
        let transfers = vec![
            transfer(0, USDC, ATK, SEARCHER, 100),
            transfer(1, TOKB, ATK, VICTIM, 50), // EOA spends outside walk
            transfer(2, USDC, SEARCHER, POOL_A, 100),
            transfer(3, TOKA, POOL_A, SEARCHER, 200),
            transfer(4, TOKA, SEARCHER, POOL_B, 200),
            transfer(5, USDC, POOL_B, SEARCHER, 110),
        ];
        let events = classify_block(&block(vec![tx_to(
            0, ATK, SEARCHER, true, swaps, transfers,
        )]));
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "EOA outside-walk spend must drop, got {events:?}"
        );
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
    fn non_cycle_profitable_swap_emits_nothing() {
        // Spec: a single-hop profitable residual is not MEV.
        let swaps = vec![swap(POOL_A, USDC, TOKA, 100, 200)];
        let transfers = vec![
            transfer(0, USDC, ATK, POOL_A, 100),
            transfer(1, TOKA, POOL_A, ATK, 200),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let events = classify_block(&input);
        assert!(
            events.is_empty(),
            "expected no events for plain swap, got {events:?}"
        );
    }

    #[test]
    fn inventory_swap_with_bidirectional_edges_is_not_arb() {
        // Regression: Avalanche tx 0x266f47… — aggregator sells WAVAX for USDC
        // across several pools. Swap edges form USDC↔WNATIVE cycles (≥2 pools)
        // so a naive graph walk looks like Exact arb, but the searcher ledger
        // is open (−WNATIVE, +USDC). Must emit nothing.
        let router = address!("70000000000000000000000000000000000000aa");
        let swaps = vec![
            swap(POOL_A, USDC, WNATIVE, 100, 10),
            swap(POOL_B, WNATIVE, USDC, 1000, 500),
        ];
        let transfers = vec![
            transfer(0, WNATIVE, ATK, router, 1000),
            transfer(1, WNATIVE, router, POOL_B, 1000),
            transfer(2, USDC, POOL_B, router, 500),
            // Reverse-looking leg that closes the *edge* graph only:
            transfer(3, USDC, router, POOL_A, 100),
            transfer(4, WNATIVE, POOL_A, router, 10),
            transfer(5, USDC, router, ATK, 400),
        ];
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let events = classify_block(&input);
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "inventory WAVAX→USDC must not be ArbAtomic, got {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|e| e.kind != MevKind::ArbAtomic && e.kind != MevKind::Utility),
            "expected no arb/unknown pollution, got {events:?}"
        );
    }

    #[test]
    fn skim_excluded_when_pool_also_receives_tokens() {
        // Undecoded AMM swap: registry V2-like pool both receives and sends
        // in the same tx — must not be classified as skim.
        let t = tx(
            0,
            ATK,
            true,
            vec![], // no decoded SwapFact
            vec![
                transfer(0, WNATIVE, ATK, POOL_A, 1000),
                transfer(1, USDC, POOL_A, ATK, 500),
            ],
        );
        let input = block_with_v2(vec![t], &[POOL_A]);
        assert!(
            !kinds(&classify_block(&input)).contains(&MevKind::Skim),
            "inbound+outbound on the pair is swap-like, not skim"
        );
    }

    #[test]
    fn transfer_cycle_does_not_promote_open_inventory() {
        // Router-mediated inventory swap: ATK → router → pool → ATK forms a
        // ≥3-entity transfer cycle, but the ledger stays open (−USDC, +TOKA).
        // Must not become ArbAtomic / Unknown.
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
        let input = block(vec![tx(0, ATK, true, swaps, transfers)]);
        let events = classify_block(&input);
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "open-inventory transfer cycle must not promote, got {events:?}"
        );
        assert!(
            events.iter().all(|e| e.kind != MevKind::Utility),
            "expected no unknown pollution, got {events:?}"
        );
    }

    #[test]
    fn incomplete_swap_edges_do_not_emit_arb_via_transfer_cycle() {
        // Closed residual (+USDC) but only one *resolved* swap edge (second
        // leg unresolved) so swap-graph cycle fails. Transfer-cycle upgrade
        // is gone — must not emit ArbAtomic.
        let router = address!("7000000000000000000000000000000000000007");
        let s0 = swap(POOL_A, USDC, TOKA, 100, 200);
        let mut s1 = swap(POOL_B, TOKA, USDC, 200, 110);
        s1.token_in = crate::explorer::decode::TOKEN0_SENTINEL;
        s1.token_out = crate::explorer::decode::TOKEN1_SENTINEL;
        let transfers = vec![
            transfer(0, USDC, ATK, router, 100),
            transfer(1, USDC, router, POOL_A, 100),
            transfer(2, TOKA, POOL_A, ATK, 200),
            transfer(3, TOKA, ATK, router, 200),
            transfer(4, TOKA, router, POOL_B, 200),
            transfer(5, USDC, POOL_B, ATK, 110),
        ];
        let input = block(vec![tx(0, ATK, true, vec![s0, s1], transfers)]);
        let events = classify_block(&input);
        assert!(
            !kinds(&events).contains(&MevKind::ArbAtomic),
            "incomplete swap edges must not emit ArbAtomic, got {events:?}"
        );
    }

    #[test]
    fn interleaved_cycle_is_exact_arb() {
        // Three-hop cycle emitted out of chain order: A::USDC->TOKA,
        // A::WNATIVE->USDC, B::TOKA->WNATIVE. The graph walk still closes it.
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
            assert_eq!(ev.details["close_tx_index"], serde_json::json!(2));
            assert!(!kinds(&events).contains(&MevKind::Sandwich));
        }
    }

    #[test]
    fn backrun_same_token_size_accepts_decimal_mismatched_amount_in() {
        // Move amount_in (50 USDC) < backrun amount_in (200 TOKA) — old gate
        // rejected. Same-token: move.amount_out (1000) >= back.amount_in (200).
        let txs = vec![
            tx(
                0,
                VICTIM,
                true,
                vec![swap(POOL_A, TOKA, USDC, 100, 110)],
                vec![
                    transfer(0, TOKA, VICTIM, POOL_A, 100),
                    transfer(1, USDC, POOL_A, VICTIM, 110),
                ],
            ),
            tx(
                1,
                MARKET,
                true,
                vec![swap(POOL_A, USDC, TOKA, 50, 1000)],
                vec![
                    transfer(0, USDC, MARKET, POOL_A, 50),
                    transfer(1, TOKA, POOL_A, MARKET, 1000),
                ],
            ),
            tx(
                2,
                ATK,
                true,
                vec![
                    swap(POOL_A, TOKA, USDC, 200, 250),
                    swap(POOL_B, USDC, TOKA, 250, 260),
                ],
                vec![
                    transfer(0, TOKA, ATK, POOL_A, 200),
                    transfer(1, USDC, POOL_A, ATK, 250),
                    transfer(2, USDC, ATK, POOL_B, 250),
                    transfer(3, TOKA, POOL_B, ATK, 260),
                ],
            ),
        ];
        let events = classify_block(&block(txs));
        assert!(
            kinds(&events).contains(&MevKind::Backrun),
            "same-token size gate should accept, got {events:?}"
        );
    }

    #[test]
    fn frontrun_multi_hop_close_uses_anchor_ledger() {
        const TOKB: Address = address!("700000000000000000000000000000000000000b");
        const POOL_C: Address = address!("300000000000000000000000000000000000000c");
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
            vec![swap(POOL_A, USDC, TOKA, 100, 150)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 100),
                transfer(1, TOKA, POOL_A, VICTIM, 150),
            ],
        );
        let c = tx(
            2,
            ATK,
            true,
            vec![
                swap(POOL_B, TOKA, TOKB, 200, 50),
                swap(POOL_C, TOKB, USDC, 50, 160),
            ],
            vec![
                transfer(0, TOKA, ATK, POOL_B, 200),
                transfer(1, TOKB, POOL_B, ATK, 50),
                transfer(2, TOKB, ATK, POOL_C, 50),
                transfer(3, USDC, POOL_C, ATK, 160),
            ],
        );
        let events = classify_block(&block(vec![f, v, c]));
        let ev = event_of(&events, MevKind::Frontrun);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(60)));
        assert!(!kinds(&events).contains(&MevKind::ArbAtomic));
    }

    #[test]
    fn frontrun_material_inventory_spend_is_rejected() {
        const TOKB: Address = address!("700000000000000000000000000000000000000b");
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
            vec![swap(POOL_A, USDC, TOKA, 100, 150)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 100),
                transfer(1, TOKA, POOL_A, VICTIM, 150),
            ],
        );
        // Close spends pre-existing TOKB (not produced by the front).
        let c = tx(
            2,
            ATK,
            true,
            vec![swap(POOL_B, TOKB, USDC, 500, 250)],
            vec![
                transfer(0, TOKB, ATK, POOL_B, 500),
                transfer(1, USDC, POOL_B, ATK, 250),
            ],
        );
        let events = classify_block(&block(vec![f, v, c]));
        assert!(
            !kinds(&events).contains(&MevKind::Frontrun),
            "material inventory spend must not be frontrun, got {events:?}"
        );
    }

    #[test]
    fn frontrun_records_close_tx_and_leaves_no_arb_on_close() {
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
            vec![swap(POOL_A, USDC, TOKA, 100, 150)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 100),
                transfer(1, TOKA, POOL_A, VICTIM, 150),
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
        let ev = event_of(&events, MevKind::Frontrun);
        assert_eq!(ev.details["close_tx_index"], serde_json::json!(2));
        assert_eq!(ev.profit_amount, Some(U256::from(150)));
        assert!(
            !events
                .iter()
                .any(|e| e.kind == MevKind::ArbAtomic && e.tx_index == 2),
            "close tx must not keep ArbAtomic, got {events:?}"
        );
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
                // A19: O basis (seized collateral) is not a ledger residual.
                confidence: Some(Confidence::Inferred),
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
                // A19: O basis (seized collateral) is not a ledger residual.
                confidence: Some(Confidence::Inferred),
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
        // A19: oracle-valued (O) basis is no longer Exact.
        assert_eq!(buy.confidence, Confidence::Inferred);
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
            kind: MevKind::Utility,
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

    /// Kinds that carry token/gas P&L (excludes tag-only Unknown).
    fn pnl_kind(k: MevKind) -> bool {
        matches!(
            k,
            MevKind::Sandwich
                | MevKind::Frontrun
                | MevKind::Backrun
                | MevKind::ArbAtomic
                | MevKind::JitArb
                | MevKind::Jit
                | MevKind::Liquidation
                | MevKind::Skim
        )
    }

    #[test]
    fn pnl_kinds_at_most_one_per_tx_index() {
        // Sandwich + arb + jit_arb fixtures: each tx_index may carry at most
        // one P&L-bearing kind (Unknown tag-only fingerprints are exempt).
        let sandwich = block(vec![
            tx(
                0,
                ATK,
                true,
                vec![swap(POOL_A, USDC, TOKA, 100, 200)],
                vec![
                    transfer(0, USDC, ATK, POOL_A, 100),
                    transfer(1, TOKA, POOL_A, ATK, 200),
                ],
            ),
            tx(
                1,
                VICTIM,
                true,
                vec![swap(POOL_A, USDC, TOKA, 200, 350)],
                vec![
                    transfer(0, USDC, VICTIM, POOL_A, 200),
                    transfer(1, TOKA, POOL_A, VICTIM, 350),
                ],
            ),
            tx(
                2,
                ATK,
                true,
                vec![swap(POOL_A, TOKA, USDC, 350, 195)],
                vec![
                    transfer(0, TOKA, ATK, POOL_A, 350),
                    transfer(1, USDC, POOL_A, ATK, 195),
                ],
            ),
        ]);
        let jit_arb = {
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
            block(vec![t0, t1])
        };
        for input in [&sandwich, &jit_arb] {
            let events = classify_block(input);
            let mut seen: HashSet<u64> = HashSet::new();
            for e in events.iter() {
                if !pnl_kind(e.kind) {
                    continue;
                }
                assert!(
                    seen.insert(e.tx_index),
                    "duplicate P&L kind on tx_index {}: {:?}",
                    e.tx_index,
                    events
                        .iter()
                        .filter(|x| x.tx_index == e.tx_index)
                        .map(|x| x.kind)
                        .collect::<Vec<_>>()
                );
            }
        }
        assert_eq!(kind_precedence(MevKind::Sandwich), 0);
        assert!(kind_precedence(MevKind::Sandwich) < kind_precedence(MevKind::Frontrun));
        assert!(kind_precedence(MevKind::Frontrun) < kind_precedence(MevKind::Backrun));
        assert!(kind_precedence(MevKind::Backrun) < kind_precedence(MevKind::ArbAtomic));
        assert!(kind_precedence(MevKind::JitArb) < kind_precedence(MevKind::Jit));
    }

    #[test]
    fn sandwich_front_supersedes_arb_on_same_tx() {
        // tx0 is the sandwich front-run (TOKA→USDC on POOL_A) *and* closes a
        // 2-pool cycle (USDC→TOKA on POOL_B), so the arb pass emits ArbAtomic
        // on the same tx_index as the Sandwich anchor. Kind precedence
        // (Sandwich > ArbAtomic) must drop the arb event: P&L is attributed
        // once, to the Sandwich.
        let t0 = tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, TOKA, USDC, 200, 250),
                swap(POOL_B, USDC, TOKA, 250, 260),
            ],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 200),
                transfer(1, USDC, POOL_A, ATK, 250),
                transfer(2, USDC, ATK, POOL_B, 250),
                transfer(3, TOKA, POOL_B, ATK, 260),
            ],
        );
        let t1 = tx(
            1,
            VICTIM,
            true,
            vec![swap(POOL_A, TOKA, USDC, 100, 110)],
            vec![
                transfer(0, TOKA, VICTIM, POOL_A, 100),
                transfer(1, USDC, POOL_A, VICTIM, 110),
            ],
        );
        let t2 = tx(
            2,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 105)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 105),
            ],
        );
        let events = classify_block(&block(vec![t0, t1, t2]));
        assert!(
            events
                .iter()
                .any(|e| e.kind == MevKind::Sandwich && e.tx_index == 0),
            "fixture must produce the sandwich anchor on tx0, got {events:?}"
        );

        let mut by_tx: HashMap<u64, Vec<MevKind>> = HashMap::new();
        for e in events.iter() {
            by_tx.entry(e.tx_index).or_default().push(e.kind);
        }
        for (tx, kinds) in by_tx {
            let has_sandwich = kinds.contains(&MevKind::Sandwich);
            let has_arb = kinds.contains(&MevKind::ArbAtomic) || kinds.contains(&MevKind::Utility);
            assert!(
                !(has_sandwich && has_arb),
                "tx {tx} carries both Sandwich and Arb/Unknown: {kinds:?}"
            );
        }
    }
}

#[cfg(test)]
mod unresolved_token_tests {
    use super::*;
    use crate::explorer::decode::{TOKEN0_SENTINEL, TOKEN1_SENTINEL};
    use crate::explorer::test_fixtures::{swap, POOL_A, TOKA, USDC};

    /// Either side of the swap can be unresolved: the old check tested the
    /// sentinels on `token_in` only, so a resolved-in / sentinel-out edge kept
    /// its event and poisoned downstream direction inference.
    #[test]
    fn sentinel_detected_on_either_side() {
        let mut s = swap(POOL_A, USDC, TOKA, 1, 1);
        assert!(!has_unresolved_tokens(&s), "resolved both sides");

        s.token_out = TOKEN0_SENTINEL;
        assert!(has_unresolved_tokens(&s), "out-side token0 sentinel");

        s.token_out = TOKA;
        s.token_in = TOKEN1_SENTINEL;
        assert!(has_unresolved_tokens(&s), "in-side token1 sentinel");

        s.token_in = Address::ZERO;
        assert!(has_unresolved_tokens(&s), "in-side zero");

        s.token_in = USDC;
        s.token_out = Address::ZERO;
        assert!(has_unresolved_tokens(&s), "out-side zero");
    }
}

#[cfg(test)]
mod confidence_basis_tests {
    use super::*;
    use crate::explorer::test_fixtures::{ATK, POOL_A, USDC};
    use alloy::primitives::B256;

    fn event_with(basis: serde_json::Value, confidence: Confidence) -> MevEvent {
        MevEvent {
            block: 1,
            ts: 1_700_000_000,
            tx_index: 0,
            tx_hash: B256::ZERO,
            kind: MevKind::ArbAtomic,
            searcher: ATK,
            contract: None,
            pools: vec![POOL_A],
            profit_token: Some(USDC),
            profit_amount: Some(U256::from(10u64)),
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::ZERO,
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({ "pnl_basis": basis }),
        }
    }

    /// A19: only a realized ledger residual may claim `Exact`; every other
    /// basis the plan names (oracle, fee fields, gross diff, gas-as-profit)
    /// is demoted.
    #[test]
    fn exact_requires_ledger_residual_basis() {
        let mut events = vec![
            event_with(serde_json::json!("R"), Confidence::Exact),
            event_with(serde_json::json!("O"), Confidence::Exact),
            event_with(serde_json::json!("F"), Confidence::Exact),
            event_with(serde_json::json!("gross_amount_diff"), Confidence::Exact),
            event_with(serde_json::json!("gas_cost_as_profit"), Confidence::Exact),
        ];
        normalize_confidence(&mut events);
        assert_eq!(
            events[0].confidence,
            Confidence::Exact,
            "ledger residual stays Exact"
        );
        for (i, e) in events.iter().enumerate().skip(1) {
            assert_eq!(
                e.confidence,
                Confidence::Inferred,
                "basis at index {i} is not a ledger residual"
            );
        }
    }

    /// A recorded reconcile failure, or no P&L amount at all, demotes even an
    /// R-basis event: there is nothing exact to stand behind.
    #[test]
    fn reconcile_failure_and_missing_amount_demote() {
        let mut mismatch = event_with(serde_json::json!("R"), Confidence::Exact);
        mismatch.details["reasons"] = serde_json::json!(["TRANSFER_MISMATCH"]);
        let mut no_amount = event_with(serde_json::json!("R"), Confidence::Exact);
        no_amount.profit_amount = None;
        let mut events = vec![mismatch, no_amount];
        normalize_confidence(&mut events);
        assert_eq!(events[0].confidence, Confidence::Inferred);
        assert_eq!(events[1].confidence, Confidence::Inferred);
    }
}

/// A20: every candidate a classify pass drops must emit a `RejectedCandidate`
/// whose reason carries the *decision*, so "0 frontrun / backrun / skim over
/// N blocks" is answerable from data instead of being a guess.
///
/// One test per instrumented pass asserts the exact reason string (and the
/// `detail` gate name) for a near-miss fixture; the absence of that reason is
/// the test failure. The last test is the structural guard: no bare `continue;`
/// may land in the instrumented files without either recording a reason or
/// saying why it is not a candidate drop.
#[cfg(test)]
mod a20_reject_reasons {
    use super::*;
    use crate::explorer::test_fixtures::{
        block, block_with_v2, jit, liquidation, swap, swap_at_tick, swap_owned, transfer, tx, ATK,
        MARKET, POOL_A, POOL_B, TOKA, USDC, VICTIM, WNATIVE,
    };
    use crate::explorer::types::V2PairOpKind;
    use alloy::primitives::address;

    const OUTSIDER: Address = address!("7000000000000000000000000000000000000011");
    const FILLER: Address = address!("f000000000000000000000000000000000000001");
    const POOL_C: Address = address!("300000000000000000000000000000000000000c");
    const TOKB: Address = address!("700000000000000000000000000000000000000b");

    /// True when a reject carries this reason, this tx anchor (`None` = the
    /// per-block aggregate row) and a `detail` starting with `gate`.
    fn has(out: &ClassifyOutput, reason: &str, tx_index: Option<u64>, gate: &str) -> bool {
        out.rejects.iter().any(|r| {
            r.reject_reason == reason
                && r.tx_index == tx_index
                && r.detail.as_deref().is_some_and(|d| d.starts_with(gate))
        })
    }

    fn dump(out: &ClassifyOutput) -> Vec<(String, Option<u64>, Option<String>)> {
        out.rejects
            .iter()
            .map(|r| (r.reject_reason.clone(), r.tx_index, r.detail.clone()))
            .collect()
    }

    /// A profitable closed cycle (the anchor the causal backrun pass needs)
    /// plus a market-moving tx at index 0, `gap` txs apart.
    fn backrun_shape(gap: u64, ref_price: (u64, u64), back_price: (u64, u64)) -> Vec<TxInput> {
        let mut txs = vec![tx(
            0,
            MARKET,
            true,
            vec![swap(POOL_A, TOKA, USDC, ref_price.0, ref_price.1)],
            vec![
                transfer(0, TOKA, MARKET, POOL_A, ref_price.0),
                transfer(1, USDC, POOL_A, MARKET, ref_price.1),
            ],
        )];
        // `gap - 1` unrelated filler txs, then the move, then the closer.
        let mut idx = 1u64;
        let fillers = gap.saturating_sub(2);
        for _ in 0..fillers {
            txs.push(tx(
                idx,
                FILLER,
                true,
                vec![swap(POOL_C, USDC, TOKA, 10, 10)],
                vec![
                    transfer(0, USDC, FILLER, POOL_C, 10),
                    transfer(1, TOKA, POOL_C, FILLER, 10),
                ],
            ));
            idx += 1;
        }
        let move_in = 1000u64;
        let move_out = 500u64;
        txs.push(tx(
            idx,
            VICTIM,
            true,
            vec![swap(POOL_A, USDC, TOKA, move_in, move_out)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, move_in),
                transfer(1, TOKA, POOL_A, VICTIM, move_out),
            ],
        ));
        idx += 1;
        txs.push(tx(
            idx,
            ATK,
            true,
            vec![
                swap(POOL_A, TOKA, USDC, back_price.0, back_price.1),
                swap(POOL_B, USDC, TOKA, back_price.1, back_price.1 + 10),
            ],
            vec![
                transfer(0, TOKA, ATK, POOL_A, back_price.0),
                transfer(1, USDC, POOL_A, ATK, back_price.1),
                transfer(2, USDC, ATK, POOL_B, back_price.1),
                transfer(3, TOKA, POOL_B, ATK, back_price.1 + 10),
            ],
        ));
        txs
    }

    #[test]
    fn cycle_gate_reports_closed_cycle_misses_by_name() {
        // (fixture, expected tx anchor, expected gate in `detail`).
        // Two resolved edges that do not close → anchored `cycle_not_closed`.
        let unclosed = block(vec![tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, TOKA, USDC, 100, 120),
                swap(POOL_B, USDC, TOKB, 120, 130),
            ],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 100),
                transfer(1, USDC, POOL_A, ATK, 120),
                transfer(2, USDC, ATK, POOL_B, 120),
                transfer(3, TOKB, POOL_B, ATK, 130),
            ],
        )]);
        let out = classify_block(&unclosed);
        assert!(
            has(&out, "cycle_not_closed", Some(0), "no_closed_cycle"),
            "two unclosed edges must report `no_closed_cycle`, got {:?}",
            dump(&out)
        );

        // A single resolved edge is not an anchor at all: one aggregate row.
        let one_edge = block(vec![tx(
            0,
            ATK,
            true,
            vec![swap(POOL_A, TOKA, USDC, 100, 120)],
            vec![
                transfer(0, TOKA, ATK, POOL_A, 100),
                transfer(1, USDC, POOL_A, ATK, 120),
            ],
        )]);
        let out = classify_block(&one_edge);
        assert!(
            has(
                &out,
                "cycle_not_closed",
                None,
                "fewer_than_2_resolved_edges"
            ),
            "a sub-2-edge tx must be aggregated, not written per tx, got {:?}",
            dump(&out)
        );

        // Closed cycle whose legs are funded by an unrelated actor.
        let mixed = block(vec![tx(
            0,
            ATK,
            true,
            vec![
                swap_owned(ATK, POOL_A, USDC, TOKA, 100, 200),
                swap_owned(OUTSIDER, POOL_B, TOKA, USDC, 200, 110),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
                transfer(2, TOKA, ATK, POOL_B, 200),
                transfer(3, USDC, POOL_B, ATK, 110),
            ],
        )]);
        let out = classify_block(&mixed);
        assert!(
            has(
                &out,
                "mixed_flow_owner",
                Some(0),
                "cycle_flow_not_attributable"
            ),
            "a closed cycle with a foreign leg owner must say so, got {:?}",
            dump(&out)
        );

        // Cycle whose only candidates dump pre-existing inventory.
        let inventory = block(vec![tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, USDC, 200, 50),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
                transfer(2, TOKA, ATK, POOL_B, 200),
                transfer(3, USDC, POOL_B, ATK, 50),
            ],
        )]);
        let out = classify_block(&inventory);
        assert!(
            has(
                &out,
                "material_deficit",
                Some(0),
                "every_candidate_spends_pre_existing_inventory"
            ),
            "an inventory-dumping cycle must report the spend, got {:?}",
            dump(&out)
        );

        // Cycle that closes at exactly break-even: no positive residual.
        let net_zero = block(vec![tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, USDC, 200, 100),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
                transfer(2, TOKA, ATK, POOL_B, 200),
                transfer(3, USDC, POOL_B, ATK, 100),
            ],
        )]);
        let out = classify_block(&net_zero);
        assert!(
            has(
                &out,
                "net_zero_searcher",
                Some(0),
                "no_candidate_holds_a_positive_residual"
            ),
            "a break-even cycle must report no residual, got {:?}",
            dump(&out)
        );
    }

    #[test]
    fn backrun_reports_no_counterparty_leg_and_the_window_gate() {
        // A closed profitable cycle that is the only leg on its pool: nothing
        // moved the market, so there is no counterparty to backrun.
        let alone = block(vec![tx(
            0,
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
        )]);
        let out = classify_block(&alone);
        assert!(
            has(
                &out,
                "no_counterparty_leg",
                Some(0),
                "no_prior_opposite_leg"
            ),
            "a cycle with no prior opposite leg must say why, got {:?}",
            dump(&out)
        );

        // Same shape, but the counterparty moved 9 txs earlier: beyond the
        // adjacency window, so the reject names the window, not the price.
        let out = classify_block(&block(backrun_shape(9, (1000, 1100), (100, 120))));
        assert!(
            has(
                &out,
                "causal_window_too_wide",
                Some(8),
                "counterparty_leg_behind_causal_window"
            ),
            "the CAUSAL_WINDOW_TXS gate must be recorded, got {:?}",
            dump(&out)
        );
    }

    #[test]
    fn backrun_reports_the_price_floor_gate() {
        // In-window counterparty move present, closed profitable cycle present,
        // but the closer executes at the pre-move reference price (1.2 vs 1.2):
        // below CAUSAL_PRICE_MIN_PCT, so it is noise, not a backrun.
        let out = classify_block(&block(backrun_shape(3, (1000, 1200), (100, 120))));
        assert!(
            has(
                &out,
                "causal_price_move_below_floor",
                Some(2),
                "backrun_price_below_floor"
            ),
            "the CAUSAL_PRICE_MIN_PCT gate must be recorded, got {:?}",
            dump(&out)
        );
    }

    #[test]
    fn frontrun_reports_empty_victim_set_and_unclosed_position() {
        // A mover with no third-party same-direction swap after it: nothing was
        // frontrun. Aggregate row (no anchor existed).
        let mover = block(vec![tx(
            0,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        )]);
        let out = classify_block(&mover);
        assert!(
            has(
                &out,
                "victim_set_empty",
                None,
                "no_third_party_same_direction_leg"
            ),
            "an empty victim set must be recorded, got {:?}",
            dump(&out)
        );

        // Degraded third-party victim (the anchor) but the position is never
        // closed by the mover's sender: a frontrun-shaped candidate that fails
        // the close gate.
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
            vec![swap(POOL_A, USDC, TOKA, 100, 150)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 100),
                transfer(1, TOKA, POOL_A, VICTIM, 150),
            ],
        );
        let out = classify_block(&block(vec![f, v]));
        assert!(
            has(&out, "cycle_not_closed", Some(0), "no_profitable_close_leg"),
            "a victim-anchored frontrun with no close must say so, got {:?}",
            dump(&out)
        );
    }

    #[test]
    fn sandwich_reports_its_leg_walk_rejections() {
        // Attacker opens and closes on one pool with no third-party swap in
        // between: a round trip, rejected because the victim set is empty.
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
        let out = classify_block(&block(vec![t0, t1]));
        assert!(
            has(
                &out,
                "victim_set_empty",
                Some(0),
                "closed_without_third_party_victim"
            ),
            "a front/back pair with no victim must be recorded, got {:?}",
            dump(&out)
        );

        // Victims did pile up between the front and a same-direction attacker
        // swap that never reverses: no back-run leg, so no sandwich.
        let front = tx(
            0,
            ATK,
            true,
            vec![swap(POOL_A, USDC, TOKA, 100, 200)],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
            ],
        );
        let victim = tx(
            1,
            VICTIM,
            true,
            vec![swap(POOL_A, USDC, TOKA, 200, 380)],
            vec![
                transfer(0, USDC, VICTIM, POOL_A, 200),
                transfer(1, TOKA, POOL_A, VICTIM, 380),
            ],
        );
        let out = classify_block(&block(vec![front, victim]));
        assert!(
            has(&out, "cycle_not_closed", Some(0), "no_closing_back_run_leg"),
            "an opened position that never reverses must be recorded, got {:?}",
            dump(&out)
        );
    }

    #[test]
    fn skim_reports_pool_set_rejections() {
        // An undecoded-pair outflow whose pool has V2 venue evidence (it emits
        // Sync) but is absent from the registry: the skim pass cannot look at
        // it. This is the exact "0 skim across N blocks" answer.
        let mut t = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 42)],
        );
        t.v2_pair_ops = vec![crate::explorer::types::V2PairOpFact {
            tx_index: 0,
            log_index: 1,
            pool: POOL_A,
            kind: V2PairOpKind::Sync,
        }];
        let out = classify_block(&block(vec![t]));
        assert!(
            has(
                &out,
                "skim_pool_not_v2_like",
                None,
                "venue_outflow_not_in_v2_like_registry"
            ),
            "a pair outflow excluded by the registry must be recorded, got {:?}",
            dump(&out)
        );

        // Registry pool that both receives and pays out in the same tx:
        // swap-like activity, not a skim.
        let swap_like = tx(
            0,
            ATK,
            true,
            vec![],
            vec![
                transfer(0, WNATIVE, ATK, POOL_A, 1000),
                transfer(1, USDC, POOL_A, ATK, 500),
            ],
        );
        let out = classify_block(&block_with_v2(vec![swap_like], &[POOL_A]));
        assert!(
            has(&out, "skim_looks_like_swap", Some(0), "pool_also_receives"),
            "an inbound+outbound pair must be recorded as swap-like, got {:?}",
            dump(&out)
        );
    }

    #[test]
    fn jit_reports_mint_to_burn_pairing_misses() {
        // A Mint with no Burn that closes it.
        let mut mint_tx = tx(0, ATK, true, vec![], vec![]);
        mint_tx.jit = vec![jit(0, true)];
        let out = classify_block(&block(vec![mint_tx.clone()]));
        assert!(
            has(&out, "jit_pairing_unmatched", Some(0), "mint_without_burn"),
            "an unmatched Mint must be recorded, got {:?}",
            dump(&out)
        );

        // Matched Mint+Burn pair, but no swap lands inside the tick range: the
        // pairing is fine, the range validation is not.
        let mut t0 = tx(
            0,
            ATK,
            true,
            vec![swap_at_tick(POOL_A, USDC, TOKA, 100, 200, 200)],
            vec![],
        );
        t0.jit = vec![jit(0, true)];
        let mut t1 = tx(1, VICTIM, true, vec![], vec![]);
        t1.jit = vec![jit(1, false)];
        let out = classify_block(&block(vec![t0, t1]));
        assert!(
            has(
                &out,
                "jit_pairing_unmatched",
                Some(0),
                "no_validating_swap_in_position_range"
            ),
            "a range-validation miss must be recorded, got {:?}",
            dump(&out)
        );
    }

    #[test]
    fn liquidation_reports_reconcile_failures() {
        // Aave-style liquidation that seizes 500 collateral with no Transfer
        // covering it: the event stands, its P&L does not reconcile.
        let mut t = tx(0, ATK, true, vec![], vec![]);
        t.liquidations = vec![liquidation("aave_v3", ATK, USDC, WNATIVE, 500, 300)];
        let out = classify_block(&block(vec![t]));
        assert!(
            has(
                &out,
                "liquidation_not_reconciled",
                Some(0),
                "transfer_net_below_seized_amount"
            ),
            "an unreconciled seizure must be recorded, got {:?}",
            dump(&out)
        );

        // Same event with a covering Transfer: reconciled, so no reject row.
        let mut ok = tx(
            0,
            ATK,
            true,
            vec![],
            vec![transfer(0, USDC, POOL_A, ATK, 500)],
        );
        ok.liquidations = vec![liquidation("aave_v3", ATK, USDC, WNATIVE, 500, 300)];
        let out = classify_block(&block(vec![ok]));
        assert!(
            !has(&out, "liquidation_not_reconciled", Some(0), ""),
            "a reconciled liquidation must not be reported, got {:?}",
            dump(&out)
        );
    }

    /// Structural guard (Step 4): no instrumented pass may drop a candidate with
    /// a bare `continue;`. Every one either records a reason on the line above
    /// or says why it is not a candidate drop (`not-a-drop:`).
    #[test]
    fn no_instrumented_pass_drops_a_candidate_without_a_reason() {
        let files = [
            ("legs.rs", include_str!("legs.rs")),
            ("causal.rs", include_str!("causal.rs")),
            ("skim.rs", include_str!("skim.rs")),
            ("sandwich.rs", include_str!("sandwich.rs")),
            ("jit.rs", include_str!("jit.rs")),
            ("liquidation.rs", include_str!("liquidation.rs")),
        ];
        const EMITS: [&str; 4] = [
            "drops.anchor(",
            "drops.bump(",
            "rejects.push(",
            "sink.anchor(",
        ];
        let mut violations: Vec<String> = Vec::new();
        for (name, src) in files {
            let mut in_tests = false;
            for (i, line) in src.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed == "#[cfg(test)]" {
                    in_tests = true;
                }
                if in_tests || !trimmed.starts_with("continue;") {
                    continue;
                }
                let justified = trimmed.contains("not-a-drop:")
                    || (i >= 3
                        && src
                            .lines()
                            .skip(i.saturating_sub(3))
                            .take(3)
                            .any(|p| EMITS.iter().any(|emit| p.contains(emit))));
                if !justified {
                    violations.push(format!("{name}:{}", i + 1));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "silent `continue;` drop sites with no reject reason: {violations:?}"
        );
    }
}
