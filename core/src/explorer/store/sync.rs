//! Sync / reorg checkpoint helpers for ExplorerStore.
use super::ExplorerStore;

impl ExplorerStore {
    /// Read `indexed_to` for a chain (0 when absent).
    pub fn get_indexed_to(&self, chain_id: u64) -> anyhow::Result<u64> {
        let v: Option<u64> = self
            .conn
            .query_row(
                "SELECT indexed_to FROM sync_state WHERE chain_id = ?1",
                [chain_id as i64],
                |r| r.get(0),
            )
            .map(Some)
            .unwrap_or(None);
        Ok(v.unwrap_or(0))
    }
    /// Upsert sync checkpoint after indexing through `indexed_to`.
    pub fn set_sync_state(&self, chain_id: u64, head: u64, indexed_to: u64) -> anyhow::Result<()> {
        let now = crate::utils::epoch_secs() as i64;
        self.conn.execute(
            "INSERT INTO sync_state(chain_id, head, indexed_to, last_indexed_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(chain_id) DO UPDATE SET
               head = excluded.head,
               indexed_to = excluded.indexed_to,
               last_indexed_at = excluded.last_indexed_at",
            rusqlite::params![chain_id as i64, head as i64, indexed_to as i64, now],
        )?;
        Ok(())
    }
    /// True when the block was already classified (idempotent skip).
    pub fn block_classified(&self, block: u64) -> anyhow::Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM blocks_classified WHERE block = ?1",
            [block as i64],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }
    /// Record a classified-block checkpoint.
    pub fn mark_block_classified(&self, block: u64, event_count: usize) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO blocks_classified(block, classified_at, event_count)
             VALUES(?1, ?2, ?3)",
            rusqlite::params![block, crate::utils::epoch_secs() as i64, event_count as i64],
        )?;
        Ok(())
    }
    /// Blocks in `[from, to]` not yet classified (gap resume).
    pub fn unclassified_blocks(&self, from: u64, to: u64) -> anyhow::Result<Vec<u64>> {
        const WINDOW: u64 = 4096;
        let mut out = Vec::new();
        if from > to {
            return Ok(out);
        }
        let mut lo = from;
        while lo <= to {
            let hi = lo.saturating_add(WINDOW - 1).min(to);
            let mut stmt = self.conn.prepare(
                "WITH RECURSIVE seq(b) AS (
                   SELECT ?1 UNION ALL SELECT b+1 FROM seq WHERE b < ?2
                 )
                 SELECT b FROM seq
                 WHERE b NOT IN (SELECT block FROM blocks_classified
                                 WHERE block BETWEEN ?1 AND ?2)",
            )?;
            let rows = stmt.query_map(rusqlite::params![lo as i64, hi as i64], |r| {
                r.get::<_, i64>(0)
            })?;
            for row in rows {
                out.push(row? as u64);
            }
            if hi == u64::MAX {
                break;
            }
            lo = hi + 1;
        }
        Ok(out)
    }
    /// Remove classified blocks >= fork block (reorg unwind).
    ///
    /// All deletes run in one transaction: a crash mid-unwind must never leave
    /// `mev_ops`/`transfers`/`txs` inconsistent with `blocks_classified`.
    /// `opportunities` and `paper_sessions`/`paper_fills` are deliberately NOT
    /// unwound — they are detector-run records (analysis artifacts), not
    /// reorg-derived chain state, so they survive the fork.
    pub fn unwind_from(&self, fork_block: u64) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM blocks_classified WHERE block >= ?1",
            [fork_block as i64],
        )?;
        tx.execute(
            "DELETE FROM mev_ops WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        tx.execute(
            "DELETE FROM transfers WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        tx.execute(
            "DELETE FROM swaps WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        tx.execute(
            "DELETE FROM txs WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        tx.execute(
            "DELETE FROM blocks WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        tx.execute(
            "DELETE FROM jit_open_positions WHERE opened_block >= ?1",
            [fork_block as i64],
        )?;
        tx.execute(
            "DELETE FROM oracle_answers WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        tx.execute(
            "DELETE FROM reserve_rates WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        // Reject rows from the abandoned chain would keep feeding the reject
        // counts the revenue report prints (and D3 exports), so the
        // "why is this kind empty" denominators would be fiction.
        tx.execute(
            "DELETE FROM rejected_candidates WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Stored block hash for reorg detection.
    pub fn block_hash(&self, block: u64) -> anyhow::Result<Option<String>> {
        let v = self
            .conn
            .query_row(
                "SELECT block_hash FROM blocks WHERE block_number = ?1",
                [block as i64],
                |r| r.get::<_, String>(0),
            )
            .map(Some)
            .unwrap_or(None);
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reject_row(block: u64) -> crate::explorer::RejectedCandidate {
        crate::explorer::RejectedCandidate {
            block_number: block,
            tx_index: Some(3),
            strategy: "two_hop_arb".into(),
            pool_a: None,
            pool_b: None,
            path: None,
            token_in: None,
            token_out: None,
            input_amount: None,
            expected_profit: None,
            expected_profit_usd: None,
            gas_cost_wei: None,
            reject_reason: "DUST".into(),
            detail: None,
            created_at: 0,
        }
    }

    fn rejected_count(store: &ExplorerStore, block: u64) -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM rejected_candidates WHERE block_number = ?1",
                [block as i64],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn unwind_from_removes_all_traces_of_forked_blocks() {
        let store = ExplorerStore::open_in_memory().unwrap();
        store.mark_block_classified(100, 0).unwrap();
        store
            .insert_rejected_candidate("run_test", "polygon", &reject_row(100))
            .unwrap();
        assert!(store.block_classified(100).unwrap());
        assert_eq!(rejected_count(&store, 100), 1);

        store.unwind_from(100).unwrap();
        assert!(!store.block_classified(100).unwrap());
        // Rejected candidates from the abandoned chain must not survive the
        // unwind — they would keep feeding reject-count denominators.
        assert_eq!(rejected_count(&store, 100), 0);

        // Idempotent: a second unwind of the same fork point is a no-op.
        store.unwind_from(100).unwrap();
        assert!(!store.block_classified(100).unwrap());
        assert_eq!(rejected_count(&store, 100), 0);
    }

    #[test]
    fn unclassified_blocks_reports_gaps_across_windows() {
        let store = ExplorerStore::open_in_memory().unwrap();
        store.mark_block_classified(4100, 0).unwrap();
        let gaps = store.unclassified_blocks(0, 5000).unwrap();
        assert!(!gaps.contains(&4100));
        assert_eq!(gaps.len(), 5000);
        assert_eq!(gaps.first().copied(), Some(0));
        assert_eq!(gaps.last().copied(), Some(5000));
    }
}
