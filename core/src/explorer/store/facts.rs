//! Block fact persistence for ExplorerStore.
use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::explorer::types::MevKind;

use super::pnl::{liquidation_pnl, note_pricing_issue};
use super::{merge_details_json, BlockFactsInput, ExplorerStore, PersistBlockStats};

impl ExplorerStore {
    /// Persist Chainlink answers for mode-B pre-poke divergence (plan P1.4).
    pub fn record_oracle_answers(
        &self,
        block: u64,
        rows: &[(Address, i128)],
    ) -> anyhow::Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO oracle_answers(block_number, feed, answer) VALUES (?1, ?2, ?3)",
        )?;
        for (feed, answer) in rows {
            stmt.execute(rusqlite::params![
                block as i64,
                format!("{feed:#x}"),
                answer.to_string(),
            ])?;
        }
        Ok(())
    }
    /// Latest answer per feed strictly before `block`.
    pub fn prior_oracle_answers(
        &self,
        block: u64,
        feeds: &[Address],
    ) -> anyhow::Result<HashMap<Address, i128>> {
        let mut out = HashMap::new();
        if feeds.is_empty() {
            return Ok(out);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT answer FROM oracle_answers
             WHERE feed = ?1 AND block_number < ?2
             ORDER BY block_number DESC LIMIT 1",
        )?;
        for feed in feeds {
            let feed_s = format!("{feed:#x}");
            if let Ok(ans) = stmt.query_row(rusqlite::params![feed_s, block as i64], |r| {
                r.get::<_, String>(0)
            }) {
                if let Ok(v) = ans.parse::<i128>() {
                    out.insert(*feed, v);
                }
            }
        }
        Ok(out)
    }
    /// Persist Aave ReserveDataUpdated rates for mode-B interest (plan P1.1).
    pub fn record_reserve_rates(&self, block: u64, rows: &[(Address, U256)]) -> anyhow::Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO reserve_rates(block_number, reserve, variable_borrow_rate)
             VALUES (?1, ?2, ?3)",
        )?;
        for (reserve, rate) in rows {
            stmt.execute(rusqlite::params![
                block as i64,
                format!("{reserve:#x}"),
                rate.to_string(),
            ])?;
        }
        Ok(())
    }
    /// Reserve + oracle rows in `[block - window, block)` for interest lookback.
    pub fn interest_lookback(
        &self,
        block: u64,
        window: u64,
    ) -> anyhow::Result<crate::explorer::interest_attr::InterestLookback> {
        let from = block.saturating_sub(window) as i64;
        let to = block as i64;
        let mut oracle_updates = Vec::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT block_number, feed FROM oracle_answers
                 WHERE block_number >= ?1 AND block_number < ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![from, to], |r| {
                Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (b, feed_s) = row?;
                if let Ok(feed) = feed_s.parse::<Address>() {
                    oracle_updates.push((b, feed));
                }
            }
        }
        let mut reserve_updates = Vec::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT block_number, reserve, variable_borrow_rate FROM reserve_rates
                 WHERE block_number >= ?1 AND block_number < ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![from, to], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (b, reserve_s, rate_s) = row?;
                if let (Ok(reserve), Ok(rate)) =
                    (reserve_s.parse::<Address>(), rate_s.parse::<U256>())
                {
                    reserve_updates.push((b, reserve, rate));
                }
            }
        }
        Ok(crate::explorer::interest_attr::InterestLookback {
            oracle_updates,
            reserve_updates,
        })
    }
    /// Insert block header + txs + swaps + transfers + mev events in one
    /// transaction. Idempotent via OR IGNORE on natural keys.
    pub fn insert_block_facts(&self, facts: BlockFactsInput<'_>) -> anyhow::Result<usize> {
        let BlockFactsInput {
            block_number,
            block_hash,
            ts,
            base_fee_gwei,
            tx_count,
            txs,
            swaps,
            transfers,
            events,
            native_price_usd,
            token_prices,
        } = facts;
        let now = crate::utils::epoch_secs() as i64;
        let conn = &self.conn;
        conn.execute("BEGIN IMMEDIATE", [])?;
        let result = (|| -> anyhow::Result<PersistBlockStats> {
            conn.execute(
                "INSERT OR IGNORE INTO blocks
                   (block_number, block_hash, ts, producer, base_fee_gwei, tx_count, indexed_at)
                 VALUES(?1, ?2, ?3, NULL, ?4, ?5, ?6)",
                rusqlite::params![
                    block_number as i64,
                    format!("{:#x}", block_hash),
                    ts as i64,
                    base_fee_gwei,
                    tx_count as i64,
                    now
                ],
            )?;

            for tx in txs {
                conn.execute(
                    "INSERT OR IGNORE INTO txs
                       (hash, block_number, tx_index, \"from\", \"to\", success,
                        gas_used, effective_gas_price_gwei, priority_fee_gwei, value_native)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        format!("{:#x}", tx.hash),
                        block_number as i64,
                        tx.tx_index as i64,
                        format!("{:#x}", tx.from),
                        tx.to.map(|a| format!("{:#x}", a)),
                        tx.success as i64,
                        tx.gas_used as i64,
                        tx.effective_gas_price_gwei,
                        tx.priority_fee_gwei,
                        tx.value.to_string(),
                    ],
                )?;
            }

            for s in swaps {
                conn.execute(
                    "INSERT OR IGNORE INTO swaps
                       (block_number, tx_index, log_index, pool, dex, amm,
                        token_in, token_out, amount_in, amount_out, sender)
                     VALUES(?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, NULL)",
                    rusqlite::params![
                        block_number as i64,
                        s.tx_index as i64,
                        s.log_index as i64,
                        format!("{:#x}", s.pool),
                        s.amm.as_str(),
                        format!("{:#x}", s.token_in),
                        format!("{:#x}", s.token_out),
                        s.amount_in.to_string(),
                        s.amount_out.to_string(),
                    ],
                )?;
            }

            for t in transfers {
                conn.execute(
                    "INSERT OR IGNORE INTO transfers
                       (block_number, tx_index, log_index, token, \"from\", \"to\",
                        amount, is_native)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                    rusqlite::params![
                        block_number as i64,
                        t.tx_index as i64,
                        t.log_index as i64,
                        format!("{:#x}", t.token),
                        format!("{:#x}", t.from),
                        format!("{:#x}", t.to),
                        t.amount.to_string(),
                    ],
                )?;
            }

            let mut inserted = 0usize;
            let mut pending_labels: Vec<(Address, Option<Address>, u64)> = Vec::new();
            for ev in events {
                // Liquidation P&L (Phase 1.3): profit ≈ collateral_usd −
                // debt_usd, valued at persist with hourly prices. Never
                // subtract raw amounts when tokens differ.
                let mut confidence = ev.confidence.as_str();
                let mut details_json = ev.details.to_string();
                let mut pricing_clamped = false;
                let mut unpriced_residual = false;
                let mut native_unpriced = false;
                let profit_usd = if ev.kind == MevKind::Liquidation {
                    let (usd, conf, details) = liquidation_pnl(ev, token_prices);
                    confidence = conf;
                    details_json = details;
                    usd
                } else if !ev.profit_tokens.is_empty() {
                    // Phase 2.3: USD-sum across every positive residual
                    // (flash-netting already cleaned the ledger in 2.2) with
                    // Phase 2.4 realized-rate fallback for unpriced tokens.
                    // A missing quote is not zero: the sum is partial and marked
                    // approximate instead of being stored as a complete figure.
                    let mut total = 0.0f64;
                    for (tok, amt) in &ev.profit_tokens {
                        if *tok == crate::explorer::profit::NATIVE_MARKER {
                            native_unpriced = true;
                            continue;
                        }
                        match crate::explorer::pricing::amount_usd_realized(
                            *tok,
                            *amt,
                            token_prices,
                            &ev.details,
                        ) {
                            Some(q) if q.usd.is_finite() => {
                                total += q.usd;
                                pricing_clamped |= q.clamped;
                            }
                            _ => unpriced_residual = true,
                        }
                    }
                    if !total.is_finite() {
                        total = 0.0;
                        unpriced_residual = true;
                    }
                    details_json = merge_details_json(
                        &details_json,
                        [(
                            "profit_tokens",
                            serde_json::json!(ev
                                .profit_tokens
                                .iter()
                                .map(|(tok, amt)| serde_json::json!({
                                    "token": format!("{tok:#x}"),
                                    "amount": amt.to_string(),
                                }))
                                .collect::<Vec<_>>()),
                        )],
                    );
                    (total > 0.0).then_some(total)
                } else {
                    match (ev.profit_token, ev.profit_amount) {
                        (Some(tok), _) if tok == crate::explorer::profit::NATIVE_MARKER => {
                            native_unpriced = true;
                            None
                        }
                        (Some(tok), Some(amt)) => {
                            match crate::explorer::pricing::amount_usd_realized(
                                tok,
                                amt,
                                token_prices,
                                &ev.details,
                            ) {
                                Some(q) if q.usd.is_finite() => {
                                    pricing_clamped |= q.clamped;
                                    Some(q.usd)
                                }
                                _ => {
                                    unpriced_residual = true;
                                    None
                                }
                            }
                        }
                        _ => None,
                    }
                };
                if unpriced_residual {
                    note_pricing_issue(&mut details_json, "MULTI_ASSET_PRICING");
                }
                if native_unpriced {
                    note_pricing_issue(&mut details_json, "NATIVE_UNPRICED");
                }
                if pricing_clamped {
                    note_pricing_issue(&mut details_json, "pricing_clamped");
                }
                // Phase 2.4: FOT / rebase profit tokens are flagged approximate
                // (recorded amounts are distorted by the token mechanics).
                if let Some(reason) = ev
                    .profit_tokens
                    .iter()
                    .map(|(tok, _)| *tok)
                    .chain(ev.profit_token)
                    .find_map(|t| crate::explorer::pricing::approximate_token(&t))
                {
                    details_json = merge_details_json(
                        &details_json,
                        [
                            ("usd_approximate", serde_json::json!(true)),
                            ("approximate_reason", serde_json::json!(reason)),
                        ],
                    );
                }
                // Gas in USD via native price (gas wei → native → USD).
                let gas_usd = native_price_usd
                    .map(|np| crate::explorer::pricing::wei_to_usd(ev.gas_cost_wei, np));
                // Flash-loan fee in USD via the borrowed token's price (2.2).
                let flashloan_fee_usd = match (ev.flashloan_fee_token, ev.flashloan_fee_wei) {
                    (Some(tok), Some(amt)) => token_prices
                        .get(&tok)
                        .map(|p| crate::explorer::pricing::token_amount_to_usd(amt, p)),
                    _ => None,
                };
                let net = match (profit_usd, gas_usd) {
                    (Some(g), Some(gg)) => Some(g - gg - flashloan_fee_usd.unwrap_or(0.0)),
                    _ => None,
                };
                // Sandwich profitability gate (Phase 1.4): a sandwich whose
                // extractable profit does not cover combined front+back gas
                // (plus any flash-loan fee) is not a realized-MEV op. Drop it
                // rather than persist a provably lossy record. Unowned-net
                // (missing price) events are kept — the gate never guesses.
                if ev.kind == MevKind::Sandwich {
                    if let Some(n) = net {
                        if n <= 0.0 {
                            continue;
                        }
                    }
                }
                // USD notional of the op's swap legs (report "volume"): priced
                // at the block's token prices. Legs with no price/decimals are
                // skipped; volume is `None` when nothing priced. Attribution is
                // per anchor tx — a tx with multiple events shares its legs.
                let volume_usd = {
                    let total = swaps.iter().filter(|s| s.tx_index == ev.tx_index).fold(
                        0.0f64,
                        |acc, s| {
                            acc + token_prices
                                .get(&s.token_in)
                                .map(|p| {
                                    crate::explorer::pricing::token_amount_to_usd(s.amount_in, p)
                                })
                                .unwrap_or(0.0)
                        },
                    );
                    (total > 0.0).then_some(total)
                };
                let canonical = crate::explorer::explorer_canonical_id(ev);
                conn.execute(
                    "INSERT INTO mev_ops
                       (block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                        confidence, canonical_id, profit_token, profit_amount,
                        volume_usd, profit_usd, gas_cost_usd, flashloan_fee_usd,
                        net_profit_usd, route_json, victim_hashes, details_json,
                        detector, created_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                            ?14, ?15, ?16, ?17, ?18, ?19, 'explorer', ?20)",
                    rusqlite::params![
                        ev.block as i64,
                        ev.tx_index as i64,
                        format!("{:#x}", ev.tx_hash),
                        ev.ts as i64,
                        ev.kind.as_str(),
                        format!("{:#x}", ev.searcher),
                        ev.contract.map(|a| format!("{:#x}", a)),
                        confidence,
                        canonical,
                        ev.profit_token.map(|a| format!("{:#x}", a)),
                        ev.profit_amount.map(|a| a.to_string()),
                        volume_usd,
                        profit_usd,
                        gas_usd,
                        flashloan_fee_usd,
                        net,
                        ev.details.get("route").map(|r| r.to_string()),
                        serde_json::to_string(
                            &ev.victim_hashes
                                .iter()
                                .map(|h| format!("{:#x}", h))
                                .collect::<Vec<_>>()
                        )
                        .ok(),
                        Some(details_json),
                        now,
                    ],
                )?;
                pending_labels.push((ev.searcher, ev.contract, ev.block));
                inserted += 1;
            }

            conn.execute(
                "INSERT OR REPLACE INTO blocks_classified(block, classified_at, event_count)
                 VALUES(?1, ?2, ?3)",
                rusqlite::params![block_number as i64, now, inserted as i64],
            )?;
            Ok((inserted, pending_labels))
        })();

        match result {
            Ok((n, pending_labels)) => {
                conn.execute("COMMIT", [])?;
                for (eoa, contract, block) in pending_labels {
                    self.label_competitor(eoa, contract, block)?;
                }
                Ok(n)
            }
            Err(e) => {
                conn.execute("ROLLBACK", [])?;
                Err(e)
            }
        }
    }
    /// Purge `transfers` older than `keep_blocks` behind head (retention).
    pub fn prune_transfers(&self, before_block: u64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM transfers WHERE block_number < ?1",
            [before_block as i64],
        )?;
        Ok(n)
    }
}
