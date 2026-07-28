use crate::data::types::TxData;

impl super::SqliteStore {
    pub fn put_pending_txs(&self, txs: &[TxData], captured_at: u64) -> anyhow::Result<()> {
        let conn = self.conn();
        let block_number: i64 = captured_at as i64;
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO pending_txs (block_number, tx_index, hash, from_addr, to_addr, input, value, gas_limit, max_fee_per_gas, max_priority_fee_per_gas, nonce, access_list, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for (i, tx) in txs.iter().enumerate() {
            let access_list_blob = super::SqliteStore::serialize_access_list(&tx.access_list)?;
            stmt.execute(rusqlite::params![
                block_number,
                i as i64,
                super::SqliteStore::b256_to_blob(&tx.hash),
                super::SqliteStore::addr_to_blob(&tx.from),
                tx.to.map(|a| super::SqliteStore::addr_to_blob(&a)),
                tx.input.to_vec(),
                super::SqliteStore::u256_to_blob(&tx.value),
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

    pub fn count_pending_txs(&self, captured_at: u64) -> anyhow::Result<usize> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_txs WHERE captured_at = ?1",
            rusqlite::params![captured_at as i64],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn total_pending_txs(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_txs",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}
