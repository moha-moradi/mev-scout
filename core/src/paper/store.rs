//! Paper session / fill persistence on the explorer SQLite database.

use crate::explorer::store::ExplorerStore;
use crate::paper::types::{LedgerResult, PaperFill, PaperMode, PaperSession};
use crate::utils::epoch_secs;

impl ExplorerStore {
    /// Ensure paper tables exist (idempotent; also called from initialize).
    pub fn ensure_paper_tables(&self) -> anyhow::Result<()> {
        self.connection().execute_batch(
            "
            CREATE TABLE IF NOT EXISTS paper_sessions(
              session_id TEXT PRIMARY KEY,
              chain TEXT NOT NULL,
              mode TEXT NOT NULL,
              linked_run_id TEXT,
              start_block INTEGER NOT NULL,
              end_block INTEGER NOT NULL,
              starting_gas_wei TEXT NOT NULL,
              ending_gas_wei TEXT NOT NULL,
              reserve_wei TEXT NOT NULL,
              fills INTEGER NOT NULL,
              skipped INTEGER NOT NULL,
              net_profit_wei TEXT NOT NULL,
              max_drawdown_wei TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS paper_sessions_created
              ON paper_sessions(created_at);
            CREATE INDEX IF NOT EXISTS paper_sessions_chain
              ON paper_sessions(chain, created_at);

            CREATE TABLE IF NOT EXISTS paper_fills(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              session_id TEXT NOT NULL,
              block_number INTEGER NOT NULL,
              tx_index INTEGER,
              canonical_id TEXT,
              strategy TEXT NOT NULL,
              gross_wei TEXT NOT NULL,
              gas_wei TEXT NOT NULL,
              net_wei TEXT NOT NULL,
              wallet_before TEXT NOT NULL,
              wallet_after TEXT NOT NULL,
              pools_json TEXT,
              mempool_only INTEGER NOT NULL DEFAULT 0,
              FOREIGN KEY(session_id) REFERENCES paper_sessions(session_id)
            );
            CREATE INDEX IF NOT EXISTS paper_fills_session
              ON paper_fills(session_id, block_number);
            ",
        )?;
        Ok(())
    }

    /// Persist a completed paper session + its fills.
    pub fn insert_paper_session(
        &self,
        session_id: &str,
        chain: &str,
        mode: PaperMode,
        linked_run_id: Option<&str>,
        ledger: &LedgerResult,
    ) -> anyhow::Result<()> {
        self.ensure_paper_tables()?;
        let start = ledger.start_block.unwrap_or(0);
        let end = ledger.end_block.unwrap_or(start);
        let created = epoch_secs();
        self.connection().execute(
            "INSERT INTO paper_sessions(
               session_id, chain, mode, linked_run_id, start_block, end_block,
               starting_gas_wei, ending_gas_wei, reserve_wei, fills, skipped,
               net_profit_wei, max_drawdown_wei, created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![
                session_id,
                chain,
                mode.as_str(),
                linked_run_id,
                start as i64,
                end as i64,
                ledger.starting_gas_wei.to_string(),
                ledger.ending_gas_wei.to_string(),
                ledger.reserve_wei.to_string(),
                ledger.fills.len() as i64,
                ledger.skips.len() as i64,
                ledger.net_profit_wei.to_string(),
                ledger.max_drawdown_wei.to_string(),
                created as i64,
            ],
        )?;
        for f in &ledger.fills {
            self.insert_paper_fill(session_id, f)?;
        }
        Ok(())
    }

    fn insert_paper_fill(&self, session_id: &str, f: &PaperFill) -> anyhow::Result<()> {
        let pools_json = serde_json::to_string(
            &f.pools
                .iter()
                .map(|a| format!("{a:#x}"))
                .collect::<Vec<_>>(),
        )?;
        self.connection().execute(
            "INSERT INTO paper_fills(
               session_id, block_number, tx_index, canonical_id, strategy,
               gross_wei, gas_wei, net_wei, wallet_before, wallet_after,
               pools_json, mempool_only
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                session_id,
                f.block_number as i64,
                f.tx_index.map(|v| v as i64),
                f.canonical_id,
                f.strategy,
                f.gross_wei.to_string(),
                f.gas_wei.to_string(),
                f.net_wei.to_string(),
                f.wallet_before.to_string(),
                f.wallet_after.to_string(),
                pools_json,
                if f.mempool_only { 1i64 } else { 0i64 },
            ],
        )?;
        Ok(())
    }

    /// Load a paper session by id.
    pub fn paper_session(&self, session_id: &str) -> anyhow::Result<Option<PaperSession>> {
        self.ensure_paper_tables()?;
        let mut stmt = self.connection().prepare(
            "SELECT session_id, chain, mode, linked_run_id, start_block, end_block,
                    starting_gas_wei, ending_gas_wei, reserve_wei, fills, skipped,
                    net_profit_wei, max_drawdown_wei, created_at
             FROM paper_sessions WHERE session_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        if let Some(r) = rows.next()? {
            Ok(Some(row_to_session(r)?))
        } else {
            Ok(None)
        }
    }

    /// List paper sessions, newest first. `since_ts == 0` means all.
    pub fn list_paper_sessions(
        &self,
        since_ts: u64,
        limit: usize,
    ) -> anyhow::Result<Vec<PaperSession>> {
        self.ensure_paper_tables()?;
        let mut stmt = self.connection().prepare(
            "SELECT session_id, chain, mode, linked_run_id, start_block, end_block,
                    starting_gas_wei, ending_gas_wei, reserve_wei, fills, skipped,
                    net_profit_wei, max_drawdown_wei, created_at
             FROM paper_sessions
             WHERE created_at >= ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![since_ts as i64, limit as i64], |r| {
            Ok(row_to_session(r)?)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Fills for a session, ordered by block then tx.
    pub fn paper_fills(&self, session_id: &str) -> anyhow::Result<Vec<PaperFill>> {
        self.ensure_paper_tables()?;
        let mut stmt = self.connection().prepare(
            "SELECT block_number, tx_index, canonical_id, strategy,
                    gross_wei, gas_wei, net_wei, wallet_before, wallet_after,
                    pools_json, mempool_only
             FROM paper_fills
             WHERE session_id = ?1
             ORDER BY block_number, tx_index",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |r| {
            let pools_json: Option<String> = r.get(9)?;
            let pools: Vec<alloy::primitives::Address> = match pools_json.as_deref() {
                Some(s) => serde_json::from_str::<Vec<String>>(s)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|a| a.parse().ok())
                    .collect(),
                None => Vec::new(),
            };
            let net_s: String = r.get(6)?;
            let net_wei: i128 = net_s.parse().unwrap_or(0);
            Ok(PaperFill {
                block_number: r.get::<_, i64>(0)? as u64,
                tx_index: r.get::<_, Option<i64>>(1)?.map(|v| v as usize),
                canonical_id: r.get(2)?,
                strategy: r.get(3)?,
                gross_wei: r.get::<_, String>(4)?.parse().unwrap_or(0),
                gas_wei: r.get::<_, String>(5)?.parse().unwrap_or(0),
                net_wei,
                wallet_before: r.get::<_, String>(7)?.parse().unwrap_or(0),
                wallet_after: r.get::<_, String>(8)?.parse().unwrap_or(0),
                pools,
                mempool_only: r.get::<_, i64>(10)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper::ledger::LedgerPolicy;
    use crate::paper::types::PaperMode;
    use crate::types::{MevOpportunity, Strategy};
    use alloy::primitives::{address, U256};

    #[test]
    fn persists_session_and_fills() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let a = address!("0x0000000000000000000000000000000000000001");
        let b = address!("0x0000000000000000000000000000000000000002");
        let mut opp = MevOpportunity::new(10, 0, Strategy::TwoHopArb, a, 0);
        opp.pool_b = b;
        opp.expected_profit = U256::from(1_000u64);
        opp.gas_cost_wei = 100;
        opp.canonical_id = Some("cid".into());
        let policy = LedgerPolicy {
            starting_gas_wei: 10_000,
            reserve_wei: 0,
            max_fills_per_block: 32,
        };
        let ledger = policy.apply(&[opp]);
        store
            .insert_paper_session(
                "paper_test_1",
                "polygon",
                PaperMode::Sim,
                Some("run_1"),
                &ledger,
            )
            .unwrap();
        let s = store.paper_session("paper_test_1").unwrap().unwrap();
        assert_eq!(s.fills, 1);
        assert_eq!(s.mode, "sim");
        let fills = store.paper_fills("paper_test_1").unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].net_wei, 900);
    }
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<PaperSession> {
    Ok(PaperSession {
        session_id: r.get(0)?,
        chain: r.get(1)?,
        mode: r.get(2)?,
        linked_run_id: r.get(3)?,
        start_block: r.get::<_, i64>(4)? as u64,
        end_block: r.get::<_, i64>(5)? as u64,
        starting_gas_wei: r.get(6)?,
        ending_gas_wei: r.get(7)?,
        reserve_wei: r.get(8)?,
        fills: r.get::<_, i64>(9)? as u64,
        skipped: r.get::<_, i64>(10)? as u64,
        net_profit_wei: r.get(11)?,
        max_drawdown_wei: r.get(12)?,
        created_at: r.get::<_, i64>(13)? as u64,
    })
}
