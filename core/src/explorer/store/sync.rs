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
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE seq(b) AS (
               SELECT ?1 UNION ALL SELECT b+1 FROM seq WHERE b < ?2
             )
             SELECT b FROM seq
             WHERE b NOT IN (SELECT block FROM blocks_classified
                             WHERE block BETWEEN ?1 AND ?2)",
        )?;
        let rows = stmt.query_map(rusqlite::params![from as i64, to as i64], |r| {
            r.get::<_, i64>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row? as u64);
        }
        Ok(out)
    }
    /// Remove classified blocks >= fork block (reorg unwind).
    pub fn unwind_from(&self, fork_block: u64) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM blocks_classified WHERE block >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM mev_ops WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM transfers WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM swaps WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM txs WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM blocks WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM jit_open_positions WHERE opened_block >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM oracle_answers WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        self.conn.execute(
            "DELETE FROM reserve_rates WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
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
