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
use crate::explorer::profit::ProfitTokenPolicy;
use crate::explorer::store::OpenPosition;
use crate::explorer::strategy_tags::{self, ArbTagOpts};
use crate::explorer::types::{
    AggregatorSignal, BuyCollateralFact, EpochRewardFact, GmxEventFact, JitFact, KeeperFact,
    MevEvent, MevKind, OracleUpdateFact, RateCacheFact, ReserveDataFact, SwapFact, TransferFact,
    UserOpFact, V2PairOpFact,
};

mod arb;
mod causal;
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
    events.extend(classify_liquidations(input, &liq_ctx));
    let liq_txs: HashSet<u64> = events
        .iter()
        .filter(|e| e.kind == MevKind::Liquidation)
        .map(|e| e.tx_index)
        .collect();

    // ── 1b. Skim pass (exact, UniV2 pair outbound without Swap/Mint/Burn/Sync)
    events.extend(classify_skims(input));

    // ── 2-3. Swap attribution + atomic arb (+ keeper/claim/bundler/upgrade)
    let (arb_events, arb_rejects) = classify_arbs(input, &liq_txs, &arb_tag_opts);
    events.extend(arb_events);
    rejects.extend(arb_rejects);

    // ── 5b. JIT block-wide pairing ──────────────────────────────────────
    let mut jit_events = classify_jit(input);
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
    let mut sandwiches = classify_sandwiches(input);

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
        // Frontrun close tx may itself be a closed cycle; drop ArbAtomic /
        // Unknown there so P&L is not double-counted (same rule as JitArb).
        if e.kind == MevKind::Frontrun {
            if let Some(c) = e.details.get("close_tx_index").and_then(|v| v.as_u64()) {
                superseder.entry(c).or_insert(MevKind::Frontrun);
            }
        }
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
            .then_with(|| kind_precedence(a.kind).cmp(&kind_precedence(b.kind)))
    });
    ClassifyOutput { events, rejects }
}

/// Kind priority for sort / supersede (lower = higher priority).
/// Single source of truth for Sandwich > Frontrun > Backrun > ArbAtomic >
/// JitArb > Jit > Liquidation > Skim > Unknown. The JitArb upgrade +
/// Frontrun/Backrun retain paths must stay consistent with this order so
/// P&L is never double-counted on one tx_index.
pub fn kind_precedence(k: MevKind) -> u8 {
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
        if let Some(mut t) = decode::decode_transfer(log) {
            t.tx_index = tx_index;
            t.log_index = log_idx;
            facts.transfers.push(t);
        }
        // Collect aggregator intent before swap-edge dedup so brand survives.
        if let Some(mut sig) = decode::decode_aggregator_signal(log) {
            sig.log_index = log_idx;
            facts.aggregator_signals.push(sig);
        }
        if let Some((amm, mut s)) = decode::decode_swap(log) {
            s.tx_index = tx_index;
            s.log_index = log_idx;
            s.amm = amm;
            facts.swaps.push(s);
        }
        if let Some(mut l) = decode::decode_liquidation(log) {
            l.tx_index = tx_index;
            l.log_index = log_idx;
            decode::remap_liquidation_protocol(&mut l, liquidation_protocol_aliases);
            facts.liquidations.push(l);
        }
        if let Some(mut f) = decode::decode_flash_loan(log) {
            f.tx_index = tx_index;
            f.log_index = log_idx;
            facts.flashloans.push(f);
        }
        if let Some(mut b) = decode::decode_buy_collateral(log) {
            b.tx_index = tx_index;
            b.log_index = log_idx;
            facts.buy_collaterals.push(b);
        }
        if let Some(mut j) = decode::decode_v3_mint_burn(log) {
            j.tx_index = tx_index;
            j.log_index = log_idx;
            facts.jit.push(j);
        } else if let Some(mut j) = decode::decode_lb_bins_liquidity(log) {
            j.tx_index = tx_index;
            j.log_index = log_idx;
            facts.jit.push(j);
        }
        if let Some(mut op) = decode::decode_v2_pair_op(log) {
            op.tx_index = tx_index;
            op.log_index = log_idx;
            facts.v2_pair_ops.push(op);
        }
        if let Some(mut o) = decode::decode_oracle_update(log) {
            o.tx_index = tx_index;
            o.log_index = log_idx;
            facts.oracle_updates.push(o);
        }
        if let Some(mut r) = decode::decode_reserve_data(log) {
            r.tx_index = tx_index;
            r.log_index = log_idx;
            facts.reserve_updates.push(r);
        }
        if let Some(mut rc) = decode::decode_rate_cache_update(log) {
            rc.tx_index = tx_index;
            rc.log_index = log_idx;
            facts.rate_cache_updates.push(rc);
        }
        if let Some(mut k) = decode::decode_keeper(log) {
            k.tx_index = tx_index;
            k.log_index = log_idx;
            facts.keepers.push(k);
        }
        if let Some(mut e) = decode::decode_epoch_reward(log) {
            e.tx_index = tx_index;
            e.log_index = log_idx;
            facts.epoch_rewards.push(e);
        }
        if let Some(mut g) = decode::decode_gmx_event(log) {
            g.tx_index = tx_index;
            g.log_index = log_idx;
            facts.gmx_events.push(g);
        }
        if let Some(mut u) = decode::decode_user_op(log) {
            u.tx_index = tx_index;
            u.log_index = log_idx;
            facts.user_ops.push(u);
        }
    }
    decode::attach_swap_tokens(&mut facts.swaps, &facts.transfers, pool_tokens);
    // drop aggregator edges already covered by DEX swap edges.
    decode::dedup_aggregator_facts(&mut facts.swaps);
    facts
}

/// True when a swap fact has unresolved direction tokens (V3 sentinel kept).
pub(crate) fn has_unresolved_tokens(s: &SwapFact) -> bool {
    s.token_in == decode::TOKEN0_SENTINEL
        || s.token_in == decode::TOKEN1_SENTINEL
        || s.token_in == Address::ZERO
        || s.token_out == Address::ZERO
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
    use crate::explorer::types::{Confidence, JitFact, MevKind, V2PairOpFact, V2PairOpKind};
    use crate::types::ChainName;
    use alloy::primitives::{address, b256, Address, U256};

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
                .all(|e| e.kind != MevKind::ArbAtomic && e.kind != MevKind::Unknown),
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
            events.iter().all(|e| e.kind != MevKind::Unknown),
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
        let mut txs = Vec::new();
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
        txs.push(tx(
            1,
            MARKET,
            true,
            vec![swap(POOL_A, USDC, TOKA, 50, 1000)],
            vec![
                transfer(0, USDC, MARKET, POOL_A, 50),
                transfer(1, TOKA, POOL_A, MARKET, 1000),
            ],
        ));
        txs.push(tx(
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
        ));
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
}
