use alloy::primitives::{B256, U256};
use rusqlite::Connection;

use crate::data::types::{BlockData, ReceiptData, TxData};

impl super::SqliteStore {
    pub fn put_block(&self, block_num: u64, block: &BlockData) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO blocks (number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase, difficulty, mix_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                block_num as i64,
                super::SqliteStore::b256_to_blob(&block.hash),
                block.timestamp as i64,
                block.base_fee_per_gas.map(|v| v as i64),
                block.gas_limit as i64,
                block.gas_used as i64,
                super::SqliteStore::addr_to_blob(&block.coinbase),
                block.difficulty.to_be_bytes::<32>().to_vec(),
                super::SqliteStore::b256_to_blob(&block.mix_hash),
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
                "INSERT OR REPLACE INTO blocks (number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase, difficulty, mix_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let mut tx_stmt = tx.prepare(
                "INSERT OR REPLACE INTO transactions (hash, block_number, tx_index, tx_type, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list, authorization_list, sig_hash, sig_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                    super::SqliteStore::b256_to_blob(&block.hash),
                    block.timestamp as i64,
                    block.base_fee_per_gas.map(|v| v as i64),
                    block.gas_limit as i64,
                    block.gas_used as i64,
                    super::SqliteStore::addr_to_blob(&block.coinbase),
                    block.difficulty.to_be_bytes::<32>().to_vec(),
                    super::SqliteStore::b256_to_blob(&block.mix_hash),
                ])?;

                let block_tx_sigs = tx_sigs_batch.and_then(|b| b.get(block_idx));
                let block_event_sigs = event_sigs_batch.and_then(|b| b.get(block_idx));

                for (tx_i, tx_data) in txs.iter().enumerate() {
                    let access_list_blob = super::SqliteStore::serialize_access_list(&tx_data.access_list)?;
                    let authorization_list_blob = super::SqliteStore::serialize_access_list(&tx_data.authorization_list)?;
                    let (sig_hash, sig_name) = block_tx_sigs.and_then(|s| s.get(tx_i))
                        .map(|(ref sel, ref name)| (Some(sel.to_vec()), name.clone()))
                        .unwrap_or((None, None));
                    tx_stmt.execute(rusqlite::params![
                        super::SqliteStore::b256_to_blob(&tx_data.hash),
                        *block_num as i64,
                        tx_data.index as i64,
                        tx_data.tx_type as i64,
                        super::SqliteStore::addr_to_blob(&tx_data.from),
                        tx_data.to.map(|a| super::SqliteStore::addr_to_blob(&a)),
                        tx_data.input.to_vec(),
                        super::SqliteStore::u256_to_blob(&tx_data.value),
                        tx_data.gas_limit as i64,
                        tx_data.max_fee_per_gas as i64,
                        tx_data.max_priority_fee_per_gas.map(|v| v as i64),
                        tx_data.nonce as i64,
                        access_list_blob,
                        authorization_list_blob,
                        sig_hash,
                        sig_name,
                    ])?;
                }

                for (ri, r) in receipts.iter().enumerate() {
                    let logs_blob = super::SqliteStore::serialize(&r.logs)?;
                    rc_stmt.execute(rusqlite::params![
                        super::SqliteStore::b256_to_blob(&r.tx_hash),
                        r.tx_index as i64,
                        r.status as i64,
                        r.gas_used as i64,
                        r.cumulative_gas_used as i64,
                        logs_blob,
                        r.contract_address.map(|a| super::SqliteStore::addr_to_blob(&a)),
                    ])?;
                    for (log_index, log_entry) in r.logs.iter().enumerate() {
                        let amount = super::SqliteStore::decode_erc20_amount(log_entry);
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
                            super::SqliteStore::addr_to_blob(&log_entry.address),
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
            "SELECT number, hash, timestamp, base_fee_per_gas, gas_limit, gas_used, coinbase, difficulty, mix_hash FROM blocks WHERE number = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64])?;
        match rows.next()? {
            Some(row) => {
                let difficulty_bytes: Vec<u8> = row.get(7)?;
                let difficulty = if difficulty_bytes.len() == 32 {
                    U256::from_be_slice(&difficulty_bytes)
                } else {
                    U256::ZERO
                };
                Ok(Some(BlockData {
                    number: row.get::<_, i64>(0)? as u64,
                    hash: super::SqliteStore::blob_to_b256(&row.get::<_, Vec<u8>>(1)?),
                    timestamp: row.get::<_, i64>(2)? as u64,
                    base_fee_per_gas: row.get::<_, Option<i64>>(3)?.map(|v| v as u128),
                    gas_limit: row.get::<_, i64>(4)? as u64,
                    gas_used: row.get::<_, i64>(5)? as u64,
                    coinbase: super::SqliteStore::blob_to_addr(&row.get::<_, Vec<u8>>(6)?),
                    difficulty,
                    mix_hash: super::SqliteStore::blob_to_b256(&row.get::<_, Vec<u8>>(8)?),
                }))
            }
            None => Ok(None),
        }
    }

    pub fn put_txs(&self, block_num: u64, txs: &[TxData]) -> anyhow::Result<()> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO transactions (hash, block_number, tx_index, tx_type, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list, authorization_list)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        for tx in txs {
            let access_list_blob = super::SqliteStore::serialize_access_list(&tx.access_list)?;
            let auth_blob = super::SqliteStore::serialize_access_list(&tx.authorization_list)?;
            stmt.execute(rusqlite::params![
                super::SqliteStore::b256_to_blob(&tx.hash),
                block_num as i64,
                tx.index as i64,
                tx.tx_type as i64,
                super::SqliteStore::addr_to_blob(&tx.from),
                tx.to.map(|a| super::SqliteStore::addr_to_blob(&a)),
                tx.input.to_vec(),
                super::SqliteStore::u256_to_blob(&tx.value),
                tx.gas_limit as i64,
                tx.max_fee_per_gas as i64,
                tx.max_priority_fee_per_gas.map(|v| v as i64),
                tx.nonce as i64,
                access_list_blob,
                auth_blob,
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
            "SELECT hash, tx_index, tx_type, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list, authorization_list
             FROM transactions WHERE block_number = ?1 ORDER BY tx_index",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64])?;
        let mut txs = Vec::new();
        while let Some(row) = rows.next()? {
            let access_list = row.get::<_, Option<Vec<u8>>>(11)?
                .map(|b| super::SqliteStore::deserialize_access_list(&b).unwrap_or_default())
                .unwrap_or_default();
            let authorization_list = row.get::<_, Option<Vec<u8>>>(12)?
                .map(|b| super::SqliteStore::deserialize_access_list(&b).unwrap_or_default())
                .unwrap_or_default();
            txs.push(TxData {
                hash: super::SqliteStore::blob_to_b256(&row.get::<_, Vec<u8>>(0)?),
                index: row.get::<_, i64>(1)? as u64,
                tx_type: row.get::<_, i64>(2)? as u8,
                from: super::SqliteStore::blob_to_addr(&row.get::<_, Vec<u8>>(3)?),
                to: row.get::<_, Option<Vec<u8>>>(4)?.map(|b| super::SqliteStore::blob_to_addr(&b)),
                input: row.get::<_, Vec<u8>>(5)?.into(),
                value: super::SqliteStore::blob_to_u256(&row.get::<_, Vec<u8>>(6)?),
                gas_limit: row.get::<_, i64>(7)? as u64,
                max_fee_per_gas: row.get::<_, i64>(8)? as u128,
                max_priority_fee_per_gas: row.get::<_, Option<i64>>(9)?.map(|v| v as u128),
                nonce: row.get::<_, i64>(10)? as u64,
                access_list,
                authorization_list,
            });
        }
        Ok(Some(txs))
    }

    pub fn put_receipts(&self, _block_num: u64, receipts: &[ReceiptData]) -> anyhow::Result<()> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO receipts (tx_hash, tx_index, status, gas_used, cumulative_gas_used, logs, contract_address)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for r in receipts {
            let logs_blob = super::SqliteStore::serialize(&r.logs)?;
            stmt.execute(rusqlite::params![
                super::SqliteStore::b256_to_blob(&r.tx_hash),
                r.tx_index as i64,
                r.status as i64,
                r.gas_used as i64,
                r.cumulative_gas_used as i64,
                logs_blob,
                r.contract_address.map(|a| super::SqliteStore::addr_to_blob(&a)),
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
            let logs: Vec<crate::data::LogData> = super::SqliteStore::deserialize(&row.get::<_, Vec<u8>>(5)?)?;
            receipts.push(ReceiptData {
                tx_hash: super::SqliteStore::blob_to_b256(&row.get::<_, Vec<u8>>(0)?),
                tx_index: row.get::<_, i64>(1)? as u64,
                status: row.get::<_, i64>(2)? != 0,
                gas_used: row.get::<_, i64>(3)? as u64,
                cumulative_gas_used: row.get::<_, i64>(4)? as u64,
                logs,
                contract_address: row.get::<_, Option<Vec<u8>>>(6)?.map(|b| super::SqliteStore::blob_to_addr(&b)),
            });
        }
        Ok(Some(receipts))
    }

    pub fn get_logs_for_block(&self, block_num: u64) -> anyhow::Result<Vec<crate::data::NormalizedLog>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT block_number, tx_index, log_index, address, topic0, topic1, topic2, topic3, data, erc20_amount, event_sig
             FROM logs WHERE block_number = ?1 ORDER BY log_index",
        )?;
        let mut rows = stmt.query(rusqlite::params![block_num as i64])?;
        let mut logs = Vec::new();
        while let Some(row) = rows.next()? {
            logs.push(super::row_to_normalized_log(&row)?);
        }
        Ok(logs)
    }

    pub fn get_logs_for_tx(&self, tx_hash: &B256) -> anyhow::Result<Vec<crate::data::NormalizedLog>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT l.block_number, l.tx_index, l.log_index, l.address, l.topic0, l.topic1, l.topic2, l.topic3, l.data, l.erc20_amount, l.event_sig
             FROM logs l
             INNER JOIN transactions t ON t.block_number = l.block_number AND t.tx_index = l.tx_index
             WHERE t.hash = ?1
             ORDER BY l.log_index",
        )?;
        let mut rows = stmt.query(rusqlite::params![super::SqliteStore::b256_to_blob(tx_hash)])?;
        let mut logs = Vec::new();
        while let Some(row) = rows.next()? {
            logs.push(super::row_to_normalized_log(&row)?);
        }
        Ok(logs)
    }

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

    pub fn missing_blocks_in_range(&self, start: u64, end: u64) -> anyhow::Result<Vec<u64>> {
        self.find_uncached_blocks(start..=end)
    }

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
}
