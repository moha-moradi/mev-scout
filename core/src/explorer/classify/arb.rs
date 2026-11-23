//! Atomic-arb attribution and adjacent Exact fingerprints
//! (keeper / claim-and-sell).
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::decode;
use crate::explorer::profit::{
    deficit_amount, is_dust_deficit, net_flash_loan, select_profit_token, DeltaLedger,
    ProfitTokenPolicy, NATIVE_MARKER,
};
use crate::explorer::reject::{RejectReason, RejectedCandidate};
use crate::explorer::strategy_tags::{self, ArbTagOpts};
use crate::explorer::types::{
    Amm, Confidence, MevEvent, MevKind, PnlBasis, SwapFact, TransferFact,
};
use crate::types::opportunity::underlying_pools_hash;
use crate::types::tags::canonical_funding_source;

use super::legs::{
    resolve_arb_routes, resolved_cycle_edges, searcher_candidates, ResolvedArbRoute,
};
use super::{append_tag, has_unresolved_tokens, BlockInput, TxInput};

const CLASSIFY_STRATEGY: &str = "explorer_classify";

/// Swap attribution + atomic arb + keeper/claim, in that order.
pub(super) fn classify_arbs(
    input: &BlockInput,
    arb_tag_opts: &ArbTagOpts<'_>,
) -> (Vec<MevEvent>, Vec<RejectedCandidate>) {
    let (mut events, rejects) = classify_atomic_arbs(input, arb_tag_opts);
    let keepers = classify_keepers(input);
    let claims = classify_claim_and_sell(input, &mut events);
    events.extend(keepers);
    events.extend(claims);
    (events, rejects)
}

/// Swap attribution + atomic arb pass (ledger, flash-loan netting, cycle).
pub(super) fn classify_atomic_arbs(
    input: &BlockInput,
    arb_tag_opts: &ArbTagOpts<'_>,
) -> (Vec<MevEvent>, Vec<RejectedCandidate>) {
    let mut events: Vec<MevEvent> = Vec::new();
    let mut rejects: Vec<RejectedCandidate> = Vec::new();
    let mut seen_gmx_adl = false;
    for tx in &input.txs {
        // P3.7: a forced-deleveraging event (ADL, GMX liquidation, or impact
        // distribution) in this or a prior tx can explain a later arb; a later
        // event cannot explain an earlier one. The route must also touch a
        // configured GMX market token, so a block-level coincidence is not a tag.
        let adl_seen = seen_gmx_adl
            || tx.gmx_events.iter().any(|g| {
                matches!(g.kind, "adl" | "liquidation" | "impact")
                    && (input.ctx.gmx_event_emitters.is_empty()
                        || input.ctx.gmx_event_emitters.contains(&g.emitter))
            });
        seen_gmx_adl = adl_seen;
        if !tx.success {
            continue;
        }
        if tx.swaps.is_empty() {
            continue;
        }

        let mut ledger = DeltaLedger::from_transfers(
            &tx.transfers,
            input.ctx.wrapped_native,
            (tx.from, tx.value),
        );

        // net flash-loan borrow/repay before computing residuals so
        // borrowed principal never counts as profit. Only net when the repay
        // leg is *absent* from the Transfer stream (provider callback internal
        // repayments); when the repay Transfer to the provider is present the
        // ledger already nets it, so subtracting again would erase real profit.
        let mut fees: Vec<(Address, U256)> = Vec::new();
        // Fees the ledger netted itself (repay leg absent). Repaid transfers
        // are already subtracted by the ledger; this set only decides whether
        // the recorded total can be marked as already netted.
        let mut netted_fee_total: Option<(Address, U256)> = None;
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
                if !fee.is_zero() {
                    netted_fee_total = match netted_fee_total {
                        None => Some((fl.token, fee)),
                        Some((token, sum)) if token == fl.token => {
                            sum.checked_add(fee).map(|n| (token, n))
                        }
                        Some(_) => None,
                    };
                }
            }
            if !fee.is_zero() {
                fees.push((fl.token, fee));
            }
        }
        // One fee token and a finite sum can be recorded as the complete
        // financing cost. Mixed tokens or overflow stay explicitly unknown;
        // the dashboard then refuses to put the operation in the ROI cohort.
        let fee_complete = fees
            .first()
            .map(|(token, _)| fees.iter().all(|(t, _)| t == token))
            .unwrap_or(true);
        let (flashloan_fee_wei, flashloan_fee_token) = if fee_complete {
            let token = fees.first().map(|(t, _)| *t);
            let total = fees
                .iter()
                .try_fold(U256::ZERO, |acc, (_, fee)| acc.checked_add(*fee));
            match (token, total) {
                (Some(t), Some(total)) if !total.is_zero() => (Some(total), Some(t)),
                _ => (None, None),
            }
        } else {
            (None, None)
        };

        // Participant scope: sender + receiver-side contracts (searcher
        // attribution: "EOA sender (or its deployed contract)") + addresses
        // with ≥2 transfer participations (see `searcher_candidates`).
        let candidates = searcher_candidates(tx);

        // ── Atomic arb ─────────────────────────────────────────────────
        // Resolved directional edges (token_in, token_out, pool). Aggregator/
        // router legs are the router's *internal* routing (e.g. a token→USDC
        // sale split across its own pools), decoded only by token proximity;
        // letting them close the cycle mislabels a one-way aggregator
        // conversion as the searcher's atomic arb (regression: two ~$76–78
        // USDC "arbs" via the KyberSwap executor that mev.zone does not
        // count). Genuine searcher cycles run over real DEX pools (v2/v3/v4).
        let resolved_edges = resolved_cycle_edges(tx);
        let routes = resolve_arb_routes(tx);

        // One event per tx, but the route is chosen from *all* closed walks
        // using ledger evidence instead of whichever walk DFS found first.
        // A cycle owned by an unrelated actor cannot be this tx's arb even if
        // its ledger residual looks attractive; filter before scoring so a
        // valid second cycle is not lost behind a contaminated first one.
        let attributable_routes: Vec<&ResolvedArbRoute> = routes
            .iter()
            .filter(|r| {
                let swaps: Vec<SwapFact> = r.swaps(tx).into_iter().cloned().collect();
                flow_attributable(&swaps, &candidates)
            })
            .collect();
        let Some(route) = select_route(
            &attributable_routes,
            &ledger,
            tx.from,
            &candidates,
            &input.ctx.profit_policy,
        ) else {
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
        let anchor = route.anchor;
        let walk_tokens = route.walk_tokens();
        // The selected walk is the single source of truth for route metadata,
        // tags, pools and canonical hash. Unrelated receipt swaps stay out.
        let route_swaps: Vec<SwapFact> = route.swaps(tx).into_iter().cloned().collect();
        let gmx_route_hit = route_swaps.iter().any(|s| {
            input.ctx.gmx_market_tokens.contains(&s.token_in)
                || input.ctx.gmx_market_tokens.contains(&s.token_out)
        });
        let gmx_adl_signal = adl_seen && gmx_route_hit;
        if !flow_attributable(&route_swaps, &candidates) {
            rejects.push(classify_reject(
                input.block,
                tx.tx_index,
                RejectReason::MixedFlowOwner,
                None,
            ));
            continue;
        }

        let hop_max = hop_max_by_token(&route_swaps);
        // Route pools often appear in `candidates` via transfer participation and
        // can hold a transient positive residual on the anchor (inventory of the
        // hop). They must never be the profit holder — otherwise a net-zero
        // executor that forwards profit never reaches the forwarded heuristic.
        let route_pools: HashSet<Address> = route.edges.iter().map(|e| e.pool).collect();
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
        let mut forwarded_token: Option<Address> = None;
        let forwarded = forwarded_recipient(
            &ledger,
            &tx.transfers,
            &route_swaps,
            &profit_candidates,
            &input.tip_sinks,
            &input.ctx.profit_policy.priority,
            tx.from,
            tx.to,
        );
        let candidate = profit_candidates
            .iter()
            .copied()
            .find(|a| ledger.net(*a, anchor) > U256::ZERO)
            .or_else(|| {
                profit_candidates
                    .iter()
                    .copied()
                    .find(|a| select_profit_token(&ledger, *a, &input.ctx.profit_policy).is_some())
            });
        let forwarded_hit = match &forwarded {
            ForwardedLookup::Hit {
                recipient,
                forwarded_from: from_addr,
                token,
            } => Some((*recipient, *from_addr, *token)),
            ForwardedLookup::TipSink | ForwardedLookup::None => None,
        };
        // A minor beneficiary can qualify as a candidate (>=2 transfer
        // participations) while the bulk of the profit is forwarded to a
        // single non-candidate payee. Prefer the forwarded payee when it
        // holds more of the same token; otherwise keep the exact candidate.
        let (searcher, chosen_forwarded) = match (candidate, forwarded_hit) {
            (Some(cand), Some((recipient, from_addr, token)))
                if {
                    let (cand_token, cand_amount) = candidate_profit(
                        &ledger,
                        tx.from,
                        cand,
                        anchor,
                        &input.ctx.profit_policy.priority,
                    );
                    cand_token == token
                        && cand_amount < profit_amount(&ledger, tx.from, recipient, token)
                } =>
            {
                (Some(recipient), Some((from_addr, token)))
            }
            (Some(cand), _) => (Some(cand), None),
            (None, Some((recipient, from_addr, token))) => {
                (Some(recipient), Some((from_addr, token)))
            }
            (None, None) => (None, None),
        };
        if let Some((from_addr, token)) = chosen_forwarded {
            forwarded_from = Some(from_addr);
            forwarded_token = Some(token);
        }
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
        //
        // The profit is not always in the anchor token: an exact-input route
        // repays the anchor leg exactly (net zero) and keeps the surplus in
        // another priority token (usually wrapped native). mev.zone reports
        // that surplus as the tx profit, so fall back to the first priority
        // token with a positive residual. Forwarded profit uses the token the
        // beneficiary actually received.
        let (profit_token, amount) = if let Some(token) = forwarded_token {
            (token, profit_amount(&ledger, tx.from, searcher, token))
        } else {
            candidate_profit(
                &ledger,
                tx.from,
                searcher,
                anchor,
                &input.ctx.profit_policy.priority,
            )
        };
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

        // Anchor-token profit when the cycle returns the surplus in the start
        // token; otherwise the priority token that retained it. Extra positive
        // residuals go to `details.other_residuals` and are intentionally kept
        // out of `profit_tokens`, which persist USD-sums as-is (see
        // `MevEvent::profit_tokens`).
        let profit_tokens: Vec<(Address, U256)> = vec![(profit_token, amount)];
        let other_positives: Vec<serde_json::Value> = ledger
            .positive_tokens(searcher)
            .into_iter()
            .filter(|t| *t != profit_token)
            .map(|t| {
                serde_json::json!({
                    "token": format!("{t:#x}"),
                    "amount": ledger.net(searcher, t).to_string(),
                })
            })
            .collect();
        // P0.4: flash-swap / flash-loan flag when principal was borrowed.
        let flash_arb = !tx.flashloans.is_empty() || flashloan_fee_wei.is_some();
        // G-03: every flash loan is a funding layer, not only the first. Keep
        // first-appearance order so `funding_source` remains the
        // backward-compatible first entry and the canonical ID is unchanged.
        let funding_providers: Vec<&'static str> = {
            let mut seen = HashSet::new();
            tx.flashloans
                .iter()
                .map(|fl| canonical_funding_source(fl.protocol))
                .filter(|provider| seen.insert(*provider))
                .collect()
        };
        // P1.3: multi-hop arb whose route includes a non-blue-chip token.
        let long_tail = is_long_tail_arb(&route_swaps, &input.ctx.profit_policy.priority);
        let mut tags: Vec<&str> = Vec::new();
        if flash_arb {
            tags.push("flash_arb");
            // WP2: the decoder identifies the funding layer. Keep the legacy
            // alias above and add a namespaced axis tag for every provider.
            for fl in &tx.flashloans {
                let tag = decode::flash_loan_funding_tag(fl);
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
        }
        if long_tail {
            tags.push("long_tail");
        }
        if is_forwarded {
            tags.push("forwarded_profit");
        }
        let tag_opts = ArbTagOpts {
            gmx_adl_signal,
            ..*arb_tag_opts
        };
        tags.extend(strategy_tags::arb_strategy_tags(&route_swaps, &tag_opts));
        let delivered = route_delivery(tx, &route_swaps);
        let mut details = serde_json::json!({
            "mode": "realized",
            "tags": tags,
            "pnl_basis": PnlBasis::R.as_str(),
            "pnl": {
                "basis": PnlBasis::R.as_str(),
                "profit_amount": amount.to_string(),
                "profit_token": format!("{profit_token:#x}"),
            },
            "other_residuals": other_positives,
            "route": route.edges.iter().enumerate().map(|(i, e)| {
                let (in_net, out_net) = delivered.get(i).copied().unwrap_or((None, None));
                serde_json::json!({
                    "swap_index": e.swap_index,
                    "log_index": e.log_index,
                    "pool": format!("{:#x}", e.pool),
                    "amm": e.amm.as_str(),
                    "token_in": format!("{:#x}", e.token_in),
                    "token_out": format!("{:#x}", e.token_out),
                    "token_source": tx.swaps[e.swap_index].token_source.as_str(),
                    "amount_in": e.amount_in.to_string(),
                    "amount_out": e.amount_out.to_string(),
                    // Tokens actually received by this pool / delivered to the
                    // next hop. Null when the flow is searcher-mediated or the
                    // same (pool, token) appears in several swaps.
                    "amount_in_net": in_net.map(|v| v.to_string()),
                    "amount_out_net": out_net.map(|v| v.to_string()),
                })
            }).collect::<Vec<_>>(),
            "arb_meta": arb_route_meta(&route_swaps, flash_arb, &input.ctx.pool_dex),
        });
        // WP5 / M-10: persist the realized funding provider(s), router
        // generation and underlying pool set. `funding_source` stays the first
        // provider so pre-G-03 canonical IDs are byte-identical; the array
        // carries every layer for multi-flash routes. `explorer_canonical_id`
        // reads only `funding_source`, never the array.
        if let Some(first) = funding_providers.first() {
            details["funding_source"] = serde_json::json!(first);
        }
        if !funding_providers.is_empty() {
            details["funding_providers"] = serde_json::json!(funding_providers);
        }
        let underlying_pools = route.pools();
        if !underlying_pools.is_empty() {
            details["underlying_pools_hash"] =
                serde_json::json!(format!("{:#x}", underlying_pools_hash(&underlying_pools)));
        }
        // G-12: aggregator-mediated routes expose only the selected DEX cycle
        // as `underlying_pools_hash`; the router's internal path is opaque in
        // logs-only mode, so the pool set is a lower bound on the tx's full
        // path, not exact.
        if tx.route_provenance.is_some() {
            details["underlying_pools_unknown"] = serde_json::json!(true);
        }
        // Route attribution is a separate confidence axis from the realized
        // P&L basis. Unknown leg owners or competing closed walks only lower
        // this classification signal; they never invent or erase ledger profit.
        let unattributed_flow = route.unattributed_legs();
        details["candidate_route_count"] = serde_json::json!(routes.len());
        // G-10: expose the closed walks that lost selection so the one-event
        // undercount is auditable. This is metadata, never a second P&L row.
        let rejected_routes: Vec<serde_json::Value> = routes
            .iter()
            .filter(|r| !std::ptr::eq(*r, route))
            .map(|r| {
                let attributable = attributable_routes
                    .iter()
                    .any(|candidate| std::ptr::eq(*candidate, r));
                serde_json::json!({
                    "anchor": format!("{:#x}", r.anchor),
                    "hop_count": r.edges.len(),
                    "pools": r
                        .pools()
                        .iter()
                        .map(|pool| format!("{pool:#x}"))
                        .collect::<Vec<_>>(),
                    "attributable": attributable,
                })
            })
            .collect();
        details["rejected_routes"] = serde_json::json!(rejected_routes);
        // G-07: mechanism attribution only; `profit_amount`/`net_profit_usd`
        // and the `R` basis stay the sole P&L authority.
        details["profit_cause"] = profit_cause(&tags);
        details["unattributed_flow"] = serde_json::json!(unattributed_flow);
        details["classification_confidence"] =
            serde_json::json!(if routes.len() == 1 && unattributed_flow == 0 {
                "exact"
            } else {
                "inferred"
            });
        if let Some(version) = tx
            .route_provenance
            .as_ref()
            .and_then(|p| p.router_version.as_deref())
        {
            details["router_version"] = serde_json::json!(version);
        }
        if tags.contains(&"savax_rate_arb") {
            details["tag_modes"] = serde_json::json!({ "savax_rate_arb": "exact" });
        }
        details["flashloan_fee_complete"] = serde_json::json!(fee_complete);
        if !fee_complete {
            details["flashloan_fees"] = serde_json::json!(fees
                .iter()
                .map(|(t, f)| serde_json::json!({
                    "token": format!("{t:#x}"),
                    "fee_wei": f.to_string(),
                }))
                .collect::<Vec<_>>());
        }
        // The recorded fee is already inside the residual only when every
        // nonzero fee was netted by the ledger. Persist must not subtract it
        // again in that case.
        if let (Some(total), Some((token, netted))) = (flashloan_fee_wei, netted_fee_total) {
            if flashloan_fee_token == Some(token) && total == netted {
                details["flashloan_fee_netted_in_ledger"] = serde_json::json!(true);
            }
        }
        if let Some(from_addr) = forwarded_from {
            details["forwarded_from"] = serde_json::json!(format!("{from_addr:#x}"));
        }
        events.push(MevEvent {
            pools: route_swaps.iter().map(|s| s.pool).collect(),
            profit_token: Some(profit_token),
            profit_amount: Some(amount),
            profit_tokens,
            gas_cost_wei: super::gas::tx_gas_cost_wei(tx),
            flashloan_fee_wei,
            flashloan_fee_token,
            confidence: if is_forwarded {
                Confidence::Inferred
            } else {
                Confidence::Exact
            },
            details,
            ..MevEvent::for_tx(
                input.block,
                input.ts,
                tx.tx_index,
                tx.tx_hash,
                MevKind::ArbAtomic,
                searcher,
                if searcher == tx.from {
                    None
                } else {
                    tx.to.or(forwarded_from)
                },
            )
        });
    }
    (events, rejects)
}

/// Pick the candidate cycle this tx's profit belongs to. Only one event is
/// emitted per tx, so ranking must be explicit rather than "first DFS walk".
///
/// The ranking is intentionally conservative: no raw amounts of different
/// tokens are compared (that would be a valuation guess). It prefers a route
/// whose anchor actually holds a searcher residual, then the route touching
/// the highest-priority residual token, then the shorter / lower-log-index
/// route as a deterministic tie-break.
fn select_route<'a>(
    routes: &[&'a ResolvedArbRoute],
    ledger: &DeltaLedger,
    eoa: Address,
    candidates: &[Address],
    policy: &ProfitTokenPolicy,
) -> Option<&'a ResolvedArbRoute> {
    let anchor_hit = |route: &ResolvedArbRoute| {
        candidates
            .iter()
            .any(|c| !profit_amount(ledger, eoa, *c, route.anchor).is_zero())
    };
    let priority_rank = |route: &ResolvedArbRoute| {
        let tokens = route.walk_tokens();
        policy
            .priority
            .iter()
            .position(|t| {
                tokens.contains(t)
                    && candidates
                        .iter()
                        .any(|c| !profit_amount(ledger, eoa, *c, *t).is_zero())
            })
            .unwrap_or(usize::MAX)
    };

    let tie_key = |route: &ResolvedArbRoute| {
        (
            route.edges.len(),
            route
                .edges
                .iter()
                .map(|e| (e.log_index, e.swap_index))
                .collect::<Vec<_>>(),
        )
    };

    let mut best: Option<&ResolvedArbRoute> = None;
    for &route in routes {
        let better = match best {
            None => true,
            Some(prev) => {
                if anchor_hit(route) != anchor_hit(prev) {
                    anchor_hit(route)
                } else if priority_rank(route) != priority_rank(prev) {
                    priority_rank(route) < priority_rank(prev)
                } else {
                    tie_key(route) < tie_key(prev)
                }
            }
        };
        if better {
            best = Some(route);
        }
    }
    best
}

/// Keeper / automation execution (plan P1.5): Gelato ExecSuccess /
/// Chainlink UpkeepPerformed|LogTriggered. Emitted as Unknown + tags so
/// MevKind stays stable; P&L basis F.
pub(super) fn classify_keepers(input: &BlockInput) -> Vec<MevEvent> {
    let mut events: Vec<MevEvent> = Vec::new();
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
                profit_token: keeper.fee_token,
                profit_amount: fee,
                details,
                ..MevEvent::for_tx(
                    input.block,
                    input.ts,
                    tx.tx_index,
                    tx.tx_hash,
                    MevKind::Utility,
                    tx.from,
                    tx.to,
                )
            });
        }
    }
    events
}

/// Airdrop claim-and-sell (plan P3.13): Transfer from zero + same-tx sell
/// swap; held claims are ignored. When the tx already produced ArbAtomic in
/// `arb_events`, only the tag is attached there (no double count).
pub(super) fn classify_claim_and_sell(
    input: &BlockInput,
    arb_events: &mut [MevEvent],
) -> Vec<MevEvent> {
    let mut events: Vec<MevEvent> = Vec::new();
    for tx in &input.txs {
        if !tx.success {
            continue;
        }
        let Some(token) = strategy_tags::claim_and_sell_token(&tx.transfers, &tx.swaps) else {
            continue;
        };
        if let Some(ev) = arb_events
            .iter_mut()
            .find(|e| e.tx_index == tx.tx_index && e.kind == MevKind::ArbAtomic)
        {
            append_tag(&mut ev.details, "claim_and_sell");
            append_tag(&mut ev.details, "airdrop");
            ev.details["claimed_token"] = serde_json::json!(format!("{:#x}", token));
            continue;
        }
        let ledger = DeltaLedger::from_transfers(
            &tx.transfers,
            input.ctx.wrapped_native,
            (tx.from, tx.value),
        );
        let Some(pt) = select_profit_token(&ledger, tx.from, &input.ctx.profit_policy) else {
            continue;
        };
        let pa = ledger.net(tx.from, pt);
        if pa.is_zero() {
            continue;
        }
        events.push(MevEvent {
            pools: tx.swaps.iter().map(|s| s.pool).collect(),
            profit_token: Some(pt),
            profit_amount: Some(pa),
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
            ..MevEvent::for_tx(
                input.block,
                input.ts,
                tx.tx_index,
                tx.tx_hash,
                MevKind::Utility,
                tx.from,
                tx.to,
            )
        });
    }
    events
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
        ..Default::default()
    }
}

/// Per swap leg: `(received_by_pool, delivered_to_next_hop)`.
///
/// Uses the receipt Transfer stream, so a transfer tax or an overpayment is
/// visible instead of the nominal swap amount being trusted blindly. `None`
/// means the amount is not attributable (searcher-mediated hop, ambiguous
/// duplicate pool/token, or no matching transfer).
fn route_delivery(tx: &TxInput, swaps: &[SwapFact]) -> Vec<(Option<U256>, Option<U256>)> {
    let mut counts: HashMap<(Address, Address), usize> = HashMap::new();
    for s in swaps {
        *counts.entry((s.pool, s.token_in)).or_default() += 1;
    }
    fn transfer_sum(tx: &TxInput, pred: impl Fn(&TransferFact) -> bool) -> Option<U256> {
        let total = tx
            .transfers
            .iter()
            .filter(|t| pred(t))
            .fold(U256::ZERO, |acc, t| acc.saturating_add(t.amount));
        (!total.is_zero()).then_some(total)
    }
    swaps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let unique_in = counts.get(&(s.pool, s.token_in)).copied().unwrap_or(0) == 1;
            let in_net = if unique_in {
                transfer_sum(tx, |t| t.to == s.pool && t.token == s.token_in)
            } else {
                None
            };
            let out_net = swaps
                .get(i + 1)
                .filter(|n| n.token_in == s.token_out)
                .and_then(|n| {
                    transfer_sum(tx, |t| {
                        t.from == s.pool && t.to == n.pool && t.token == s.token_out
                    })
                });
            (in_net, out_net)
        })
        .collect()
}

/// Derive route geometry metadata for an atomic arb (spec /).
/// Does not invent new `MevKind`s — annotation only.
fn arb_route_meta(
    swaps: &[SwapFact],
    flashloan_funded: bool,
    pool_dex: &HashMap<Address, String>,
) -> serde_json::Value {
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
    // G-02: `arb_shape` is a token-count label, matching the existing docs
    // ("triangular 6-hop", "two_pool 4-hop"). Pool geometry is reported
    // separately so a shape claim is reproducible from pool count instead of
    // conflating hops with pools; `hop_count` stays the hop fact.
    let arb_shape = match unique_tokens {
        2 => "two_pool",
        3 => "triangular",
        _ => "multi_hop",
    };
    let pool_shape = match pools.len() {
        2 => "two_pool",
        3 => "three_pool",
        _ => "multi_pool",
    };
    // Prefer real DEX identity when every leg has one. Without it we keep the
    // legacy AMM-family answer but say so explicitly instead of silently
    // equating "v2" with a DEX.
    let known_venues: Vec<&str> = swaps
        .iter()
        .filter_map(|s| {
            pool_dex
                .get(&s.pool)
                .map(|v| v.as_str())
                .filter(|v| !v.is_empty())
        })
        .collect();
    let protocol_basis = !swaps.is_empty() && known_venues.len() == swaps.len();
    let mut venues: Vec<&str> = known_venues;
    venues.sort_unstable();
    venues.dedup();
    let (dex_count, is_cross_dex, cross_dex_basis) = if protocol_basis {
        (venues.len(), venues.len() > 1, "protocol")
    } else {
        (amms.len(), amms.len() > 1, "amm_family")
    };
    let mut meta = serde_json::json!({
        "hop_count": hop_count,
        "pool_count": pools.len(),
        "amm_family_count": amms.len(),
        // Legacy alias: protocol identity when known, AMM-family count otherwise.
        "dex_count": dex_count,
        "is_cross_dex": is_cross_dex,
        "cross_dex_basis": cross_dex_basis,
        "flashloan_funded": flashloan_funded,
        "arb_shape": arb_shape,
        "arb_shape_basis": "token_count",
        "pool_shape": pool_shape,
        "unique_tokens": unique_tokens,
    });
    if protocol_basis {
        meta["venues"] = serde_json::json!(venues);
    }
    meta
}

/// G-07: why the realized residual exists, as metadata. The mechanism is a
/// label over already-emitted strategy tags; the P&L authority stays
/// `compute_net_profit` and the family basis (`R`) is unchanged.
fn profit_cause(tags: &[&str]) -> serde_json::Value {
    const MECHANISMS: &[&str] = &[
        "savax_rate_arb",
        "gmx_adl_arb",
        "epoch_transition",
        "curve_imbalance",
        "rate_provider_staleness",
        "fot_arb",
        "rebase_arb",
    ];
    let evidence_tags: Vec<&str> = MECHANISMS
        .iter()
        .copied()
        .filter(|tag| tags.contains(tag))
        .collect();
    serde_json::json!({
        "mechanism": evidence_tags.first().copied().unwrap_or("cross_pool_arb"),
        "evidence_tags": evidence_tags,
        "basis": PnlBasis::R.as_str(),
    })
}

/// Multi-hop arb whose route includes a non-blue-chip token (plan P1.3 /).
///
/// Blue-chip allowlist = profit-token priority (stables + wrapped native).
/// Requires ≥2 swap legs so single-hop dust is never labeled long-tail.
pub(crate) fn is_long_tail_arb(swaps: &[SwapFact], blue_chips: &[Address]) -> bool {
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
        token: Address,
    },
    /// Single residual holder is coinbase / configured tip sink — not PnL.
    TipSink,
    None,
}

/// When no searcher candidate holds a positive anchor residual, recover a
/// single third-party recipient that received the cycle profit solely from
/// `tx.from` / `tx.to` and did not participate in swaps.
///
/// The surplus is not always denominated in the anchor token: an exact-input
/// route can repay the anchor leg to the wei and forward the remainder in
/// wrapped native. Every priority token is therefore considered, in order.
// Eight correlated ledger views of one tx.
#[allow(clippy::too_many_arguments)]
fn forwarded_recipient(
    ledger: &DeltaLedger,
    transfers: &[TransferFact],
    swaps: &[SwapFact],
    candidates: &[Address],
    tip_sinks: &HashSet<Address>,
    tokens: &[Address],
    from: Address,
    to: Option<Address>,
) -> ForwardedLookup {
    let route_pools: HashSet<Address> = swaps
        .iter()
        .filter(|s| !has_unresolved_tokens(s) && s.amm != Amm::Aggregator)
        .map(|s| s.pool)
        .collect();
    let funders: HashSet<Address> = [Some(from), to].into_iter().flatten().collect();
    for token in tokens {
        let mut holders: Vec<Address> = ledger
            .pos
            .keys()
            .filter(|(addr, tok)| {
                *tok == *token
                    && !addr.is_zero()
                    && !candidates.contains(addr)
                    && !route_pools.contains(addr)
                    && ledger.net(*addr, *token) > U256::ZERO
            })
            .map(|(addr, _)| *addr)
            .collect();
        holders.sort();
        holders.dedup();
        // A positive holder financed by a third party is a bystander (a
        // victim, fee address, or inventory hop), not the forwarded payee.
        // Drop those *before* requiring a single recipient: one such holder
        // must not hide the one funder-fed payee.
        holders.retain(|recipient| {
            let inbound: Vec<&TransferFact> = transfers
                .iter()
                .filter(|t| t.token == *token && t.to == *recipient && !t.amount.is_zero())
                .collect();
            !inbound.is_empty() && inbound.iter().all(|t| funders.contains(&t.from))
        });
        if holders.len() != 1 {
            continue;
        }
        let recipient = holders[0];
        // Recipient must not have funded any swap leg (not a cycle actor).
        if swaps.iter().any(|s| s.owner == Some(recipient)) {
            continue;
        }
        if tip_sinks.contains(&recipient) {
            return ForwardedLookup::TipSink;
        }
        // Prefer the contract (`to`) as forwarded_from when it sent; else EOA.
        let forwarded_from = to
            .filter(|c| {
                transfers.iter().any(|t| {
                    t.token == *token && t.to == recipient && t.from == *c && !t.amount.is_zero()
                })
            })
            .unwrap_or(from);
        if forwarded_from.is_zero() {
            continue;
        }
        return ForwardedLookup::Hit {
            recipient,
            forwarded_from,
            token: *token,
        };
    }
    ForwardedLookup::None
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
/// must be tx-level actors. A multi-router route is still one searcher's
/// cycle when every external funder is a candidate (sender, receiver-side
/// contract, or a transfer participant): the executor contract and the
/// searcher contract each fund their own legs.
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
    let mut owners: Vec<Address> = Vec::new();
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
        if !owners.contains(&o) {
            owners.push(o);
        }
    }
    owners.iter().all(|o| candidates.contains(o))
}

/// Anchor profit: searcher net, or searcher+EOA when the EOA funded the
/// searcher (so capital inflow is not counted as profit).
fn profit_amount(ledger: &DeltaLedger, eoa: Address, searcher: Address, token: Address) -> U256 {
    let searcher_net = ledger.net_signed(searcher, token);
    if eoa.is_zero() || eoa == searcher {
        return if searcher_net.is_positive() {
            searcher_net.into_raw()
        } else {
            U256::ZERO
        };
    }
    let eoa_net = ledger.net_signed(eoa, token);
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

/// Reported profit for a searcher: anchor token first, else the first
/// priority token with a positive residual.
fn candidate_profit(
    ledger: &DeltaLedger,
    eoa: Address,
    searcher: Address,
    anchor: Address,
    priority: &[Address],
) -> (Address, U256) {
    let anchor_amount = profit_amount(ledger, eoa, searcher, anchor);
    if !anchor_amount.is_zero() {
        return (anchor, anchor_amount);
    }
    priority
        .iter()
        .filter(|t| **t != anchor)
        .find_map(|t| {
            let amt = profit_amount(ledger, eoa, searcher, *t);
            (!amt.is_zero()).then_some((*t, amt))
        })
        .unwrap_or((anchor, U256::ZERO))
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
mod split_tests {
    use super::*;
    use crate::explorer::canonical::explorer_canonical_id;
    use crate::explorer::classify::RouteProvenance;
    use crate::explorer::test_fixtures::{
        block, swap, swap_agg, swap_at_tick, swap_owned, transfer, tx, ATK, MARKET, POOL_A, POOL_B,
        TOKA, USDC, WNATIVE,
    };
    use crate::explorer::types::{FlashLoanFact, KeeperFact};
    use crate::types::opportunity::underlying_pools_hash;
    use alloy::primitives::address;

    fn tag_opts() -> ArbTagOpts<'static> {
        ArbTagOpts {
            wrapped_native: WNATIVE,
            savax: None,
            savax_exchange_rate_wad: None,
            epoch_reward_tokens: &[],
            epoch_venue_pools: &[],
            gmx_adl_signal: false,
            rate_cache_updates: &[],
        }
    }

    fn arb_tx(idx: u64) -> TxInput {
        tx(
            idx,
            ATK,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, USDC, 200, 110),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
                transfer(2, TOKA, ATK, POOL_B, 200),
                transfer(3, USDC, POOL_B, ATK, 110),
            ],
        )
    }

    /// WP5: the realized classifier persists the same canonical provenance the
    /// simulated side and `explorer_canonical_id` consume.
    #[test]
    fn realized_arb_carries_canonical_provenance_in_details() {
        let mut t = arb_tx(0);
        t.flashloans.push(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: ATK,
            token: Address::ZERO,
            amount: U256::ZERO,
            fee: None,
            recipient: Address::ZERO,
            provider: Address::ZERO,
        });
        // Router leg must not enter the underlying pool hash.
        t.swaps.push(swap_agg(MARKET, USDC, TOKA, 10, 10));
        t.route_provenance = Some(RouteProvenance {
            router_version: Some("paraswap_v5_v6".into()),
        });
        let input = block(vec![t]);
        let (events, _) = classify_atomic_arbs(&input, &tag_opts());
        assert_eq!(events.len(), 1, "arb fixture must still classify");
        let d = &events[0].details;
        assert_eq!(d["funding_source"].as_str(), Some("aave_v3"));
        assert_eq!(d["router_version"].as_str(), Some("paraswap_v5_v6"));
        let expected = format!("{:#x}", underlying_pools_hash(&[POOL_A, POOL_B]));
        assert_eq!(d["underlying_pools_hash"].as_str(), Some(expected.as_str()));
        // G-12: aggregator-mediated routes flag the pool set as a lower bound.
        assert_eq!(d["underlying_pools_unknown"].as_bool(), Some(true));
        let id = explorer_canonical_id(&events[0]);
        assert!(
            id.contains("|h:") && id.contains("|r:paraswap_v5_v6|f:aave_v3"),
            "canonical id must carry the new suffix: {id}"
        );
    }

    /// G-12: a DEX-only cycle (no router provenance) hashes its exact pool set
    /// and must not claim the underlying pools are unknown.
    #[test]
    fn dex_only_cycle_leaves_underlying_pools_known() {
        let t = arb_tx(0);
        let input = block(vec![t]);
        let (events, _) = classify_atomic_arbs(&input, &tag_opts());
        assert_eq!(events.len(), 1, "dex-only arb fixture must still classify");
        let d = &events[0].details;
        assert_eq!(d["underlying_pools_unknown"].as_bool(), None);
    }

    #[test]
    fn orchestrator_matches_the_passes_in_order() {
        let mut keeper_tx = tx(1, ATK, true, vec![], vec![]);
        keeper_tx.keepers = vec![KeeperFact {
            tx_index: 1,
            log_index: 0,
            protocol: "gelato",
            emitter: ATK,
            fee: Some(U256::from(10u64)),
            fee_token: Some(USDC),
        }];
        let input = block(vec![arb_tx(0), keeper_tx]);
        let opts = tag_opts();

        let (orchestrated, rejects) = classify_arbs(&input, &opts);

        let (mut manual, manual_rejects) = classify_atomic_arbs(&input, &opts);
        let keepers = classify_keepers(&input);
        let claims = classify_claim_and_sell(&input, &mut manual);
        manual.extend(keepers);
        manual.extend(claims);

        let signature = |v: &[MevEvent]| -> Vec<(MevKind, u64)> {
            v.iter().map(|e| (e.kind, e.tx_index)).collect()
        };
        assert_eq!(signature(&orchestrated), signature(&manual));
        assert_eq!(rejects.len(), manual_rejects.len());
        assert_eq!(
            signature(&orchestrated),
            vec![(MevKind::ArbAtomic, 0), (MevKind::Utility, 1)]
        );
    }

    #[test]
    fn claim_and_sell_tags_the_existing_arb_instead_of_double_counting() {
        // Zero-from TOKA claim is sold by the cycle's TOKA→USDC leg, so the
        // claim pass must tag the arb event rather than emit a second event.
        let mut t = arb_tx(0);
        t.transfers.push(transfer(9, TOKA, Address::ZERO, ATK, 7));
        let input = block(vec![t]);
        let opts = tag_opts();

        let (mut events, _) = classify_atomic_arbs(&input, &opts);
        assert_eq!(events.len(), 1);
        let claims = classify_claim_and_sell(&input, &mut events);

        assert!(claims.is_empty(), "arb tx must not produce a second event");
        let tagged = events[0].details["tags"].as_array().is_some_and(|tags| {
            tags.iter().any(|t| t.as_str() == Some("claim_and_sell"))
                && tags.iter().any(|t| t.as_str() == Some("airdrop"))
        });
        assert!(
            tagged,
            "arb event must carry claim_and_sell and airdrop tags"
        );
    }

    /// A2: two real DEX legs that chain without returning to the start token,
    /// plus an aggregator leg that would close the loop. With the aggregator
    /// leg excluded the chain is open, so `classify_atomic_arbs` emits no
    /// ArbAtomic event.
    #[test]
    fn aggregator_only_cycle_produces_no_arb() {
        let t = tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, TOKA, USDC, 100, 200),
                swap(POOL_B, USDC, WNATIVE, 200, 300),
                swap_agg(MARKET, WNATIVE, TOKA, 300, 400),
            ],
            vec![],
        );
        let input = block(vec![t]);
        let opts = tag_opts();

        let (events, _) = classify_atomic_arbs(&input, &opts);
        assert!(
            events.iter().all(|e| e.kind != MevKind::ArbAtomic),
            "aggregator-only cycle must not emit an arb event: {:?}",
            events
                .iter()
                .map(|e| (e.kind, e.tx_index))
                .collect::<Vec<_>>()
        );
    }

    const POOL_C: Address = address!("3000000000000000000000000000000000000005");
    const POOL_D: Address = address!("3000000000000000000000000000000000000006");
    const TOKB: Address = address!("5000000000000000000000000000000000000007");

    /// Two independent closed walks in one receipt must still emit one event,
    /// and that event's route/meta/hash must describe only the winning walk.
    #[test]
    fn two_cycles_emit_one_event_from_selected_route() {
        let t = tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, USDC, 200, 110),
                swap(POOL_C, WNATIVE, TOKB, 5, 10),
                swap(POOL_D, TOKB, WNATIVE, 10, 6),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
                transfer(2, TOKA, ATK, POOL_B, 200),
                transfer(3, USDC, POOL_B, ATK, 110),
                transfer(4, WNATIVE, ATK, POOL_C, 5),
                transfer(5, TOKB, POOL_C, ATK, 10),
                transfer(6, TOKB, ATK, POOL_D, 10),
                transfer(7, WNATIVE, POOL_D, ATK, 6),
            ],
        );
        let input = block(vec![t]);
        let (events, _) = classify_atomic_arbs(&input, &tag_opts());

        assert_eq!(events.len(), 1, "one event per tx even with two cycles");
        let ev = &events[0];
        assert_eq!(ev.details["candidate_route_count"], serde_json::json!(2));
        assert_eq!(ev.details["classification_confidence"], "inferred");
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.pools, vec![POOL_A, POOL_B]);
        let route = ev.details["route"].as_array().expect("route");
        assert_eq!(route.len(), 2);
        assert_eq!(route[0]["pool"], format!("{POOL_A:#x}"));
        assert_eq!(route[1]["pool"], format!("{POOL_B:#x}"));
        assert_eq!(ev.details["arb_meta"]["unique_tokens"], 2);
        // G-10: the losing walk is visible without becoming a second event.
        let rejected = ev.details["rejected_routes"]
            .as_array()
            .expect("rejected_routes");
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["hop_count"], 2);
        assert_eq!(rejected[0]["attributable"], true);
        let rejected_pools: Vec<String> = rejected[0]["pools"]
            .as_array()
            .expect("rejected pools")
            .iter()
            .map(|v| v.as_str().expect("pool string").to_string())
            .collect();
        assert_eq!(
            rejected_pools,
            vec![format!("{POOL_C:#x}"), format!("{POOL_D:#x}")]
        );
        // G-07: plain cyclic arb still reports a mechanism without moving P&L.
        assert_eq!(ev.details["profit_cause"]["mechanism"], "cross_pool_arb");
        assert_eq!(ev.details["profit_cause"]["basis"], "R");
        let expected = format!("{:#x}", underlying_pools_hash(&[POOL_A, POOL_B]));
        assert_eq!(
            ev.details["underlying_pools_hash"].as_str(),
            Some(expected.as_str())
        );
    }

    /// A profitable one-way swap in the same receipt is not part of the cycle.
    #[test]
    fn unrelated_swap_is_excluded_from_route_and_meta() {
        let t = tx(
            0,
            ATK,
            true,
            vec![
                swap_owned(ATK, POOL_A, USDC, TOKA, 100, 200),
                swap_owned(ATK, POOL_B, TOKA, USDC, 200, 110),
                swap(POOL_C, USDC, TOKB, 7, 9),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, ATK, 200),
                transfer(2, TOKA, ATK, POOL_B, 200),
                transfer(3, USDC, POOL_B, ATK, 110),
                transfer(4, USDC, ATK, POOL_C, 7),
                transfer(5, TOKB, POOL_C, ATK, 9),
            ],
        );
        let input = block(vec![t]);
        let (events, _) = classify_atomic_arbs(&input, &tag_opts());

        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.details["candidate_route_count"], serde_json::json!(1));
        assert_eq!(ev.details["unattributed_flow"], serde_json::json!(0));
        assert_eq!(ev.details["classification_confidence"], "exact");
        assert_eq!(ev.pools, vec![POOL_A, POOL_B]);
        assert_eq!(ev.details["arb_meta"]["hop_count"], 2);
        assert_eq!(ev.details["arb_meta"]["unique_tokens"], 2);
        let route = ev.details["route"].as_array().expect("route");
        assert_eq!(route.len(), 2);
    }

    /// Unknown funder identities are a route-attribution signal only; they
    /// must not change the realized P&L confidence axis.
    #[test]
    fn unknown_owner_lowers_classification_confidence_only() {
        let input = block(vec![arb_tx(0)]);
        let (events, _) = classify_atomic_arbs(&input, &tag_opts());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].details["unattributed_flow"], serde_json::json!(2));
        assert_eq!(events[0].details["classification_confidence"], "inferred");
        assert_eq!(events[0].confidence, Confidence::Exact);
    }

    /// G-02: `arb_shape` is the token-count label; `pool_shape` reports pool
    /// geometry separately so a shape claim never conflates hops with pools.
    #[test]
    fn route_shape_matrix_separates_tokens_from_pools() {
        const TOKC: Address = address!("6000000000000000000000000000000000000008");
        let case = |swaps: Vec<SwapFact>| {
            let meta = arb_route_meta(&swaps, false, &HashMap::new());
            (
                meta["hop_count"].as_u64().unwrap(),
                meta["pool_count"].as_u64().unwrap(),
                meta["unique_tokens"].as_u64().unwrap(),
                meta["arb_shape"].as_str().unwrap().to_string(),
                meta["pool_shape"].as_str().unwrap().to_string(),
            )
        };
        // (hop=3, pool=2, token=2)
        assert_eq!(
            case(vec![
                swap(POOL_A, TOKA, USDC, 100, 100),
                swap(POOL_B, USDC, TOKA, 100, 100),
                swap(POOL_A, TOKA, USDC, 100, 100),
            ]),
            (3, 2, 2, "two_pool".into(), "two_pool".into())
        );
        // (hop=3, token=3)
        assert_eq!(
            case(vec![
                swap(POOL_A, TOKA, USDC, 100, 100),
                swap(POOL_B, USDC, TOKB, 100, 100),
                swap(POOL_C, TOKB, TOKA, 100, 100),
            ]),
            (3, 3, 3, "triangular".into(), "three_pool".into())
        );
        // (hop=6, token=3): repeats the same three tokens/pools twice.
        assert_eq!(
            case(vec![
                swap(POOL_A, TOKA, USDC, 100, 100),
                swap(POOL_B, USDC, TOKB, 100, 100),
                swap(POOL_C, TOKB, TOKA, 100, 100),
                swap(POOL_A, TOKA, USDC, 100, 100),
                swap(POOL_B, USDC, TOKB, 100, 100),
                swap(POOL_C, TOKB, TOKA, 100, 100),
            ]),
            (6, 3, 3, "triangular".into(), "three_pool".into())
        );
        // (hop=4, token=4)
        assert_eq!(
            case(vec![
                swap(POOL_A, TOKA, USDC, 100, 100),
                swap(POOL_B, USDC, TOKB, 100, 100),
                swap(POOL_C, TOKB, TOKC, 100, 100),
                swap(POOL_D, TOKC, TOKA, 100, 100),
            ]),
            (4, 4, 4, "multi_hop".into(), "multi_pool".into())
        );
    }

    #[test]
    fn cross_dex_uses_protocol_identity_not_amm_family() {
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap(POOL_B, TOKA, USDC, 200, 110),
        ];
        let pool_dex = HashMap::from([
            (POOL_A, "sushiswap".to_string()),
            (POOL_B, "uniswap".to_string()),
        ]);
        let meta = arb_route_meta(&swaps, false, &pool_dex);
        assert_eq!(meta["is_cross_dex"], true);
        assert_eq!(meta["dex_count"], 2);
        assert_eq!(meta["amm_family_count"], 1);
        assert_eq!(meta["cross_dex_basis"], "protocol");
        assert_eq!(meta["venues"][0], "sushiswap");
        assert_eq!(meta["venues"][1], "uniswap");
    }

    #[test]
    fn same_dex_different_amm_is_not_cross_dex() {
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap_at_tick(POOL_B, TOKA, USDC, 200, 110, 0),
        ];
        let pool_dex = HashMap::from([
            (POOL_A, "uniswap".to_string()),
            (POOL_B, "uniswap".to_string()),
        ]);
        let meta = arb_route_meta(&swaps, false, &pool_dex);
        assert_eq!(meta["is_cross_dex"], false);
        assert_eq!(meta["dex_count"], 1);
        assert_eq!(meta["amm_family_count"], 2);
        assert_eq!(meta["cross_dex_basis"], "protocol");
    }

    #[test]
    fn cross_dex_metadata_declares_amm_family_fallback() {
        let swaps = vec![
            swap(POOL_A, USDC, TOKA, 100, 200),
            swap_at_tick(POOL_B, TOKA, USDC, 200, 110, 0),
        ];
        let meta = arb_route_meta(&swaps, false, &HashMap::new());
        assert_eq!(meta["is_cross_dex"], true);
        assert_eq!(meta["dex_count"], 2);
        assert_eq!(meta["cross_dex_basis"], "amm_family");
        assert!(meta["venues"].is_null());
    }

    #[test]
    fn keeper_pass_emits_utility() {
        let mut keeper_tx = tx(0, ATK, true, vec![], vec![]);
        keeper_tx.keepers = vec![KeeperFact {
            tx_index: 0,
            log_index: 0,
            protocol: "chainlink",
            emitter: ATK,
            fee: None,
            fee_token: None,
        }];
        let input = block(vec![keeper_tx]);

        let keepers = classify_keepers(&input);
        assert_eq!(keepers.len(), 1);
        assert_eq!(keepers[0].kind, MevKind::Utility);
        assert_eq!(keepers[0].details["protocol"], "chainlink");
    }
}

#[cfg(test)]
mod attribution_tests {
    use super::*;
    use crate::explorer::classify::classify_block;
    use crate::explorer::test_fixtures::{
        block, swap, swap_owned, transfer, tx_to, ATK, POOL_A, POOL_B, TOKA, USDC, WNATIVE,
    };
    use alloy::primitives::address;

    const EXEC: Address = address!("9000000000000000000000000000000000000009");
    const POOL_C: Address = address!("3000000000000000000000000000000000000003");
    const BENEFICIARY: Address = address!("b000000000000000000000000000000000000001");
    const MINOR_BENEFICIARY: Address = address!("b000000000000000000000000000000000000002");
    const BYSTANDER: Address = address!("d000000000000000000000000000000000000001");
    const OTHER_PAYEE: Address = address!("d000000000000000000000000000000000000002");
    const TIP_RECIPIENT: Address = address!("c000000000000000000000000000000000000001");
    const FUNDER: Address = address!("5000000000000000000000000000000000000005");

    fn arb_event(input: &BlockInput) -> (MevEvent, Vec<RejectedCandidate>) {
        let out = classify_block(input);
        let ev = out
            .events
            .iter()
            .find(|e| e.kind == MevKind::ArbAtomic)
            .cloned();
        (
            ev.unwrap_or_else(|| panic!("no arb event; rejects: {:?}", out.rejects)),
            out.rejects,
        )
    }

    /// Exact-input route: the last hop returns exactly the 100 USDC anchor
    /// input, so the anchor nets zero and the surplus is retained as 10
    /// WNATIVE by the executor. Profit must be reported in WNATIVE.
    #[test]
    fn profit_retained_in_priority_token_when_anchor_nets_zero() {
        let mut input = block(vec![tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, WNATIVE, 200, 50),
                swap(POOL_C, WNATIVE, USDC, 40, 100),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, EXEC, 200),
                transfer(2, TOKA, EXEC, POOL_B, 200),
                transfer(3, WNATIVE, POOL_B, EXEC, 50),
                transfer(4, WNATIVE, EXEC, POOL_C, 40),
                transfer(5, USDC, POOL_C, ATK, 100),
            ],
        )]);
        input.ctx.profit_policy.priority.push(WNATIVE);

        let (ev, _) = arb_event(&input);
        assert_eq!(ev.profit_token, Some(WNATIVE));
        assert_eq!(ev.profit_amount, Some(U256::from(10u64)));
        assert_eq!(ev.confidence, Confidence::Exact);
        assert_eq!(
            ev.details["pnl"]["profit_token"].as_str(),
            Some(format!("{WNATIVE:#x}").as_str())
        );
    }

    /// Executor nets zero in every priority token and forwards the 10 WNATIVE
    /// surplus (not the USDC anchor) to a single non-swap beneficiary. The
    /// forwarded residual must classify as Inferred with `forwarded_profit`.
    #[test]
    fn forwarded_profit_in_priority_token_is_inferred() {
        let mut input = block(vec![tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, WNATIVE, 200, 50),
                swap(POOL_C, WNATIVE, USDC, 40, 100),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, EXEC, 200),
                transfer(2, TOKA, EXEC, POOL_B, 200),
                transfer(3, WNATIVE, POOL_B, EXEC, 50),
                transfer(4, WNATIVE, EXEC, BENEFICIARY, 10),
                transfer(5, WNATIVE, EXEC, POOL_C, 40),
                transfer(6, USDC, POOL_C, ATK, 100),
            ],
        )]);
        input.ctx.profit_policy.priority.push(WNATIVE);

        let (ev, _) = arb_event(&input);
        assert_eq!(ev.profit_token, Some(WNATIVE));
        assert_eq!(ev.profit_amount, Some(U256::from(10u64)));
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(
            ev.details["forwarded_from"].as_str(),
            Some(format!("{EXEC:#x}").as_str())
        );
        assert!(ev.details["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|t| t.as_str() == Some("forwarded_profit"))));
    }

    /// A positive holder financed by an unrelated third party is a bystander,
    /// not the forwarded payee: it must not hide the single funder-fed payee
    /// (holdout regression: profit split between a payee and a fee address).
    #[test]
    fn third_party_holder_does_not_hide_forwarded_payee() {
        let mut input = block(vec![tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, WNATIVE, 200, 50),
                swap(POOL_C, WNATIVE, USDC, 40, 100),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, EXEC, 200),
                transfer(2, TOKA, EXEC, POOL_B, 200),
                transfer(3, WNATIVE, POOL_B, EXEC, 50),
                transfer(4, WNATIVE, EXEC, BENEFICIARY, 10),
                transfer(5, WNATIVE, EXEC, POOL_C, 40),
                transfer(6, USDC, POOL_C, ATK, 100),
                // Bystander: positive WNATIVE funded by a third party.
                transfer(7, WNATIVE, FUNDER, BYSTANDER, 7),
            ],
        )]);
        input.ctx.profit_policy.priority.push(WNATIVE);

        let (ev, _) = arb_event(&input);
        assert_eq!(ev.searcher, BENEFICIARY);
        assert_eq!(ev.profit_token, Some(WNATIVE));
        assert_eq!(ev.profit_amount, Some(U256::from(10u64)));
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert!(ev.details["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|t| t.as_str() == Some("forwarded_profit"))));
    }

    /// Two funder-fed payees keep the conservative rejection: no single
    /// recipient can be attributed.
    #[test]
    fn two_funder_fed_payees_still_reject() {
        let mut input = block(vec![tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, WNATIVE, 200, 50),
                swap(POOL_C, WNATIVE, USDC, 40, 100),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, EXEC, 200),
                transfer(2, TOKA, EXEC, POOL_B, 200),
                transfer(3, WNATIVE, POOL_B, EXEC, 50),
                transfer(4, WNATIVE, EXEC, BENEFICIARY, 5),
                transfer(5, WNATIVE, EXEC, OTHER_PAYEE, 5),
                transfer(6, WNATIVE, EXEC, POOL_C, 40),
                transfer(7, USDC, POOL_C, ATK, 100),
            ],
        )]);
        input.ctx.profit_policy.priority.push(WNATIVE);

        let out = classify_block(&input);
        assert!(
            !out.events.iter().any(|e| e.kind == MevKind::ArbAtomic),
            "two funder-fed payees must not be attributed to one"
        );
        assert!(out
            .rejects
            .iter()
            .any(|r| r.reject_reason == "net_zero_searcher"));
    }

    /// G-09: a normal transfer between two unrelated addresses must not be
    /// mistaken for a forwarded payee. The executor is net-zero and no holder
    /// is funder-fed, so the tx is rejected instead of attributed.
    #[test]
    fn unrelated_transfer_is_not_a_forwarded_payee() {
        let mut input = block(vec![tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, WNATIVE, 200, 50),
                swap(POOL_C, WNATIVE, USDC, 40, 100),
            ],
            vec![
                transfer(0, USDC, ATK, POOL_A, 100),
                transfer(1, TOKA, POOL_A, EXEC, 200),
                transfer(2, TOKA, EXEC, POOL_B, 200),
                transfer(3, WNATIVE, POOL_B, EXEC, 50),
                transfer(4, WNATIVE, EXEC, POOL_C, 50),
                transfer(5, USDC, POOL_C, ATK, 100),
                // Unrelated peer-to-peer transfer: neither side financed the
                // executor, so this holder must be dropped as a bystander.
                transfer(6, USDC, BYSTANDER, OTHER_PAYEE, 20),
            ],
        )]);
        input.ctx.profit_policy.priority.push(WNATIVE);

        let out = classify_block(&input);
        assert!(
            !out.events.iter().any(|e| e.kind == MevKind::ArbAtomic),
            "a bystander-funded EOA is not a forwarded payee"
        );
        assert!(
            out.rejects
                .iter()
                .any(|r| r.reject_reason == "net_zero_searcher"),
            "expected net_zero_searcher, got {:?}",
            out.rejects
                .iter()
                .map(|r| r.reject_reason.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// Multi-router route: the searcher contract funds the first leg and the
    /// executor contract funds the second; both are tx-level candidates, so
    /// the cycle is attributable and must classify.
    #[test]
    fn multi_router_owners_that_are_candidates_are_attributable() {
        let input = block(vec![tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap_owned(FUNDER, POOL_A, USDC, TOKA, 100, 200),
                swap_owned(EXEC, POOL_B, TOKA, USDC, 200, 110),
            ],
            vec![
                transfer(0, USDC, ATK, FUNDER, 100),
                transfer(1, USDC, FUNDER, POOL_A, 100),
                transfer(2, TOKA, POOL_A, EXEC, 200),
                transfer(3, TOKA, EXEC, POOL_B, 200),
                transfer(4, USDC, POOL_B, FUNDER, 110),
            ],
        )]);

        let (ev, rejects) = arb_event(&input);
        assert!(
            !rejects
                .iter()
                .any(|r| r.reject_reason == RejectReason::MixedFlowOwner.as_str()),
            "candidate owners must not veto: {rejects:?}"
        );
        assert_eq!(ev.searcher, FUNDER);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(10u64)));
    }

    /// A minor beneficiary (two transfers) is a candidate, but the executor
    /// forwards the bulk of the profit to a single non-candidate payee. The
    /// payee must win; attributing the minor beneficiary under-reports profit.
    #[test]
    fn forwarded_payee_wins_over_minor_candidate() {
        let input = block(vec![tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, USDC, 200, 110),
            ],
            vec![
                transfer(0, USDC, EXEC, POOL_A, 100),
                transfer(1, TOKA, POOL_A, EXEC, 200),
                transfer(2, TOKA, EXEC, POOL_B, 200),
                transfer(3, USDC, POOL_B, EXEC, 110),
                transfer(4, USDC, EXEC, TIP_RECIPIENT, 8),
                transfer(5, USDC, EXEC, MINOR_BENEFICIARY, 1),
                transfer(6, USDC, EXEC, MINOR_BENEFICIARY, 1),
            ],
        )]);

        let (ev, _) = arb_event(&input);
        assert_eq!(ev.searcher, TIP_RECIPIENT);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(8u64)));
        assert_eq!(ev.confidence, Confidence::Inferred);
        assert_eq!(
            ev.details["forwarded_from"].as_str(),
            Some(format!("{EXEC:#x}").as_str())
        );
    }

    /// Same shape, but the exact candidate holds more than the forwarded
    /// payee: keep the candidate and its Exact confidence.
    #[test]
    fn exact_candidate_kept_when_it_holds_more_than_forwarded_payee() {
        let input = block(vec![tx_to(
            0,
            ATK,
            EXEC,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 100, 200),
                swap(POOL_B, TOKA, USDC, 200, 110),
            ],
            vec![
                transfer(0, USDC, EXEC, POOL_A, 100),
                transfer(1, TOKA, POOL_A, EXEC, 200),
                transfer(2, TOKA, EXEC, POOL_B, 200),
                transfer(3, USDC, POOL_B, EXEC, 110),
                transfer(4, USDC, EXEC, TIP_RECIPIENT, 1),
                transfer(5, USDC, EXEC, MINOR_BENEFICIARY, 5),
                transfer(6, USDC, EXEC, MINOR_BENEFICIARY, 4),
            ],
        )]);

        let (ev, _) = arb_event(&input);
        assert_eq!(ev.searcher, MINOR_BENEFICIARY);
        assert_eq!(ev.profit_token, Some(USDC));
        assert_eq!(ev.profit_amount, Some(U256::from(9u64)));
        assert_eq!(ev.confidence, Confidence::Exact);
    }
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

    /// Two netted loans in the same token must record their sum, not just the
    /// larger fee. Both principals/fees are already inside the ledger residual.
    #[test]
    fn multiple_flash_fees_are_summed_when_complete() {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 1000, 1100),
                swap(POOL_B, TOKA, USDC, 1100, 1200),
            ],
            vec![
                transfer(0, USDC, PROVIDER, ATK, 500),
                transfer(1, USDC, PROVIDER, ATK, 500),
                transfer(2, USDC, ATK, POOL_A, 1000),
                transfer(3, TOKA, POOL_A, ATK, 1100),
                transfer(4, TOKA, ATK, POOL_B, 1100),
                transfer(5, USDC, POOL_B, ATK, 1200),
            ],
        );
        t.flashloans.push(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: ATK,
            token: USDC,
            amount: U256::from(500u64),
            fee: Some(U256::from(5u64)),
            recipient: ATK,
            provider: PROVIDER,
        });
        t.flashloans.push(FlashLoanFact {
            tx_index: 0,
            log_index: 1,
            protocol: "aave_v3",
            initiator: ATK,
            token: USDC,
            amount: U256::from(500u64),
            fee: Some(U256::from(7u64)),
            recipient: ATK,
            provider: PROVIDER,
        });
        let out = classify_block(&block(vec![t]));
        let arb = out
            .events
            .iter()
            .find(|e| e.kind == MevKind::ArbAtomic)
            .expect("arb present");
        assert_eq!(arb.flashloan_fee_wei, Some(U256::from(12u64)));
        assert_eq!(arb.flashloan_fee_token, Some(USDC));
        assert_eq!(
            arb.details
                .get("flashloan_fee_complete")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            arb.details
                .get("flashloan_fee_netted_in_ledger")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    /// G-03: two flash loans from different providers must persist both in
    /// `funding_providers`, emit one funding tag each, keep the first as the
    /// backward-compatible `funding_source`, and sum the fees only once.
    #[test]
    fn multiple_flash_providers_are_all_persisted_and_tagged() {
        let mut t = tx(
            0,
            ATK,
            true,
            vec![
                swap(POOL_A, USDC, TOKA, 1000, 1100),
                swap(POOL_B, TOKA, USDC, 1100, 1200),
            ],
            vec![
                transfer(0, USDC, PROVIDER, ATK, 500),
                transfer(1, USDC, PROVIDER, ATK, 500),
                transfer(2, USDC, ATK, POOL_A, 1000),
                transfer(3, TOKA, POOL_A, ATK, 1100),
                transfer(4, TOKA, ATK, POOL_B, 1100),
                transfer(5, USDC, POOL_B, ATK, 1200),
            ],
        );
        t.flashloans.push(FlashLoanFact {
            tx_index: 0,
            log_index: 0,
            protocol: "aave_v3",
            initiator: ATK,
            token: USDC,
            amount: U256::from(500u64),
            fee: Some(U256::from(5u64)),
            recipient: ATK,
            provider: PROVIDER,
        });
        t.flashloans.push(FlashLoanFact {
            tx_index: 0,
            log_index: 1,
            protocol: "balancer_v2",
            initiator: ATK,
            token: USDC,
            amount: U256::from(500u64),
            fee: Some(U256::from(7u64)),
            recipient: ATK,
            provider: PROVIDER,
        });
        let out = classify_block(&block(vec![t]));
        let arb = out
            .events
            .iter()
            .find(|e| e.kind == MevKind::ArbAtomic)
            .expect("arb present");
        let providers: Vec<&str> = arb.details["funding_providers"]
            .as_array()
            .expect("funding_providers must be an array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(providers, vec!["aave_v3", "balancer_v2_vault"]);
        // Backward-compatible scalar stays the first provider.
        assert_eq!(arb.details["funding_source"].as_str(), Some("aave_v3"));
        let tags: Vec<&str> = arb.details["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        assert!(tags.contains(&"funding:aave_v3"));
        assert!(tags.contains(&"funding:balancer_v2_vault"));
        // Fees are summed once and already netted by the ledger: 1200 - 1000
        // principal - 12 fees = 188. A second subtraction would yield 176.
        assert_eq!(arb.flashloan_fee_wei, Some(U256::from(12u64)));
        assert_eq!(arb.profit_amount, Some(U256::from(188u64)));
        assert_eq!(
            arb.details
                .get("flashloan_fee_netted_in_ledger")
                .and_then(|v| v.as_bool()),
            Some(true)
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
