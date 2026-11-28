use alloy::primitives::Address;

use crate::pool::state::PoolInfo;

impl super::SqliteStore {
    pub fn get_discovered_pool(&self, address: &Address) -> anyhow::Result<Option<PoolInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol, tvl_usd, volume_usd_24h, volume_usd_30d, derivation_version
             FROM pool_info WHERE address = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![super::SqliteStore::addr_to_blob(address)])?;
        match rows.next()? {
            Some(row) => Ok(Some(super::row_to_pool_info(row)?)),
            None => Ok(None),
        }
    }

    pub fn list_discovered_pools(&self) -> anyhow::Result<Vec<PoolInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block, pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, token1_symbol, tvl_usd, volume_usd_24h, volume_usd_30d, derivation_version
             FROM pool_info",
        )?;
        let mut rows = stmt.query([])?;
        let mut pools = Vec::new();
        while let Some(row) = rows.next()? {
            pools.push(super::row_to_pool_info(row)?);
        }
        Ok(pools)
    }

    /// Pool → DEX identity from the dashboard's DexScreener cache.
    ///
    /// `pool_dex` is created by the dashboard enrich pass, not by the scanner
    /// schema, so an absent table is a normal "no labels cached yet" state.
    /// The stable `dex_id` is preferred over the display `dex_name` because
    /// these labels feed cross-DEX classification, not the UI.
    pub fn list_pool_dex_names(&self) -> anyhow::Result<Vec<(Address, String)>> {
        let conn = self.conn();
        let mut stmt = match conn.prepare("SELECT pool, dex_id, dex_name FROM pool_dex") {
            Ok(stmt) => stmt,
            Err(e) => {
                tracing::debug!("pool_dex labels unavailable: {e}");
                return Ok(Vec::new());
            }
        };
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows.flatten() {
            let (pool, dex_id, dex_name) = row;
            if pool.len() != 20 {
                continue;
            }
            let name = if dex_id.trim().is_empty() {
                dex_name
            } else {
                dex_id
            };
            out.push((super::SqliteStore::blob_to_addr(&pool), name));
        }
        Ok(out)
    }

    /// Earliest `creation_block` seen per pool factory — the "first-observed-block
    /// cache" used by the panel-discovery start-block guard. Remote-sourced pools
    /// carry `creation_block == 0`, so those are
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

    /// Number of cached pools. Cheap `COUNT(*)` so callers can decide whether
    /// a discovery bootstrap is needed without materializing every row.
    pub fn pool_count(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM pool_info", [], |row| row.get(0))?;
        Ok(count.max(0) as usize)
    }

    /// Inclusive block span already covered by an on-chain discovery scan.
    pub fn discovery_watermark(&self) -> anyhow::Result<Option<(u64, u64)>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT low_block, high_block FROM discovery_scan_watermark WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => {
                let low: i64 = row.get(0)?;
                let high: i64 = row.get(1)?;
                Ok(Some((low as u64, high as u64)))
            }
            None => Ok(None),
        }
    }

    /// Expand the stored discovery span to include `[from, to]`.
    pub fn note_discovery_scan(&self, from: u64, to: u64) -> anyhow::Result<()> {
        if from > to {
            return Ok(());
        }
        let (low, high) = match self.discovery_watermark()? {
            Some((low, high)) => (low.min(from), high.max(to)),
            None => (from, to),
        };
        let conn = self.conn();
        conn.execute(
            "INSERT INTO discovery_scan_watermark (id, low_block, high_block) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET low_block = ?1, high_block = ?2",
            rusqlite::params![low as i64, high as i64],
        )?;
        Ok(())
    }

    pub fn max_creation_block(&self) -> anyhow::Result<Option<u64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT MAX(creation_block) FROM pool_info")?;
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
        use crate::dex_type::DexType;
        use crate::pool::state::PoolInfo;

        let store = SqliteStore::open(":memory:").unwrap();
        for (i, block, factory) in [(1u8, 5000u64, 4u8), (2u8, 7000, 4), (3u8, 6000, 5)] {
            let mut addr = [0u8; 20];
            addr[0] = i;
            let mut t0 = [0u8; 20];
            t0[0] = i + 10;
            let mut t1 = [0u8; 20];
            t1[0] = i + 20;
            let mut fac = [0u8; 20];
            fac[0] = factory;
            store
                .put_discovered_pool(&PoolInfo {
                    address: Address::from_slice(&addr),
                    token0: Address::from_slice(&t0),
                    token1: Address::from_slice(&t1),
                    fee: 3000,
                    dex_type: DexType::UniswapV3,
                    creation_block: block,
                    factory: Some(Address::from_slice(&fac)),
                    ..Default::default()
                })
                .unwrap();
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
        use crate::dex_type::DexType;
        use crate::pool::state::PoolInfo;

        let store = SqliteStore::open(":memory:").unwrap();
        let mut addr = [9u8; 20];
        addr[0] = 1;
        store
            .put_discovered_pool(&PoolInfo {
                address: Address::from_slice(&addr),
                token0: Address::repeat_byte(2),
                token1: Address::repeat_byte(3),
                fee: 3000,
                dex_type: DexType::UniswapV3,
                // creation_block 0 ⇒ remote-sourced ⇒ ignored
                creation_block: 0,
                factory: Some(Address::repeat_byte(4)),
                ..Default::default()
            })
            .unwrap();
        assert!(store
            .earliest_creation_block_by_factory()
            .unwrap()
            .is_empty());
    }

    /// Migration v13 must expose the column on a fresh DB (migrations run from
    /// version 0) and the value must survive the PoolInfo round-trip.
    #[test]
    fn derivation_version_column_exists_and_roundtrips() {
        use crate::dex_type::{DexType, POOL_DERIVATION_VERSION};
        use crate::pool::state::PoolInfo;

        let store = SqliteStore::open(":memory:").unwrap();
        {
            let conn = store.conn();
            let mut stmt = conn.prepare("PRAGMA table_info(pool_info)").unwrap();
            let names: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .map(|n| n.unwrap())
                .collect();
            assert!(
                names.iter().any(|n| n == "derivation_version"),
                "migration v13 must add derivation_version: {names:?}"
            );
        }

        let pool = PoolInfo {
            address: Address::repeat_byte(0x11),
            token0: Address::repeat_byte(0x22),
            token1: Address::repeat_byte(0x33),
            fee: 30,
            dex_type: DexType::TraderJoeLB,
            derivation_version: Some(POOL_DERIVATION_VERSION),
            dex_name: Some(std::sync::Arc::from("pharaoh")),
            ..Default::default()
        };
        store.put_discovered_pool(&pool).unwrap();
        let got = store
            .get_discovered_pool(&pool.address)
            .unwrap()
            .expect("row exists");
        assert_eq!(got.dex_type, DexType::TraderJoeLB);
        assert_eq!(got.derivation_version, Some(POOL_DERIVATION_VERSION));
        assert_eq!(store.list_discovered_pools().unwrap().len(), 1);

        // Unversioned (pre-v13) rows read back as `None`, which forces
        // re-derivation instead of trust.
        let legacy = PoolInfo {
            address: Address::repeat_byte(0x44),
            token0: Address::repeat_byte(0x55),
            token1: Address::repeat_byte(0x66),
            dex_type: DexType::Unknown,
            derivation_version: None,
            ..Default::default()
        };
        store.put_discovered_pool(&legacy).unwrap();
        let got = store
            .get_discovered_pool(&legacy.address)
            .unwrap()
            .expect("row exists");
        assert_eq!(got.derivation_version, None);
        assert_eq!(got.dex_type, DexType::Unknown);
    }

    /// G-01: the dashboard label cache is an optional second identity source.
    /// It must degrade to an empty list before the enrich pass creates it and
    /// return the stable `dex_id` (not the display name) once present.
    #[test]
    fn pool_dex_names_are_optional_and_use_stable_id() {
        let store = SqliteStore::open(":memory:").unwrap();
        assert!(store.list_pool_dex_names().unwrap().is_empty());

        // The same connection used by the store creates the optional table.
        {
            let conn = store.conn();
            conn.execute_batch(
                "CREATE TABLE pool_dex (
                     pool BLOB PRIMARY KEY,
                     dex_id TEXT NOT NULL,
                     dex_name TEXT NOT NULL,
                     label TEXT,
                     url TEXT,
                     fetched_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
            let addr = Address::repeat_byte(0x77);
            conn.execute(
                "INSERT INTO pool_dex(pool, dex_id, dex_name, fetched_at) VALUES (?1, ?2, ?3, 0)",
                rusqlite::params![addr.as_slice().to_vec(), "traderjoe", "Trader Joe"],
            )
            .unwrap();
            let addr2 = Address::repeat_byte(0x88);
            conn.execute(
                "INSERT INTO pool_dex(pool, dex_id, dex_name, fetched_at) VALUES (?1, ?2, ?3, 0)",
                rusqlite::params![addr2.as_slice().to_vec(), "", "Pangolin"],
            )
            .unwrap();
        }
        let labels = store.list_pool_dex_names().unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(
            labels
                .iter()
                .find(|(a, _)| *a == Address::repeat_byte(0x77))
                .map(|(_, n)| n.as_str()),
            Some("traderjoe")
        );
        assert_eq!(
            labels
                .iter()
                .find(|(a, _)| *a == Address::repeat_byte(0x88))
                .map(|(_, n)| n.as_str()),
            Some("Pangolin"),
            "empty dex_id falls back to the display name"
        );
    }
}
