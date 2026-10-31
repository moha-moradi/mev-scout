//! JIT open-positions + price cache for ExplorerStore.
use std::collections::HashMap;

use alloy::primitives::Address;
use rusqlite::types::Value;

use super::{ExplorerStore, OpenPosition};

/// A quote is usable only when it is a positive, finite USD amount. `0.0`
/// (and NaN/inf) rows are treated as absent on both write and read.
fn is_usable_quote(usd: f64) -> bool {
    usd.is_finite() && usd > 0.0
}

impl ExplorerStore {
    /// Price cache lookup: USD for a token at an hour bucket (exact hour, or
    /// the most recent cached hour within 24h).
    ///
    /// Only a finite, positive quote is returned. Legacy databases can still
    /// hold `0.0` rows written before writes were guarded, so such a row is
    /// treated as missing and callers re-fetch.
    pub fn price_at(&self, token: Address, hour: u64) -> anyhow::Result<Option<(f64, String)>> {
        let tok = format!("{:#x}", token);
        let exact: Option<(f64, String)> = self
            .conn
            .query_row(
                "SELECT usd, source FROM prices WHERE token = ?1 AND hour = ?2",
                rusqlite::params![tok, hour as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .unwrap_or(None)
            .filter(|(usd, _)| is_usable_quote(*usd));
        if exact.is_some() {
            return Ok(exact);
        }
        let v = self
            .conn
            .query_row(
                "SELECT usd, source FROM prices
                 WHERE token = ?1 AND hour <= ?2 AND hour > ?2 - 24
                 ORDER BY hour DESC LIMIT 1",
                rusqlite::params![tok, hour as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .unwrap_or(None)
            .filter(|(usd, _)| is_usable_quote(*usd));
        Ok(v)
    }

    /// Exact-hour quotes for many tokens, then the newest usable quote within
    /// 24h for the ones that missed. One query per phase, not one per token.
    pub fn prices_at(
        &self,
        tokens: &[Address],
        hour: u64,
    ) -> anyhow::Result<HashMap<Address, (f64, String)>> {
        let mut out = HashMap::new();
        if tokens.is_empty() {
            return Ok(out);
        }
        let keys: Vec<(Address, String)> = tokens
            .iter()
            .copied()
            .filter(|t| !t.is_zero())
            .map(|t| (t, format!("{t:#x}")))
            .collect();
        self.fill_prices(&keys, hour, true, &mut out)?;
        let missing: Vec<(Address, String)> = keys
            .into_iter()
            .filter(|(addr, _)| !out.contains_key(addr))
            .collect();
        self.fill_prices(&missing, hour, false, &mut out)?;
        Ok(out)
    }

    fn fill_prices(
        &self,
        keys: &[(Address, String)],
        hour: u64,
        exact: bool,
        out: &mut HashMap<Address, (f64, String)>,
    ) -> anyhow::Result<()> {
        const CHUNK: usize = 400;
        let by_token: HashMap<&str, Address> = keys.iter().map(|(a, s)| (s.as_str(), *a)).collect();
        for chunk in keys.chunks(CHUNK) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = (0..chunk.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            let sql = if exact {
                format!(
                    "SELECT token, usd, source FROM prices
                     WHERE hour = ?1 AND token IN ({placeholders})"
                )
            } else {
                format!(
                    "SELECT p.token, p.usd, p.source FROM prices p
                     WHERE p.hour <= ?1 AND p.hour > ?1 - 24 AND p.token IN ({placeholders})
                       AND p.hour = (
                         SELECT MAX(hour) FROM prices
                         WHERE token = p.token AND hour <= ?1 AND hour > ?1 - 24)"
                )
            };
            let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 1);
            let hour_i = hour as i64;
            params.push(&hour_i);
            for (_, s) in chunk {
                params.push(s);
            }
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (token_s, usd, source) = row?;
                if !is_usable_quote(usd) {
                    continue;
                }
                if let Some(addr) = by_token.get(token_s.as_str()) {
                    out.insert(*addr, (usd, source));
                }
            }
        }
        Ok(())
    }
    /// Cache a price observation.
    ///
    /// A non-finite or non-positive quote is refused: writing it would let a
    /// bad provider response value a whole hour bucket of amounts at $0.
    pub fn put_price(
        &self,
        token: Address,
        hour: u64,
        usd: f64,
        source: &str,
    ) -> anyhow::Result<()> {
        if !is_usable_quote(usd) {
            tracing::warn!(
                token = %format!("{:#x}", token),
                hour,
                source,
                usd,
                "refusing to cache a non-finite or non-positive price quote"
            );
            return Ok(());
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO prices(hour, token, usd, source) VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![hour as i64, format!("{:#x}", token), usd, source],
        )?;
        Ok(())
    }
    /// Native USD price at the hour bucket nearest `ts` (native keyed at
    /// `0x…0` / `0x0`). Drives the USD→wei conversion for the detector-vs-
    /// realized verdict when a trace native delta is unavailable.
    ///
    /// `ts` is epoch seconds and is divided down to an hour bucket before
    /// matching, because `prices.hour` stores hour buckets, not seconds. Only
    /// rows within ±24h of that bucket are considered; a timestamp with no
    /// nearby observation yields `None` rather than a stale price.
    ///
    /// Convention caveat: token prices are keyed by *block* hour
    /// (`ingest.rs` `pricing::hour_bucket(block ts)`), while native prices are
    /// keyed by *fetch-time* hour (every `native_price_cached` call site passes
    /// `crate::utils::epoch_secs()`). Those two conventions differ, and this
    /// lookup does not resolve the mismatch; see the deferred historical
    /// native-pricing work.
    pub fn native_price_near(&self, ts: u64) -> anyhow::Result<Option<f64>> {
        let bucket = ts / 3600;
        let mut stmt = self.conn.prepare(
            "SELECT usd FROM prices
             WHERE token IN (
               '0x0000000000000000000000000000000000000000',
               '0x0')
               AND hour BETWEEN ?1 - 24 AND ?1 + 24
             ORDER BY ABS(hour - ?1) ASC LIMIT 1",
        )?;
        let mut rows = stmt.query([bucket as i64])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get::<_, f64>(0)?),
            None => None,
        })
    }
    /// Persist (or refresh) open JIT Mint positions.
    ///
    /// Runs on the store connection, so a caller that already holds a
    /// transaction on it (the ingest path's per-block JIT sweep) gets these
    /// writes enrolled in that transaction; this method must not open one of
    /// its own. Rows are inserted in bounded multi-row statements.
    pub fn record_jit_open(&self, positions: &[OpenPosition]) -> anyhow::Result<()> {
        // 6 bound values per position; 150 stays below SQLite's variable limit.
        const CHUNK: usize = 150;
        for chunk in positions.chunks(CHUNK) {
            let rows = vec!["(?, ?, ?, ?, ?, ?)"; chunk.len()].join(", ");
            let sql = format!(
                "INSERT OR REPLACE INTO jit_open_positions
                   (pool, owner, tick_lower, tick_upper, opened_block, liquidity)
                 VALUES {rows}"
            );
            let mut stmt = self.conn.prepare_cached(&sql)?;
            let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * 6);
            for p in chunk {
                params.push(Value::Text(format!("{:#x}", p.pool)));
                params.push(Value::Text(format!("{:#x}", p.owner)));
                params.push(Value::Integer(p.tick_lower as i64));
                params.push(Value::Integer(p.tick_upper as i64));
                params.push(Value::Integer(p.opened_block as i64));
                params.push(Value::Text(p.liquidity.to_string()));
            }
            stmt.execute(rusqlite::params_from_iter(params))?;
        }
        Ok(())
    }
    /// Delete every observed close in bounded multi-row statements. Participates
    /// in the caller's transaction, like [`Self::record_jit_open`].
    pub fn close_jit_positions(
        &self,
        closes: &[(Address, Address, i32, i32)],
    ) -> anyhow::Result<()> {
        // 4 bound values per position; 200 keeps the statement well below
        // SQLite's variable limit on every supported build.
        const CHUNK: usize = 200;
        for chunk in closes.chunks(CHUNK) {
            let rows = vec!["(?, ?, ?, ?)"; chunk.len()].join(", ");
            let sql = format!(
                "DELETE FROM jit_open_positions
                  WHERE (pool, owner, tick_lower, tick_upper) IN (VALUES {rows})"
            );
            let mut stmt = self.conn.prepare_cached(&sql)?;
            let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * 4);
            for (pool, owner, tick_lower, tick_upper) in chunk {
                params.push(Value::Text(format!("{pool:#x}")));
                params.push(Value::Text(format!("{owner:#x}")));
                params.push(Value::Integer(*tick_lower as i64));
                params.push(Value::Integer(*tick_upper as i64));
            }
            stmt.execute(rusqlite::params_from_iter(params))?;
        }
        Ok(())
    }

    /// Delete an open position once its exact-liquidity Burn is observed.
    ///
    /// Like [`Self::record_jit_open`], this participates in a transaction the
    /// caller already holds on the shared connection.
    pub fn close_jit_position(
        &self,
        pool: Address,
        owner: Address,
        tick_lower: i32,
        tick_upper: i32,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM jit_open_positions
              WHERE pool = ?1 AND owner = ?2 AND tick_lower = ?3 AND tick_upper = ?4",
            rusqlite::params![
                format!("{:#x}", pool),
                format!("{:#x}", owner),
                tick_lower as i64,
                tick_upper as i64,
            ],
        )?;
        Ok(())
    }
    /// Open positions still inside the block window (`opened_block >= min_block`).
    pub fn open_positions(&self, min_block: u64) -> anyhow::Result<Vec<OpenPosition>> {
        let mut stmt = self.conn.prepare(
            "SELECT pool, owner, tick_lower, tick_upper, opened_block, liquidity
               FROM jit_open_positions WHERE opened_block >= ?1",
        )?;
        let rows = stmt.query_map([min_block as i64], |r| {
            let pool: String = r.get(0)?;
            let owner: String = r.get(1)?;
            let liq: String = r.get(5)?;
            Ok((
                pool,
                owner,
                r.get::<_, i64>(2)? as i32,
                r.get::<_, i64>(3)? as i32,
                r.get::<_, i64>(4)? as u64,
                liq,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (pool, owner, tick_lower, tick_upper, opened_block, liq) = row?;
            let (Ok(pool), Ok(owner)) = (pool.parse::<Address>(), owner.parse::<Address>()) else {
                continue;
            };
            out.push(OpenPosition {
                pool,
                owner,
                tick_lower,
                tick_upper,
                opened_block,
                liquidity: liq.parse::<u128>().unwrap_or(0),
            });
        }
        Ok(out)
    }
    /// Prune positions older than the block window. Returns rows removed.
    pub fn prune_jit_positions(&self, before_block: u64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM jit_open_positions WHERE opened_block < ?1",
            [before_block as i64],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::store::ExplorerStore;

    #[test]
    fn native_price_near_returns_bucket_nearest_ts() {
        let store = ExplorerStore::open_in_memory().expect("open store");
        let hour_a = 100u64;
        let hour_b = 200u64;
        store
            .put_price(Address::ZERO, hour_a, 10.0, "test")
            .unwrap();
        store
            .put_price(Address::ZERO, hour_b, 99.0, "test")
            .unwrap();
        // ts inside hour_a: 100*3600 + 1800
        let got = store.native_price_near(100 * 3600 + 1800).unwrap();
        assert_eq!(
            got,
            Some(10.0),
            "must pick the hour bucket containing ts, not the latest row"
        );
        // ts nearest hour_b
        let got = store.native_price_near(199 * 3600 + 3500).unwrap();
        assert_eq!(got, Some(99.0));
    }

    #[test]
    fn put_price_refuses_non_positive_quote() {
        let store = ExplorerStore::open_in_memory().expect("open store");
        let token = Address::repeat_byte(0x11);
        let hour = 7u64;
        store.put_price(token, hour, 0.0, "test").unwrap();
        assert_eq!(
            store.price_at(token, hour).unwrap(),
            None,
            "a 0.0 quote must not be cached as a price"
        );
        store.put_price(token, hour + 1, f64::NAN, "test").unwrap();
        assert_eq!(
            store.price_at(token, hour + 1).unwrap(),
            None,
            "a NaN quote must not be cached as a price"
        );
        store
            .put_price(token, hour + 2, f64::INFINITY, "test")
            .unwrap();
        assert_eq!(
            store.price_at(token, hour + 2).unwrap(),
            None,
            "an infinite quote must not be cached as a price"
        );
        store.put_price(token, hour, 1.5, "test").unwrap();
        assert_eq!(
            store.price_at(token, hour).unwrap(),
            Some((1.5, "test".to_string())),
            "a positive quote still persists"
        );
    }

    /// Legacy DBs can hold `0.0` rows written before `put_price` guarded
    /// non-positive quotes. Both the exact-hour lookup and the within-24h
    /// fallback must treat such a row as missing.
    #[test]
    fn price_at_ignores_legacy_zero_rows() {
        let store = ExplorerStore::open_in_memory().expect("open store");
        let token = Address::repeat_byte(0x22);
        let hour = 50u64;
        // `put_price` refuses 0.0, so write the legacy row directly.
        store
            .connection()
            .execute(
                "INSERT OR REPLACE INTO prices(hour, token, usd, source) VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![hour as i64, format!("{:#x}", token), 0.0f64, "legacy"],
            )
            .expect("insert legacy 0.0 row");
        assert_eq!(
            store.price_at(token, hour).unwrap(),
            None,
            "exact-hour 0.0 row is not a quote"
        );
        assert_eq!(
            store.price_at(token, hour + 1).unwrap(),
            None,
            "within-24h fallback must reject a 0.0 row too"
        );
        store.put_price(token, hour, 2.5, "test").unwrap();
        assert_eq!(
            store.price_at(token, hour).unwrap(),
            Some((2.5, "test".to_string())),
            "a positive row at the same hour still reads back"
        );
    }

    #[test]
    fn native_price_near_returns_none_outside_staleness_bound() {
        let store = ExplorerStore::open_in_memory().expect("open store");
        store.put_price(Address::ZERO, 100, 10.0, "test").unwrap();
        // Bucket 200 is 100 hours past the only cached observation, far outside
        // the +-24h window. Pins the "stale is None" contract, so dropping the
        // BETWEEN bound (falling back to an unbounded nearest-row lookup) fails.
        let got = store.native_price_near(200 * 3600).unwrap();
        assert_eq!(
            got, None,
            "must return None rather than a price outside the +-24h window"
        );
    }
}
