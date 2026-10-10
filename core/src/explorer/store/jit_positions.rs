//! JIT open-positions + price cache for ExplorerStore.
use alloy::primitives::Address;

use super::{ExplorerStore, OpenPosition};

impl ExplorerStore {
    /// Price cache lookup: USD for a token at an hour bucket (exact hour, or
    /// the most recent cached hour within 24h).
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
            .unwrap_or(None);
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
            .unwrap_or(None);
        Ok(v)
    }
    /// Cache a price observation.
    pub fn put_price(
        &self,
        token: Address,
        hour: u64,
        usd: f64,
        source: &str,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO prices(hour, token, usd, source) VALUES(?1, ?2, ?3, ?4)",
            rusqlite::params![hour as i64, format!("{:#x}", token), usd, source],
        )?;
        Ok(())
    }
    /// Native USD price at the hour bucket nearest `ts` (native keyed at
    /// `0x…0` / `0x0`). Drives the USD→wei conversion for the detector-vs-
    /// realized verdict when a trace native delta is unavailable.
    pub fn native_price_near(&self, ts: u64) -> anyhow::Result<Option<f64>> {
        let mut stmt = self.conn.prepare(
            "SELECT usd FROM prices
             WHERE lower(token) IN ('0x0000000000000000000000000000000000000000', '0x0')
             ORDER BY ABS(hour - ?1) ASC LIMIT 1",
        )?;
        let mut rows = stmt.query([ts as i64])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get::<_, f64>(0)?),
            None => None,
        })
    }
    /// Persist (or refresh) open JIT Mint positions (Phase 1.5).
    pub fn record_jit_open(&self, positions: &[OpenPosition]) -> anyhow::Result<()> {
        for p in positions {
            self.conn.execute(
                "INSERT OR REPLACE INTO jit_open_positions
                   (pool, owner, tick_lower, tick_upper, opened_block, liquidity)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("{:#x}", p.pool),
                    format!("{:#x}", p.owner),
                    p.tick_lower as i64,
                    p.tick_upper as i64,
                    p.opened_block as i64,
                    p.liquidity.to_string(),
                ],
            )?;
        }
        Ok(())
    }
    /// Delete an open position once its exact-liquidity Burn is observed.
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
