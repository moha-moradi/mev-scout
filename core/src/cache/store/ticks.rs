use std::collections::BTreeMap;

use alloy::primitives::Address;

impl super::SqliteStore {
    /// Read a cached V3 tick-bootstrap result for a pool.
    ///
    /// The cache is keyed by the tick-bitmap center word of the window the
    /// ticks were fetched from, so a window that hasn't moved (same center
    /// word) can be reused across resyncs/restarts without re-querying the
    /// chain. Returns the stored `tick_spacing` alongside the tick map.
    pub fn get_v3_tick_cache(
        &self,
        address: &Address,
        center_word: i32,
    ) -> anyhow::Result<Option<(i32, BTreeMap<i32, i128>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT tick_spacing, ticks FROM v3_tick_cache WHERE address = ?1 AND center_word = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![
            super::SqliteStore::addr_to_blob(address),
            center_word as i64,
        ])?;
        match rows.next()? {
            Some(row) => {
                let spacing: i64 = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                let ticks: BTreeMap<i32, i128> = super::SqliteStore::deserialize(&bytes)?;
                Ok(Some((spacing as i32, ticks)))
            }
            None => Ok(None),
        }
    }

    /// Persist a bootstrapped V3 tick map for a pool, keyed by the tick-bitmap
    /// center word of the window it covers. Only non-empty tick maps are stored
    /// (an empty window is cheap to re-probe and may gain liquidity later).
    pub fn put_v3_tick_cache(
        &self,
        address: &Address,
        center_word: i32,
        tick_spacing: i32,
        ticks: &BTreeMap<i32, i128>,
    ) -> anyhow::Result<()> {
        if ticks.is_empty() {
            return Ok(());
        }
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO v3_tick_cache (address, center_word, tick_spacing, ticks, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                super::SqliteStore::addr_to_blob(address),
                center_word as i64,
                tick_spacing as i64,
                super::SqliteStore::serialize(ticks)?,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> super::super::SqliteStore {
        super::super::SqliteStore::open(":memory:").unwrap()
    }

    #[test]
    fn v3_tick_cache_roundtrip() {
        let s = store();
        let addr = Address::repeat_byte(0xab);

        let mut ticks = BTreeMap::new();
        ticks.insert(-120, 2_i128 << 70);
        ticks.insert(0, 1_i128 << 70);
        ticks.insert(240, 5_i128 << 70);

        s.put_v3_tick_cache(&addr, 512, 60, &ticks).unwrap();

        let hit = s.get_v3_tick_cache(&addr, 512).unwrap().expect("hit");
        assert_eq!(hit.0, 60);
        assert_eq!(hit.1, ticks);

        let miss_center = s.get_v3_tick_cache(&addr, 1024).unwrap();
        assert!(miss_center.is_none(), "different center word must miss");
    }

    #[test]
    fn v3_tick_cache_skips_empty_maps() {
        let s = store();
        let addr = Address::repeat_byte(0xcd);

        s.put_v3_tick_cache(&addr, 0, 60, &BTreeMap::new()).unwrap();
        let got = s.get_v3_tick_cache(&addr, 0).unwrap();
        assert!(got.is_none(), "empty tick maps must not be cached");
    }

    #[test]
    fn v3_tick_cache_updates_by_center_word() {
        let s = store();
        let addr = Address::repeat_byte(0xef);

        let mut a = BTreeMap::new();
        a.insert(-60, 1_i128 << 70);
        let mut b = BTreeMap::new();
        b.insert(60, 1_i128 << 70);

        s.put_v3_tick_cache(&addr, 128, 60, &a).unwrap();
        s.put_v3_tick_cache(&addr, 512, 60, &b).unwrap();

        assert_eq!(s.get_v3_tick_cache(&addr, 128).unwrap().unwrap().1, a);
        assert_eq!(s.get_v3_tick_cache(&addr, 512).unwrap().unwrap().1, b);
    }
}
