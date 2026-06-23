//! Persistent block/state cache backed by SQLite.
//!
//! `SqliteStore` is the local-first persistence layer for the backtest engine.
//! All fetched block data (headers, transactions, receipts, account state,
//! storage slots, contract code, pool state) is stored in a single SQLite
//! database file for portability and offline querying.

use std::path::Path;
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, Bytes, B256, U256};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::data::{AccountData, BlockData, ReceiptData, TxData};
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

/// SQLite-backed persistent cache for block data, EVM state, and run metadata.
///
/// Replaces the previous sled-backed cache. All data is stored in a
/// single SQLite database file with proper indexes for fast lookups.
/// Complex fields (logs, access lists, pool state) are stored as bincode BLOBs.
#[derive(Clone)]
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    chain_id: u64,
}



impl SqliteStore {
    /// Open (or create) a SQLite database at the given path.
    pub fn open(path: impl AsRef<Path>, chain_id: u64) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let store = SqliteStore {
            conn: Arc::new(Mutex::new(conn)),
            chain_id,
        };
        store.initialize_tables()?;
        Ok(store)
    }

    /// Create the SQLite schema if it does not exist.
    fn initialize_tables(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS blocks (
                number     INTEGER PRIMARY KEY,
                hash       BLOB NOT NULL,
                timestamp  INTEGER NOT NULL,
                base_fee_per_gas INTEGER,
                gas_limit  INTEGER NOT NULL,
                gas_used   INTEGER NOT NULL,
                coinbase   BLOB NOT NULL
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
                factory    BLOB
            );

            CREATE TABLE IF NOT EXISTS pool_states (
                address    BLOB NOT NULL,
                block_number INTEGER NOT NULL,
                state_data BLOB NOT NULL,
                PRIMARY KEY (address, block_number)
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
            ",
        )?;
        // L6: migration — add factory column to pool_info if missing (backward compat)
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN factory BLOB;");
        Ok(())
    }

    fn serialize<T: Serialize + ?Sized>(val: &T) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(val)?)
    }

    fn deserialize<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
        Ok(bincode::deserialize(bytes)?)
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

    // ---- Block ----

    pub fn put_block(&self, block_num: u64, block: &BlockData) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO blocks (number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                block_num as i64,
                Self::b256_to_blob(&block.hash),
                block.timestamp as i64,
                block.base_fee_per_gas.map(|v| v as i64),
                block.gas_limit as i64,
                block.gas_used as i64,
                Self::addr_to_blob(&block.coinbase),
            ],
        )?;
        Ok(())
    }

    pub fn put_block_data(
        &self,
        block_num: u64,
        block: &BlockData,
        txs: &[TxData],
        receipts: &[ReceiptData],
    ) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT OR REPLACE INTO blocks (number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                block_num as i64,
                Self::b256_to_blob(&block.hash),
                block.timestamp as i64,
                block.base_fee_per_gas.map(|v| v as i64),
                block.gas_limit as i64,
                block.gas_used as i64,
                Self::addr_to_blob(&block.coinbase),
            ],
        )?;

        {
            let mut tx_stmt = tx.prepare(
                "INSERT OR REPLACE INTO transactions (hash, block_number, tx_index, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for tx_data in txs {
                let access_list_blob = if tx_data.access_list.is_empty() {
                    None
                } else {
                    Some(Self::serialize(&tx_data.access_list)?)
                };
                tx_stmt.execute(rusqlite::params![
                    Self::b256_to_blob(&tx_data.hash),
                    block_num as i64,
                    tx_data.index as i64,
                    Self::addr_to_blob(&tx_data.from),
                    tx_data.to.map(|a| Self::addr_to_blob(&a)),
                    tx_data.input.to_vec(),
                    Self::u256_to_blob(&tx_data.value),
                    tx_data.gas_limit as i64,
                    tx_data.max_fee_per_gas as i64,
                    tx_data.max_priority_fee_per_gas.map(|v| v as i64),
                    tx_data.nonce as i64,
                    access_list_blob,
                ])?;
            }
        }

        {
            let mut rc_stmt = tx.prepare(
                "INSERT OR REPLACE INTO receipts (tx_hash, tx_index, status, gas_used, cumulative_gas_used, logs, contract_address)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for r in receipts {
                let logs_blob = Self::serialize(&r.logs)?;
                rc_stmt.execute(rusqlite::params![
                    Self::b256_to_blob(&r.tx_hash),
                    r.tx_index as i64,
                    r.status as i64,
                    r.gas_used as i64,
                    r.cumulative_gas_used as i64,
                    logs_blob,
                    r.contract_address.map(|a| Self::addr_to_blob(&a)),
                ])?;
            }
        }

        tx.execute(
            "INSERT OR REPLACE INTO block_meta (number, txs_fetched) VALUES (?1, 1)",
            rusqlite::params![block_num as i64],
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn put_block_data_batch(
        &self,
        batch: &[(u64, BlockData, Vec<TxData>, Vec<ReceiptData>)],
    ) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        {
            let mut block_stmt = tx.prepare(
                "INSERT OR REPLACE INTO blocks (number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut tx_stmt = tx.prepare(
                "INSERT OR REPLACE INTO transactions (hash, block_number, tx_index, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            let mut rc_stmt = tx.prepare(
                "INSERT OR REPLACE INTO receipts (tx_hash, tx_index, status, gas_used, cumulative_gas_used, logs, contract_address)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut meta_stmt = tx.prepare(
                "INSERT OR REPLACE INTO block_meta (number, txs_fetched) VALUES (?1, 1)",
            )?;

            for (block_num, block, txs, receipts) in batch {
                block_stmt.execute(rusqlite::params![
                    *block_num as i64,
                    Self::b256_to_blob(&block.hash),
                    block.timestamp as i64,
                    block.base_fee_per_gas.map(|v| v as i64),
                    block.gas_limit as i64,
                    block.gas_used as i64,
                    Self::addr_to_blob(&block.coinbase),
                ])?;

                for tx_data in txs {
                    let access_list_blob = if tx_data.access_list.is_empty() {
                        None
                    } else {
                        Some(Self::serialize(&tx_data.access_list)?)
                    };
                    tx_stmt.execute(rusqlite::params![
                        Self::b256_to_blob(&tx_data.hash),
                        *block_num as i64,
                        tx_data.index as i64,
                        Self::addr_to_blob(&tx_data.from),
                        tx_data.to.map(|a| Self::addr_to_blob(&a)),
                        tx_data.input.to_vec(),
                        Self::u256_to_blob(&tx_data.value),
                        tx_data.gas_limit as i64,
                        tx_data.max_fee_per_gas as i64,
                        tx_data.max_priority_fee_per_gas.map(|v| v as i64),
                        tx_data.nonce as i64,
                        access_list_blob,
                    ])?;
                }

                for r in receipts {
                    let logs_blob = Self::serialize(&r.logs)?;
                    rc_stmt.execute(rusqlite::params![
                        Self::b256_to_blob(&r.tx_hash),
                        r.tx_index as i64,
                        r.status as i64,
                        r.gas_used as i64,
                        r.cumulative_gas_used as i64,
                        logs_blob,
                        r.contract_address.map(|a| Self::addr_to_blob(&a)),
                    ])?;
                }

                meta_stmt.execute(rusqlite::params![*block_num as i64])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn get_block(&self, block_num: u64) -> anyhow::Result<Option<BlockData>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase FROM blocks WHERE number = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64])?;
        match rows.next()? {
            Some(row) => Ok(Some(BlockData {
                number: row.get::<_, i64>(0)? as u64,
                hash: Self::blob_to_b256(&row.get::<_, Vec<u8>>(1)?),
                timestamp: row.get::<_, i64>(2)? as u64,
                base_fee_per_gas: row.get::<_, Option<i64>>(3)?.map(|v| v as u128),
                gas_limit: row.get::<_, i64>(4)? as u64,
                gas_used: row.get::<_, i64>(5)? as u64,
                coinbase: Self::blob_to_addr(&row.get::<_, Vec<u8>>(6)?),
            })),
            None => Ok(None),
        }
    }

    // ---- Txs ----

    pub fn put_txs(&self, block_num: u64, txs: &[TxData]) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO transactions (hash, block_number, tx_index, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for tx in txs {
            let access_list_blob = if tx.access_list.is_empty() {
                None
            } else {
                Some(Self::serialize(&tx.access_list)?)
            };
            stmt.execute(rusqlite::params![
                Self::b256_to_blob(&tx.hash),
                block_num as i64,
                tx.index as i64,
                Self::addr_to_blob(&tx.from),
                tx.to.map(|a| Self::addr_to_blob(&a)),
                tx.input.to_vec(),
                Self::u256_to_blob(&tx.value),
                tx.gas_limit as i64,
                tx.max_fee_per_gas as i64,
                tx.max_priority_fee_per_gas.map(|v| v as i64),
                tx.nonce as i64,
                access_list_blob,
            ])?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO block_meta (number, txs_fetched) VALUES (?1, 1)",
            rusqlite::params![block_num as i64],
        )?;
        Ok(())
    }

    pub fn get_txs(&self, block_num: u64) -> anyhow::Result<Option<Vec<TxData>>> {
        let conn = self.conn.lock().unwrap();
        if !Self::block_txs_fetched(&conn, block_num) {
            return Ok(None);
        }
        let mut stmt = conn.prepare(
            "SELECT hash, tx_index, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list
             FROM transactions WHERE block_number = ?1 ORDER BY tx_index",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64])?;
        let mut txs = Vec::new();
        while let Some(row) = rows.next()? {
            let access_list = match row.get::<_, Option<Vec<u8>>>(10)? {
                Some(bytes) => Self::deserialize(&bytes).unwrap_or_default(),
                None => Vec::new(),
            };
            txs.push(TxData {
                hash: Self::blob_to_b256(&row.get::<_, Vec<u8>>(0)?),
                index: row.get::<_, i64>(1)? as u64,
                from: Self::blob_to_addr(&row.get::<_, Vec<u8>>(2)?),
                to: row.get::<_, Option<Vec<u8>>>(3)?.map(|b| Self::blob_to_addr(&b)),
                input: row.get::<_, Vec<u8>>(4)?.into(),
                value: Self::blob_to_u256(&row.get::<_, Vec<u8>>(5)?),
                gas_limit: row.get::<_, i64>(6)? as u64,
                max_fee_per_gas: row.get::<_, i64>(7)? as u128,
                max_priority_fee_per_gas: row.get::<_, Option<i64>>(8)?.map(|v| v as u128),
                nonce: row.get::<_, i64>(9)? as u64,
                access_list,
            });
        }
        Ok(Some(txs))
    }

    // ---- Receipts ----

    pub fn put_receipts(&self, _block_num: u64, receipts: &[ReceiptData]) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO receipts (tx_hash, tx_index, status, gas_used, cumulative_gas_used, logs, contract_address)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for r in receipts {
            let logs_blob = Self::serialize(&r.logs)?;
            stmt.execute(rusqlite::params![
                Self::b256_to_blob(&r.tx_hash),
                r.tx_index as i64,
                r.status as i64,
                r.gas_used as i64,
                r.cumulative_gas_used as i64,
                logs_blob,
                r.contract_address.map(|a| Self::addr_to_blob(&a)),
            ])?;
        }
        Ok(())
    }

    pub fn get_receipts(&self, block_num: u64) -> anyhow::Result<Option<Vec<ReceiptData>>> {
        let conn = self.conn.lock().unwrap();
        if !Self::block_txs_fetched(&conn, block_num) {
            return Ok(None);
        }
        let mut stmt = conn.prepare(
            "SELECT r.tx_hash, r.tx_index, r.status, r.gas_used, r.cumulative_gas_used, r.logs, r.contract_address
             FROM receipts r
             INNER JOIN transactions t ON t.hash = r.tx_hash
             WHERE t.block_number = ?1
             ORDER BY r.tx_index",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64])?;
        let mut receipts = Vec::new();
        while let Some(row) = rows.next()? {
            let logs: Vec<crate::data::LogData> = Self::deserialize(&row.get::<_, Vec<u8>>(5)?)?;
            receipts.push(ReceiptData {
                tx_hash: Self::blob_to_b256(&row.get::<_, Vec<u8>>(0)?),
                tx_index: row.get::<_, i64>(1)? as u64,
                status: row.get::<_, i64>(2)? != 0,
                gas_used: row.get::<_, i64>(3)? as u64,
                cumulative_gas_used: row.get::<_, i64>(4)? as u64,
                logs,
                contract_address: row.get::<_, Option<Vec<u8>>>(6)?.map(|b| Self::blob_to_addr(&b)),
            });
        }
        Ok(Some(receipts))
    }

    // ---- Check integrity ----

    pub fn has_block(&self, block_num: u64) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM blocks WHERE number = ?1",
            rusqlite::params![block_num as i64],
            |row| row.get(0),
        )?;
        if count == 0 {
            return Ok(false);
        }
        let meta_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM block_meta WHERE number = ?1 AND txs_fetched = 1",
            rusqlite::params![block_num as i64],
            |row| row.get(0),
        )?;
        Ok(meta_count > 0)
    }

    fn block_txs_fetched(conn: &Connection, block_num: u64) -> bool {
        conn.query_row(
            "SELECT 1 FROM block_meta WHERE number = ?1 AND txs_fetched = 1",
            rusqlite::params![block_num as i64],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn check_integrity(&self, start: u64, end: u64) -> anyhow::Result<Vec<u64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT number FROM blocks
             INNER JOIN block_meta USING(number)
             WHERE number BETWEEN ?1 AND ?2 AND txs_fetched = 1
             ORDER BY number",
        )?;
        let existing: std::collections::HashSet<u64> = stmt
            .query_map(rusqlite::params![start as i64, end as i64], |row| {
                row.get::<_, i64>(0).map(|v| v as u64)
            })?
            .filter_map(|r| r.ok())
            .collect();
        let mut missing = Vec::new();
        for n in start..=end {
            if !existing.contains(&n) {
                missing.push(n);
            }
        }
        Ok(missing)
    }

    pub fn check_integrity_range(&self, blocks: &[u64]) -> anyhow::Result<Vec<u64>> {
        if blocks.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let placeholders: Vec<String> = blocks.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT number FROM blocks
             INNER JOIN block_meta USING(number)
             WHERE number IN ({}) AND txs_fetched = 1",
            placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<rusqlite::types::Value> = blocks
            .iter()
            .map(|n| rusqlite::types::Value::Integer(*n as i64))
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        let existing: std::collections::HashSet<u64> = stmt
            .query_map(param_refs.as_slice(), |row| {
                row.get::<_, i64>(0).map(|v| v as u64)
            })?
            .filter_map(|r| r.ok())
            .collect();
        let missing: Vec<u64> = blocks
            .iter()
            .copied()
            .filter(|n| !existing.contains(n))
            .collect();
        Ok(missing)
    }

    // ---- Account / Slot / Code ----

    pub fn put_account(
        &self,
        block_num: u64,
        address: Address,
        account: &AccountData,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO accounts (block_number, address, nonce, balance, code_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                block_num as i64,
                Self::addr_to_blob(&address),
                account.nonce as i64,
                Self::u256_to_blob(&account.balance),
                Self::b256_to_blob(&account.code_hash),
            ],
        )?;
        Ok(())
    }

    pub fn get_account(
        &self,
        block_num: u64,
        address: Address,
    ) -> anyhow::Result<Option<AccountData>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT nonce, balance, code_hash FROM accounts WHERE block_number = ?1 AND address = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64, Self::addr_to_blob(&address)])?;
        match rows.next()? {
            Some(row) => Ok(Some(AccountData {
                nonce: row.get::<_, i64>(0)? as u64,
                balance: Self::blob_to_u256(&row.get::<_, Vec<u8>>(1)?),
                code_hash: Self::blob_to_b256(&row.get::<_, Vec<u8>>(2)?),
            })),
            None => Ok(None),
        }
    }

    pub fn put_slot(
        &self,
        block_num: u64,
        address: Address,
        slot: U256,
        value: U256,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO storage_slots (block_number, address, slot, value)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                block_num as i64,
                Self::addr_to_blob(&address),
                Self::u256_to_blob(&slot),
                Self::u256_to_blob(&value),
            ],
        )?;
        Ok(())
    }

    pub fn get_slot(
        &self,
        block_num: u64,
        address: Address,
        slot: U256,
    ) -> anyhow::Result<Option<U256>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT value FROM storage_slots WHERE block_number = ?1 AND address = ?2 AND slot = ?3",
        )?;
        let mut rows = stmt.query(rusqlite::params![
            block_num as i64,
            Self::addr_to_blob(&address),
            Self::u256_to_blob(&slot),
        ])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::blob_to_u256(&row.get::<_, Vec<u8>>(0)?))),
            None => Ok(None),
        }
    }

    pub fn put_code(&self, address: Address, code: &Bytes) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO contract_code (address, code) VALUES (?1, ?2)",
            rusqlite::params![Self::addr_to_blob(&address), code.to_vec()],
        )?;
        Ok(())
    }

    pub fn get_code(&self, address: Address) -> anyhow::Result<Option<Bytes>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT code FROM contract_code WHERE address = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![Self::addr_to_blob(&address)])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, Vec<u8>>(0)?.into())),
            None => Ok(None),
        }
    }

    // ---- RunManifest ----

    pub fn put_manifest(&self, manifest: &RunManifest) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO run_manifests (run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                manifest.run_id,
                manifest.chain,
                manifest.start_block as i64,
                manifest.end_block as i64,
                manifest.resolved_at as i64,
                manifest.range_mode,
                manifest.strategies.join(","),
                manifest.flash_loan_provider,
            ],
        )?;
        Ok(())
    }

    pub fn get_manifest(&self, run_id: &str) -> anyhow::Result<Option<RunManifest>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider
             FROM run_manifests WHERE run_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![run_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(RunManifest {
                run_id: row.get(0)?,
                chain: row.get(1)?,
                start_block: row.get::<_, i64>(2)? as u64,
                end_block: row.get::<_, i64>(3)? as u64,
                resolved_at: row.get::<_, i64>(4)? as u64,
                range_mode: row.get(5)?,
                strategies: row.get::<_, String>(6)?.split(',').map(|s| s.to_string()).filter(|s| !s.is_empty()).collect(),
                flash_loan_provider: row.get(7)?,
            })),
            None => Ok(None),
        }
    }

    pub fn list_manifests(&self) -> anyhow::Result<Vec<(String, RunManifest)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider
             FROM run_manifests ORDER BY resolved_at DESC",
        )?;
        let mut rows = stmt.query([])?;
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            let run_id: String = row.get(0)?;
            let manifest = RunManifest {
                run_id: run_id.clone(),
                chain: row.get(1)?,
                start_block: row.get::<_, i64>(2)? as u64,
                end_block: row.get::<_, i64>(3)? as u64,
                resolved_at: row.get::<_, i64>(4)? as u64,
                range_mode: row.get(5)?,
                strategies: row.get::<_, String>(6)?.split(',').map(|s| s.to_string()).filter(|s| !s.is_empty()).collect(),
                flash_loan_provider: row.get(7)?,
            };
            results.push((run_id, manifest));
        }
        Ok(results)
    }

    // ---- Pool Discovery ----

    pub fn put_discovered_pool(&self, pool: &PoolInfo) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let pool_id_blob = pool.pool_id.map(|id| id.to_vec());
        let factory_blob = pool.factory.map(|f| f.to_vec());
        conn.execute(
            "INSERT OR REPLACE INTO pool_info (address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                Self::addr_to_blob(&pool.address),
                Self::addr_to_blob(&pool.token0),
                Self::addr_to_blob(&pool.token1),
                pool.fee as i64,
                pool.dex_type as i64,
                pool.tick_spacing,
                pool.creation_block as i64,
                pool_id_blob,
                factory_blob,
            ],
        )?;
        Ok(())
    }

    pub fn get_discovered_pool(&self, address: &Address) -> anyhow::Result<Option<PoolInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory
             FROM pool_info WHERE address = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![Self::addr_to_blob(address)])?;
        match rows.next()? {
            Some(row) => {
                let pool_id = row.get::<_, Option<Vec<u8>>>(7)?.map(|v| {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&v);
                    arr
                });
                // L6: read factory from column 8 (optional, may be NULL for legacy rows)
                let factory = row.get::<_, Option<Vec<u8>>>(8).ok()
                    .and_then(|v| v.and_then(|bytes| {
                        if bytes.len() == 20 { Some(Address::from_slice(&bytes)) } else { None }
                    }));
                Ok(Some(PoolInfo {
                    address: Self::blob_to_addr(&row.get::<_, Vec<u8>>(0)?),
                    token0: Self::blob_to_addr(&row.get::<_, Vec<u8>>(1)?),
                    token1: Self::blob_to_addr(&row.get::<_, Vec<u8>>(2)?),
                    fee: row.get::<_, i64>(3)? as u32,
                    name: None,
                    dex_type: dex_type_from_i64(row.get::<_, i64>(4)?)?,
                    tick_spacing: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    creation_block: row.get::<_, i64>(6)? as u64,
                    pool_id,
                    factory,
                }))
            }
            None => Ok(None),
        }
    }

    pub fn list_discovered_pools(&self) -> anyhow::Result<Vec<PoolInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory
             FROM pool_info",
        )?;
        let mut rows = stmt.query([])?;
        let mut pools = Vec::new();
        while let Some(row) = rows.next()? {
            let pool_id = row.get::<_, Option<Vec<u8>>>(7)?.map(|v| {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&v);
                arr
            });
            let factory_addr = row.get::<_, Option<Vec<u8>>>(8).ok()
                .and_then(|v| v.and_then(|bytes| {
                    if bytes.len() == 20 { Some(Address::from_slice(&bytes)) } else { None }
                }));
            pools.push(PoolInfo {
                address: Self::blob_to_addr(&row.get::<_, Vec<u8>>(0)?),
                token0: Self::blob_to_addr(&row.get::<_, Vec<u8>>(1)?),
                token1: Self::blob_to_addr(&row.get::<_, Vec<u8>>(2)?),
                fee: row.get::<_, i64>(3)? as u32,
                name: None,
                dex_type: dex_type_from_i64(row.get::<_, i64>(4)?)?,
                tick_spacing: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                creation_block: row.get::<_, i64>(6)? as u64,
                pool_id,
                factory: factory_addr,
            });
        }
        Ok(pools)
    }

    pub fn put_discovery_cursor(&self, factory: &Address, block: u64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO discovery_cursors (factory, block_number) VALUES (?1, ?2)",
            rusqlite::params![Self::addr_to_blob(factory), block as i64],
        )?;
        Ok(())
    }

    pub fn get_discovery_cursor(&self, factory: &Address) -> anyhow::Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT block_number FROM discovery_cursors WHERE factory = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![Self::addr_to_blob(factory)])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, i64>(0)? as u64)),
            None => Ok(None),
        }
    }

    // ---- Pending Transactions (H8) ----

    /// Store pending transactions captured from the mempool.
    /// The `captured_at` timestamp is the Unix epoch second of capture.
    pub fn put_pending_txs(&self, txs: &[TxData], captured_at: u64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        // Use a single block_number for all pending txs (the capture timestamp is a proxy)
        let block_number: i64 = captured_at as i64;
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO pending_txs (block_number, tx_index, hash, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for (i, tx) in txs.iter().enumerate() {
            let access_list_blob = if tx.access_list.is_empty() {
                None
            } else {
                Some(Self::serialize(&tx.access_list)?)
            };
            stmt.execute(rusqlite::params![
                block_number,
                i as i64,
                Self::b256_to_blob(&tx.hash),
                Self::addr_to_blob(&tx.from),
                tx.to.map(|a| Self::addr_to_blob(&a)),
                tx.input.to_vec(),
                Self::u256_to_blob(&tx.value),
                tx.gas_limit as i64,
                tx.max_fee_per_gas as i64,
                tx.max_priority_fee_per_gas.map(|v| v as i64),
                tx.nonce as i64,
                access_list_blob,
                captured_at as i64,
            ])?;
        }
        Ok(())
    }

    /// Count pending transactions captured at the given timestamp.
    pub fn count_pending_txs(&self, captured_at: u64) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_txs WHERE captured_at = ?1",
            rusqlite::params![captured_at as i64],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Count all pending transactions in the cache.
    pub fn total_pending_txs(&self) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_txs",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Flush pending writes (WAL checkpoint).
    pub fn flush(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

fn dex_type_from_i64(v: i64) -> anyhow::Result<crate::pool::dex_type::DexType> {
    match v {
        0 => Ok(crate::pool::dex_type::DexType::UniswapV2),
        1 => Ok(crate::pool::dex_type::DexType::UniswapV3),
        2 => Ok(crate::pool::dex_type::DexType::Curve),
        3 => Ok(crate::pool::dex_type::DexType::Balancer),
        n => anyhow::bail!("invalid dex_type discriminant: {}", n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256, B256, U256};
    use crate::data::{AccountData, BlockData, ReceiptData, TxData};
    use crate::pool::dex_type::DexType;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DB_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_store() -> SqliteStore {
        let id = DB_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("mev_ut_sqlite_{}.sqlite", id));
        let _ = std::fs::remove_file(&path);
        SqliteStore::open(&path, 137).unwrap()
    }

    #[test]
    fn test_put_get_block() {
        let store = temp_store();
        let block = BlockData {
            number: 42,
            hash: b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            timestamp: 1000,
            base_fee_per_gas: Some(50_000_000_000),
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            coinbase: address!("dead000000000000000000000000000000000000"),
        };
        store.put_block(42, &block).unwrap();
        let fetched = store.get_block(42).unwrap().unwrap();
        assert_eq!(fetched.number, 42);
        assert_eq!(fetched.hash, block.hash);
        assert_eq!(fetched.timestamp, 1000);
    }

    #[test]
    fn test_put_get_txs() {
        let store = temp_store();
        let txs = vec![TxData {
            hash: b256!("0000000000000000000000000000000000000000000000000000000000000002"),
            index: 0,
            from: address!("aa00000000000000000000000000000000000000"),
            to: Some(address!("bb00000000000000000000000000000000000000")),
            input: vec![0x12, 0x34].into(),
            value: U256::from(1000u64),
            gas_limit: 100_000,
            max_fee_per_gas: 100_000_000_000,
            max_priority_fee_per_gas: Some(1_000_000_000),
            nonce: 5,
            access_list: vec![],
        }];
        store.put_block(42, &BlockData {
            number: 42, hash: B256::ZERO, timestamp: 0,
            base_fee_per_gas: None, gas_limit: 0, gas_used: 0, coinbase: Address::ZERO,
        }).unwrap();
        store.put_txs(42, &txs).unwrap();
        let fetched = store.get_txs(42).unwrap().unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].nonce, 5);
    }

    #[test]
    fn test_put_get_receipts() {
        let store = temp_store();
        store.put_block(42, &BlockData {
            number: 42, hash: B256::ZERO, timestamp: 0,
            base_fee_per_gas: None, gas_limit: 0, gas_used: 0, coinbase: Address::ZERO,
        }).unwrap();
        let tx_hash = b256!("0000000000000000000000000000000000000000000000000000000000000003");
        store.put_txs(42, &[TxData {
            hash: tx_hash, index: 0,
            from: Address::ZERO, to: None, input: Bytes::default(),
            value: U256::ZERO, gas_limit: 0, max_fee_per_gas: 0,
            max_priority_fee_per_gas: None, nonce: 0, access_list: vec![],
        }]).unwrap();
        let receipts = vec![ReceiptData {
            tx_hash,
            tx_index: 0,
            status: true,
            gas_used: 50_000,
            cumulative_gas_used: 50_000,
            logs: vec![],
            contract_address: None,
        }];
        store.put_receipts(42, &receipts).unwrap();
        let fetched = store.get_receipts(42).unwrap().unwrap();
        assert_eq!(fetched.len(), 1);
        assert!(fetched[0].status);
        assert_eq!(fetched[0].gas_used, 50_000);
    }

    #[test]
    fn test_has_block_and_check_integrity() {
        let store = temp_store();
        let block = BlockData {
            number: 1, hash: B256::ZERO, timestamp: 0,
            base_fee_per_gas: None, gas_limit: 0, gas_used: 0, coinbase: Address::ZERO,
        };
        store.put_block(1, &block).unwrap();
        assert!(!store.has_block(1).unwrap());
        store.put_txs(1, &[]).unwrap();
        assert!(store.has_block(1).unwrap());
        let missing = store.check_integrity(1, 3).unwrap();
        assert_eq!(missing, vec![2, 3]);
    }

    #[test]
    fn test_get_nonexistent() {
        let store = temp_store();
        assert!(store.get_block(999).unwrap().is_none());
        assert!(store.get_txs(999).unwrap().is_none());
        assert!(store.get_receipts(999).unwrap().is_none());
    }

    #[test]
    fn test_account_roundtrip() {
        let store = temp_store();
        let addr = address!("abcdef0000000000000000000000000000000001");
        let acc = AccountData {
            nonce: 10,
            balance: U256::from(1_000_000_000u64),
            code_hash: b256!("0000000000000000000000000000000000000000000000000000000000000004"),
        };
        store.put_account(42, addr, &acc).unwrap();
        let fetched = store.get_account(42, addr).unwrap().unwrap();
        assert_eq!(fetched.nonce, 10);
        assert_eq!(fetched.balance, U256::from(1_000_000_000u64));
    }

    #[test]
    fn test_slot_roundtrip() {
        let store = temp_store();
        let addr = address!("abcdef0000000000000000000000000000000002");
        store.put_slot(42, addr, U256::from(7u64), U256::from(42u64)).unwrap();
        let fetched = store.get_slot(42, addr, U256::from(7u64)).unwrap().unwrap();
        assert_eq!(fetched, U256::from(42u64));
    }

    #[test]
    fn test_code_roundtrip() {
        let store = temp_store();
        let addr = address!("abcdef0000000000000000000000000000000003");
        let code = Bytes::from(vec![0x60, 0x00, 0x52]);
        store.put_code(addr, &code).unwrap();
        let fetched = store.get_code(addr).unwrap().unwrap();
        assert_eq!(fetched, code);
    }

    #[test]
    fn test_manifest_roundtrip() {
        let store = temp_store();
        let manifest = RunManifest {
            run_id: "test-run-1".into(),
            chain: "polygon".into(),
            start_block: 1,
            end_block: 100,
            resolved_at: 1000,
            range_mode: "blocks".into(),
            strategies: vec!["two_hop_arp".into()],
            flash_loan_provider: "auto".into(),
        };
        store.put_manifest(&manifest).unwrap();
        let fetched = store.get_manifest("test-run-1").unwrap().unwrap();
        assert_eq!(fetched.run_id, "test-run-1");
        assert_eq!(fetched.start_block, 1);
        assert_eq!(fetched.end_block, 100);
    }

    #[test]
    fn test_discovered_pool_roundtrip() {
        let store = temp_store();
        let pool = PoolInfo {
            address: address!("cafe000000000000000000000000000000000001"),
            token0: address!("aaaa0000000000000000000000000000000000aa"),
            token1: address!("bbbb0000000000000000000000000000000000bb"),
            fee: 3000,
            name: None,
            dex_type: DexType::UniswapV2,
            tick_spacing: None,
            creation_block: 0,
            pool_id: None,
            factory: None,
        };
        store.put_discovered_pool(&pool).unwrap();
        let fetched = store.get_discovered_pool(&pool.address).unwrap().unwrap();
        assert_eq!(fetched.address, pool.address);
        assert_eq!(fetched.dex_type, DexType::UniswapV2);
        let all = store.list_discovered_pools().unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_discovery_cursor_roundtrip() {
        let store = temp_store();
        let factory = Address::from_slice(&[0xfa; 20]);
        assert!(store.get_discovery_cursor(&factory).unwrap().is_none());
        store.put_discovery_cursor(&factory, 42_000_000).unwrap();
        let cursor = store.get_discovery_cursor(&factory).unwrap().unwrap();
        assert_eq!(cursor, 42_000_000);
    }

    #[test]
    fn test_discovery_namespaced_by_chain() {
        let path_a = std::env::temp_dir().join("disc_test_a.sqlite");
        let path_b = std::env::temp_dir().join("disc_test_b.sqlite");
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
        let store_a = SqliteStore::open(&path_a, 137).unwrap();
        let store_b = SqliteStore::open(&path_b, 31337).unwrap();
        let pool = PoolInfo {
            address: address!("cafe000000000000000000000000000000000002"),
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 0,
            name: None,
            dex_type: DexType::UniswapV2,
            tick_spacing: None,
            creation_block: 0,
            pool_id: None,
            factory: None,
        };
        store_a.put_discovered_pool(&pool).unwrap();
        assert_eq!(store_b.list_discovered_pools().unwrap().len(), 0);
        assert_eq!(store_a.list_discovered_pools().unwrap().len(), 1);
    }

    #[test]
    fn test_manifest_list() {
        let store = temp_store();
        let m1 = RunManifest {
            run_id: "run-1".into(), chain: "eth".into(),
            start_block: 1, end_block: 10, resolved_at: 100,
            range_mode: "blocks".into(), strategies: vec!["arb".into()],
            flash_loan_provider: "balancer".into(),
        };
        let m2 = RunManifest {
            run_id: "run-2".into(), chain: "polygon".into(),
            start_block: 5, end_block: 15, resolved_at: 200,
            range_mode: "range".into(), strategies: vec!["jit".into()],
            flash_loan_provider: "aave".into(),
        };
        store.put_manifest(&m1).unwrap();
        store.put_manifest(&m2).unwrap();
        let list = store.list_manifests().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].0, "run-2");
        assert_eq!(list[1].0, "run-1");
    }
}
