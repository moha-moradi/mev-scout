//! Persistent block/state cache backed by SQLite.
//!
//! SqliteStore is the local-first persistence layer for the backtest engine.
//! All fetched block data (headers, transactions, receipts, account state,
//! storage slots, contract code, pool state) is stored in a single SQLite
//! database file for portability and offline querying.

use std::path::Path;
use std::sync::{Arc, Mutex};

use alloy::primitives::{b256, Address, Bytes, B256, U256};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::data::types::{AccountData, BlockData, ReceiptData, TxData};
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
    #[allow(dead_code)]
    chain_id: u64,
}

impl SqliteStore {
    /// Acquire the SQLite connection mutex guard.
    /// Panics with a clear message if the mutex is poisoned (process state corrupted).
    fn conn(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn.lock().expect("SQLite connection mutex poisoned")
    }
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
        let conn = self.conn();
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
            ",
        )?;

        // Migration: add tx_type column if missing (EIP-7702 support)
        let has_tx_type: bool = conn
            .prepare("SELECT tx_type FROM transactions LIMIT 0")
            .is_ok();
        if !has_tx_type {
            conn.execute_batch("ALTER TABLE transactions ADD COLUMN tx_type INTEGER NOT NULL DEFAULT 0")?;
        }

        // Migration: add is_stable column to pool_info if missing (Solidly/Camelot stable pools)
        let has_is_stable: bool = conn
            .prepare("SELECT is_stable FROM pool_info LIMIT 0")
            .is_ok();
        if !has_is_stable {
            conn.execute_batch("ALTER TABLE pool_info ADD COLUMN is_stable INTEGER")?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS receipts (
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
            ",
        )?;
        // L6: migration -- add factory column to pool_info if missing (backward compat)
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN factory BLOB;");
        // Phase 10: propagate full token list for Curve/Balancer
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN underlying_tokens TEXT;");
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN balancer_pool_type INTEGER;");
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN hook_address BLOB;");
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN bin_step INTEGER;");
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN maturity_timestamp INTEGER;");
        // Phase 10.2: human-readable dex name and token symbols
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN dex_name TEXT;");
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN token0_symbol TEXT;");
        let _ = conn.execute_batch("ALTER TABLE pool_info ADD COLUMN token1_symbol TEXT;");
        // Phase 3: add signature columns to transactions (idempotent)
        let _ = conn.execute_batch("ALTER TABLE transactions ADD COLUMN sig_hash BLOB;");
        let _ = conn.execute_batch("ALTER TABLE transactions ADD COLUMN sig_name TEXT;");
        // Phase 3/competition: competitor profiles and extraction tables (removed)
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

    /// Extract ERC20 Transfer amount from a log's data bytes.
    /// Returns None if the log is not an ERC20 Transfer event or if data is too short.
    pub fn decode_erc20_amount(log: &crate::data::LogData) -> Option<U256> {
        if log.topics.first() == Some(&TRANSFER_EVENT_TOPIC) && log.data.len() >= 32 {
            Some(U256::from_be_slice(&log.data[log.data.len() - 32..]))
        } else {
            None
        }
    }

    // ---- Block ----

    pub fn put_block(&self, block_num: u64, block: &BlockData) -> anyhow::Result<()> {
        let conn = self.conn();
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
        tx_sigs: Option<&[([u8; 4], Option<String>)]>,
        event_sigs: Option<&[Vec<Option<String>>]>,
    ) -> anyhow::Result<()> {
        let tx_sigs_batch = tx_sigs.map(|s| vec![s.to_vec()]);
        let event_sigs_batch = event_sigs.map(|es| vec![es.to_vec()]);
        self.put_block_data_batch(
            &[(block_num, block.clone(), txs.to_vec(), receipts.to_vec())],
            tx_sigs_batch.as_deref(),
            event_sigs_batch.as_deref(),
        )
    }

    pub fn put_block_data_batch(
        &self,
        batch: &[(u64, BlockData, Vec<TxData>, Vec<ReceiptData>)],
        tx_sigs_batch: Option<&[Vec<([u8; 4], Option<String>)>]>,
        event_sigs_batch: Option<&[Vec<Vec<Option<String>>>]>,
    ) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;

        {
            let mut block_stmt = tx.prepare(
                "INSERT OR REPLACE INTO blocks (number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut tx_stmt = tx.prepare(
                "INSERT OR REPLACE INTO transactions (hash, block_number, tx_index, tx_type, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list, sig_hash, sig_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            let mut rc_stmt = tx.prepare(
                "INSERT OR REPLACE INTO receipts (tx_hash, tx_index, status, gas_used, cumulative_gas_used, logs, contract_address)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut log_stmt = tx.prepare(
                "INSERT OR REPLACE INTO logs (block_number, tx_index, log_index, address, topic0, topic1, topic2, topic3, data, erc20_amount, event_sig)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            let mut meta_stmt = tx.prepare(
                "INSERT OR REPLACE INTO block_meta (number, txs_fetched) VALUES (?1, 1)",
            )?;

            for (block_idx, (block_num, block, txs, receipts)) in batch.iter().enumerate() {
                block_stmt.execute(rusqlite::params![
                    *block_num as i64,
                    Self::b256_to_blob(&block.hash),
                    block.timestamp as i64,
                    block.base_fee_per_gas.map(|v| v as i64),
                    block.gas_limit as i64,
                    block.gas_used as i64,
                    Self::addr_to_blob(&block.coinbase),
                ])?;

                let block_tx_sigs = tx_sigs_batch.and_then(|b| b.get(block_idx));
                let block_event_sigs = event_sigs_batch.and_then(|b| b.get(block_idx));

                for (tx_i, tx_data) in txs.iter().enumerate() {
                    let access_list_blob = if tx_data.access_list.is_empty() {
                        None
                    } else {
                        Some(Self::serialize(&tx_data.access_list)?)
                    };
                    let (sig_hash, sig_name) = block_tx_sigs.and_then(|s| s.get(tx_i))
                        .map(|(ref sel, ref name)| (Some(sel.to_vec()), name.clone()))
                        .unwrap_or((None, None));
                    tx_stmt.execute(rusqlite::params![
                        Self::b256_to_blob(&tx_data.hash),
                        *block_num as i64,
                        tx_data.index as i64,
                        tx_data.tx_type as i64,
                        Self::addr_to_blob(&tx_data.from),
                        tx_data.to.map(|a| Self::addr_to_blob(&a)),
                        tx_data.input.to_vec(),
                        Self::u256_to_blob(&tx_data.value),
                        tx_data.gas_limit as i64,
                        tx_data.max_fee_per_gas as i64,
                        tx_data.max_priority_fee_per_gas.map(|v| v as i64),
                        tx_data.nonce as i64,
                        access_list_blob,
                        sig_hash,
                        sig_name,
                    ])?;
                }

                for (ri, r) in receipts.iter().enumerate() {
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
                    for (log_index, log_entry) in r.logs.iter().enumerate() {
                        let amount = Self::decode_erc20_amount(log_entry);
                        let topic0 = log_entry.topics.get(0).map(|t| t.as_slice().to_vec());
                        let topic1 = log_entry.topics.get(1).map(|t| t.as_slice().to_vec());
                        let topic2 = log_entry.topics.get(2).map(|t| t.as_slice().to_vec());
                        let topic3 = log_entry.topics.get(3).map(|t| t.as_slice().to_vec());
                        let event_sig = block_event_sigs
                            .and_then(|es| es.get(ri))
                            .and_then(|tx_es| tx_es.get(log_index))
                            .and_then(|s| s.clone());
                        log_stmt.execute(rusqlite::params![
                            *block_num as i64,
                            r.tx_index as i64,
                            log_index as i64,
                            Self::addr_to_blob(&log_entry.address),
                            topic0,
                            topic1,
                            topic2,
                            topic3,
                            log_entry.data.to_vec(),
                            amount.map(|a| a.to_be_bytes::<32>().to_vec()),
                            event_sig,
                        ])?;
                    }
                }

                meta_stmt.execute(rusqlite::params![*block_num as i64])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn get_block(&self, block_num: u64) -> anyhow::Result<Option<BlockData>> {
        let conn = self.conn();
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
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO transactions (hash, block_number, tx_index, tx_type, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                tx.tx_type as i64,
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
        let conn = self.conn();
        if !Self::block_txs_fetched(&conn, block_num) {
            return Ok(None);
        }
        let mut stmt = conn.prepare(
            "SELECT hash, tx_index, tx_type, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list
             FROM transactions WHERE block_number = ?1 ORDER BY tx_index",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64])?;
        let mut txs = Vec::new();
        while let Some(row) = rows.next()? {
            let access_list = match row.get::<_, Option<Vec<u8>>>(11)? {
                Some(bytes) => Self::deserialize(&bytes).unwrap_or_default(),
                None => Vec::new(),
            };
            txs.push(TxData {
                hash: Self::blob_to_b256(&row.get::<_, Vec<u8>>(0)?),
                index: row.get::<_, i64>(1)? as u64,
                tx_type: row.get::<_, i64>(2)? as u8,
                from: Self::blob_to_addr(&row.get::<_, Vec<u8>>(3)?),
                to: row.get::<_, Option<Vec<u8>>>(4)?.map(|b| Self::blob_to_addr(&b)),
                input: row.get::<_, Vec<u8>>(5)?.into(),
                value: Self::blob_to_u256(&row.get::<_, Vec<u8>>(6)?),
                gas_limit: row.get::<_, i64>(7)? as u64,
                max_fee_per_gas: row.get::<_, i64>(8)? as u128,
                max_priority_fee_per_gas: row.get::<_, Option<i64>>(9)?.map(|v| v as u128),
                nonce: row.get::<_, i64>(10)? as u64,
                access_list,
            });
        }
        Ok(Some(txs))
    }

    // ---- Receipts ----

    pub fn put_receipts(&self, _block_num: u64, receipts: &[ReceiptData]) -> anyhow::Result<()> {
        let conn = self.conn();
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
        let conn = self.conn();
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

    // ---- Normalized Logs ----

    /// Return all normalized logs for a given block.
    pub fn get_logs_for_block(&self, block_num: u64) -> anyhow::Result<Vec<crate::data::NormalizedLog>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT block_number, tx_index, log_index, address, topic0, topic1, topic2, topic3, data, erc20_amount, event_sig
             FROM logs WHERE block_number = ?1 ORDER BY log_index",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64])?;
        let mut logs = Vec::new();
        while let Some(row) = rows.next()? {
            logs.push(crate::data::NormalizedLog {
                block_number: row.get::<_, i64>(0)? as u64,
                tx_index: row.get::<_, i64>(1)? as u64,
                log_index: row.get::<_, i64>(2)? as u64,
                address: Self::blob_to_addr(&row.get::<_, Vec<u8>>(3)?),
                topic0: row.get::<_, Option<Vec<u8>>>(4)?.map(|b| Self::blob_to_b256(&b)),
                topic1: row.get::<_, Option<Vec<u8>>>(5)?.map(|b| Self::blob_to_b256(&b)),
                topic2: row.get::<_, Option<Vec<u8>>>(6)?.map(|b| Self::blob_to_b256(&b)),
                topic3: row.get::<_, Option<Vec<u8>>>(7)?.map(|b| Self::blob_to_b256(&b)),
                data: row.get::<_, Vec<u8>>(8)?.into(),
                erc20_amount: row.get::<_, Option<Vec<u8>>>(9)?.map(|b| Self::blob_to_u256(&b)),
                event_sig: row.get::<_, Option<String>>(10)?,
            });
        }
        Ok(logs)
    }

    /// Return all normalized logs for a specific transaction.
    pub fn get_logs_for_tx(&self, tx_hash: &B256) -> anyhow::Result<Vec<crate::data::NormalizedLog>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT l.block_number, l.tx_index, l.log_index, l.address, l.topic0, l.topic1, l.topic2, l.topic3, l.data, l.erc20_amount, l.event_sig
             FROM logs l
             INNER JOIN transactions t ON t.block_number = l.block_number AND t.tx_index = l.tx_index
             WHERE t.hash = ?1
             ORDER BY l.log_index",
        )?;
        let mut rows = stmt.query(rusqlite::params![Self::b256_to_blob(tx_hash)])?;
        let mut logs = Vec::new();
        while let Some(row) = rows.next()? {
            logs.push(crate::data::NormalizedLog {
                block_number: row.get::<_, i64>(0)? as u64,
                tx_index: row.get::<_, i64>(1)? as u64,
                log_index: row.get::<_, i64>(2)? as u64,
                address: Self::blob_to_addr(&row.get::<_, Vec<u8>>(3)?),
                topic0: row.get::<_, Option<Vec<u8>>>(4)?.map(|b| Self::blob_to_b256(&b)),
                topic1: row.get::<_, Option<Vec<u8>>>(5)?.map(|b| Self::blob_to_b256(&b)),
                topic2: row.get::<_, Option<Vec<u8>>>(6)?.map(|b| Self::blob_to_b256(&b)),
                topic3: row.get::<_, Option<Vec<u8>>>(7)?.map(|b| Self::blob_to_b256(&b)),
                data: row.get::<_, Vec<u8>>(8)?.into(),
                erc20_amount: row.get::<_, Option<Vec<u8>>>(9)?.map(|b| Self::blob_to_u256(&b)),
                event_sig: row.get::<_, Option<String>>(10)?,
            });
        }
        Ok(logs)
    }

    // ---- Batch missing-block query (Phase 1) ----

    /// Single query: return all cached block numbers in [start, end].
    pub fn get_cached_blocks_in_range(&self, start: u64, end: u64) -> anyhow::Result<Vec<u64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT number FROM blocks
             INNER JOIN block_meta USING(number)
             WHERE number BETWEEN ?1 AND ?2 AND txs_fetched = 1
             ORDER BY number",
        )?;
        let blocks = stmt
            .query_map(rusqlite::params![start as i64, end as i64], |row| {
                row.get::<_, i64>(0).map(|v| v as u64)
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(blocks)
    }

    /// Returns blocks in [start, end] that are NOT in the cache.
    pub fn missing_blocks_in_range(&self, start: u64, end: u64) -> anyhow::Result<Vec<u64>> {
        let cached = self.get_cached_blocks_in_range(start, end)?;
        let set: std::collections::HashSet<u64> = cached.into_iter().collect();
        Ok((start..=end).filter(|n| !set.contains(n)).collect())
    }

    /// Group sorted, deduped block numbers into contiguous (start, end) inclusive ranges.
    pub fn contiguous_ranges(blocks: &[u64]) -> Vec<(u64, u64)> {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for &block in blocks {
            match ranges.last_mut() {
                Some(last) if block == last.1 + 1 => last.1 = block,
                _ => ranges.push((block, block)),
            }
        }
        ranges
    }

    // ---- Check integrity ----

    pub fn has_block(&self, block_num: u64) -> anyhow::Result<bool> {
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO contract_code (address, code) VALUES (?1, ?2)",
            rusqlite::params![Self::addr_to_blob(&address), code.to_vec()],
        )?;
        Ok(())
    }

    pub fn get_code(&self, address: Address) -> anyhow::Result<Option<Bytes>> {
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
        let pool_id_blob = pool.pool_id.map(|id| id.to_vec());
        let factory_blob = pool.factory.map(|f| f.to_vec());
        let is_stable_int: Option<i64> = pool.is_stable.map(|b| b as i64);
        let underlying_json: Option<String> = pool.underlying_tokens.as_ref().map(|tokens| {
            let hexes: Vec<String> = tokens.iter().map(|a| format!("{a}")).collect();
            serde_json::to_string(&hexes).unwrap_or_default()
        });
        let balancer_type_int: Option<i64> = pool.balancer_pool_type.map(|v| v as i64);
        let hook_blob = pool.hook_address.map(|f| f.to_vec());
        let bin_step_int: Option<i64> = pool.bin_step.map(|v| v as i64);
        let maturity_ts_int: Option<i64> = pool.maturity_timestamp.map(|v| v as i64);
        let dex_name = pool.dex_name.as_deref();
        let token0_symbol = pool.token0_symbol.as_deref();
        let token1_symbol = pool.token1_symbol.as_deref();
        conn.execute(
            "INSERT OR REPLACE INTO pool_info (address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
                is_stable_int,
                underlying_json,
                balancer_type_int,
                hook_blob,
                bin_step_int,
                maturity_ts_int,
                dex_name,
                token0_symbol,
                token1_symbol,
            ],
        )?;
        Ok(())
    }

    pub fn get_discovered_pool(&self, address: &Address) -> anyhow::Result<Option<PoolInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol
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
                let is_stable = row.get::<_, Option<i64>>(9).ok().flatten().map(|v| v != 0);
                let underlying_tokens: Option<Vec<Address>> = row.get::<_, Option<String>>(10).ok()
                    .flatten()
                    .and_then(|json_str| {
                        let hexes: Vec<String> = serde_json::from_str(&json_str).ok()?;
                        let addrs: Vec<Address> = hexes.iter()
                            .filter_map(|h| h.strip_prefix("0x")
                                .or(Some(h.as_str())))
                            .filter_map(|h| hex::decode(h).ok())
                            .filter(|b| b.len() == 20)
                            .map(|b| Address::from_slice(&b))
                            .collect();
                        if addrs.is_empty() { None } else { Some(addrs) }
                    });
                let balancer_pool_type = row.get::<_, Option<i64>>(11).ok().flatten().map(|v| v as u8);
                let hook_address = row.get::<_, Option<Vec<u8>>>(12).ok()
                    .and_then(|v| v.and_then(|bytes| {
                        if bytes.len() == 20 { Some(Address::from_slice(&bytes)) } else { None }
                    }));
                let bin_step = row.get::<_, Option<i64>>(13).ok().flatten().map(|v| v as u32);
                let maturity_timestamp = row.get::<_, Option<i64>>(14).ok().flatten().map(|v| v as u64);
                let dex_name = row.get::<_, Option<String>>(15).ok().flatten();
                let token0_symbol = row.get::<_, Option<String>>(16).ok().flatten();
                let token1_symbol = row.get::<_, Option<String>>(17).ok().flatten();
                let token0 = Self::blob_to_addr(&row.get::<_, Vec<u8>>(1)?);
                let token1 = Self::blob_to_addr(&row.get::<_, Vec<u8>>(2)?);
                let is_fot = Some(crate::pool::state::pool_types::is_fee_on_transfer_token(&token0)
                    || crate::pool::state::pool_types::is_fee_on_transfer_token(&token1));
                let is_rebase = Some(crate::pool::state::pool_types::is_rebase_token(&token0)
                    || crate::pool::state::pool_types::is_rebase_token(&token1));
                Ok(Some(PoolInfo {
                    address: Self::blob_to_addr(&row.get::<_, Vec<u8>>(0)?),
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
                    dex_name,
                    token0_symbol,
                    token1_symbol,
                }))
            }
            None => Ok(None),
        }
    }

    pub fn list_discovered_pools(&self) -> anyhow::Result<Vec<PoolInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol
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
                    if addrs.is_empty() { None } else { Some(addrs) }
                });
            let balancer_pool_type = row.get::<_, Option<i64>>(11).ok().flatten().map(|v| v as u8);
            let hook_address = row.get::<_, Option<Vec<u8>>>(12).ok()
                .and_then(|v| v.and_then(|bytes| {
                    if bytes.len() == 20 { Some(Address::from_slice(&bytes)) } else { None }
                }));
            let bin_step = row.get::<_, Option<i64>>(13).ok().flatten().map(|v| v as u32);
            let maturity_timestamp = row.get::<_, Option<i64>>(14).ok().flatten().map(|v| v as u64);
            let dex_name = row.get::<_, Option<String>>(15).ok().flatten();
            let token0_symbol = row.get::<_, Option<String>>(16).ok().flatten();
            let token1_symbol = row.get::<_, Option<String>>(17).ok().flatten();
            let token0 = Self::blob_to_addr(&row.get::<_, Vec<u8>>(1)?);
            let token1 = Self::blob_to_addr(&row.get::<_, Vec<u8>>(2)?);
            let is_fot = Some(crate::pool::state::pool_types::is_fee_on_transfer_token(&token0)
                || crate::pool::state::pool_types::is_fee_on_transfer_token(&token1));
            let is_rebase = Some(crate::pool::state::pool_types::is_rebase_token(&token0)
                || crate::pool::state::pool_types::is_rebase_token(&token1));
            pools.push(PoolInfo {
                address: Self::blob_to_addr(&row.get::<_, Vec<u8>>(0)?),
                token0,
                token1,
                fee: row.get::<_, i64>(3)? as u32,
                name: None,
                dex_type: dex_type_from_i64(row.get::<_, i64>(4)?)?,
                tick_spacing: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                creation_block: row.get::<_, i64>(6)? as u64,
                pool_id,
                factory: factory_addr,
                is_stable,
                is_fot,
                is_rebase,
                underlying_tokens,
                balancer_pool_type,
                hook_address,
                bin_step,
                maturity_timestamp,
                dex_name,
                token0_symbol,
                token1_symbol,
            });
        }
        Ok(pools)
    }

    /// Returns the maximum `creation_block` across all cached discovered pools,
    /// or `None` if no pools are cached. Used by `--incremental` mode.
    pub fn max_creation_block(&self) -> anyhow::Result<Option<u64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT MAX(creation_block) FROM pool_info",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => {
                let val: Option<i64> = row.get(0)?;
                Ok(val.map(|v| v as u64))
            }
            None => Ok(None),
        }
    }

    /// Returns the number of cached discovered pools for the given chain.
    pub fn count_discovered_pools(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pool_info", [], |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn put_discovery_cursor(&self, factory: &Address, block: u64) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO discovery_cursors (factory, block_number) VALUES (?1, ?2)",
            rusqlite::params![Self::addr_to_blob(factory), block as i64],
        )?;
        Ok(())
    }

    pub fn get_discovery_cursor(&self, factory: &Address) -> anyhow::Result<Option<u64>> {
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_txs WHERE captured_at = ?1",
            rusqlite::params![captured_at as i64],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Count all pending transactions in the cache.
    pub fn total_pending_txs(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_txs",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    // ---- Competitor Profiles ---- (removed)

    /// Flush pending writes (WAL checkpoint).
    pub fn flush(&self) -> anyhow::Result<()> {
        let conn = self.conn();
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
        4 => Ok(crate::pool::dex_type::DexType::Dodo),
        5 => Ok(crate::pool::dex_type::DexType::Clipper),
        6 => Ok(crate::pool::dex_type::DexType::Solidly),
        7 => Ok(crate::pool::dex_type::DexType::Camelot),
        8 => Ok(crate::pool::dex_type::DexType::UniswapV4),
        9 => Ok(crate::pool::dex_type::DexType::TraderJoeLB),
        10 => Ok(crate::pool::dex_type::DexType::Pendle),
        n => anyhow::bail!("invalid dex_type discriminant: {}", n),
    }
}



