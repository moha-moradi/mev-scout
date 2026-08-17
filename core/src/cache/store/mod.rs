//! Persistent block/state cache backed by SQLite.
//!
//! SqliteStore is the local-first persistence layer for the backtest engine.
//! All fetched block data (headers, transactions, receipts, account state,
//! storage slots, contract code, pool state) is stored in a single SQLite
//! database file for portability and offline querying.

pub mod accounts;
pub mod blocks;
pub mod integrity;
pub mod manifests;
pub mod pending;
pub mod pools;
pub mod ticks;

use std::path::Path;
use std::sync::{Arc, Mutex};

use alloy::primitives::{b256, Address, B256, U256};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::pool::state::PoolInfo;

/// Metadata for a completed simulation run, stored alongside cached block data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub chain: String,
    pub start_block: u64,
    pub end_block: u64,
    pub resolved_at: u64,
    pub range_mode: String,
    pub strategies: Vec<String>,
    pub flash_loan_provider: String,
}

/// Transfer(address,address,uint256) event topic hash
pub const TRANSFER_EVENT_TOPIC: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

/// SQLite-backed persistent cache for block data, EVM state, and run metadata.
///
/// Replaces the previous sled-backed cache. All data is stored in a
/// single SQLite database file with proper indexes for fast lookups.
/// Complex fields (logs, access lists, pool state) are stored as bincode BLOBs.
#[derive(Clone)]
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    /// Acquire the SQLite connection mutex guard.
    /// Panics with a clear message if the mutex is poisoned (process state corrupted).
    pub fn conn(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn.lock().expect("SQLite connection mutex poisoned")
    }
}

impl SqliteStore {
    /// Open (or create) a SQLite database at the given path.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let store = SqliteStore {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.initialize_tables()?;
        Ok(store)
    }

    /// Current schema version. Increment when adding a migration below.
    const SCHEMA_VERSION: u64 = 9;

    /// Create the SQLite schema if it does not exist.
    fn initialize_tables(&self) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            );

            CREATE TABLE IF NOT EXISTS blocks (
                number     INTEGER PRIMARY KEY,
                hash       BLOB NOT NULL,
                timestamp  INTEGER NOT NULL,
                base_fee_per_gas INTEGER,
                gas_limit  INTEGER NOT NULL,
                gas_used   INTEGER NOT NULL,
                coinbase   BLOB NOT NULL,
                difficulty INTEGER NOT NULL DEFAULT 0,
                mix_hash   BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
            );

            CREATE TABLE IF NOT EXISTS block_meta (
                number      INTEGER PRIMARY KEY,
                txs_fetched INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS transactions (
                hash       BLOB PRIMARY KEY,
                block_number INTEGER NOT NULL,
                tx_index   INTEGER NOT NULL,
                from_addr  BLOB NOT NULL,
                to_addr    BLOB,
                input      BLOB NOT NULL,
                value      BLOB NOT NULL,
                gas_limit  INTEGER NOT NULL,
                max_fee_per_gas INTEGER NOT NULL,
                max_priority_fee_per_gas INTEGER,
                nonce      INTEGER NOT NULL,
                access_list BLOB
            );
            CREATE INDEX IF NOT EXISTS idx_txs_block ON transactions(block_number);

            CREATE TABLE IF NOT EXISTS receipts (
                tx_hash    BLOB PRIMARY KEY,
                tx_index   INTEGER NOT NULL,
                status     INTEGER NOT NULL,
                gas_used   INTEGER NOT NULL,
                cumulative_gas_used INTEGER NOT NULL,
                logs       BLOB NOT NULL,
                contract_address BLOB
            );

            CREATE TABLE IF NOT EXISTS accounts (
                block_number INTEGER NOT NULL,
                address    BLOB NOT NULL,
                nonce      INTEGER NOT NULL,
                balance    BLOB NOT NULL,
                code_hash  BLOB NOT NULL,
                PRIMARY KEY (block_number, address)
            );

            CREATE TABLE IF NOT EXISTS storage_slots (
                block_number INTEGER NOT NULL,
                address    BLOB NOT NULL,
                slot       BLOB NOT NULL,
                value      BLOB NOT NULL,
                PRIMARY KEY (block_number, address, slot)
            );

            CREATE TABLE IF NOT EXISTS contract_code (
                address    BLOB PRIMARY KEY,
                code       BLOB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS pool_info (
                address    BLOB PRIMARY KEY,
                token0     BLOB NOT NULL,
                token1     BLOB NOT NULL,
                fee        INTEGER NOT NULL,
                dex_type   INTEGER NOT NULL,
                tick_spacing INTEGER,
                creation_block INTEGER NOT NULL,
                pool_id    BLOB,
                factory    BLOB,
                is_stable  INTEGER
            );

            CREATE TABLE IF NOT EXISTS pool_states (
                address    BLOB NOT NULL,
                block_number INTEGER NOT NULL,
                state_data BLOB NOT NULL,
                PRIMARY KEY (address, block_number)
            );

            CREATE TABLE IF NOT EXISTS v3_tick_cache (
                address     BLOB NOT NULL,
                center_word INTEGER NOT NULL,
                tick_spacing INTEGER NOT NULL,
                ticks       BLOB NOT NULL,
                updated_at  INTEGER NOT NULL,
                PRIMARY KEY (address, center_word)
            );

            CREATE TABLE IF NOT EXISTS discovery_cursors (
                factory    BLOB PRIMARY KEY,
                block_number INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS run_manifests (
                run_id     TEXT PRIMARY KEY,
                chain      TEXT NOT NULL,
                start_block INTEGER NOT NULL,
                end_block  INTEGER NOT NULL,
                resolved_at INTEGER NOT NULL,
                range_mode TEXT NOT NULL,
                strategies TEXT NOT NULL,
                flash_loan_provider TEXT NOT NULL

            );

            CREATE TABLE IF NOT EXISTS logs (
                block_number INTEGER NOT NULL,
                tx_index     INTEGER NOT NULL,
                log_index    INTEGER NOT NULL,
                address      BLOB NOT NULL,
                topic0       BLOB,
                topic1       BLOB,
                topic2       BLOB,
                topic3       BLOB,
                data         BLOB NOT NULL,
                erc20_amount BLOB,
                event_sig    TEXT,
                PRIMARY KEY (block_number, log_index)
            );
            CREATE INDEX IF NOT EXISTS idx_logs_address ON logs(address);
            CREATE INDEX IF NOT EXISTS idx_logs_topic0 ON logs(topic0);

            CREATE TABLE IF NOT EXISTS pending_txs (
                block_number INTEGER NOT NULL,
                tx_index     INTEGER NOT NULL,
                hash         BLOB NOT NULL,
                from_addr    BLOB NOT NULL,
                to_addr      BLOB,
                input        BLOB NOT NULL,
                value        BLOB NOT NULL,
                gas_limit    INTEGER NOT NULL,
                max_fee_per_gas INTEGER NOT NULL,
                max_priority_fee_per_gas INTEGER,
                nonce        INTEGER NOT NULL,
                access_list  BLOB,
                captured_at  INTEGER NOT NULL,
                PRIMARY KEY (block_number, tx_index)
            );

            CREATE TABLE IF NOT EXISTS token_symbols (
                address    BLOB PRIMARY KEY,
                symbol     TEXT NOT NULL,
                decimals   INTEGER
            );
            ",
        )?;

        let current_version: u64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get::<_, i64>(0).map(|v| v as u64),
            )
            .unwrap_or(0);

        if current_version < Self::SCHEMA_VERSION {
            self.run_migrations(&conn, current_version)?;
        }

        Ok(())
    }

    /// Apply pending SQLite schema migrations in order.
    fn run_migrations(&self, conn: &Connection, from: u64) -> anyhow::Result<()> {
        let migrations: Vec<&str> = vec![
            // v1: tx_type column (EIP-7702 support)
            "ALTER TABLE transactions ADD COLUMN tx_type INTEGER NOT NULL DEFAULT 0",
            // v2: is_stable column for Solidly/Camelot stable pools
            "ALTER TABLE pool_info ADD COLUMN is_stable INTEGER",
            // v3: factory column on pool_info
            "ALTER TABLE pool_info ADD COLUMN factory BLOB",
            // v4: extended pool metadata columns
            "ALTER TABLE pool_info ADD COLUMN underlying_tokens TEXT",
            "ALTER TABLE pool_info ADD COLUMN balancer_pool_type INTEGER",
            "ALTER TABLE pool_info ADD COLUMN hook_address BLOB",
            "ALTER TABLE pool_info ADD COLUMN bin_step INTEGER",
            "ALTER TABLE pool_info ADD COLUMN maturity_timestamp INTEGER",
            // v5: human-readable dex name and token symbols
            "ALTER TABLE pool_info ADD COLUMN dex_name TEXT",
            "ALTER TABLE pool_info ADD COLUMN token0_symbol TEXT",
            "ALTER TABLE pool_info ADD COLUMN token1_symbol TEXT",
            // v6: transaction signature columns
            "ALTER TABLE transactions ADD COLUMN sig_hash BLOB",
            "ALTER TABLE transactions ADD COLUMN sig_name TEXT",
            // v7: EIP-7702 authorization list on transactions
            "ALTER TABLE transactions ADD COLUMN authorization_list BLOB",
            // v8: V3 tick bootstrap cache (keyed by tick-bitmap center word)
            "CREATE TABLE IF NOT EXISTS v3_tick_cache (
                address     BLOB NOT NULL,
                center_word INTEGER NOT NULL,
                tick_spacing INTEGER NOT NULL,
                ticks       BLOB NOT NULL,
                updated_at  INTEGER NOT NULL,
                PRIMARY KEY (address, center_word)
            )",
            // v9: difficulty and mix_hash for accurate EVM replay
            "ALTER TABLE blocks ADD COLUMN difficulty INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE blocks ADD COLUMN mix_hash BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'",
        ];

        for (i, sql) in migrations.iter().enumerate() {
            let version = (i + 1) as u64;
            if version > from {
                // Ignore "duplicate column" on ALTER TABLE, propagate other errors
                let r = conn.execute_batch(sql);
                if let Err(rusqlite::Error::SqliteFailure(_e, Some(msg))) = &r {
                    if !msg.contains("duplicate column") {
                        r?;
                    }
                }
                conn.execute(
                    "UPDATE schema_version SET version = ?1",
                    rusqlite::params![version as i64],
                )?;
            }
        }

        Ok(())
    }

    // ---- Private serialization helpers ----

    fn serialize<T: Serialize + ?Sized>(val: &T) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(val)?)
    }

    fn deserialize<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
        Ok(bincode::deserialize(bytes)?)
    }

    pub fn serialize_access_list<T: Serialize>(list: &[T]) -> anyhow::Result<Option<Vec<u8>>> {
        if list.is_empty() { Ok(None) } else { Ok(Some(Self::serialize(list)?)) }
    }

    pub fn deserialize_access_list<T: serde::de::DeserializeOwned>(
        bytes: &[u8],
    ) -> anyhow::Result<Vec<T>> {
        Self::deserialize(bytes)
    }

    fn addr_to_blob(addr: &Address) -> Vec<u8> {
        addr.as_slice().to_vec()
    }

    fn blob_to_addr(blob: &[u8]) -> Address {
        Address::from_slice(blob)
    }

    fn u256_to_blob(val: &U256) -> Vec<u8> {
        val.to_be_bytes::<32>().to_vec()
    }

    fn blob_to_u256(blob: &[u8]) -> U256 {
        U256::from_be_slice(blob)
    }

    fn b256_to_blob(val: &B256) -> Vec<u8> {
        val.as_slice().to_vec()
    }

    fn blob_to_b256(blob: &[u8]) -> B256 {
        B256::from_slice(blob)
    }

    /// Extract ERC20 Transfer amount from a log's data bytes.
    /// Returns None if the log is not an ERC20 Transfer event or if data is too short.
    pub fn decode_erc20_amount(log: &crate::data::LogData) -> Option<U256> {
        if log.topics.first() == Some(&TRANSFER_EVENT_TOPIC) && log.data.len() >= 32 {
            Some(U256::from_be_slice(&log.data[log.data.len() - 32..]))
        } else {
            None
        }
    }

    /// Flush pending writes (WAL checkpoint).
    pub fn flush(&self) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

fn row_to_pool_info(row: &rusqlite::Row) -> anyhow::Result<PoolInfo> {
    let pool_id = row.get::<_, Option<Vec<u8>>>(7)?.map(|v| {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&v);
        arr
    });
    let factory = row.get::<_, Option<Vec<u8>>>(8).ok()
        .and_then(|v| v.and_then(|bytes| (bytes.len() == 20).then(|| Address::from_slice(&bytes))));
    let is_stable = row.get::<_, Option<i64>>(9).ok().flatten().map(|v| v != 0);
    let underlying_tokens: Option<Vec<Address>> = row.get::<_, Option<String>>(10).ok()
        .flatten()
        .and_then(|json_str| {
            let hexes: Vec<String> = serde_json::from_str(&json_str).ok()?;
            let addrs: Vec<Address> = hexes.iter()
                .filter_map(|h| h.strip_prefix("0x").or(Some(h.as_str())))
                .filter_map(|h| hex::decode(h).ok())
                .filter(|b| b.len() == 20)
                .map(|b| Address::from_slice(&b))
                .collect();
            (!addrs.is_empty()).then_some(addrs)
        });
    let balancer_pool_type = row.get::<_, Option<i64>>(11).ok().flatten().map(|v| v as u8);
    let hook_address = row.get::<_, Option<Vec<u8>>>(12).ok()
        .and_then(|v| v.and_then(|bytes| (bytes.len() == 20).then(|| Address::from_slice(&bytes))));
    let bin_step = row.get::<_, Option<i64>>(13).ok().flatten().map(|v| v as u32);
    let maturity_timestamp = row.get::<_, Option<i64>>(14).ok().flatten().map(|v| v as u64);
    let dex_name = row.get::<_, Option<String>>(15).ok().flatten();
    let token0_symbol = row.get::<_, Option<String>>(16).ok().flatten();
    let token1_symbol = row.get::<_, Option<String>>(17).ok().flatten();
    let token0 = SqliteStore::blob_to_addr(&row.get::<_, Vec<u8>>(1)?);
    let token1 = SqliteStore::blob_to_addr(&row.get::<_, Vec<u8>>(2)?);
    let is_fot = Some(crate::pool::state::pool_types::is_fee_on_transfer_token(&token0)
        || crate::pool::state::pool_types::is_fee_on_transfer_token(&token1));
    let is_rebase = Some(crate::pool::state::pool_types::is_rebase_token(&token0)
        || crate::pool::state::pool_types::is_rebase_token(&token1));
    Ok(PoolInfo {
        address: SqliteStore::blob_to_addr(&row.get::<_, Vec<u8>>(0)?),
        token0,
        token1,
        fee: row.get::<_, i64>(3)? as u32,
        name: None,
        dex_type: dex_type_from_i64(row.get::<_, i64>(4)?)?,
        tick_spacing: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
        creation_block: row.get::<_, i64>(6)? as u64,
        pool_id,
        factory,
        is_stable,
        is_fot,
        is_rebase,
        underlying_tokens,
        balancer_pool_type,
        hook_address,
        bin_step,
        maturity_timestamp,
        dex_name: dex_name.map(Arc::from),
        token0_symbol: token0_symbol.map(Arc::from),
        token1_symbol: token1_symbol.map(Arc::from),
    })
}

fn row_to_normalized_log(row: &rusqlite::Row) -> anyhow::Result<crate::data::NormalizedLog> {
    Ok(crate::data::NormalizedLog {
        block_number: row.get::<_, i64>(0)? as u64,
        tx_index: row.get::<_, i64>(1)? as u64,
        log_index: row.get::<_, i64>(2)? as u64,
        address: SqliteStore::blob_to_addr(&row.get::<_, Vec<u8>>(3)?),
        topic0: row.get::<_, Option<Vec<u8>>>(4)?.map(|b| SqliteStore::blob_to_b256(&b)),
        topic1: row.get::<_, Option<Vec<u8>>>(5)?.map(|b| SqliteStore::blob_to_b256(&b)),
        topic2: row.get::<_, Option<Vec<u8>>>(6)?.map(|b| SqliteStore::blob_to_b256(&b)),
        topic3: row.get::<_, Option<Vec<u8>>>(7)?.map(|b| SqliteStore::blob_to_b256(&b)),
        data: row.get::<_, Vec<u8>>(8)?.into(),
        erc20_amount: row.get::<_, Option<Vec<u8>>>(9)?.map(|b| SqliteStore::blob_to_u256(&b)),
        event_sig: row.get::<_, Option<String>>(10)?,
    })
}

fn row_to_manifest(row: &rusqlite::Row) -> anyhow::Result<RunManifest> {
    Ok(RunManifest {
        run_id: row.get(0)?,
        chain: row.get(1)?,
        start_block: row.get::<_, i64>(2)? as u64,
        end_block: row.get::<_, i64>(3)? as u64,
        resolved_at: row.get::<_, i64>(4)? as u64,
        range_mode: row.get(5)?,
        strategies: row.get::<_, String>(6)?.split(',').map(|s| s.to_string()).filter(|s| !s.is_empty()).collect(),
        flash_loan_provider: row.get(7)?,
    })
}

fn dex_type_from_i64(v: i64) -> anyhow::Result<crate::dex_type::DexType> {
    match v {
        0 => Ok(crate::dex_type::DexType::UniswapV2),
        1 => Ok(crate::dex_type::DexType::UniswapV3),
        2 => Ok(crate::dex_type::DexType::Curve),
        3 => Ok(crate::dex_type::DexType::Balancer),
        5 => Ok(crate::dex_type::DexType::Solidly),
        6 => Ok(crate::dex_type::DexType::Camelot),
        7 => Ok(crate::dex_type::DexType::UniswapV4),
        8 => Ok(crate::dex_type::DexType::TraderJoeLB),
        9 => Ok(crate::dex_type::DexType::Pendle),
        n => anyhow::bail!("invalid dex_type discriminant: {}", n),
    }
}
