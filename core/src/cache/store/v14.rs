//! Stage 2 v14 Master Inventory schema and v13 import.
//!
//! The legacy `pool_info` table is renamed to `pool_info_v13_archive`. All
//! current readers see `pool_info` as a read-only view over
//! `pool_legacy_snapshot`; writes go through `InventoryWriter`.

use anyhow::Context;
use rusqlite::{params, Connection};

use crate::pool::identity::PoolKey;

const LEGACY_COLUMNS: &str = "address, token0, token1, fee, dex_type, tick_spacing, \
     creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, \
     hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol, \
     tvl_usd, volume_usd_24h, volume_usd_30d, derivation_version";

pub(super) fn table_exists(conn: &Connection, name: &str) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub(super) fn create_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pool_identity (
            pool_key        TEXT PRIMARY KEY,
            chain_id        INTEGER NOT NULL,
            namespace       TEXT NOT NULL,
            family          TEXT NOT NULL,
            version         TEXT,
            legacy_address  BLOB,
            created_block   INTEGER,
            first_seen_at   TEXT NOT NULL,
            identity_state  TEXT NOT NULL DEFAULT 'provisional',
            registry_revision INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_pool_identity_chain ON pool_identity(chain_id);

        CREATE TABLE IF NOT EXISTS pool_observation (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            pool_key        TEXT NOT NULL REFERENCES pool_identity(pool_key),
            event_key       TEXT NOT NULL UNIQUE,
            chain_id        INTEGER NOT NULL,
            source          TEXT NOT NULL,
            method          TEXT NOT NULL,
            authority       TEXT NOT NULL,
            run_id          TEXT,
            endpoint        TEXT,
            block_number    INTEGER,
            block_hash      TEXT,
            tx_hash         TEXT,
            log_index       INTEGER,
            observed_at     TEXT NOT NULL,
            raw_identifier  TEXT,
            payload_json    TEXT NOT NULL,
            conflict        INTEGER NOT NULL DEFAULT 0,
            valid           INTEGER NOT NULL DEFAULT 1,
            orphaned_at     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_pool_observation_key ON pool_observation(pool_key);
        CREATE INDEX IF NOT EXISTS idx_pool_observation_valid ON pool_observation(valid);

        CREATE TABLE IF NOT EXISTS pool_role (
            pool_key       TEXT NOT NULL REFERENCES pool_identity(pool_key),
            role           TEXT NOT NULL,
            value          TEXT NOT NULL,
            observation_id INTEGER REFERENCES pool_observation(id),
            PRIMARY KEY (pool_key, role, value)
        );

        CREATE TABLE IF NOT EXISTS pool_status (
            pool_key          TEXT PRIMARY KEY REFERENCES pool_identity(pool_key),
            validation_state  TEXT NOT NULL DEFAULT 'unverified',
            quoteable         INTEGER NOT NULL DEFAULT 0,
            simulation_ready  INTEGER NOT NULL DEFAULT 0,
            execution_ready   INTEGER NOT NULL DEFAULT 0,
            admission_reason  TEXT,
            registry_revision INTEGER NOT NULL DEFAULT 0,
            updated_at        TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS scan_window (
            window_key        TEXT PRIMARY KEY,
            chain_id          INTEGER NOT NULL,
            source            TEXT NOT NULL,
            scan_type         TEXT NOT NULL,
            subject           TEXT NOT NULL,
            from_block        INTEGER NOT NULL,
            to_block          INTEGER NOT NULL,
            state             TEXT NOT NULL,
            coverage          TEXT NOT NULL,
            canonical_head_hash TEXT,
            cursor_block      INTEGER,
            attempts          INTEGER NOT NULL DEFAULT 0,
            last_error        TEXT,
            created_at        TEXT NOT NULL,
            updated_at        TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_scan_window_retry
            ON scan_window(chain_id, source, state);

        CREATE TABLE IF NOT EXISTS reorg_event (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            chain_id            INTEGER NOT NULL,
            detected_at         TEXT NOT NULL,
            old_head_hash       TEXT,
            new_head_hash       TEXT,
            common_ancestor     TEXT,
            affected_windows_json TEXT,
            status              TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS registry_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS pool_legacy_snapshot (
            address    BLOB PRIMARY KEY,
            token0     BLOB NOT NULL,
            token1     BLOB NOT NULL,
            fee        INTEGER NOT NULL,
            dex_type   INTEGER NOT NULL,
            tick_spacing INTEGER,
            creation_block INTEGER NOT NULL,
            pool_id    BLOB,
            factory    BLOB,
            is_stable  INTEGER,
            underlying_tokens TEXT,
            balancer_pool_type INTEGER,
            hook_address BLOB,
            bin_step   INTEGER,
            maturity_timestamp INTEGER,
            dex_name   TEXT,
            token0_symbol TEXT,
            token1_symbol TEXT,
            tvl_usd    REAL,
            volume_usd_24h REAL,
            volume_usd_30d REAL,
            derivation_version INTEGER,
            authority  TEXT NOT NULL DEFAULT 'legacy',
            authority_rank INTEGER NOT NULL DEFAULT 0,
            observed_at TEXT NOT NULL DEFAULT '0',
            pool_key   TEXT
        );

        CREATE VIEW IF NOT EXISTS pool_info_legacy_complete AS
        SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block,
               pool_id, factory, is_stable, underlying_tokens, balancer_pool_type,
               hook_address, bin_step, maturity_timestamp, dex_name,
               token0_symbol, token1_symbol, tvl_usd, volume_usd_24h,
               volume_usd_30d, derivation_version
        FROM pool_legacy_snapshot;

        CREATE VIEW IF NOT EXISTS pool_info AS
        SELECT * FROM pool_info_legacy_complete;
        ",
    )?;
    create_read_only_triggers(conn)?;
    Ok(())
}

fn create_read_only_triggers(conn: &Connection) -> anyhow::Result<()> {
    for view in ["pool_info", "pool_info_legacy_complete"] {
        for (op, trigger) in [
            ("INSERT", "insert"),
            ("UPDATE", "update"),
            ("DELETE", "delete"),
        ] {
            conn.execute_batch(&format!(
                "CREATE TRIGGER IF NOT EXISTS {view}_no_{trigger}
                 INSTEAD OF {op} ON {view}
                 BEGIN
                     SELECT RAISE(ABORT, '{view} is read-only; use InventoryWriter');
                 END;"
            ))?;
        }
    }
    Ok(())
}

/// Add optional columns that are missing from a sparse v13 `pool_info` table.
///
/// `PRAGMA table_info` is authoritative here: an unseeded `schema_version`
/// must not be trusted to infer which columns exist.
fn ensure_legacy_columns(conn: &Connection, table: &str) -> anyhow::Result<()> {
    let mut present = std::collections::HashSet::new();
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        present.insert(name?);
    }
    drop(stmt);

    const OPTIONAL: &[(&str, &str)] = &[
        ("pool_id", "BLOB"),
        ("factory", "BLOB"),
        ("is_stable", "INTEGER"),
        ("underlying_tokens", "TEXT"),
        ("balancer_pool_type", "INTEGER"),
        ("hook_address", "BLOB"),
        ("bin_step", "INTEGER"),
        ("maturity_timestamp", "INTEGER"),
        ("dex_name", "TEXT"),
        ("token0_symbol", "TEXT"),
        ("token1_symbol", "TEXT"),
        ("tvl_usd", "REAL"),
        ("volume_usd_24h", "REAL"),
        ("volume_usd_30d", "REAL"),
        ("derivation_version", "INTEGER"),
        ("tick_spacing", "INTEGER"),
    ];
    for (name, kind) in OPTIONAL {
        if !present.contains(*name) {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {name} {kind}"))?;
        }
    }
    Ok(())
}

/// Import a v13 database after the legacy v1–v13 migrations have normalized
/// its columns. The whole import runs in one `BEGIN IMMEDIATE` transaction.
pub(super) fn migrate_v13_to_v14(conn: &Connection, chain_id: u64) -> anyhow::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let result = (|| -> anyhow::Result<()> {
        // Rename before `create_schema`: it recreates `pool_info` as a view.
        if table_exists(conn, "pool_info")? && !table_exists(conn, "pool_info_v13_archive")? {
            conn.execute_batch("ALTER TABLE pool_info RENAME TO pool_info_v13_archive;")?;
        }
        anyhow::ensure!(
            table_exists(conn, "pool_info_v13_archive")?,
            "v13 migration expected pool_info or pool_info_v13_archive"
        );

        create_schema(conn)?;
        conn.execute(
            "INSERT OR IGNORE INTO registry_meta(key, value) VALUES ('chain_id', ?1)",
            params![chain_id.to_string()],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO registry_meta(key, value) VALUES ('registry_revision', '1')",
            [],
        )?;
        ensure_legacy_columns(conn, "pool_info_v13_archive")?;

        let already_imported: i64 =
            conn.query_row("SELECT COUNT(*) FROM pool_legacy_snapshot", [], |row| {
                row.get(0)
            })?;
        let pools = if already_imported == 0 {
            let copy = format!(
                "INSERT OR IGNORE INTO pool_legacy_snapshot
                 ({LEGACY_COLUMNS}, authority, authority_rank, observed_at, pool_key)
                 SELECT {LEGACY_COLUMNS}, 'legacy', 0, '0', NULL
                 FROM pool_info_v13_archive"
            );
            conn.execute_batch(&copy)?;

            let select = format!("SELECT {LEGACY_COLUMNS} FROM pool_legacy_snapshot");
            let mut stmt = conn.prepare(&select)?;
            let mut rows = stmt.query([])?;
            let mut pools = Vec::new();
            while let Some(row) = rows.next()? {
                pools.push(super::row_to_pool_info(row)?);
            }
            drop(rows);
            drop(stmt);
            pools
        } else {
            Vec::new()
        };

        let observed_at = now_string();
        for pool in &pools {
            // Approved contract §10.3: a migrated singleton-capable row keeps
            // its address key and stays provisional. The full PoolId is stored
            // as a role/observation until Stage 4 verifies on-chain identity.
            let key = PoolKey::EvmPool {
                chain_id,
                address: pool.address,
            };
            let key_text = key.to_string();
            let namespace = "evm_pool_address";
            conn.execute(
                "INSERT OR IGNORE INTO pool_identity
                 (pool_key, chain_id, namespace, family, version, legacy_address,
                  created_block, first_seen_at, identity_state, registry_revision)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, 'provisional', 1)",
                params![
                    key_text,
                    chain_id as i64,
                    namespace,
                    pool.dex_type.to_string(),
                    key.legacy_address().as_slice().to_vec(),
                    pool.creation_block as i64,
                    observed_at,
                ],
            )?;

            let payload = serde_json::to_string(pool)
                .context("serialize legacy pool row for v14 observation")?;
            conn.execute(
                "INSERT OR IGNORE INTO pool_observation
                 (pool_key, event_key, chain_id, source, method, authority, observed_at,
                  payload_json, raw_identifier)
                 VALUES (?1, ?2, ?3, 'legacy_v13', 'archive', 'legacy', ?4, ?5, ?6)",
                params![
                    key_text,
                    format!("legacy_v13:{key_text}"),
                    chain_id as i64,
                    observed_at,
                    payload,
                    hex_addr(&pool.address),
                ],
            )?;

            conn.execute(
                "INSERT OR IGNORE INTO pool_status
                 (pool_key, validation_state, quoteable, simulation_ready,
                  execution_ready, admission_reason, registry_revision, updated_at)
                 VALUES (?1, 'unverified', 0, 0, 0, 'legacy row requires on-chain verification', 1, ?2)",
                params![key_text, observed_at],
            )?;

            insert_role(conn, &key_text, "pool_address", &hex_addr(&pool.address))?;
            insert_role(
                conn,
                &key_text,
                "legacy_address",
                &hex_addr(&key.legacy_address()),
            )?;
            if let Some(factory) = pool.factory {
                insert_role(conn, &key_text, "factory", &hex_addr(&factory))?;
            }
            if let Some(pool_id) = pool.pool_id {
                let pool_id_text = format!("0x{}", hex::encode(pool_id));
                insert_role(conn, &key_text, "pool_id", &pool_id_text)?;
                let singleton = pool.factory.unwrap_or(pool.address);
                insert_role(
                    conn,
                    &key_text,
                    "singleton_candidate",
                    &format!("{}:{pool_id_text}", hex_addr(&singleton)),
                )?;
            }
            if let Some(hook) = pool.hook_address {
                insert_role(conn, &key_text, "hook", &hex_addr(&hook))?;
            }
            if let Some(tokens) = &pool.underlying_tokens {
                for token in tokens {
                    insert_role(conn, &key_text, "underlying_tokens", &hex_addr(token))?;
                }
            }
        }

        conn.execute_batch("COMMIT;")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = conn.execute_batch("ROLLBACK;");
    }
    result
}

fn hex_addr(address: &alloy::primitives::Address) -> String {
    format!("0x{}", hex::encode(address.as_slice()))
}

fn insert_role(conn: &Connection, key: &str, role: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pool_role(pool_key, role, value, observation_id)
         VALUES (?1, ?2, ?3, NULL)",
        params![key, role, value],
    )?;
    Ok(())
}

pub(super) fn now_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

/// Nanosecond timestamp used to make non-chain observation event keys unique.
pub(super) fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
