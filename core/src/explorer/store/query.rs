//! Query / stats / report surface for ExplorerStore.
use alloy::primitives::{Address, U256};

use crate::explorer::types::MevKind;
use crate::types::{MevOpportunity, Strategy};

use super::{
    latest_native_price, map_feed_row, map_mev_op_row, map_report_row, map_stats_row,
    stats_grouped, top_grouped, ExplorerStore, FeedRow, MevOpRow, OpportunityInput, OpportunityRow,
    OverviewRow, PoolSwapRow, ReportOverview, ReportRow, StatsRow, TraceVerification,
};

impl ExplorerStore {
    /// Insert one opportunity row from a `ResultsFile` entry.
    pub fn insert_opportunity(&self, row: OpportunityInput<'_>) -> anyhow::Result<()> {
        let OpportunityInput {
            run_id,
            chain,
            block_number,
            tx_index,
            strategy,
            pool_a,
            pool_b,
            token_in,
            token_out,
            input_amount,
            expected_profit,
            gas_cost_wei,
            path,
            timestamp,
            mempool_only,
            confidence,
            sender,
            tx_hash,
            detection_path,
            canonical_id,
        } = row;
        self.conn.execute(
            "INSERT INTO opportunities
               (run_id, chain, block_number, tx_index, strategy, pool_a, pool_b,
                token_in, token_out, input_amount, expected_profit, gas_cost_wei,
                path, timestamp, mempool_only, confidence, sender, tx_hash,
                detection_path, canonical_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            rusqlite::params![
                run_id,
                chain,
                block_number as i64,
                tx_index.map(|v| v as i64),
                strategy,
                pool_a.map(|a| format!("{:#x}", a)),
                pool_b.map(|a| format!("{:#x}", a)),
                token_in.map(|a| format!("{:#x}", a)),
                token_out.map(|a| format!("{:#x}", a)),
                input_amount.map(|v| v.to_string()),
                expected_profit.map(|v| v.to_string()),
                gas_cost_wei.map(|v| v.to_string()),
                path,
                timestamp.map(|v| v as i64),
                mempool_only as i64,
                confidence,
                sender.map(|a| format!("{:#x}", a)),
                tx_hash.map(|h| format!("{:#x}", h)),
                detection_path,
                canonical_id,
            ],
        )?;
        Ok(())
    }
    /// Live feed: most recent ops (optionally filtered by kind).
    pub fn feed_tail(&self, limit: usize, kinds: &[MevKind]) -> anyhow::Result<Vec<FeedRow>> {
        let order = if kinds.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
            format!("WHERE kind IN ({})", list.join(","))
        };
        let sql = format!(
            "SELECT ts, block_number, kind, profit_token, profit_amount, profit_usd, gas_cost_usd,
                    net_profit_usd, eoa, tx_hash, route_json, details_json
             FROM mev_ops {order}
             ORDER BY block_number DESC, tx_index DESC, id DESC LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_feed_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        let native = latest_native_price(&self.conn)?;
        for row in &mut out {
            row.native_price_usd = native;
        }
        Ok(out)
    }
    /// Period stats grouped by kind (`stats`).
    pub fn stats_by_kind(&self, since_ts: u64) -> anyhow::Result<Vec<StatsRow>> {
        self.stats_by_kind_filtered(since_ts, None)
    }
    /// `stats_by_kind` restricted to one `kind` value (`stats --kind`).
    pub fn stats_by_kind_filtered(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        stats_grouped(&self.conn, "kind", since_ts, kind)
    }
    /// Sender leaderboard (`top --by sender`).
    pub fn top_senders(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        self.top_senders_filtered(since_ts, limit, None)
    }
    /// `top_senders` restricted to one `kind` value (`stats --kind`).
    pub fn top_senders_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        top_grouped(&self.conn, "eoa", since_ts, limit, kind)
    }
    /// Competitor leaderboard: group by bot identity
    /// `COALESCE(contract, eoa)` so EOAs sharing an executor collapse.
    pub fn top_competitors_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT COALESCE(NULLIF(contract, ''), eoa) AS label,
                    COUNT(*) AS ops,
                    COALESCE(SUM(profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(net_profit_usd), 0) AS net_usd
             FROM mev_ops
             WHERE ts >= {since_ts} {kind_clause}
             GROUP BY label
             ORDER BY gross_usd DESC
             LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_stats_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Most profitable tokens (`top --by token`).
    pub fn top_tokens(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        self.top_tokens_filtered(since_ts, limit, None)
    }
    /// `top_tokens` restricted to one `kind` value.
    pub fn top_tokens_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        top_grouped(&self.conn, "profit_token", since_ts, limit, kind)
    }
    /// Busiest pools (`top --by pool`) — pool list comes from route_json.
    pub fn top_pools(&self, since_ts: u64, limit: usize) -> anyhow::Result<Vec<StatsRow>> {
        self.top_pools_filtered(since_ts, limit, None)
    }
    /// `top_pools` restricted to one `kind` value (`stats --kind`).
    pub fn top_pools_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        // Pools live inside route_json; extract with json_each.
        let kind_clause = kind
            .map(|k| format!("AND m.kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT j.value->>'$.pool' AS label,
                    COUNT(DISTINCT m.id) AS ops,
                    COALESCE(SUM(m.profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(m.net_profit_usd), 0) AS net_usd
             FROM mev_ops m, json_each(COALESCE(m.route_json, '[]')) j
             WHERE m.ts >= {since_ts} {kind_clause} AND j.value->>'$.pool' IS NOT NULL
             GROUP BY label
             ORDER BY gross_usd DESC
             LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_stats_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Daily breakdown (`stats --window day`).
    pub fn stats_daily(&self, since_ts: u64) -> anyhow::Result<Vec<StatsRow>> {
        self.stats_daily_filtered(since_ts, None)
    }
    /// `stats_daily` restricted to one `kind` value (`stats --kind`).
    pub fn stats_daily_filtered(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT date(ts, 'unixepoch') AS label,
                    COUNT(*) AS ops,
                    COALESCE(SUM(profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(net_profit_usd), 0) AS net_usd
             FROM mev_ops
             WHERE ts >= {since_ts} {kind_clause}
             GROUP BY label
             ORDER BY label DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_stats_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Full op detail for `show <TX>`.
    pub fn ops_for_tx(&self, tx_hash: &str) -> anyhow::Result<Vec<MevOpRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, flashloan_fee_usd, volume_usd, net_profit_usd,
                    route_json, victim_hashes, details_json, detector, created_at
             FROM mev_ops WHERE tx_hash = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([tx_hash], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Distinct pools seen in realized swaps: `(pool, amm, token_in, token_out)`.
    ///
    /// Re-scoped for the detector corpus (MEV-VERIFICATION §D): the backtest
    /// detector's pool registry is normally built by live runs, but the repo's
    /// seed block cache holds none, so the corpus re-seeds it from the swap
    /// endpoints the realizer actually saw, then lets `init_from_rpc` hydrate
    /// fee/tick/reserves over RPC before detection.
    pub fn pools_from_swaps(&self) -> anyhow::Result<Vec<PoolSwapRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT pool, amm, token_in, token_out FROM swaps
             WHERE pool IS NOT NULL AND token_in IS NOT NULL AND token_out IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Detector opportunities anchored to a tx hash (`show` MEV verdict;
    /// tx_hash-only realization join per MEV-VERIFICATION §A).
    pub fn opportunities_for_tx(&self, tx_hash: &str) -> anyhow::Result<Vec<OpportunityRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, block_number, tx_index, strategy, pool_a, pool_b,
                    token_in, token_out, expected_profit, gas_cost_wei,
                    mempool_only, detection_path, canonical_id, tx_hash
             FROM opportunities
             WHERE tx_hash = ?1
             ORDER BY block_number",
        )?;
        let rows = stmt.query_map([tx_hash], |r| {
            Ok(OpportunityRow {
                run_id: r.get(0)?,
                block_number: r.get::<_, i64>(1)? as u64,
                tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                strategy: r.get(3)?,
                pool_a: r.get(4)?,
                pool_b: r.get(5)?,
                token_in: r.get(6)?,
                token_out: r.get(7)?,
                expected_profit: r.get(8)?,
                gas_cost_wei: r.get(9)?,
                mempool_only: r.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
                detection_path: r.get(11)?,
                canonical_id: r.get(12)?,
                tx_hash: r.get(13)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Realized ops in a block window (`validate` ground truth).
    pub fn ops_in_range(
        &self,
        from_block: u64,
        to_block: u64,
        kinds: &[MevKind],
    ) -> anyhow::Result<Vec<MevOpRow>> {
        let kind_filter = if kinds.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
            format!("AND kind IN ({})", list.join(","))
        };
        let sql = format!(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, flashloan_fee_usd, volume_usd, net_profit_usd,
                    route_json, victim_hashes, details_json, detector, created_at
             FROM mev_ops
             WHERE block_number BETWEEN {from_block} AND {to_block} {kind_filter}
             ORDER BY block_number, tx_index"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Opportunities in a block window (`validate` scanner side).
    pub fn opportunities_in_range(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<OpportunityRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, block_number, tx_index, strategy, pool_a, pool_b,
                    token_in, token_out, expected_profit, gas_cost_wei,
                    mempool_only, detection_path, canonical_id, tx_hash
             FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3
             ORDER BY block_number",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| {
                Ok(OpportunityRow {
                    run_id: r.get(0)?,
                    block_number: r.get::<_, i64>(1)? as u64,
                    tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    strategy: r.get(3)?,
                    pool_a: r.get(4)?,
                    pool_b: r.get(5)?,
                    token_in: r.get(6)?,
                    token_out: r.get(7)?,
                    expected_profit: r.get(8)?,
                    gas_cost_wei: r.get(9)?,
                    mempool_only: r.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
                    detection_path: r.get(11)?,
                    canonical_id: r.get(12)?,
                    tx_hash: r.get(13)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Reconstruct the `MevOpportunity` list recorded for a run id
    /// (SQLite-backed `report`). Rows whose strategy string cannot be parsed
    /// are skipped with a warning. Fields not persisted in the `opportunities`
    /// table (raw/slippage profit variants, JIT tick bounds, sandwich
    /// victim/backrun indices) come back as defaults.
    pub fn opportunities_by_run(&self, run_id: &str) -> anyhow::Result<Vec<MevOpportunity>> {
        let mut stmt = self.conn.prepare(
            "SELECT block_number, tx_index, strategy, pool_a, pool_b,
                    token_in, token_out, input_amount, expected_profit, gas_cost_wei,
                    path, timestamp, mempool_only, confidence, sender, tx_hash,
                    detection_path, canonical_id
             FROM opportunities
             WHERE run_id = ?1
             ORDER BY block_number, tx_index",
        )?;
        let rows = stmt.query_map(rusqlite::params![run_id], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, Option<i64>>(1)?.map(|v| v as usize),
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                r.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<String>>(14)?,
                r.get::<_, Option<String>>(15)?,
                r.get::<_, Option<String>>(16)?,
                r.get::<_, Option<String>>(17)?,
            ))
        })?;
        let parse_addr = |s: &Option<String>| -> anyhow::Result<Address> {
            match s {
                Some(v) => v.parse::<Address>().map_err(|e| {
                    anyhow::anyhow!(
                        "opportunities table holds unparseable address '{v}' (schema drift?): {e}"
                    )
                }),
                None => Ok(Address::ZERO),
            }
        };
        let mut out = Vec::new();
        for row in rows {
            let (
                block_number,
                tx_index,
                strategy_str,
                pool_a,
                pool_b,
                token_in,
                token_out,
                input_amount,
                expected_profit,
                gas_cost_wei,
                path,
                timestamp,
                mempool_only,
                confidence,
                sender,
                tx_hash,
                detection_path,
                canonical_id,
            ) = row?;
            let strategy = match strategy_str.parse::<Strategy>() {
                Ok(s) => s,
                Err(_) => {
                    tracing::warn!(
                        "opportunities_by_run: skipping row with unknown strategy '{strategy_str}'"
                    );
                    continue;
                }
            };
            out.push(MevOpportunity {
                canonical_id,
                block_number,
                tx_index: tx_index.unwrap_or(0),
                strategy,
                pool_a: parse_addr(&pool_a)?,
                pool_b: parse_addr(&pool_b)?,
                token_in: parse_addr(&token_in)?,
                token_out: parse_addr(&token_out)?,
                input_amount: match &input_amount {
                    Some(v) => v.parse::<U256>().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable input_amount '{v}' (schema drift?): {e}"
                        )
                    })?,
                    None => U256::ZERO,
                },
                expected_profit: match &expected_profit {
                    Some(v) => v.parse::<U256>().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable expected_profit '{v}' (schema drift?): {e}"
                        )
                    })?,
                    None => U256::ZERO,
                },
                raw_profit: None,
                profit_slippage_p1: None,
                profit_slippage_m1: None,
                profit_slippage_p2: None,
                profit_slippage_m2: None,
                gas_cost_wei: match &gas_cost_wei {
                    Some(v) => v.parse::<u128>().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable gas_cost_wei '{v}' (schema drift?): {e}"
                        )
                    })?,
                    None => 0,
                },
                timestamp: timestamp.unwrap_or(0),
                path: match &path {
                    Some(p) => {
                        if p.is_empty() {
                            None
                        } else {
                            let parsed: anyhow::Result<Vec<Address>> =
                                p.split(',').map(|a| {
                                    a.parse::<Address>().map_err(|e| {
                                        anyhow::anyhow!(
                                            "opportunities table holds unparseable path address '{a}' (schema drift?): {e}"
                                        )
                                    })
                                }).collect();
                            Some(parsed?).filter(|v| !v.is_empty())
                        }
                    }
                    None => None,
                },
                tick_lower: None,
                tick_upper: None,
                liquidity_amount: None,
                victim_tx_index: None,
                backrun_tx_index: None,
                mempool_only,
                confidence: confidence.as_deref().and_then(|v| v.parse().ok()),
                sender: match &sender {
                    Some(v) => Some(v.parse::<Address>().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable sender '{v}' (schema drift?): {e}"
                        )
                    })?),
                    None => None,
                },
                tx_hash: match &tx_hash {
                    Some(v) => Some(v.parse().map_err(|e| {
                        anyhow::anyhow!(
                            "opportunities table holds unparseable tx_hash '{v}' (schema drift?): {e}"
                        )
                    })?),
                    None => None,
                },
                detection_path: detection_path
                    .as_deref()
                    .and_then(|v| v.parse::<crate::types::DetectionPath>().ok()),
            });
        }
        Ok(out)
    }
    /// Blocks with realized ops of a kind (block-level recall, T3).
    pub fn blocks_with_kind(
        &self,
        from_block: u64,
        to_block: u64,
        kind: MevKind,
    ) -> anyhow::Result<std::collections::HashSet<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT block_number FROM mev_ops
             WHERE kind = ?1 AND block_number BETWEEN ?2 AND ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![kind.as_str(), from_block as i64, to_block as i64],
            |r| r.get::<_, i64>(0),
        )?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r? as u64);
        }
        Ok(out)
    }
    /// Blocks where the scanner recorded opportunities (M7 coverage check).
    pub fn opportunity_blocks(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<std::collections::HashSet<u64>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT block_number FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| r.get::<_, i64>(0),
        )?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r? as u64);
        }
        Ok(out)
    }
    /// Count of ops by kind since a timestamp (quick overview).
    pub fn op_count_since(&self, since_ts: u64) -> anyhow::Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM mev_ops WHERE ts >= ?1",
            [since_ts as i64],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }
    /// Pools seen in realized ops (missing-pool report, `--emit-missing-pools`).
    pub fn distinct_route_pools(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<String>> {
        let sql = format!(
            "SELECT DISTINCT j.value->>'$.pool' AS pool
             FROM mev_ops m, json_each(COALESCE(m.route_json, '[]')) j
             WHERE m.block_number BETWEEN {from_block} AND {to_block}
               AND j.value->>'$.pool' IS NOT NULL
             ORDER BY pool"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Whole-window overview stats (`stats` header block).
    pub fn stats_overview(&self, since_ts: u64) -> anyhow::Result<OverviewRow> {
        self.stats_overview_filtered(since_ts, None)
    }
    /// `stats_overview` restricted to one `kind` value (`stats --kind`).
    pub fn stats_overview_filtered(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<OverviewRow> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(profit_usd), 0),
                    COALESCE(SUM(net_profit_usd), 0),
                    COALESCE(SUM(gas_cost_usd), 0),
                    COALESCE(MAX(profit_usd), 0),
                    COUNT(DISTINCT eoa)
             FROM mev_ops WHERE ts >= {since_ts} {kind_clause}"
        );
        let row = self.conn.query_row(&sql, [], |r| {
            Ok(OverviewRow {
                ops: r.get::<_, i64>(0)?,
                gross_usd: r.get(1)?,
                net_usd: r.get(2)?,
                gas_usd: r.get(3)?,
                highest_single_usd: r.get(4)?,
                searchers: r.get::<_, i64>(5)?,
            })
        })?;
        Ok(row)
    }
    /// All ops since a timestamp (`export`).
    pub fn ops_since(&self, since_ts: u64) -> anyhow::Result<Vec<MevOpRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, flashloan_fee_usd, volume_usd, net_profit_usd,
                    route_json, victim_hashes, details_json, detector, created_at
             FROM mev_ops WHERE ts >= ?1 ORDER BY block_number, tx_index",
        )?;
        let rows = stmt.query_map([since_ts as i64], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Window-wide revenue aggregates (bare `explorer` overview).
    pub fn report_window_overview(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<ReportOverview> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(volume_usd), 0),
                    COALESCE(SUM(profit_usd), 0),
                    COALESCE(SUM(gas_cost_usd), 0),
                    COALESCE(SUM(flashloan_fee_usd), 0),
                    COALESCE(SUM(net_profit_usd), 0),
                    COALESCE(MAX(profit_usd), 0),
                    COUNT(DISTINCT eoa),
                    COUNT(DISTINCT COALESCE(NULLIF(contract, ''), eoa))
             FROM mev_ops WHERE ts >= {since_ts} {kind_clause}"
        );
        let row = self.conn.query_row(&sql, [], |r| {
            Ok(ReportOverview {
                ops: r.get::<_, i64>(0)?,
                volume_usd: r.get(1)?,
                gross_usd: r.get(2)?,
                gas_usd: r.get(3)?,
                flash_fee_usd: r.get(4)?,
                net_usd: r.get(5)?,
                highest_single_usd: r.get(6)?,
                searchers: r.get::<_, i64>(7)?,
                competitors: r.get::<_, i64>(8)?,
            })
        })?;
        Ok(row)
    }
    /// Per-kind revenue rows in a window (bare `explorer` kinds table).
    pub fn report_by_kind(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<ReportRow>> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT kind AS label, COUNT(*) AS ops,
                    COALESCE(SUM(volume_usd), 0),
                    COALESCE(SUM(profit_usd), 0) AS gross,
                    COALESCE(SUM(gas_cost_usd), 0),
                    COALESCE(SUM(flashloan_fee_usd), 0),
                    COALESCE(SUM(net_profit_usd), 0)
             FROM mev_ops
             WHERE ts >= {since_ts} {kind_clause}
             GROUP BY kind
             ORDER BY gross DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_report_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Daily revenue series in a window (bare `explorer` trend).
    pub fn report_daily(
        &self,
        since_ts: u64,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<ReportRow>> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT date(ts, 'unixepoch') AS label, COUNT(*),
                    COALESCE(SUM(volume_usd), 0),
                    COALESCE(SUM(profit_usd), 0),
                    COALESCE(SUM(gas_cost_usd), 0),
                    COALESCE(SUM(flashloan_fee_usd), 0),
                    COALESCE(SUM(net_profit_usd), 0)
             FROM mev_ops
             WHERE ts >= {since_ts} {kind_clause}
             GROUP BY label
             ORDER BY label DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_report_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Highest net-value ops in a window (drill-down detail list), by
    /// `net_profit_usd` descending.
    pub fn top_ops(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<MevOpRow>> {
        let kind_clause = kind
            .map(|k| format!("AND kind = '{k}'"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, flashloan_fee_usd, volume_usd, net_profit_usd,
                    route_json, victim_hashes, details_json, detector, created_at
             FROM mev_ops
             WHERE ts >= {since_ts} {kind_clause}
             ORDER BY COALESCE(net_profit_usd, 0) DESC, id DESC
             LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_mev_op_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
    /// Blocks indexed whose timestamp falls in the window (coverage). Each
    /// classified block persists a `blocks` row, so this counts indexed blocks
    /// even when they carried no realized ops.
    pub fn blocks_in_window(&self, since_ts: u64) -> anyhow::Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM blocks WHERE ts >= ?1",
            [since_ts as i64],
            |r| r.get(0),
        )?;
        Ok(n)
    }
    pub fn opportunity_pools(
        &self,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT pool_a, pool_b FROM opportunities
             WHERE chain = ?1 AND block_number BETWEEN ?2 AND ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![chain, from_block as i64, to_block as i64],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            let (a, b) = r?;
            if let Some(a) = a {
                out.insert(a);
            }
            if let Some(b) = b {
                out.insert(b);
            }
        }
        Ok(out)
    }
    /// Store `show --trace` verification results on the op's details.
    pub fn mark_trace_verified(
        &self,
        tx_hash: &str,
        verification: &TraceVerification,
    ) -> anyhow::Result<()> {
        for op in self.ops_for_tx(tx_hash)? {
            let mut details: serde_json::Value = op
                .details_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .filter(serde_json::Value::is_object)
                .unwrap_or(serde_json::json!({}));

            if let Some(object) = details.as_object_mut() {
                for key in [
                    "trace_verified",
                    "trace_check",
                    "trace_check_reason",
                    "trace_profit_usd",
                    "expected_profit_usd",
                    "profit_error_pct",
                    "trace_native_delta_wei",
                    "trace_note",
                ] {
                    object.remove(key);
                }
            }
            if let Some(check) = &verification.trace_check {
                details["trace_verified"] = serde_json::json!(true);
                details["trace_check"] = serde_json::json!(check);
            }
            if let Some(reason) = &verification.trace_check_reason {
                details["trace_check_reason"] = serde_json::json!(reason);
            }
            if let Some(usd) = verification.trace_profit_usd {
                details["trace_profit_usd"] = serde_json::json!(usd);
            }
            if let Some(usd) = verification.expected_profit_usd {
                details["expected_profit_usd"] = serde_json::json!(usd);
            }
            if let Some(pct) = verification.profit_error_pct {
                details["profit_error_pct"] = serde_json::json!(pct);
            }
            if !verification.native_delta_wei.is_empty() {
                details["trace_native_delta_wei"] =
                    serde_json::json!(verification.native_delta_wei);
            }
            if !verification.note.is_empty() {
                details["trace_note"] = serde_json::json!(verification.note);
            }
            self.conn.execute(
                "UPDATE mev_ops SET details_json = ?1 WHERE id = ?2",
                rusqlite::params![details.to_string(), op.id],
            )?;
        }
        Ok(())
    }
}
