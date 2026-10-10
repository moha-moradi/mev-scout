//! Schema migrate + open helpers for ExplorerStore.
use std::path::Path;

use rusqlite::Connection;

use super::ExplorerStore;

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
                ('arb_atomic','sandwich','frontrun','backrun','liquidation','jit','jit_arb','skim','unknown')),
              eoa TEXT NOT NULL,
              contract TEXT,
              confidence TEXT NOT NULL CHECK(confidence IN ('exact','inferred')),
              canonical_id TEXT,
              profit_token TEXT,
              profit_amount TEXT,
              profit_usd REAL,
              volume_usd REAL,
              gas_cost_usd REAL,
              flashloan_fee_usd REAL,
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

            -- Phase 5a-0: JIT open Mint positions, persisted so cross-block JIT
            -- detection survives restarts. Feeder table for Phase 1.5; pruned by
            -- opened_block block-window cap.
            CREATE TABLE IF NOT EXISTS jit_open_positions(
              pool TEXT NOT NULL,
              owner TEXT NOT NULL,
              tick_lower INTEGER NOT NULL,
              tick_upper INTEGER NOT NULL,
              opened_block INTEGER NOT NULL,
              liquidity TEXT NOT NULL,
              PRIMARY KEY(pool, owner, tick_lower, tick_upper)
            );
            CREATE INDEX IF NOT EXISTS jit_open_positions_prune
              ON jit_open_positions(opened_block);

            -- Plan P1.1 / P1.4 mode-B lookback: prior oracle answers + reserve
            -- borrow-rate series (single-chain, logs-only).
            CREATE TABLE IF NOT EXISTS oracle_answers(
              block_number INTEGER NOT NULL,
              feed TEXT NOT NULL,
              answer TEXT NOT NULL,
              PRIMARY KEY(block_number, feed)
            );
            CREATE INDEX IF NOT EXISTS oracle_answers_feed
              ON oracle_answers(feed, block_number);
            CREATE TABLE IF NOT EXISTS reserve_rates(
              block_number INTEGER NOT NULL,
              reserve TEXT NOT NULL,
              variable_borrow_rate TEXT NOT NULL,
              PRIMARY KEY(block_number, reserve)
            );
            CREATE INDEX IF NOT EXISTS reserve_rates_reserve
              ON reserve_rates(reserve, block_number);

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
              mempool_only INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS paper_fills_session
              ON paper_fills(session_id, block_number);
            ",
        )?;
        // Phase 2.2 runtime migration for databases created before the
        // flash-loan fee column existed. `volume_usd` likewise for pre-report DBs.
        Self::ensure_column(&self.conn, "mev_ops", "flashloan_fee_usd", "REAL")?;
        Self::ensure_column(&self.conn, "mev_ops", "volume_usd", "REAL")?;
        Self::ensure_mev_ops_kind_skim(&self.conn)?;
        Ok(())
    }
    /// Widen `mev_ops.kind` CHECK to include `'skim'` (SQLite cannot ALTER CHECK).
    fn ensure_mev_ops_kind_skim(conn: &rusqlite::Connection) -> anyhow::Result<()> {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='mev_ops'",
                [],
                |r| r.get(0),
            )
            .ok();
        let Some(sql) = sql else {
            return Ok(());
        };
        if sql.contains("'skim'") {
            return Ok(());
        }
        conn.execute_batch(
            "
            BEGIN;
            CREATE TABLE mev_ops_new(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              block_number INTEGER NOT NULL,
              tx_index INTEGER,
              tx_hash TEXT NOT NULL,
              ts INTEGER NOT NULL,
              kind TEXT NOT NULL CHECK(kind IN
                ('arb_atomic','sandwich','frontrun','backrun','liquidation','jit','jit_arb','skim','unknown')),
              eoa TEXT NOT NULL,
              contract TEXT,
              confidence TEXT NOT NULL CHECK(confidence IN ('exact','inferred')),
              canonical_id TEXT,
              profit_token TEXT,
              profit_amount TEXT,
              profit_usd REAL,
              volume_usd REAL,
              gas_cost_usd REAL,
              flashloan_fee_usd REAL,
              net_profit_usd REAL,
              route_json TEXT,
              victim_hashes TEXT,
              details_json TEXT,
              detector TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            INSERT INTO mev_ops_new SELECT
              id, block_number, tx_index, tx_hash, ts, kind, eoa, contract, confidence,
              canonical_id, profit_token, profit_amount, profit_usd, volume_usd,
              gas_cost_usd, flashloan_fee_usd, net_profit_usd, route_json,
              victim_hashes, details_json, detector, created_at
            FROM mev_ops;
            DROP TABLE mev_ops;
            ALTER TABLE mev_ops_new RENAME TO mev_ops;
            CREATE INDEX IF NOT EXISTS mev_ops_block ON mev_ops(block_number);
            CREATE INDEX IF NOT EXISTS mev_ops_sender ON mev_ops(eoa, block_number);
            CREATE INDEX IF NOT EXISTS mev_ops_kind_ts ON mev_ops(kind, ts);
            CREATE INDEX IF NOT EXISTS mev_ops_canonical ON mev_ops(canonical_id);
            COMMIT;
            ",
        )?;
        Ok(())
    }
    /// Add `column` to `table` when missing (idempotent, for existing DBs).
    fn ensure_column(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
        ty: &str,
    ) -> anyhow::Result<()> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        let mut exists = false;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                exists = true;
                break;
            }
        }
        drop(rows);
        drop(stmt);
        if !exists {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))?;
        }
        Ok(())
    }
}
