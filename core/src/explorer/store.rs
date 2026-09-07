//! Explorer store — SQLite persistence for realized-MEV facts (plan §9).
//!
//! Separate database file from the scanner cache (`explorer_{chain}.sqlite`)
//! so backfill writes never contend with replay-path reads. WAL mode,
//! single-writer, batched transactions. Schema is Postgres-portable.
//!
//! Layers:
//! - forensic facts: `blocks`, `txs`, `transfers`, `swaps`, `mev_ops` (§9.1)
//! - results layer: `opportunities` fed from `ResultsFile` (§9.2)
//! - rejection capture: `rejected_candidates` (§9.3)
//! - checkpointing: `sync_state` + `blocks_classified` (gap-safe resume)

use std::path::Path;

use alloy::primitives::{Address, B256, U256};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::explorer::types::{MevEvent, MevKind};

/// One persisted realized-MEV operation row (`mev_ops`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevOpRow {
    pub id: i64,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub tx_hash: String,
    pub ts: u64,
    pub kind: String,
    pub eoa: String,
    pub contract: Option<String>,
    pub confidence: String,
    pub canonical_id: Option<String>,
    pub profit_token: Option<String>,
    pub profit_amount: Option<String>,
    pub profit_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub route_json: Option<String>,
    pub victim_hashes: Option<String>,
    pub details_json: Option<String>,
    pub detector: String,
    pub created_at: u64,
}

/// Aggregate row for stats/leaderboard queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsRow {
    pub label: String,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
}

/// Live-feed row (tail of `mev_ops`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedRow {
    pub ts: u64,
    pub block_number: u64,
    pub kind: String,
    pub profit_token: Option<String>,
    pub profit_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub eoa: String,
    pub tx_hash: String,
    pub route_json: Option<String>,
}

pub struct ExplorerStore {
    conn: Connection,
}

impl ExplorerStore {
    /// Open (or create) the explorer database at `path`.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let store = ExplorerStore { conn };
        store.initialize()?;
        Ok(store)
    }

    /// Open an in-memory database (tests).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = ExplorerStore { conn };
        store.initialize()?;
        Ok(store)
    }

    fn initialize(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS blocks(
              block_number INTEGER PRIMARY KEY,
              block_hash TEXT NOT NULL,
              ts INTEGER NOT NULL,
              producer TEXT,
              base_fee_gwei REAL,
              tx_count INTEGER,
              indexed_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS txs(
              hash TEXT PRIMARY KEY,
              block_number INTEGER NOT NULL,
              tx_index INTEGER NOT NULL,
              \"from\" TEXT NOT NULL,
              \"to\" TEXT,
              success INTEGER NOT NULL,
              gas_used INTEGER,
              effective_gas_price_gwei REAL,
              priority_fee_gwei REAL,
              value_native TEXT
            );
            CREATE INDEX IF NOT EXISTS txs_block ON txs(block_number);

            CREATE TABLE IF NOT EXISTS transfers(
              block_number INTEGER NOT NULL,
              tx_index INTEGER NOT NULL,
              log_index INTEGER NOT NULL,
              token TEXT NOT NULL,
              \"from\" TEXT NOT NULL,
              \"to\" TEXT NOT NULL,
              amount TEXT NOT NULL,
              is_native INTEGER NOT NULL DEFAULT 0,
              PRIMARY KEY(block_number, tx_index, log_index)
            );
            CREATE INDEX IF NOT EXISTS transfers_token ON transfers(token, block_number);

            CREATE TABLE IF NOT EXISTS swaps(
              block_number INTEGER NOT NULL,
              tx_index INTEGER NOT NULL,
              log_index INTEGER NOT NULL,
              pool TEXT NOT NULL,
              dex TEXT,
              amm TEXT,
              token_in TEXT NOT NULL,
              token_out TEXT NOT NULL,
              amount_in TEXT NOT NULL,
              amount_out TEXT NOT NULL,
              sender TEXT,
              PRIMARY KEY(block_number, tx_index, log_index)
            );
            CREATE INDEX IF NOT EXISTS swaps_pool ON swaps(pool, block_number);

            CREATE TABLE IF NOT EXISTS mev_ops(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              block_number INTEGER NOT NULL,
              tx_index INTEGER,
              tx_hash TEXT NOT NULL,
              ts INTEGER NOT NULL,
              kind TEXT NOT NULL CHECK(kind IN
                ('arb_atomic','sandwich','liquidation','jit','jit_arb','unknown')),
              eoa TEXT NOT NULL,
              contract TEXT,
              confidence TEXT NOT NULL CHECK(confidence IN ('exact','inferred')),
              canonical_id TEXT,
              profit_token TEXT,
              profit_amount TEXT,
              profit_usd REAL,
              gas_cost_usd REAL,
              net_profit_usd REAL,
              route_json TEXT,
              victim_hashes TEXT,
              details_json TEXT,
              detector TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS mev_ops_block ON mev_ops(block_number);
            CREATE INDEX IF NOT EXISTS mev_ops_sender ON mev_ops(eoa, block_number);
            CREATE INDEX IF NOT EXISTS mev_ops_kind_ts ON mev_ops(kind, ts);
            CREATE INDEX IF NOT EXISTS mev_ops_canonical ON mev_ops(canonical_id);

            CREATE TABLE IF NOT EXISTS labels(
              address TEXT PRIMARY KEY,
              kind TEXT,
              name TEXT,
              entity TEXT,
              evidence TEXT,
              first_seen_block INTEGER,
              source TEXT
            );

            CREATE TABLE IF NOT EXISTS prices(
              hour INTEGER NOT NULL,
              token TEXT NOT NULL,
              usd REAL NOT NULL,
              source TEXT,
              PRIMARY KEY(hour, token)
            );

            CREATE TABLE IF NOT EXISTS sync_state(
              chain_id INTEGER PRIMARY KEY,
              head INTEGER,
              indexed_to INTEGER,
              last_indexed_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS blocks_classified(
              block INTEGER PRIMARY KEY,
              classified_at INTEGER NOT NULL,
              event_count INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS opportunities(
              run_id TEXT,
              chain TEXT,
              block_number INTEGER NOT NULL,
              tx_index INTEGER,
              strategy TEXT NOT NULL,
              pool_a TEXT,
              pool_b TEXT,
              token_in TEXT,
              token_out TEXT,
              input_amount TEXT,
              expected_profit TEXT,
              gas_cost_wei TEXT,
              path TEXT,
              timestamp INTEGER,
              mempool_only INTEGER,
              confidence TEXT,
              sender TEXT,
              tx_hash TEXT,
              detection_path TEXT,
              canonical_id TEXT
            );
            CREATE INDEX IF NOT EXISTS opportunities_block
              ON opportunities(chain, block_number);
            CREATE INDEX IF NOT EXISTS opportunities_canonical
              ON opportunities(canonical_id);

            CREATE TABLE IF NOT EXISTS rejected_candidates(
              run_id TEXT,
              chain TEXT,
              block_number INTEGER NOT NULL,
              tx_index INTEGER,
              strategy TEXT NOT NULL,
              pool_a TEXT,
              pool_b TEXT,
              path TEXT,
              token_in TEXT,
              token_out TEXT,
              input_amount TEXT,
              expected_profit TEXT,
              expected_profit_usd REAL,
              gas_cost_wei TEXT,
              reject_reason TEXT NOT NULL,
              detail TEXT,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS rejected_block
              ON rejected_candidates(chain, block_number);
            CREATE INDEX IF NOT EXISTS rejected_reason
              ON rejected_candidates(reject_reason);
            ",
        )?;
        Ok(())
    }

    // ── sync_state / checkpoints ────────────────────────────────────────

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
    pub fn set_sync_state(
        &self,
        chain_id: u64,
        head: u64,
        indexed_to: u64,
    ) -> anyhow::Result<()> {
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
    pub fn unclassified_blocks(
        &self,
        from: u64,
        to: u64,
    ) -> anyhow::Result<Vec<u64>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE seq(b) AS (
               SELECT ?1 UNION ALL SELECT b+1 FROM seq WHERE b < ?2
             )
             SELECT b FROM seq
             WHERE b NOT IN (SELECT block FROM blocks_classified
                             WHERE block BETWEEN ?1 AND ?2)",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![from as i64, to as i64],
            |r| r.get::<_, i64>(0),
        )?;
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
        self.conn
            .execute("DELETE FROM txs WHERE block_number >= ?1", [fork_block as i64])?;
        self.conn.execute(
            "DELETE FROM blocks WHERE block_number >= ?1",
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

    // ── facts inserts ───────────────────────────────────────────────────

    /// Insert block header + txs + swaps + transfers + mev events in one
    /// transaction. Idempotent via OR IGNORE on natural keys.
    pub fn insert_block_facts(
        &self,
        block_number: u64,
        block_hash: &B256,
        ts: u64,
        base_fee_gwei: Option<f64>,
        tx_count: usize,
        txs: &[TxRow],
        swaps: &[SwapRow],
        transfers: &[TransferRow],
        events: &[MevEvent],
        native_price_usd: Option<f64>,
        token_prices: &std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd>,
    ) -> anyhow::Result<usize> {
        let now = crate::utils::epoch_secs() as i64;
        let conn = &self.conn;
        conn.execute("BEGIN IMMEDIATE", [])?;
        let result = (|| -> anyhow::Result<usize> {
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

            for ev in events {
                let profit_usd = match (ev.profit_token, ev.profit_amount) {
                    (Some(tok), Some(amt)) => token_prices
                        .get(&tok)
                        .map(|p| crate::explorer::pricing::token_amount_to_usd(amt, p)),
                    _ => None,
                };
                // Gas in USD via native price (gas wei → native → USD).
                let gas_usd = native_price_usd
                    .map(|np| crate::explorer::pricing::wei_to_usd(ev.gas_cost_wei, np));
                let net = match (profit_usd, gas_usd) {
                    (Some(g), Some(gg)) => Some(g - gg),
                    _ => None,
                };
                let canonical = crate::explorer::explorer_canonical_id(ev);
                conn.execute(
                    "INSERT INTO mev_ops
                       (block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                        confidence, canonical_id, profit_token, profit_amount,
                        profit_usd, gas_cost_usd, net_profit_usd, route_json,
                        victim_hashes, details_json, detector, created_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                            ?14, ?15, ?16, ?17, 'explorer', ?18)",
                    rusqlite::params![
                        ev.block as i64,
                        ev.tx_index as i64,
                        format!("{:#x}", ev.tx_hash),
                        ev.ts as i64,
                        ev.kind.as_str(),
                        format!("{:#x}", ev.searcher),
                        ev.contract.map(|a| format!("{:#x}", a)),
                        ev.confidence.as_str(),
                        canonical,
                        ev.profit_token.map(|a| format!("{:#x}", a)),
                        ev.profit_amount.map(|a| a.to_string()),
                        profit_usd,
                        gas_usd,
                        net,
                        ev.details.get("route").map(|r| r.to_string()),
                        serde_json::to_string(
                            &ev.victim_hashes.iter().map(|h| format!("{:#x}", h))
                                .collect::<Vec<_>>()
                        )
                        .ok(),
                        Some(ev.details.to_string()),
                        now,
                    ],
                )?;
            }

            conn.execute(
                "INSERT OR REPLACE INTO blocks_classified(block, classified_at, event_count)
                 VALUES(?1, ?2, ?3)",
                rusqlite::params![block_number as i64, now, events.len() as i64],
            )?;
            Ok(events.len())
        })();

        match result {
            Ok(n) => {
                conn.execute("COMMIT", [])?;
                Ok(n)
            }
            Err(e) => {
                conn.execute("ROLLBACK", [])?;
                Err(e)
            }
        }
    }

    /// Purge `transfers` older than `keep_blocks` behind head (retention §9.4).
    pub fn prune_transfers(&self, before_block: u64) -> anyhow::Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM transfers WHERE block_number < ?1", [
                before_block as i64,
            ])?;
        Ok(n)
    }

    // ── results layer (§9.2) ────────────────────────────────────────────

    /// Insert one opportunity row from a `ResultsFile` entry.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_opportunity(
        &self,
        run_id: &str,
        chain: &str,
        block_number: u64,
        tx_index: Option<u64>,
        strategy: &str,
        pool_a: Option<Address>,
        pool_b: Option<Address>,
        token_in: Option<Address>,
        token_out: Option<Address>,
        input_amount: Option<U256>,
        expected_profit: Option<U256>,
        gas_cost_wei: Option<U256>,
        path: Option<&str>,
        timestamp: Option<u64>,
        mempool_only: bool,
        confidence: Option<&str>,
        sender: Option<Address>,
        tx_hash: Option<B256>,
        detection_path: Option<&str>,
        canonical_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO opportunities
               (run_id, chain, block_number, tx_index, strategy, pool_a, pool_b,
                token_in, token_out, input_amount, expected_profit, gas_cost_wei,
                path, timestamp, mempool_only, confidence, sender, tx_hash,
                detection_path, canonical_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            rusqlite::params![
                run_id,
                chain,
                block_number as i64,
                tx_index.map(|v| v as i64),
                strategy,
                pool_a.map(|a| format!("{:#x}", a)),
                pool_b.map(|a| format!("{:#x}", a)),
                token_in.map(|a| format!("{:#x}", a)),
                token_out.map(|a| format!("{:#x}", a)),
                input_amount.map(|v| v.to_string()),
                expected_profit.map(|v| v.to_string()),
                gas_cost_wei.map(|v| v.to_string()),
                path,
                timestamp.map(|v| v as i64),
                mempool_only as i64,
                confidence,
                sender.map(|a| format!("{:#x}", a)),
                tx_hash.map(|h| format!("{:#x}", h)),
                detection_path,
                canonical_id,
            ],
        )?;
        Ok(())
    }

    // ── queries ─────────────────────────────────────────────────────────

    /// Live feed: most recent ops (optionally filtered by kind).
    pub fn feed_tail(&self, limit: usize, kinds: &[MevKind]) -> anyhow::Result<Vec<FeedRow>> {
        let order = if kinds.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
            format!("WHERE kind IN ({})", list.join(","))
        };
        let sql = format!(
            "SELECT ts, block_number, kind, profit_token, profit_usd, net_profit_usd,
                    eoa, tx_hash, route_json
             FROM mev_ops {order}
             ORDER BY block_number DESC, tx_index DESC, id DESC LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_feed_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Period stats grouped by kind (§10 `stats`).
    pub fn stats_by_kind(&self, since_ts: u64) -> anyhow::Result<Vec<StatsRow>> {
        stats_grouped(
            &self.conn,
            "kind",
            since_ts,
            None,
        )
    }

    /// Sender leaderboard (`top --by sender`).
    pub fn top_senders(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        top_grouped(&self.conn, "eoa", since_ts, limit)
    }

    /// Most profitable tokens (`top --by token`).
    pub fn top_tokens(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        top_grouped(&self.conn, "profit_token", since_ts, limit)
    }

    /// Busiest pools (`top --by pool`) — pool list comes from route_json.
    pub fn top_pools(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        // Pools live inside route_json; extract with json_each.
        let sql = format!(
            "SELECT j.value->>'$.pool' AS label,
                    COUNT(DISTINCT m.id) AS ops,
                    COALESCE(SUM(m.profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(m.net_profit_usd), 0) AS net_usd
             FROM mev_ops m, json_each(COALESCE(m.route_json, '[]')) j
             WHERE m.ts >= {since_ts} AND j.value->>'$.pool' IS NOT NULL
             GROUP BY label
             ORDER BY gross_usd DESC
             LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_stats_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Daily breakdown (`stats --window day`).
    pub fn stats_daily(&self, since_ts: u64) -> anyhow::Result<Vec<StatsRow>> {
        let sql = format!(
            "SELECT date(ts, 'unixepoch') AS label,
                    COUNT(*) AS ops,
                    COALESCE(SUM(profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(net_profit_usd), 0) AS net_usd
             FROM mev_ops
             WHERE ts >= {since_ts}
             GROUP BY label
             ORDER BY label DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_stats_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Full op detail for `show <TX>`.
    pub fn ops_for_tx(&self, tx_hash: &str) -> anyhow::Result<Vec<MevOpRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, net_profit_usd, route_json, victim_hashes,
                    details_json, detector, created_at
             FROM mev_ops WHERE tx_hash = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([tx_hash], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Realized ops in a block window (`validate` ground truth).
    pub fn ops_in_range(
        &self,
        from_block: u64,
        to_block: u64,
        kinds: &[MevKind],
    ) -> anyhow::Result<Vec<MevOpRow>> {
        let kind_filter = if kinds.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
            format!("AND kind IN ({})", list.join(","))
        };
        let sql = format!(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, net_profit_usd, route_json, victim_hashes,
                    details_json, detector, created_at
             FROM mev_ops
             WHERE block_number BETWEEN {from_block} AND {to_block} {kind_filter}
             ORDER BY block_number, tx_index"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Opportunities in a block window (`validate` scanner side).
    pub fn opportunities_in_range(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<OpportunityRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, block_number, tx_index, strategy, pool_a, pool_b,
                    token_in, token_out, expected_profit, mempool_only,
                    detection_path, canonical_id, tx_hash
             FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3
             ORDER BY block_number",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| {
                Ok(OpportunityRow {
                    run_id: r.get(0)?,
                    block_number: r.get::<_, i64>(1)? as u64,
                    tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    strategy: r.get(3)?,
                    pool_a: r.get(4)?,
                    pool_b: r.get(5)?,
                    token_in: r.get(6)?,
                    token_out: r.get(7)?,
                    expected_profit: r.get(8)?,
                    mempool_only: r.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
                    detection_path: r.get(10)?,
                    canonical_id: r.get(11)?,
                    tx_hash: r.get(12)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Blocks with realized ops of a kind (block-level recall, T3).
    pub fn blocks_with_kind(
        &self,
        from_block: u64,
        to_block: u64,
        kind: MevKind,
    ) -> anyhow::Result<std::collections::HashSet<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT block_number FROM mev_ops
             WHERE kind = ?1 AND block_number BETWEEN ?2 AND ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![kind.as_str(), from_block as i64, to_block as i64],
            |r| r.get::<_, i64>(0),
        )?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r? as u64);
        }
        Ok(out)
    }

    /// Blocks where the scanner recorded opportunities (M7 coverage check).
    pub fn opportunity_blocks(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<std::collections::HashSet<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT block_number FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| r.get::<_, i64>(0),
        )?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r? as u64);
        }
        Ok(out)
    }

    /// Price cache lookup: USD for a token at an hour bucket (exact hour, or
    /// the most recent cached hour within 24h).
    pub fn price_at(&self, token: Address, hour: u64) -> anyhow::Result<Option<(f64, String)>> {
        let tok = format!("{:#x}", token);
        let exact: Option<(f64, String)> = self
            .conn
            .query_row(
                "SELECT usd, source FROM prices WHERE token = ?1 AND hour = ?2",
                rusqlite::params![tok, hour as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .unwrap_or(None);
        if exact.is_some() {
            return Ok(exact);
        }
        let v = self
            .conn
            .query_row(
                "SELECT usd, source FROM prices
                 WHERE token = ?1 AND hour <= ?2 AND hour > ?2 - 24
                 ORDER BY hour DESC LIMIT 1",
                rusqlite::params![tok, hour as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .unwrap_or(None);
        Ok(v)
    }

    /// Cache a price observation.
    pub fn put_price(
        &self,
        token: Address,
        hour: u64,
        usd: f64,
        source: &str,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO prices(hour, token, usd, source) VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![hour as i64, format!("{:#x}", token), usd, source],
        )?;
        Ok(())
    }

    /// Count of ops by kind since a timestamp (quick overview).
    pub fn op_count_since(&self, since_ts: u64) -> anyhow::Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM mev_ops WHERE ts >= ?1",
            [since_ts as i64],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Insert an address label (classifier growth loop, §8.3).
    pub fn upsert_label(
        &self,
        address: Address,
        name: &str,
        source: &str,
        first_seen_block: u64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO labels(address, kind, name, entity, evidence, first_seen_block, source)
             VALUES(?1, 'searcher', ?2, NULL, NULL, ?3, ?4)
             ON CONFLICT(address) DO NOTHING",
            rusqlite::params![
                format!("{:#x}", address),
                name,
                first_seen_block as i64,
                source
            ],
        )?;
        Ok(())
    }

    /// Pools seen in realized ops (missing-pool report, `--emit-missing-pools`).
    pub fn distinct_route_pools(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<String>> {
        let sql = format!(
            "SELECT DISTINCT j.value->>'$.pool' AS pool
             FROM mev_ops m, json_each(COALESCE(m.route_json, '[]')) j
             WHERE m.block_number BETWEEN {from_block} AND {to_block}
               AND j.value->>'$.pool' IS NOT NULL
             ORDER BY pool"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

/// A transaction row to persist alongside block facts.
pub struct TxRow {
    pub hash: B256,
    pub tx_index: u64,
    pub from: Address,
    pub to: Option<Address>,
    pub success: bool,
    pub gas_used: u64,
    pub effective_gas_price_gwei: f64,
    pub priority_fee_gwei: f64,
    pub value: U256,
}

/// A swap row to persist.
pub struct SwapRow {
    pub tx_index: u64,
    pub log_index: u64,
    pub pool: Address,
    pub amm: crate::explorer::types::Amm,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
}

/// A transfer row to persist.
pub struct TransferRow {
    pub tx_index: u64,
    pub log_index: u64,
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub amount: U256,
}

/// One scanner opportunity row (`opportunities` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityRow {
    pub run_id: Option<String>,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub strategy: String,
    pub pool_a: Option<String>,
    pub pool_b: Option<String>,
    pub token_in: Option<String>,
    pub token_out: Option<String>,
    pub expected_profit: Option<String>,
    pub mempool_only: bool,
    pub detection_path: Option<String>,
    pub canonical_id: Option<String>,
    pub tx_hash: Option<String>,
}

fn map_feed_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FeedRow> {
    Ok(FeedRow {
        ts: r.get::<_, i64>(0)? as u64,
        block_number: r.get::<_, i64>(1)? as u64,
        kind: r.get(2)?,
        profit_token: r.get(3)?,
        profit_usd: r.get(4)?,
        net_profit_usd: r.get(5)?,
        eoa: r.get(6)?,
        tx_hash: r.get(7)?,
        route_json: r.get(8)?,
    })
}

fn map_stats_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StatsRow> {
    Ok(StatsRow {
        label: r.get(0)?,
        ops: r.get(1)?,
        gross_usd: r.get(2)?,
        net_usd: r.get(3)?,
    })
}

fn map_mev_op_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MevOpRow> {
    Ok(MevOpRow {
        id: r.get(0)?,
        block_number: r.get::<_, i64>(1)? as u64,
        tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        tx_hash: r.get(3)?,
        ts: r.get::<_, i64>(4)? as u64,
        kind: r.get(5)?,
        eoa: r.get(6)?,
        contract: r.get(7)?,
        confidence: r.get(8)?,
        canonical_id: r.get(9)?,
        profit_token: r.get(10)?,
        profit_amount: r.get(11)?,
        profit_usd: r.get(12)?,
        gas_cost_usd: r.get(13)?,
        net_profit_usd: r.get(14)?,
        route_json: r.get(15)?,
        victim_hashes: r.get(16)?,
        details_json: r.get(17)?,
        detector: r.get(18)?,
        created_at: r.get::<_, i64>(19)? as u64,
    })
}

fn stats_grouped(
    conn: &Connection,
    group_col: &str,
    since_ts: u64,
    kind: Option<&str>,
) -> anyhow::Result<Vec<StatsRow>> {
    let kind_clause = kind
        .map(|k| format!("AND kind = '{k}'"))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {group_col} AS label, COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts} {kind_clause}
         GROUP BY label
         ORDER BY gross_usd DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn top_grouped(
    conn: &Connection,
    group_col: &str,
    since_ts: u64,
    limit: usize,
) -> anyhow::Result<Vec<StatsRow>> {
    let sql = format!(
        "SELECT {group_col} AS label, COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts} AND {group_col} IS NOT NULL
         GROUP BY label
         ORDER BY gross_usd DESC
         LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::types::{Confidence, MevEvent};
    use alloy::primitives::{address, b256};

    fn sample_event(block: u64) -> MevEvent {
        MevEvent {
            block,
            ts: 1_700_000_000,
            tx_index: 3,
            tx_hash: b256!("1111111111111111111111111111111111111111111111111111111111111111"),
            kind: MevKind::ArbAtomic,
            searcher: address!("2222000000000000000000000000000000000002"),
            contract: None,
            pools: vec![address!("3333000000000000000000000000000000000003")],
            profit_token: Some(address!("4444000000000000000000000000000000000004")),
            profit_amount: Some(U256::from(1_000_000u64)),
            profit_usd: None,
            gas_cost_wei: U256::from(100_000_000_000_000u64),
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({
                "route": [{"pool": "0x3333", "amm": "v3"}]
            }),
        }
    }

    #[test]
    fn insert_and_query_roundtrip() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            crate::explorer::pricing::TokenUsd { usd: 0.5, decimals: 6 },
        );
        let n = store
            .insert_block_facts(
                100,
                &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                1_700_000_000,
                Some(25.0),
                5,
                &[],
                &[],
                &[],
                &[sample_event(100)],
                Some(0.75),
                &prices,
            )
            .unwrap();
        assert_eq!(n, 1);
        assert!(store.block_classified(100).unwrap());
        assert!(!store.block_classified(101).unwrap());

        let ops = store.ops_in_range(100, 100, &[]).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].profit_usd.unwrap() > 0.0);
        assert!(ops[0].net_profit_usd.unwrap() < ops[0].profit_usd.unwrap());
        assert!(ops[0].canonical_id.as_deref().unwrap().starts_with("ArbAtomic|"));

        let blocks = store
            .blocks_with_kind(100, 100, MevKind::ArbAtomic)
            .unwrap();
        assert!(blocks.contains(&100));

        let gaps = store.unclassified_blocks(99, 102).unwrap();
        assert_eq!(gaps, vec![99, 101, 102]);

        store.unwind_from(100).unwrap();
        assert!(!store.block_classified(100).unwrap());
    }

    #[test]
    fn sync_state_roundtrip() {
        let store = ExplorerStore::open_in_memory().unwrap();
        assert_eq!(store.get_indexed_to(137).unwrap(), 0);
        store.set_sync_state(137, 1000, 990).unwrap();
        assert_eq!(store.get_indexed_to(137).unwrap(), 990);
    }

    #[test]
    fn price_cache_window() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let tok = address!("4444000000000000000000000000000000000004");
        store.put_price(tok, 1000, 1.23, "test").unwrap();
        assert!(store.price_at(tok, 1000).unwrap().is_some());
        assert!(store.price_at(tok, 1010).unwrap().is_some());
        assert!(store.price_at(tok, 2000).unwrap().is_none());
    }
}
