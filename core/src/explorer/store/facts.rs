//! Block fact persistence for ExplorerStore.
use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use super::pnl::{annotate_event, PersistedOp};
use super::{BlockFactsInput, ExplorerStore, PendingLabels};

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
    ///
    /// The transaction covers every fact insert plus the `blocks_classified`
    /// checkpoint, so a failure cannot leave a half-written block that the
    /// checkpoint then reports as done. Annotated rows are built *before* the
    /// transaction opens, so the write lock is held for the inserts only.
    ///
    /// Competitor labels run after the commit and are best-effort: a failure is
    /// logged and the block still returns `Ok`. The facts are already durable,
    /// so propagating a label error would tell the caller the block is
    /// unindexed while `blocks_classified` says otherwise; a missed label only
    /// costs entity attribution, which the idempotent
    /// [`Self::backfill_competitor_labels`] can repair.
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
        // All business logic (PnL annotation, sandwich gate, volume,
        // flash-fee conversion, native residual, confidence) lives in
        // `pnl::annotate_event`; the transaction below only binds and
        // inserts the already-annotated rows.
        let ops: Vec<PersistedOp> = events
            .iter()
            .map(|ev| annotate_event(ev, swaps, token_prices, native_price_usd, now))
            .collect();
        let pending_labels: PendingLabels = events
            .iter()
            .map(|ev| (ev.searcher, ev.contract, ev.block))
            .collect();
        let inserted = ops.len();

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
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

        for t in txs {
            tx.execute(
                "INSERT OR IGNORE INTO txs
                   (hash, block_number, tx_index, \"from\", \"to\", success,
                    gas_used, effective_gas_price_gwei, priority_fee_gwei, value_native)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    format!("{:#x}", t.hash),
                    block_number as i64,
                    t.tx_index as i64,
                    format!("{:#x}", t.from),
                    t.to.map(|a| format!("{:#x}", a)),
                    t.success as i64,
                    t.gas_used as i64,
                    t.effective_gas_price_gwei,
                    t.priority_fee_gwei,
                    t.value.to_string(),
                ],
            )?;
        }

        for s in swaps {
            tx.execute(
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
            tx.execute(
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

        for op in &ops {
            tx.execute(
                "INSERT INTO mev_ops
                   (block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount,
                    volume_usd, profit_usd, gas_cost_usd, flashloan_fee_usd,
                    net_profit_usd, route_json, victim_hashes, details_json,
                    detector, created_at, excluded_from_net)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                        ?14, ?15, ?16, ?17, ?18, ?19, 'explorer', ?20, ?21)",
                rusqlite::params![
                    op.block_number as i64,
                    op.tx_index as i64,
                    op.tx_hash,
                    op.ts as i64,
                    op.kind,
                    op.eoa,
                    op.contract,
                    op.confidence,
                    op.canonical_id,
                    op.profit_token,
                    op.profit_amount,
                    op.volume_usd,
                    op.profit_usd,
                    op.gas_cost_usd,
                    op.flashloan_fee_usd,
                    op.net_profit_usd,
                    op.route_json,
                    op.victim_hashes,
                    op.details_json,
                    op.created_at,
                    op.excluded_from_net,
                ],
            )?;
        }

        tx.execute(
            "INSERT OR REPLACE INTO blocks_classified(block, classified_at, event_count)
             VALUES(?1, ?2, ?3)",
            rusqlite::params![block_number as i64, now, inserted as i64],
        )?;
        tx.commit()?;

        // Best-effort (see the doc comment): the facts above are committed.
        for (eoa, contract, block) in pending_labels {
            if let Err(e) = self.label_competitor(eoa, contract, block) {
                tracing::warn!(
                    block,
                    eoa = %format!("{eoa:#x}"),
                    "competitor label upsert failed after block facts committed: {e}"
                );
            }
        }
        Ok(inserted)
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
