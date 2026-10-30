//! Labels / rejections for ExplorerStore.
use alloy::primitives::Address;

use super::ExplorerStore;

impl ExplorerStore {
    /// Insert an address label (classifier growth loop).
    ///
    /// `entity` is the competitor bot id (`COALESCE(contract, eoa)` hex).
    pub fn upsert_label(
        &self,
        address: Address,
        name: &str,
        source: &str,
        first_seen_block: u64,
        entity: Option<&str>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO labels(address, kind, name, entity, evidence, first_seen_block, source)
             VALUES(?1, 'searcher', ?2, ?3, NULL, ?4, ?5)
             ON CONFLICT(address) DO NOTHING",
            rusqlite::params![
                format!("{:#x}", address),
                name,
                entity,
                first_seen_block as i64,
                source
            ],
        )?;
        Ok(())
    }
    /// Label a realized-MEV searcher (and optional executor contract) under one
    /// competitor `entity` = `COALESCE(contract, eoa)`.
    pub fn label_competitor(
        &self,
        eoa: Address,
        contract: Option<Address>,
        block: u64,
    ) -> anyhow::Result<()> {
        let bot_id = format!("{:#x}", contract.unwrap_or(eoa));
        self.upsert_label(
            eoa,
            "unclassified-searcher",
            "classifier",
            block,
            Some(&bot_id),
        )?;
        if let Some(c) = contract {
            if c != eoa {
                self.upsert_label(c, "searcher-contract", "classifier", block, Some(&bot_id))?;
            }
        }
        Ok(())
    }
    /// Idempotent: ensure every distinct `mev_ops` (eoa, contract) has labels.
    /// Returns how many new `labels` rows were inserted.
    pub fn backfill_competitor_labels(&self) -> anyhow::Result<usize> {
        let before: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))?;
        let mut stmt = self.conn.prepare(
            "SELECT eoa, contract, MIN(block_number)
             FROM mev_ops
             GROUP BY eoa, contract",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)? as u64,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (eoa_s, contract_s, block) in rows {
            let Ok(eoa) = eoa_s.parse::<Address>() else {
                continue;
            };
            let contract = contract_s.and_then(|s| s.parse::<Address>().ok());
            self.label_competitor(eoa, contract, block)?;
        }
        let after: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))?;
        Ok((after - before).max(0) as usize)
    }
    /// Insert one rejected-candidate row (run_id/chain stamped here).
    ///
    /// Conflict-tolerant on the `rejected_candidates_natural` unique index
    /// (see `schema.rs`): re-indexing a block re-derives the same rejects with
    /// only `created_at` differing, and those must not be counted twice by the
    /// reject denominators in the revenue report.
    pub fn insert_rejected_candidate(
        &self,
        run_id: &str,
        chain: &str,
        r: &crate::explorer::RejectedCandidate,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO rejected_candidates
               (run_id, chain, block_number, tx_index, strategy, pool_a, pool_b, path,
                token_in, token_out, input_amount, expected_profit, expected_profit_usd,
                gas_cost_wei, reject_reason, detail, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                run_id,
                chain,
                r.block_number as i64,
                r.tx_index.map(|v| v as i64),
                r.strategy,
                r.pool_a,
                r.pool_b,
                r.path,
                r.token_in,
                r.token_out,
                r.input_amount,
                r.expected_profit,
                r.expected_profit_usd,
                r.gas_cost_wei,
                r.reject_reason,
                r.detail,
                r.created_at as i64,
            ],
        )?;
        Ok(())
    }
    /// Label a searcher automatically after a confirmed classifier hit
    /// (growth loop): address → label with `source = classifier`.
    pub fn label_searcher_auto(&self, address: Address, block: u64) -> anyhow::Result<()> {
        self.label_competitor(address, None, block)
    }
    /// `(name, entity)` for an address label, if present.
    pub fn get_label(&self, address: Address) -> anyhow::Result<Option<(String, Option<String>)>> {
        let addr = format!("{address:#x}");
        let mut stmt = self
            .conn
            .prepare("SELECT name, entity FROM labels WHERE address = ?1")?;
        let mut rows = stmt.query(rusqlite::params![addr])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }
    /// Test helper: wipe the labels table (simulate a pre-competitor DB).
    #[cfg(test)]
    pub(super) fn clear_labels(&self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM labels", [])?;
        Ok(())
    }
}
