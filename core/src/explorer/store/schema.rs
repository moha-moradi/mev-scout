//! Schema migrate + open helpers for ExplorerStore.
use std::path::Path;

use rusqlite::Connection;

use super::ExplorerStore;

/// Milliseconds a writer waits on a locked explorer DB before failing.
///
/// WAL plus `unchecked_transaction` otherwise returns `SQLITE_BUSY`
/// immediately when a live indexer and a backfill share the file.
const BUSY_TIMEOUT_MS: i64 = 5_000;

/// Ordered migrations. `Vn` runs when `user_version < n`. Never edit a landed
/// migration; append one.
///
/// A file created before this list has `user_version = 0` and already contains
/// the V1 tables. That file is stamped at version 1 and does not replay V1.
/// V2+ are idempotent, so a DB that already received the old `ensure_column`
/// / skim rebuild / reject index still opens.
///
/// Not in this list: a `chain` column and composite keys on the fact tables
/// (A18's schema half). `blocks_classified` is still keyed by block number
/// alone. The config-load guard refuses a shared `explorer.db_path`, which is
/// the live safety; a column nothing reads would not isolate two chains.
/// That rebuild has to land with the store API taking a chain id.
const MIGRATIONS: &[fn(&Connection) -> anyhow::Result<()>] = &[
    migrate_v1_create,
    migrate_v2_flashloan_fee_usd,
    migrate_v3_volume_usd,
    migrate_v4_excluded_from_net,
    migrate_v5_kind_skim,
    migrate_v6_rejected_natural_key,
];

fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
    // `:memory:` stays `journal_mode=memory` (SQLite will not put WAL on a
    // memory database) but still accepts `synchronous` and `busy_timeout`,
    // so tests and file opens share one pragma setup.
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;
    // `foreign_keys` stays OFF. There is no FOREIGN KEY clause in the schema,
    // and enabling the pragma later has to be its own migration with a
    // pre-flight orphan count — an existing DB with orphans would fail to open.
    Ok(())
}

fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let mut v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v == 0 && table_exists(conn, "blocks_classified")? {
        conn.pragma_update(None, "user_version", 1i64)?;
        v = 1;
    }
    for (i, step) in MIGRATIONS.iter().enumerate() {
        let target = (i + 1) as i64;
        if v < target {
            step(conn)?;
            conn.pragma_update(None, "user_version", target)?;
            v = target;
        }
    }
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> anyhow::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn migrate_v1_create(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(SCHEMA_V1)?;
    Ok(())
}

fn migrate_v2_flashloan_fee_usd(conn: &Connection) -> anyhow::Result<()> {
    ExplorerStore::ensure_column(conn, "mev_ops", "flashloan_fee_usd", "REAL")
}

fn migrate_v3_volume_usd(conn: &Connection) -> anyhow::Result<()> {
    ExplorerStore::ensure_column(conn, "mev_ops", "volume_usd", "REAL")
}

fn migrate_v4_excluded_from_net(conn: &Connection) -> anyhow::Result<()> {
    ExplorerStore::ensure_column(
        conn,
        "mev_ops",
        "excluded_from_net",
        "INTEGER NOT NULL DEFAULT 0",
    )
}

fn migrate_v5_kind_skim(conn: &Connection) -> anyhow::Result<()> {
    ExplorerStore::ensure_mev_ops_kind_skim(conn)
}

fn migrate_v6_rejected_natural_key(conn: &Connection) -> anyhow::Result<()> {
    ExplorerStore::ensure_rejected_candidates_unique(conn)
}

/// V1 table set. `CREATE TABLE IF NOT EXISTS` so replaying it on a file that
/// already has the tables is a no-op. Columns added after the first release
/// (`flashloan_fee_usd`, `volume_usd`, `excluded_from_net`) are in this DDL
/// for a fresh database and re-applied idempotently by V2–V4 for a file that
/// was created before they existed.
const SCHEMA_V1: &str = "
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
              created_at INTEGER NOT NULL,
              excluded_from_net INTEGER NOT NULL DEFAULT 0
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

            -- JIT open Mint positions, persisted so cross-block JIT detection
            -- survives restarts. Pruned by
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

            -- prior oracle answers + reserve
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
            ";

impl ExplorerStore {
    /// Open (or create) the explorer database at `path`.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        configure_connection(&conn)?;
        migrate(&conn)?;
        Ok(ExplorerStore { conn })
    }
    /// Open an in-memory database (tests). Same pragmas as [`Self::open`]
    /// except that SQLite keeps `journal_mode=memory`.
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        migrate(&conn)?;
        Ok(ExplorerStore { conn })
    }
    /// Natural key of one rejected candidate, as a column list usable in both
    /// a `UNIQUE` index and a `GROUP BY`.
    ///
    /// Identity = where the reject was seen (`chain`, `block_number`,
    /// `tx_index`), which pass produced it (`strategy`), the candidate geometry
    /// that pass was looking at (pool/path/token/amount columns), why it was
    /// dropped (`reject_reason`) and the pass's own note (`detail`).
    ///
    /// `created_at` and `run_id` are deliberately excluded: a re-index
    /// re-derives every other field, and it must be a no-op instead of
    /// appending a second copy of every reject. Scanner rows keep their
    /// distinctness through the geometry columns, so two candidates dropped
    /// from the same tx for the same reason (the per-tx top-N cap) stay
    /// separate rows — only a byte-identical candidate collapses.
    ///
    /// Nullable columns are wrapped in `COALESCE(…, '')` because SQLite treats
    /// NULLs as distinct inside a unique index; without that the classify rows
    /// (whose geometry columns are all NULL) would never conflict and
    /// re-indexing would keep duplicating them.
    const REJECTED_NATURAL_KEY: &str = "
        COALESCE(chain, ''), block_number, COALESCE(tx_index, -1), strategy,
        COALESCE(pool_a, ''), COALESCE(pool_b, ''), COALESCE(path, ''),
        COALESCE(token_in, ''), COALESCE(token_out, ''), COALESCE(input_amount, ''),
        COALESCE(expected_profit, ''), reject_reason, COALESCE(detail, '')";
    /// Create the `rejected_candidates` natural-key unique index.
    ///
    /// V6. A pre-B7 database can already hold duplicate reject rows — that is
    /// the bug this index closes — so a constraint failure on the first
    /// attempt collapses each natural key onto its earliest row and retries
    /// once. Any other error surfaces unchanged.
    fn ensure_rejected_candidates_unique(conn: &rusqlite::Connection) -> anyhow::Result<()> {
        let create = format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS rejected_candidates_natural
               ON rejected_candidates({})",
            Self::REJECTED_NATURAL_KEY
        );
        match conn.execute_batch(&create) {
            Ok(()) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                conn.execute_batch(&format!(
                    "DELETE FROM rejected_candidates
                      WHERE rowid NOT IN (
                        SELECT MIN(rowid) FROM rejected_candidates
                        GROUP BY {}
                      )",
                    Self::REJECTED_NATURAL_KEY
                ))?;
                tracing::warn!(
                    "rejected_candidates held duplicate rows from before the \
                     natural-key index; collapsed them onto the earliest row \
                     of each key before indexing"
                );
                conn.execute_batch(&create)?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
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
              created_at INTEGER NOT NULL,
              excluded_from_net INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO mev_ops_new SELECT
              id, block_number, tx_index, tx_hash, ts, kind, eoa, contract, confidence,
              canonical_id, profit_token, profit_amount, profit_usd, volume_usd,
              gas_cost_usd, flashloan_fee_usd, net_profit_usd, route_json,
              victim_hashes, details_json, detector, created_at, excluded_from_net
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

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_db_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "mev-scout-schema-{tag}-{}.sqlite",
            std::process::id()
        ))
    }

    fn remove_db(path: &std::path::Path) {
        for p in [
            path.to_path_buf(),
            path.with_extension("sqlite-wal"),
            path.with_extension("sqlite-shm"),
        ] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// A pre-B7 database can already hold duplicate rejects (the bug the
    /// natural-key index closes). Opening it must collapse each key onto its
    /// earliest row instead of failing on the new unique index, and reopening
    /// must stay a no-op.
    #[test]
    fn opening_a_legacy_db_collapses_duplicate_rejects() {
        let path = legacy_db_path("rejects");
        remove_db(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE rejected_candidates(
                   run_id TEXT, chain TEXT, block_number INTEGER NOT NULL, tx_index INTEGER,
                   strategy TEXT NOT NULL, pool_a TEXT, pool_b TEXT, path TEXT,
                   token_in TEXT, token_out TEXT, input_amount TEXT, expected_profit TEXT,
                   expected_profit_usd REAL, gas_cost_wei TEXT, reject_reason TEXT NOT NULL,
                   detail TEXT, created_at INTEGER NOT NULL);
                 INSERT INTO rejected_candidates
                   (chain, block_number, tx_index, strategy, reject_reason, created_at)
                 VALUES ('polygon', 7, 1, 'explorer_classify', 'unresolved_cycle', 0),
                        ('polygon', 7, 1, 'explorer_classify', 'unresolved_cycle', 99);",
            )
            .unwrap();
        }

        let count = |store: &ExplorerStore| -> i64 {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM rejected_candidates", [], |r| r.get(0))
                .unwrap()
        };
        {
            let store = ExplorerStore::open(&path).unwrap();
            assert_eq!(
                count(&store),
                1,
                "duplicates collapse onto the earliest row"
            );
        }
        {
            // Reopening (the index already exists) is a no-op.
            let store = ExplorerStore::open(&path).unwrap();
            assert_eq!(count(&store), 1);
        }
        remove_db(&path);
    }

    fn user_version(store: &ExplorerStore) -> i64 {
        store
            .connection()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn fresh_store_is_at_head_and_sets_busy_timeout() {
        let store = ExplorerStore::open_in_memory().unwrap();
        assert_eq!(user_version(&store), super::MIGRATIONS.len() as i64);
        let timeout: i64 = store
            .connection()
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(timeout, super::BUSY_TIMEOUT_MS);

        let path = legacy_db_path("fresh");
        remove_db(&path);
        {
            let store = ExplorerStore::open(&path).unwrap();
            assert_eq!(user_version(&store), super::MIGRATIONS.len() as i64);
        }
        {
            let store = ExplorerStore::open(&path).unwrap();
            assert_eq!(user_version(&store), super::MIGRATIONS.len() as i64);
        }
        remove_db(&path);
    }

    /// A pre-migration file (`user_version = 0`, tables already present) is
    /// stamped at V1 and still receives the later column alters.
    #[test]
    fn legacy_db_gains_mev_ops_columns_and_lands_on_head() {
        let path = legacy_db_path("columns");
        remove_db(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE blocks_classified(
                   block INTEGER PRIMARY KEY,
                   classified_at INTEGER NOT NULL,
                   event_count INTEGER NOT NULL);
                 CREATE TABLE mev_ops(
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   block_number INTEGER NOT NULL,
                   tx_index INTEGER,
                   tx_hash TEXT NOT NULL,
                   ts INTEGER NOT NULL,
                   kind TEXT NOT NULL CHECK(kind IN
                     ('arb_atomic','sandwich','frontrun','backrun','liquidation','jit','jit_arb','skim','unknown')),
                   eoa TEXT NOT NULL,
                   contract TEXT,
                   confidence TEXT NOT NULL,
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
                   created_at INTEGER NOT NULL);
                 CREATE TABLE rejected_candidates(
                   run_id TEXT, chain TEXT, block_number INTEGER NOT NULL, tx_index INTEGER,
                   strategy TEXT NOT NULL, pool_a TEXT, pool_b TEXT, path TEXT,
                   token_in TEXT, token_out TEXT, input_amount TEXT, expected_profit TEXT,
                   expected_profit_usd REAL, gas_cost_wei TEXT, reject_reason TEXT NOT NULL,
                   detail TEXT, created_at INTEGER NOT NULL);",
            )
            .unwrap();
        }
        let store = ExplorerStore::open(&path).unwrap();
        assert_eq!(user_version(&store), super::MIGRATIONS.len() as i64);
        for column in ["flashloan_fee_usd", "volume_usd", "excluded_from_net"] {
            let present: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('mev_ops') WHERE name = ?1",
                    [column],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                present, 1,
                "{column} must be migrated onto a legacy mev_ops"
            );
        }
        remove_db(&path);
    }
}
