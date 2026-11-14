//! JIT open-positions + ordered LP journal + price cache for ExplorerStore.
use std::collections::HashMap;

use alloy::primitives::Address;
use rusqlite::types::Value;
use rusqlite::Connection;

use super::{ExplorerStore, OpenPosition};
use crate::explorer::jit_accounting::LpBlock;

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
        Ok(self
            .price_quote_at(token, hour)?
            .map(|(usd, source, _)| (usd, source)))
    }

    /// Exact-hour quote, then the newest usable quote at or before `hour`
    /// within 24h. Never reads a future observation: that would let a later
    /// price rewrite a historical cashflow. Returns the observed hour too.
    pub fn price_quote_at(
        &self,
        token: Address,
        hour: u64,
    ) -> anyhow::Result<Option<(f64, String, u64)>> {
        let tok = format!("{:#x}", token);
        let row = |sql: &str| -> anyhow::Result<Option<(f64, String, u64)>> {
            let v = self
                .conn
                .query_row(sql, rusqlite::params![tok, hour as i64], |r| {
                    Ok((
                        r.get::<_, f64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? as u64,
                    ))
                })
                .map(Some)
                .unwrap_or(None)
                .filter(|(usd, _, _)| is_usable_quote(*usd));
            Ok(v)
        };
        if let Some(v) = row("SELECT usd, source, hour FROM prices WHERE token = ?1 AND hour = ?2")?
        {
            return Ok(Some(v));
        }
        row("SELECT usd, source, hour FROM prices
             WHERE token = ?1 AND hour < ?2 AND hour > ?2 - 24
             ORDER BY hour DESC LIMIT 1")
    }

    /// Exact-hour quotes for many tokens, then the newest usable quote within
    /// 24h for the ones that missed. One query per phase, not one per token.
    pub fn prices_at(
        &self,
        tokens: &[Address],
        hour: u64,
    ) -> anyhow::Result<HashMap<Address, (f64, String, u64)>> {
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
        out: &mut HashMap<Address, (f64, String, u64)>,
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
                    "SELECT token, usd, source, hour FROM prices
                     WHERE hour = ?1 AND token IN ({placeholders})"
                )
            } else {
                format!(
                    "SELECT p.token, p.usd, p.source, p.hour FROM prices p
                     WHERE p.hour < ?1 AND p.hour > ?1 - 24 AND p.token IN ({placeholders})
                       AND p.hour = (
                         SELECT MAX(hour) FROM prices
                         WHERE token = p.token AND hour < ?1 AND hour > ?1 - 24)"
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
                    r.get::<_, i64>(3)? as u64,
                ))
            })?;
            for row in rows {
                let (token_s, usd, source, observed) = row?;
                if !is_usable_quote(usd) {
                    continue;
                }
                if let Some(addr) = by_token.get(token_s.as_str()) {
                    out.insert(*addr, (usd, source, observed));
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
    /// matching, because `prices.hour` stores hour buckets, not seconds. The
    /// exact bucket wins; otherwise only observations at or before it within
    /// 24h are considered. A future quote is never used, so repricing a
    /// historical block cannot borrow a later price.
    pub fn native_price_near(&self, ts: u64) -> anyhow::Result<Option<f64>> {
        let bucket = ts / 3600;
        let mut stmt = self.conn.prepare(
            "SELECT usd FROM prices
             WHERE token IN (
               '0x0000000000000000000000000000000000000000',
               '0x0')
               AND hour <= ?1 AND hour > ?1 - 24
             ORDER BY hour DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([bucket as i64])?;
        Ok(match rows.next()? {
            Some(row) => {
                let usd: f64 = row.get(0)?;
                is_usable_quote(usd).then_some(usd)
            }
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
            // A unreadable column must fail the read, not silently become a
            // zero-liquidity position that then skews JIT in-range checks.
            let pool = pool
                .parse::<Address>()
                .map_err(|e| anyhow::anyhow!("jit_open_positions.pool {pool:?}: {e}"))?;
            let owner = owner
                .parse::<Address>()
                .map_err(|e| anyhow::anyhow!("jit_open_positions.owner {owner:?}: {e}"))?;
            let liquidity = liq
                .parse::<u128>()
                .map_err(|e| anyhow::anyhow!("jit_open_positions.liquidity {liq:?}: {e}"))?;
            out.push(OpenPosition {
                pool,
                owner,
                tick_lower,
                tick_upper,
                opened_block,
                liquidity,
            });
        }
        Ok(out)
    }
    /// Persist one block's ordered LP journal (receipt-local log order).
    ///
    /// Rows are replaced, not appended: a retried block must converge instead
    /// of duplicating facts. Callers inside the block-facts transaction reach
    /// [`write_jit_block`] directly so evidence, state and checkpoint commit
    /// together.
    pub fn put_jit_block(&self, block: &LpBlock) -> anyhow::Result<()> {
        write_jit_block(&self.conn, block)
    }

    /// True when this block already has an LP journal row.
    pub fn jit_block_exists(&self, block: u64) -> anyhow::Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM jit_blocks WHERE block_number = ?1",
            [block as i64],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }
    /// Reconstruct open positions as of `block` by replaying retained journals
    /// only. Used for backfill/retry so state from later blocks cannot leak
    /// backwards. A journal gap yields no positions: an unknown history must
    /// not be replaced by the current materialized table.
    pub fn open_positions_as_of(
        &self,
        block: u64,
        min_block: u64,
    ) -> anyhow::Result<Vec<OpenPosition>> {
        if block <= min_block {
            return Ok(Vec::new());
        }
        let journals = self.jit_blocks_range(min_block, block - 1)?;
        let expected = block - min_block;
        if journals.len() as u64 != expected
            || journals
                .iter()
                .enumerate()
                .any(|(i, b)| b.block_number != min_block + i as u64)
        {
            tracing::warn!(
                block,
                min_block,
                found = journals.len(),
                "JIT journal gap; refusing future position state for backfill"
            );
            return Ok(Vec::new());
        }
        Ok(replay_open_positions(&journals))
    }
    /// Ordered journal rows `from_block..=to_block`, ascending. A corrupt row
    /// fails the read instead of being skipped: missing evidence must not look
    /// like an empty block when reconciling a position cycle.
    pub fn jit_blocks_range(&self, from_block: u64, to_block: u64) -> anyhow::Result<Vec<LpBlock>> {
        let mut stmt = self.conn.prepare(
            "SELECT journal FROM jit_blocks
              WHERE block_number BETWEEN ?1 AND ?2
              ORDER BY block_number ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![from_block as i64, to_block as i64], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            let json = row?;
            let block = serde_json::from_str::<LpBlock>(&json)
                .map_err(|e| anyhow::anyhow!("jit_blocks.journal is not a valid LpBlock: {e}"))?;
            out.push(block);
        }
        Ok(out)
    }
}

/// In-memory replay of ordered journals for an as-of read. Mirrors
/// [`apply_jit_lifecycle`] without touching the materialized table.
fn replay_open_positions(journals: &[LpBlock]) -> Vec<OpenPosition> {
    let mut open: HashMap<(Address, Address, i32, i32), (u64, u128)> = HashMap::new();
    for block in journals {
        let mut txs: Vec<_> = block.txs.iter().collect();
        txs.sort_by_key(|t| t.tx_index);
        for tx in txs {
            let mut facts: Vec<_> = tx.facts.iter().collect();
            facts.sort_by_key(|f| f.log_index);
            for f in facts {
                let key = (f.pool, f.owner, f.tick_lower, f.tick_upper);
                if f.is_mint {
                    let (opened, liquidity) =
                        open.get(&key).copied().unwrap_or((block.block_number, 0));
                    let Some(sum) = liquidity.checked_add(f.liquidity) else {
                        open.remove(&key);
                        continue;
                    };
                    if sum == 0 {
                        open.remove(&key);
                    } else {
                        open.insert(key, (opened.min(block.block_number), sum));
                    }
                } else if !f.is_collect {
                    if let Some((opened, liquidity)) = open.get(&key).copied() {
                        if f.liquidity >= liquidity {
                            open.remove(&key);
                        } else {
                            open.insert(key, (opened, liquidity - f.liquidity));
                        }
                    }
                }
            }
        }
    }
    let mut out: Vec<OpenPosition> = open
        .into_iter()
        .filter(|(_, (_, liquidity))| *liquidity > 0)
        .map(
            |((pool, owner, tick_lower, tick_upper), (opened_block, liquidity))| OpenPosition {
                pool,
                owner,
                tick_lower,
                tick_upper,
                opened_block,
                liquidity,
            },
        )
        .collect();
    out.sort_by_key(|p| (p.opened_block, p.pool, p.owner, p.tick_lower, p.tick_upper));
    out
}

/// Delete the materialized position table and replay every retained journal
/// into it. Used after a reorg, inside the unwind transaction.
pub(crate) fn replay_jit_positions(conn: &Connection) -> anyhow::Result<()> {
    let mut stmt = conn.prepare("SELECT journal FROM jit_blocks ORDER BY block_number ASC")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut journals = Vec::new();
    for row in rows {
        let json = row?;
        journals.push(
            serde_json::from_str::<LpBlock>(&json)
                .map_err(|e| anyhow::anyhow!("jit_blocks.journal is not a valid LpBlock: {e}"))?,
        );
    }
    drop(stmt);
    conn.execute("DELETE FROM jit_open_positions", [])?;
    for block in &journals {
        apply_jit_lifecycle(conn, block)?;
    }
    Ok(())
}

/// Insert/replace one journal row on a connection that may already hold the
/// block-facts transaction.
pub(crate) fn write_jit_block(conn: &Connection, block: &LpBlock) -> anyhow::Result<()> {
    let journal = serde_json::to_string(block).map_err(|e| {
        anyhow::anyhow!("serialize LP journal for block {}: {e}", block.block_number)
    })?;
    conn.execute(
        "INSERT OR REPLACE INTO jit_blocks
           (block_number, block_hash, parent_hash, ts, complete, journal)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            block.block_number as i64,
            format!("{:#x}", block.hash),
            block.parent_hash.map(|h| format!("{h:#x}")),
            block.ts as i64,
            block.complete as i64,
            journal,
        ],
    )?;
    Ok(())
}

/// Apply one block's LP facts to the open-position state in receipt order.
///
/// This replaces overwrite-on-mint/delete-on-burn: a Mint adds liquidity and
/// keeps the earliest opening block, a partial Burn leaves the remainder open,
/// and a Mint after a full Burn is a distinct reopen. Collects move value but
/// do not change open liquidity.
pub(crate) fn apply_jit_lifecycle(conn: &Connection, journal: &LpBlock) -> anyhow::Result<()> {
    let read_liquidity = |f: &crate::explorer::types::JitFact| -> anyhow::Result<Option<u128>> {
        let v: Option<String> = conn
            .query_row(
                "SELECT liquidity FROM jit_open_positions
                  WHERE pool = ?1 AND owner = ?2 AND tick_lower = ?3 AND tick_upper = ?4",
                rusqlite::params![
                    format!("{:#x}", f.pool),
                    format!("{:#x}", f.owner),
                    f.tick_lower as i64,
                    f.tick_upper as i64,
                ],
                |r| r.get(0),
            )
            .map(Some)
            .unwrap_or(None);
        match v {
            Some(s) => s
                .parse::<u128>()
                .map(Some)
                .map_err(|e| anyhow::anyhow!("jit_open_positions.liquidity {s:?}: {e}")),
            None => Ok(None),
        }
    };
    let delete = |f: &crate::explorer::types::JitFact| -> anyhow::Result<()> {
        conn.execute(
            "DELETE FROM jit_open_positions
              WHERE pool = ?1 AND owner = ?2 AND tick_lower = ?3 AND tick_upper = ?4",
            rusqlite::params![
                format!("{:#x}", f.pool),
                format!("{:#x}", f.owner),
                f.tick_lower as i64,
                f.tick_upper as i64,
            ],
        )?;
        Ok(())
    };
    let upsert = |f: &crate::explorer::types::JitFact, liquidity: u128| -> anyhow::Result<()> {
        conn.execute(
            "INSERT INTO jit_open_positions
               (pool, owner, tick_lower, tick_upper, opened_block, liquidity)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(pool, owner, tick_lower, tick_upper) DO UPDATE SET
               liquidity = excluded.liquidity,
               opened_block = MIN(jit_open_positions.opened_block, excluded.opened_block)",
            rusqlite::params![
                format!("{:#x}", f.pool),
                format!("{:#x}", f.owner),
                f.tick_lower as i64,
                f.tick_upper as i64,
                journal.block_number as i64,
                liquidity.to_string(),
            ],
        )?;
        Ok(())
    };
    for tx in &journal.txs {
        for f in &tx.facts {
            let current = read_liquidity(f)?;
            if f.is_mint {
                let open = current.unwrap_or(0);
                let Some(sum) = open.checked_add(f.liquidity) else {
                    // An overflowing lifetime is not a position we can track;
                    // drop it rather than keep a wrong smaller number.
                    delete(f)?;
                    continue;
                };
                if sum > 0 {
                    upsert(f, sum)?;
                } else {
                    delete(f)?;
                }
            } else if !f.is_collect {
                match current {
                    Some(open) if f.liquidity >= open => delete(f)?,
                    Some(open) => upsert(f, open - f.liquidity)?,
                    None => {}
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::jit_accounting::{LpBlock, LpTx};
    use crate::explorer::store::ExplorerStore;
    use crate::explorer::types::JitFact;
    use alloy::primitives::{b256, B256, U256};

    fn position_fact(is_mint: bool, liquidity: u128, log_index: u64) -> JitFact {
        JitFact {
            tx_index: 0,
            log_index,
            pool: Address::repeat_byte(0x33),
            owner: Address::repeat_byte(0x11),
            tick_lower: -10,
            tick_upper: 10,
            is_mint,
            is_collect: false,
            sender: None,
            recipient: None,
            liquidity,
            amount0: U256::ZERO,
            amount1: U256::ZERO,
            bin_amm: false,
        }
    }

    fn journal(block_number: u64, seed: u8, facts: Vec<JitFact>) -> LpBlock {
        LpBlock {
            block_number,
            hash: B256::repeat_byte(seed),
            parent_hash: None,
            ts: block_number * 12,
            complete: true,
            txs: vec![LpTx {
                tx_hash: b256!("00000000000000000000000000000000000000000000000000000000000000aa"),
                tx_index: 0,
                from: Address::repeat_byte(0x11),
                to: None,
                facts,
                transfers: vec![],
                swaps: vec![],
                gas: None,
                other_activity: false,
            }],
            swaps: vec![],
        }
    }

    fn ingest(store: &ExplorerStore, block: &LpBlock) -> anyhow::Result<()> {
        store.put_jit_block(block)?;
        apply_jit_lifecycle(store.connection(), block)
    }

    /// Additions add liquidity, partial burns leave the remainder open, and a
    /// Mint after a full Burn is a distinct reopen — not an overwrite.
    #[test]
    fn lifecycle_is_additive_and_ordered() {
        let store = ExplorerStore::open_in_memory().expect("open store");
        ingest(&store, &journal(1, 0x01, vec![position_fact(true, 100, 1)])).expect("mint");
        assert_eq!(store.open_positions(0).expect("open")[0].liquidity, 100);

        ingest(&store, &journal(2, 0x02, vec![position_fact(false, 40, 1)])).expect("partial");
        let open = store.open_positions(0).expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(
            open[0].liquidity, 60,
            "partial burn must leave the remainder"
        );

        ingest(&store, &journal(3, 0x03, vec![position_fact(false, 60, 1)])).expect("close");
        assert!(store.open_positions(0).expect("open").is_empty());

        ingest(&store, &journal(4, 0x04, vec![position_fact(true, 30, 1)])).expect("reopen");
        let open = store.open_positions(0).expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].liquidity, 30);
        assert_eq!(open[0].opened_block, 4, "reopen is a new position");
    }

    /// Liquidity added while a position is open is summed, and the earliest
    /// opening block is kept (the cycle still starts there).
    #[test]
    fn addition_while_open_sums_and_keeps_earliest_block() {
        let store = ExplorerStore::open_in_memory().expect("open store");
        ingest(&store, &journal(5, 0x05, vec![position_fact(true, 100, 1)])).expect("mint");
        ingest(&store, &journal(6, 0x06, vec![position_fact(true, 25, 1)])).expect("add");
        let open = store.open_positions(0).expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].liquidity, 125);
        assert_eq!(open[0].opened_block, 5);
    }

    /// A reorg removes the journal rows from the abandoned chain and rebuilds
    /// open state from surviving evidence: a position opened before the fork
    /// must come back as open instead of vanishing with its deleted close.
    #[test]
    fn reorg_rebuilds_open_state_from_surviving_journal() {
        let store = ExplorerStore::open_in_memory().expect("open store");
        ingest(&store, &journal(1, 0x11, vec![position_fact(true, 100, 1)])).expect("mint");
        ingest(&store, &journal(2, 0x12, vec![position_fact(false, 40, 1)])).expect("partial");
        ingest(&store, &journal(3, 0x13, vec![position_fact(false, 60, 1)])).expect("close");
        assert!(store.open_positions(0).expect("open").is_empty());

        store.unwind_from(2).expect("unwind");
        let open = store.open_positions(0).expect("open");
        assert_eq!(open.len(), 1, "opening before the fork survives");
        assert_eq!(open[0].liquidity, 100);
        assert_eq!(
            store.jit_blocks_range(1, 10).expect("journal").len(),
            1,
            "abandoned journal rows are gone"
        );
    }

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
        // A later observation is not a historical price.
        let got = store.native_price_near(199 * 3600 + 3500).unwrap();
        assert_eq!(got, None, "future quotes must never be used");
        store.put_price(Address::ZERO, 198, 5.0, "test").unwrap();
        let got = store.native_price_near(199 * 3600 + 3500).unwrap();
        assert_eq!(got, Some(5.0), "the newest observation at/before ts wins");
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
