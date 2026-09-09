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
            "INSERT OR REPLACE INTO pool_info (address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol, tvl_usd, volume_usd_24h, volume_usd_30d)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
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
                pool.tvl_usd,
                pool.volume_usd_24h,
                pool.volume_usd_30d,
            ],
        )?;
        Ok(())
    }

    pub fn get_discovered_pool(&self, address: &Address) -> anyhow::Result<Option<PoolInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol, tvl_usd, volume_usd_24h, volume_usd_30d
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
            "SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol, tvl_usd, volume_usd_24h, volume_usd_30d
             FROM pool_info",
        )?;
        let mut rows = stmt.query([])?;
        let mut pools = Vec::new();
        while let Some(row) = rows.next()? {
            pools.push(super::row_to_pool_info(&row)?);
        }
        Ok(pools)
    }

    /// Earliest `creation_block` seen per pool factory — the "first-observed-block
    /// cache" used by the panel-discovery start-block guard (DEX_COVERAGE_PLAN
    /// Phase 2.5). Remote-sourced pools carry `creation_block == 0`, so those are
    /// ignored; a factory with only remote rows never appears.
    pub fn earliest_creation_block_by_factory(&self) -> anyhow::Result<Vec<(Address, u64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT factory, MIN(creation_block) FROM pool_info \
             WHERE factory IS NOT NULL AND creation_block > 0 \
             GROUP BY factory",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let factory: Vec<u8> = row.get(0)?;
            let block: i64 = row.get(1)?;
            if factory.len() == 20 {
                out.push((Address::from_slice(&factory), block as u64));
            }
        }
        Ok(out)
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
}

#[cfg(test)]
mod tests {
    use super::super::SqliteStore;
    use alloy::primitives::Address;

    #[test]
    fn earliest_creation_block_groups_by_factory() {
        let store = SqliteStore::open(":memory:").unwrap();
        {
            let conn = store.conn();
            for (i, block, factory) in [
                (1u8, 5000i64, 4u8),
                (2u8, 7000i64, 4u8),
                (3u8, 6000i64, 5u8),
            ] {
                let mut addr = [0u8; 20];
                addr[0] = i;
                let mut t0 = [0u8; 20];
                t0[0] = i + 10;
                let mut t1 = [0u8; 20];
                t1[0] = i + 20;
                let mut fac = [0u8; 20];
                fac[0] = factory;
                conn.execute(
                    "INSERT INTO pool_info (address, token0, token1, fee, dex_type, creation_block, factory)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        addr.to_vec(),
                        t0.to_vec(),
                        t1.to_vec(),
                        3000i64,
                        1i64,
                        block,
                        Some(fac.to_vec()),
                    ],
                )
                .unwrap();
            }
        }
        let by_factory = store.earliest_creation_block_by_factory().unwrap();
        assert_eq!(by_factory.len(), 2);
        let mut f4 = [0u8; 20];
        f4[0] = 4;
        assert!(by_factory.contains(&(Address::from_slice(&f4), 5000)));
        let mut f5 = [0u8; 20];
        f5[0] = 5;
        assert!(by_factory.contains(&(Address::from_slice(&f5), 6000)));
    }

    #[test]
    fn earliest_creation_block_ignores_remote_rows() {
        let store = SqliteStore::open(":memory:").unwrap();
        {
            let conn = store.conn();
            let mut addr = [9u8; 20];
            addr[0] = 1;
            conn.execute(
                "INSERT INTO pool_info (address, token0, token1, fee, dex_type, creation_block, factory)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    addr.to_vec(),
                    vec![2u8; 20],
                    vec![3u8; 20],
                    3000i64,
                    1i64,
                    0i64,
                    Some(vec![4u8; 20]), // creation_block 0 ⇒ remote-sourced ⇒ ignored
                ],
            )
            .unwrap();
        }
        assert!(store.earliest_creation_block_by_factory().unwrap().is_empty());
    }
}
