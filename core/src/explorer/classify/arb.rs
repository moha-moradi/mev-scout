//! Atomic-arb attribution and adjacent Exact fingerprints
//! (keeper / claim-and-sell / ERC-4337 bundler).
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::decode;
use crate::explorer::profit::{
    deficit_amount, find_closed_cycle, is_dust_deficit, net_flash_loan, select_profit_token,
    DeltaLedger, NATIVE_MARKER,
};
use crate::explorer::reject::{RejectReason, RejectedCandidate};
use crate::explorer::strategy_tags::{self, ArbTagOpts};
use crate::explorer::types::{
    Amm, Confidence, MevEvent, MevKind, PnlBasis, SwapFact, TransferFact,
};

use super::{append_tag, has_unresolved_tokens, BlockInput, TxInput};

const CLASSIFY_STRATEGY: &str = "explorer_classify";

/// Swap attribution + atomic arb + keeper/claim/bundler.
pub(super) fn classify_arbs(
    input: &BlockInput,
    liq_txs: &HashSet<u64>,
    arb_tag_opts: &ArbTagOpts<'_>,
) -> (Vec<MevEvent>, Vec<RejectedCandidate>) {
    let mut events: Vec<MevEvent> = Vec::new();
    let mut rejects: Vec<RejectedCandidate> = Vec::new();
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

        // net flash-loan borrow/repay before computing residuals so
        // borrowed principal never counts as profit. Only net when the repay
        // leg is *absent* from the Transfer stream (provider callback internal
        // repayments); when the repay Transfer to the provider is present the
        // ledger already nets it, so subtracting again would erase real profit.
        let mut flashloan_fee_wei: Option<U256> = None;
        let mut flashloan_fee_token: Option<Address> = None;
        // Largest fee among loans the ledger actually netted. The event's
        // recorded fee is the max across *all* loans, so only an equal value
        // proves the recorded fee is already inside the residual.
        let mut max_netted_fee: Option<U256> = None;
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
            // Aggregate: Balancer-style repays can be split into several
            // transfers, so a single full-amount leg is not required — the sum
            // of repay legs to the provider/recipient is what the ledger nets.
            let repaid_total = tx
                .transfers
                .iter()
                .filter(|t| {
                    t.token == fl.token
                        && (t.from == borrower || t.from == tx.from)
                        && (t.to == fl.provider || t.to == fl.recipient)
                })
                .fold(U256::ZERO, |acc, t| acc.saturating_add(t.amount));
            let repaid = repaid_total >= fl.amount;
            if !repaid {
                // The principal sits where the loan was delivered: the
                // recipient (executor contract) when the provider names one,
                // else the borrower. Netting `tx.to` in addition used to strip
                // the same principal from the EOA, erasing its real profit.
                let owner = if fl.recipient.is_zero() {
                    borrower
                } else {
                    fl.recipient
                };
                net_flash_loan(&mut ledger, owner, fl.token, fl.amount, fee);
                if !fee.is_zero() && max_netted_fee.is_none_or(|f| fee > f) {
                    max_netted_fee = Some(fee);
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

        // ── Atomic arb ─────────────────────────────────────────────────
        // Resolved directional edges (token_in, token_out, pool).
        //
        // Aggregator/router legs are excluded: those edges are the router's
        // *internal* routing (e.g. a token→USDC sale split across its own
        // pools), decoded only by token proximity. Letting them close the
        // cycle mislabels a one-way aggregator conversion as the searcher's
        // atomic arb (regression: two ~$76–78 USDC "arbs" via the KyberSwap
        // executor that mev.zone does not count). Genuine searcher cycles run
        // over real DEX pools (v2/v3/v4).
        let resolved_edges: Vec<(Address, Address, Address)> = tx
            .swaps
            .iter()
            .filter(|s| {
                s.amm != Amm::Aggregator
                    && !has_unresolved_tokens(s)
                    && s.token_in != decode::TOKEN0_SENTINEL
                    && s.token_in != decode::TOKEN1_SENTINEL
            })
            .map(|s| (s.token_in, s.token_out, s.pool))
            .collect();

        let Some((anchor, walk_tokens)) = find_closed_cycle(&resolved_edges) else {
            if resolved_edges.len() >= 2 {
                rejects.push(classify_reject(
                    input.block,
                    tx.tx_index,
                    RejectReason::UnresolvedCycle,
                    None,
                ));
            }
            continue;
        };
        if !flow_attributable(&tx.swaps, &candidates) {
            rejects.push(classify_reject(
                input.block,
                tx.tx_index,
                RejectReason::MixedFlowOwner,
                None,
            ));
            continue;
        }

        let hop_max = hop_max_by_token(&tx.swaps);
        // Route pools often appear in `candidates` via transfer participation and
        // can hold a transient positive residual on the anchor (inventory of the
        // hop). They must never be the profit holder — otherwise a net-zero
        // executor that forwards profit never reaches the forwarded heuristic.
        let route_pools: HashSet<Address> = resolved_edges.iter().map(|(_, _, p)| *p).collect();
        let profit_candidates: Vec<Address> = candidates
            .iter()
            .copied()
            .filter(|a| !route_pools.contains(a))
            .collect();
        // Prefer a candidate with a positive anchor residual (EOA-funded
        // contract holds amountOut−amountIn while the EOA is negative).
        // When the executor nets zero and forwards the residual to a single
        // non-swap third party, attribute Inferred + forwarded_profit (unless
        // that party is a tip sink / coinbase).
        let mut forwarded_from: Option<Address> = None;
        let forwarded = forwarded_anchor_recipient(
            &ledger,
            &tx.transfers,
            &tx.swaps,
            &profit_candidates,
            &input.tip_sinks,
            anchor,
            tx.from,
            tx.to,
        );
        let searcher = profit_candidates
            .iter()
            .copied()
            .find(|a| ledger.net(*a, anchor) > U256::ZERO)
            .or_else(|| {
                profit_candidates
                    .iter()
                    .copied()
                    .find(|a| select_profit_token(&ledger, *a, &input.profit_policy).is_some())
            })
            .or_else(|| match &forwarded {
                ForwardedLookup::Hit {
                    recipient,
                    forwarded_from: from_addr,
                } => {
                    forwarded_from = Some(*from_addr);
                    Some(*recipient)
                }
                ForwardedLookup::TipSink | ForwardedLookup::None => None,
            });
        let Some(searcher) = searcher else {
            let reason = match forwarded {
                ForwardedLookup::TipSink => RejectReason::BuilderTip,
                _ => RejectReason::NetZeroSearcher,
            };
            rejects.push(classify_reject(input.block, tx.tx_index, reason, None));
            continue;
        };
        let is_forwarded = forwarded_from.is_some();
        // When the EOA only funds the anchor into the searcher contract, the
        // contract ledger still shows +amountOut (funding credit + cycle).
        // Combine with the EOA's negative anchor residual so profit is
        // amountOut−amountIn, not the gross contract balance.
        let amount = anchor_profit_amount(&ledger, tx.from, searcher, anchor);
        if amount.is_zero() {
            rejects.push(classify_reject(
                input.block,
                tx.tx_index,
                RejectReason::NetZeroSearcher,
                Some("anchor_profit_zero".into()),
            ));
            continue;
        }

        // Material deficit on the profit holder: outside-walk negatives always
        // veto; in-walk negatives veto unless dust (≤1 wei or ≤1 bps of hop).
        if has_material_deficit(&ledger, searcher, &walk_tokens, &hop_max) {
            rejects.push(classify_reject(
                input.block,
                tx.tx_index,
                RejectReason::MaterialDeficit,
                None,
            ));
            continue;
        }
        // EOA may fund only the anchor into the profit holder; any other
        // ERC-20 spend by tx.from (outside-walk or material in-walk) vetoes.
        if initiator_blocks_cycle(tx, &ledger, searcher, anchor, &walk_tokens, &hop_max) {
            rejects.push(classify_reject(
                input.block,
                tx.tx_index,
                RejectReason::OpenInventory,
                None,
            ));
            continue;
        }

        // Anchor-only profit (matches mev.zone amountOut−amountIn). Extra
        // positive residuals stay in details, not profit_tokens (USD-sum).
        let profit_tokens: Vec<(Address, U256)> = vec![(anchor, amount)];
        let other_positives: Vec<serde_json::Value> = ledger
            .positive_tokens(searcher)
            .into_iter()
            .filter(|t| *t != anchor)
            .map(|t| {
                serde_json::json!({
                    "token": format!("{t:#x}"),
                    "amount": ledger.net(searcher, t).to_string(),
                })
            })
            .collect();
        // P0.4: flash-swap / flash-loan flag when principal was borrowed.
        let flash_arb = !tx.flashloans.is_empty() || flashloan_fee_wei.is_some();
        // P1.3: multi-hop arb whose route includes a non-blue-chip token.
        let long_tail = is_long_tail_arb(&tx.swaps, &input.profit_policy.priority);
        let mut tags: Vec<&str> = Vec::new();
        if flash_arb {
            tags.push("flash_arb");
        }
        if long_tail {
            tags.push("long_tail");
        }
        if is_forwarded {
            tags.push("forwarded_profit");
        }
        tags.extend(strategy_tags::arb_strategy_tags(&tx.swaps, arb_tag_opts));
        let mut details = serde_json::json!({
            "mode": "realized",
            "tags": tags,
            "pnl_basis": PnlBasis::R.as_str(),
            "pnl": {
                "basis": PnlBasis::R.as_str(),
                "profit_amount": amount.to_string(),
                "profit_token": format!("{anchor:#x}"),
            },
            "other_residuals": other_positives,
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
        });
        if tags.contains(&"savax_rate_arb") {
            details["tag_modes"] = serde_json::json!({ "savax_rate_arb": "exact" });
        }
        // The recorded fee is already inside the residual iff its loan was
        // netted above (repay leg absent). Persist must not subtract it again.
        if flashloan_fee_wei.is_some() && flashloan_fee_wei == max_netted_fee {
            details["flashloan_fee_netted_in_ledger"] = serde_json::json!(true);
        }
        if let Some(from_addr) = forwarded_from {
            details["forwarded_from"] = serde_json::json!(format!("{from_addr:#x}"));
        }
        events.push(MevEvent {
            block: input.block,
            ts: input.ts,
            tx_index: tx.tx_index,
            tx_hash: tx.tx_hash,
            kind: MevKind::ArbAtomic,
            searcher,
            contract: if searcher == tx.from {
                None
            } else {
                tx.to.or(forwarded_from)
            },
            pools: tx.swaps.iter().map(|s| s.pool).collect(),
            profit_token: Some(anchor),
            profit_amount: Some(amount),
            profit_tokens,
            profit_usd: None, // pricing applied at persist time
            gas_cost_wei: U256::from(tx.gas_used)
                .saturating_mul(U256::from((tx.effective_gas_price_gwei * 1e9) as u128)),
            flashloan_fee_wei,
            flashloan_fee_token,
            confidence: if is_forwarded {
                Confidence::Inferred
            } else {
                Confidence::Exact
            },
            victim_hashes: vec![],
            victim_swap_size: None,
            details,
        });

        // ── 5. JIT pass (Mint+Burn same position, same block) ──────────
        // Handled block-wide below (needs cross-tx pairing); per-tx we only
        // mark minted positions.
    }

    // ── 4b. Keeper / automation execution (plan P1.5 /) ─────────────
    // Mode A: Gelato ExecSuccess / Chainlink UpkeepPerformed|LogTriggered.
    // Emitted as Unknown + tags so MevKind stays stable; P&L basis F.
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

    // ── 4c. Airdrop claim-and-sell (plan P3.13 /) ──────────────────
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

    // ── 4d. ERC-4337 bundler executions (plan P3.11 /) ─────────────
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
                    "pnl_basis": PnlBasis::GasCostAsProfit.as_str(),
                    "pnl": {
                        "basis": PnlBasis::R.as_str(),
                        "actual_gas_cost": uo.actual_gas_cost.to_string(),
                    },
                }),
            });
        }
    }

    (events, rejects)
}

fn classify_reject(
    block_number: u64,
    tx_index: u64,
    reason: RejectReason,
    detail: Option<String>,
) -> RejectedCandidate {
    RejectedCandidate {
        block_number,
        tx_index: Some(tx_index),
        strategy: CLASSIFY_STRATEGY.to_string(),
        pool_a: None,
        pool_b: None,
        path: None,
        token_in: None,
        token_out: None,
        input_amount: None,
        expected_profit: None,
        expected_profit_usd: None,
        gas_cost_wei: None,
        reject_reason: reason.as_str().to_string(),
        detail,
        created_at: crate::utils::epoch_secs(),
    }
}

/// Derive route geometry metadata for an atomic arb (spec /).
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
    let arb_shape = if hop_count == 2 || unique_tokens == 2 {
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

/// Multi-hop arb whose route includes a non-blue-chip token (plan P1.3 /).
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

/// Outcome of looking for a third-party residual after a net-zero executor.
enum ForwardedLookup {
    Hit {
        recipient: Address,
        forwarded_from: Address,
    },
    /// Single residual holder is coinbase / configured tip sink — not PnL.
    TipSink,
    None,
}

/// When no searcher candidate holds a positive anchor residual, recover a
/// single third-party recipient that received the cycle profit solely from
/// `tx.from` / `tx.to` and did not participate in swaps.
// Eight correlated ledger views; Task B2 splits the arb pass.
#[allow(clippy::too_many_arguments)]
fn forwarded_anchor_recipient(
    ledger: &DeltaLedger,
    transfers: &[TransferFact],
    swaps: &[SwapFact],
    candidates: &[Address],
    tip_sinks: &HashSet<Address>,
    anchor: Address,
    from: Address,
    to: Option<Address>,
) -> ForwardedLookup {
    let route_pools: HashSet<Address> = swaps
        .iter()
        .filter(|s| !has_unresolved_tokens(s) && s.amm != Amm::Aggregator)
        .map(|s| s.pool)
        .collect();
    let mut holders: Vec<Address> = ledger
        .pos
        .keys()
        .filter(|(addr, tok)| {
            *tok == anchor
                && !addr.is_zero()
                && !candidates.contains(addr)
                && !route_pools.contains(addr)
                && ledger.net(*addr, anchor) > U256::ZERO
        })
        .map(|(addr, _)| *addr)
        .collect();
    holders.sort();
    holders.dedup();
    if holders.len() != 1 {
        return ForwardedLookup::None;
    }
    let recipient = holders[0];
    if tip_sinks.contains(&recipient) {
        return ForwardedLookup::TipSink;
    }
    // Recipient must not have funded any swap leg (not a cycle actor).
    if swaps.iter().any(|s| s.owner == Some(recipient)) {
        return ForwardedLookup::None;
    }
    let funders: HashSet<Address> = [Some(from), to].into_iter().flatten().collect();
    let inbound: Vec<&TransferFact> = transfers
        .iter()
        .filter(|t| t.token == anchor && t.to == recipient && !t.amount.is_zero())
        .collect();
    if inbound.is_empty() {
        return ForwardedLookup::None;
    }
    if !inbound.iter().all(|t| funders.contains(&t.from)) {
        return ForwardedLookup::None;
    }
    // Prefer the contract (`to`) as forwarded_from when it sent; else EOA.
    let forwarded_from = to
        .filter(|c| inbound.iter().any(|t| t.from == *c))
        .unwrap_or(from);
    if forwarded_from.is_zero() {
        return ForwardedLookup::None;
    }
    ForwardedLookup::Hit {
        recipient,
        forwarded_from,
    }
}

/// flow ownership: a closed cycle must be attributable
/// to one searcher's route. Among direction-resolved swap legs, the distinct
/// funder-owners must collapse to a single actor, and that actor must be one
/// of the tx's searcher candidates. Fully-unattributed legs (no inbound
/// transfer observable) cannot disprove ownership and pass (preserves recall
/// for flash-mint / internal-balance flows); an unrelated actor's legs always
/// fail.
///
/// Owners that are *pools of this route* are neutral: in a genuine atomic
/// cycle the intermediate token legitimately moves pool→pool, so a hop's
/// input is funded by the previous pool rather than by the searcher contract
/// (regression: 5 mev.zone arbs where exactly one hop was funded by a route
/// pool while the rest were funded by the searcher). Only *external* owners
/// must collapse to a single candidate; a pool never appearing in the
/// resolved route (e.g. an aggregator/router) is not neutralized.
pub(crate) fn flow_attributable(swaps: &[SwapFact], candidates: &[Address]) -> bool {
    let route_pools: HashSet<Address> = swaps
        .iter()
        .filter(|s| {
            s.amm != Amm::Aggregator
                && !has_unresolved_tokens(s)
                && s.token_in != decode::TOKEN0_SENTINEL
                && s.token_in != decode::TOKEN1_SENTINEL
        })
        .map(|s| s.pool)
        .collect();
    let mut owner: Option<Address> = None;
    for s in swaps {
        if has_unresolved_tokens(s) {
            continue;
        }
        let Some(o) = s.owner else {
            continue;
        };
        if route_pools.contains(&o) {
            continue; // intermediate hop funded by a pool of this route
        }
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

/// Anchor profit: searcher net, or searcher+EOA when the EOA funded the
/// searcher (so capital inflow is not counted as profit).
fn anchor_profit_amount(
    ledger: &DeltaLedger,
    eoa: Address,
    searcher: Address,
    anchor: Address,
) -> U256 {
    let searcher_net = ledger.net_signed(searcher, anchor);
    if eoa.is_zero() || eoa == searcher {
        return if searcher_net.is_positive() {
            searcher_net.into_raw()
        } else {
            U256::ZERO
        };
    }
    let eoa_net = ledger.net_signed(eoa, anchor);
    let funded = eoa_net.is_negative() && searcher_net.is_positive();
    let combined = if funded {
        searcher_net + eoa_net
    } else {
        searcher_net
    };
    if combined.is_positive() {
        combined.into_raw()
    } else {
        U256::ZERO
    }
}

/// Largest hop amount per token across resolved swap legs (for dust bps).
fn hop_max_by_token(swaps: &[SwapFact]) -> HashMap<Address, U256> {
    let mut out: HashMap<Address, U256> = HashMap::new();
    let bump = |m: &mut HashMap<Address, U256>, tok: Address, amt: U256| {
        if tok.is_zero() || amt.is_zero() {
            return;
        }
        let e = m.entry(tok).or_insert(U256::ZERO);
        if amt > *e {
            *e = amt;
        }
    };
    for s in swaps {
        if s.amm == Amm::Aggregator || has_unresolved_tokens(s) {
            continue;
        }
        bump(&mut out, s.token_in, s.amount_in);
        bump(&mut out, s.token_out, s.amount_out);
    }
    out
}

/// True when `addr` has a material (non-dust) negative residual on any token.
/// Outside-walk deficits always count; in-walk deficits need >1 wei and >1 bps.
fn has_material_deficit(
    ledger: &DeltaLedger,
    addr: Address,
    walk_tokens: &HashSet<Address>,
    hop_max: &HashMap<Address, U256>,
) -> bool {
    for t in ledger.touched_tokens(addr) {
        if t == NATIVE_MARKER {
            continue;
        }
        let def = deficit_amount(ledger, addr, t);
        if def.is_zero() {
            continue;
        }
        if !walk_tokens.contains(&t) {
            return true;
        }
        let max = hop_max.get(&t).copied().unwrap_or(U256::ZERO);
        if !is_dust_deficit(def, max) {
            return true;
        }
    }
    false
}

/// EOA funding of the anchor into the profit holder is allowed. Any other
/// material ERC-20 spend by `tx.from` vetoes (inventory conversion).
fn initiator_blocks_cycle(
    tx: &TxInput,
    ledger: &DeltaLedger,
    searcher: Address,
    anchor: Address,
    walk_tokens: &HashSet<Address>,
    hop_max: &HashMap<Address, U256>,
) -> bool {
    if tx.from.is_zero() || tx.from == searcher {
        return false;
    }
    for t in ledger.touched_tokens(tx.from) {
        if t == NATIVE_MARKER {
            continue;
        }
        let def = deficit_amount(ledger, tx.from, t);
        if def.is_zero() {
            continue;
        }
        if t == anchor {
            // Allowed when the EOA funded the profit holder and that holder
            // is net-positive on the anchor (do not sum EOA+contract nets).
            let funded = tx.transfers.iter().any(|tr| {
                tr.token == anchor
                    && tr.from == tx.from
                    && tr.to == searcher
                    && !tr.amount.is_zero()
            });
            if funded && ledger.net(searcher, anchor) > U256::ZERO {
                continue;
            }
            return true;
        }
        if walk_tokens.contains(&t) {
            let max = hop_max.get(&t).copied().unwrap_or(U256::ZERO);
            if is_dust_deficit(def, max) {
                continue;
            }
        }
        return true;
    }
    false
}

#[cfg(test)]
mod repay_tests {
    use super::*;
    use crate::explorer::classify::classify_block;
    use crate::explorer::test_fixtures::{
        block, swap, transfer, tx, tx_to, ATK, POOL_A, POOL_B, TOKA, USDC,
    };
    use crate::explorer::types::FlashLoanFact;
    use alloy::primitives::address;

    /// Balancer-style vault: distinct from the borrower and from the pools.
    const PROVIDER: Address = address!("7000000000000000000000000000000000000009");

    /// One tx: 1000 USDC flash loan in, two-hop arb to +200 USDC, repaid as
    /// two 500 transfer legs (Balancer split repay) instead of one 1000 leg.
    fn split_repay_input() -> BlockInput {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 1000, 1100),
                swap(POOL_B, TOKA, USDC, 1100, 1200),
            ],
            vec![
                transfer(0, USDC, PROVIDER, ATK, 1000), // loan in
                transfer(1, USDC, ATK, POOL_A, 1000),
                transfer(2, TOKA, POOL_A, ATK, 1100),
                transfer(3, TOKA, ATK, POOL_B, 1100),
                transfer(4, USDC, POOL_B, ATK, 1200),
                transfer(5, USDC, ATK, PROVIDER, 500), // repay half
                transfer(6, USDC, ATK, PROVIDER, 500), // repay half
            ],
        );
        t.flashloans.push(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "balancer_v2",
            initiator: ATK,
            token: USDC,
            amount: U256::from(1000u64),
            fee: Some(U256::ZERO),
            recipient: ATK,
            provider: PROVIDER,
        });
        block(vec![t])
    }

    /// Two half-amount repayments to the provider must count as repaid, so
    /// `net_flash_loan` must NOT subtract the principal again (which erases
    /// the real +200 profit and rejects the op as `NetZeroSearcher`).
    #[test]
    fn split_flash_repay_is_detected() {
        let input = split_repay_input();
        let out = classify_block(&input);
        assert!(
            out.events.iter().any(|e| e.kind == MevKind::ArbAtomic),
            "split-repay flash arb must classify: rejects were {:?}",
            out.rejects
        );
        let arb = out.events.iter().find(|e| e.kind == MevKind::ArbAtomic);
        assert_eq!(
            arb.and_then(|e| e.profit_amount),
            Some(U256::from(200u64)),
            "principal must not be netted twice; residual is +200"
        );
    }

    /// Executor contract distinct from the EOA: the loan principal is held by
    /// the recipient (`tx.to`), and the provider pulls the repay back
    /// internally (no repay Transfer). Only the recipient owns that principal,
    /// so netting the EOA as well erases the swept +200 and rejects the op.
    fn executor_owned_loan_input() -> BlockInput {
        const EXEC: Address = address!("9000000000000000000000000000000000000009");
        let mut t = tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 1000, 1100),
                swap(POOL_B, TOKA, USDC, 1100, 1200),
            ],
            vec![
                transfer(0, USDC, PROVIDER, EXEC, 1000), // loan in → executor
                transfer(1, USDC, EXEC, POOL_A, 1000),
                transfer(2, TOKA, POOL_A, EXEC, 1100),
                transfer(3, TOKA, EXEC, POOL_B, 1100),
                transfer(4, USDC, POOL_B, EXEC, 1200),
                // Profit sweep; no repay Transfer follows (the provider pulls
                // 1005 back internally), so this exercises the `!repaid` branch.
                transfer(5, USDC, EXEC, ATK, 200),
            ],
        );
        t.flashloans.push(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: ATK,
            token: USDC,
            amount: U256::from(1000u64),
            fee: Some(U256::from(5u64)),
            recipient: EXEC,
            provider: PROVIDER,
        });
        block(vec![t])
    }

    /// The borrowed principal must be netted against exactly one owner (the
    /// loan recipient), leaving the EOA's swept profit intact.
    #[test]
    fn executor_held_flash_loan_is_netted_against_one_owner() {
        let input = executor_owned_loan_input();
        let out = classify_block(&input);
        assert!(
            out.events.iter().any(|e| e.kind == MevKind::ArbAtomic),
            "executor-held flash loan must classify: rejects were {:?}",
            out.rejects
        );
        let arb = out
            .events
            .iter()
            .find(|e| e.kind == MevKind::ArbAtomic)
            .expect("arb present");
        assert_eq!(
            arb.profit_amount,
            Some(U256::from(200u64)),
            "the EOA's swept profit (200) must survive; only the recipient owns the principal"
        );
        // The recorded fee came from the netted loan, so persist must not
        // subtract it from the already fee-netted residual again.
        assert_eq!(
            arb.details
                .get("flashloan_fee_netted_in_ledger")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }
}
