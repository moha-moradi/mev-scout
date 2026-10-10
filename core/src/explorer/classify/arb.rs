//! Atomic-arb attribution, Unknown upgrades, and adjacent Exact fingerprints
//! (keeper / claim-and-sell / ERC-4337 bundler).
use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::explorer::decode;
use crate::explorer::profit::{has_closed_cycle, net_flash_loan, select_profit_token, DeltaLedger};
use crate::explorer::strategy_tags::{self, ArbTagOpts};
use crate::explorer::types::{Confidence, MevEvent, MevKind, PnlBasis, SwapFact, TransferFact};

use super::{append_tag, has_unresolved_tokens, BlockInput, TxInput};

/// Swap attribution + atomic arb + keeper/claim/bundler + transfer-cycle upgrade.
pub(super) fn classify_arbs(
    input: &BlockInput,
    liq_txs: &HashSet<u64>,
    arb_tag_opts: &ArbTagOpts<'_>,
) -> Vec<MevEvent> {
    let mut events: Vec<MevEvent> = Vec::new();
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
                        tags.extend(strategy_tags::arb_strategy_tags(&tx.swaps, arb_tag_opts));
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
    events
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

/// Phase 1.2 flow ownership (§7.1/§8.1): a closed cycle must be attributable
/// to one searcher's route. Among direction-resolved swap legs, the distinct
/// funder-owners must collapse to a single actor, and that actor must be one
/// of the tx's searcher candidates. Fully-unattributed legs (no inbound
/// transfer observable) cannot disprove ownership and pass (preserves recall
/// for flash-mint / internal-balance flows); an unrelated actor's legs always
/// fail.
pub(crate) fn flow_attributable(swaps: &[SwapFact], candidates: &[Address]) -> bool {
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
