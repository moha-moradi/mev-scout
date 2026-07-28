use alloy::primitives::Address;

use crate::pool::state::PoolInfo;

impl super::SqliteStore {
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
                super::SqliteStore::addr_to_blob(&pool.address),
                super::SqliteStore::addr_to_blob(&pool.token0),
                super::SqliteStore::addr_to_blob(&pool.token1),
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
        let mut rows = stmt.query(rusqlite::params![super::SqliteStore::addr_to_blob(address)])?;
        match rows.next()? {
            Some(row) => Ok(Some(super::row_to_pool_info(&row)?)),
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
            pools.push(super::row_to_pool_info(&row)?);
        }
        Ok(pools)
    }

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
            rusqlite::params![super::SqliteStore::addr_to_blob(factory), block as i64],
        )?;
        Ok(())
    }

    pub fn get_discovery_cursor(&self, factory: &Address) -> anyhow::Result<Option<u64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT block_number FROM discovery_cursors WHERE factory = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![super::SqliteStore::addr_to_blob(factory)])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, i64>(0)? as u64)),
            None => Ok(None),
        }
    }
}
