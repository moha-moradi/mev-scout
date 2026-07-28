use alloy::primitives::{Address, Bytes, U256};

use crate::data::types::AccountData;

impl super::SqliteStore {
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
                super::SqliteStore::addr_to_blob(&address),
                account.nonce as i64,
                super::SqliteStore::u256_to_blob(&account.balance),
                super::SqliteStore::b256_to_blob(&account.code_hash),
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
        let mut rows = stmt.query(rusqlite::params![block_num as i64, super::SqliteStore::addr_to_blob(&address)])?;
        match rows.next()? {
            Some(row) => Ok(Some(AccountData {
                nonce: row.get::<_, i64>(0)? as u64,
                balance: super::SqliteStore::blob_to_u256(&row.get::<_, Vec<u8>>(1)?),
                code_hash: super::SqliteStore::blob_to_b256(&row.get::<_, Vec<u8>>(2)?),
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
                super::SqliteStore::addr_to_blob(&address),
                super::SqliteStore::u256_to_blob(&slot),
                super::SqliteStore::u256_to_blob(&value),
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
            super::SqliteStore::addr_to_blob(&address),
            super::SqliteStore::u256_to_blob(&slot),
        ])?;
        match rows.next()? {
            Some(row) => Ok(Some(super::SqliteStore::blob_to_u256(&row.get::<_, Vec<u8>>(0)?))),
            None => Ok(None),
        }
    }

    pub fn put_code(&self, address: Address, code: &Bytes) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO contract_code (address, code) VALUES (?1, ?2)",
            rusqlite::params![super::SqliteStore::addr_to_blob(&address), code.to_vec()],
        )?;
        Ok(())
    }

    pub fn get_code(&self, address: Address) -> anyhow::Result<Option<Bytes>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT code FROM contract_code WHERE address = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![super::SqliteStore::addr_to_blob(&address)])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, Vec<u8>>(0)?.into())),
            None => Ok(None),
        }
    }
}
