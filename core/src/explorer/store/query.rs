//! Query / stats / report surface for ExplorerStore.
use alloy::primitives::{Address, U256};

use crate::explorer::types::{Confidence, MevKind};
use crate::types::{MevOpportunity, Strategy};

use super::{
    map_mev_op_row, map_report_row, map_stats_row, top_grouped, ExplorerStore, MevOpRow,
    OpportunityInput, PoolSwapRow, RejectCount, ReportOverview, ReportQuality, ReportRow, StatsRow,
};

/// SQL fragment for optional `--kind` / `--confidence` filters on `mev_ops`.
///
/// Takes the enums (not `&str`) so arbitrary strings cannot reach SQL: the
/// SQL spelling is always [`MevKind::as_str`] / [`Confidence::as_str`].
fn report_filter_clause(kind: Option<MevKind>, confidence: Option<Confidence>) -> String {
    let mut s = String::new();
    if let Some(k) = kind {
        s.push_str(&format!("AND kind = '{}' ", k.as_str()));
    }
    if let Some(c) = confidence {
        s.push_str(&format!("AND confidence = '{}' ", c.as_str()));
    }
    s
}

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
    /// `top_senders` restricted to one `kind` value (`stats --kind`).
    pub fn top_senders_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<MevKind>,
        confidence: Option<Confidence>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        top_grouped(&self.conn, "eoa", since_ts, limit, kind, confidence)
    }
    /// Competitor leaderboard: group by bot identity
    /// `COALESCE(contract, eoa)` so EOAs sharing an executor collapse.
    pub fn top_competitors_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<MevKind>,
        confidence: Option<Confidence>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        let filter = report_filter_clause(kind, confidence);
        let sql = format!(
            "SELECT COALESCE(NULLIF(contract, ''), eoa) AS label,
                    COUNT(*) AS ops,
                    COALESCE(SUM(profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(net_profit_usd), 0) AS net_usd
             FROM mev_ops
             WHERE ts >= {since_ts} {filter}
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
    /// `top_pools` restricted to one `kind` value (`stats --kind`).
    pub fn top_pools_filtered(
        &self,
        since_ts: u64,
        limit: usize,
        kind: Option<MevKind>,
        confidence: Option<Confidence>,
    ) -> anyhow::Result<Vec<StatsRow>> {
        // Pools live inside route_json; extract with json_each.
        let mut filter = String::new();
        if let Some(k) = kind {
            filter.push_str(&format!("AND m.kind = '{}' ", k.as_str()));
        }
        if let Some(c) = confidence {
            filter.push_str(&format!("AND m.confidence = '{}' ", c.as_str()));
        }
        let sql = format!(
            "SELECT j.value->>'$.pool' AS label,
                    COUNT(DISTINCT m.id) AS ops,
                    COALESCE(SUM(m.profit_usd), 0) AS gross_usd,
                    COALESCE(SUM(m.net_profit_usd), 0) AS net_usd
             FROM mev_ops m, json_each(COALESCE(m.route_json, '[]')) j
             WHERE m.ts >= {since_ts} {filter} AND j.value->>'$.pool' IS NOT NULL
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
    /// Realized ops in a block window.
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
    /// Count of ops by kind since a timestamp (quick overview).
    pub fn op_count_since(&self, since_ts: u64) -> anyhow::Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM mev_ops WHERE ts >= ?1",
            [since_ts as i64],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }
    /// Window-wide revenue aggregates (bare `explorer` overview).
    pub fn report_window_overview(
        &self,
        since_ts: u64,
        kind: Option<MevKind>,
        confidence: Option<Confidence>,
    ) -> anyhow::Result<ReportOverview> {
        let filter = report_filter_clause(kind, confidence);
        // Reason counts scan `details_json`. These tokens are written verbatim
        // into the `reasons` array (and `pricing_clamped` is also a top-level
        // key), so a substring match is precise for the values `note_pricing_issue`
        // emits. `SUM` skips NULL, so `gross` already counts only priced ops
        // (unpriced rows carry NULL `profit_usd`).
        let sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(volume_usd), 0),
                    COALESCE(SUM(profit_usd), 0),
                    COALESCE(SUM(gas_cost_usd), 0),
                    COALESCE(SUM(flashloan_fee_usd), 0),
                    COALESCE(SUM(net_profit_usd), 0),
                    COALESCE(MAX(profit_usd), 0),
                    COUNT(DISTINCT eoa),
                    COUNT(DISTINCT COALESCE(NULLIF(contract, ''), eoa)),
                    COALESCE(SUM(CASE WHEN profit_usd IS NOT NULL THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN profit_usd IS NULL THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN confidence = 'exact' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN confidence = 'inferred' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN confidence = 'exact' THEN net_profit_usd ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN confidence = 'inferred' THEN net_profit_usd ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN details_json LIKE '%\"usd_approximate\":true%' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN details_json LIKE '%NATIVE_UNPRICED%' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN details_json LIKE '%MULTI_ASSET_PRICING%' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN details_json LIKE '%pricing_clamped%' THEN 1 ELSE 0 END), 0)
             FROM mev_ops WHERE ts >= {since_ts} {filter}"
        );
        let row = self.conn.query_row(&sql, [], |r| {
            let ops = r.get::<_, i64>(0)?;
            let ops_priced = r.get::<_, i64>(9).unwrap_or(0);
            Ok(ReportOverview {
                ops,
                volume_usd: r.get(1)?,
                gross_usd: r.get(2)?,
                gas_usd: r.get(3)?,
                flash_fee_usd: r.get(4)?,
                net_usd: r.get(5)?,
                highest_single_usd: r.get(6)?,
                searchers: r.get::<_, i64>(7)?,
                competitors: r.get::<_, i64>(8)?,
                quality: ReportQuality {
                    ops_priced,
                    ops_unpriced: r.get::<_, i64>(10).unwrap_or(0),
                    ops_exact: r.get::<_, i64>(11).unwrap_or(0),
                    ops_inferred: r.get::<_, i64>(12).unwrap_or(0),
                    net_usd_exact: r.get(13).unwrap_or(0.0),
                    net_usd_inferred: r.get(14).unwrap_or(0.0),
                    ops_approx: r.get::<_, i64>(15).unwrap_or(0),
                    gross_usd_priced_only: r.get(2)?,
                    pricing_coverage_pct: if ops == 0 {
                        100.0
                    } else {
                        (ops_priced as f64 / ops as f64) * 100.0
                    },
                    ops_native_unpriced: r.get::<_, i64>(16).unwrap_or(0),
                    ops_multi_asset: r.get::<_, i64>(17).unwrap_or(0),
                    ops_pricing_clamped: r.get::<_, i64>(18).unwrap_or(0),
                },
            })
        })?;
        Ok(row)
    }
    /// `explorer_classify` reject counts by reason inside a window.
    ///
    /// Window membership uses the block's indexed ts when the block row
    /// exists, falling back to the reject row's `created_at` (approximate for
    /// backfilled blocks whose block row is missing).
    pub fn report_classify_rejects(&self, since_ts: u64) -> anyhow::Result<Vec<RejectCount>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.reject_reason, COUNT(*)
             FROM rejected_candidates r
             LEFT JOIN blocks b ON b.block_number = r.block_number
             WHERE r.strategy = 'explorer_classify'
               AND COALESCE(b.ts, r.created_at) >= ?1
             GROUP BY r.reject_reason
             ORDER BY COUNT(*) DESC, r.reject_reason",
        )?;
        let rows = stmt
            .query_map([since_ts as i64], |r| {
                Ok(RejectCount {
                    reason: r.get(0)?,
                    count: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
    /// Per-kind revenue rows in a window (bare `explorer` kinds table).
    pub fn report_by_kind(
        &self,
        since_ts: u64,
        kind: Option<MevKind>,
        confidence: Option<Confidence>,
    ) -> anyhow::Result<Vec<ReportRow>> {
        let filter = report_filter_clause(kind, confidence);
        let sql = format!(
            "SELECT kind AS label, COUNT(*) AS ops,
                    COALESCE(SUM(volume_usd), 0),
                    COALESCE(SUM(profit_usd), 0) AS gross,
                    COALESCE(SUM(gas_cost_usd), 0),
                    COALESCE(SUM(flashloan_fee_usd), 0),
                    COALESCE(SUM(net_profit_usd), 0)
             FROM mev_ops
             WHERE ts >= {since_ts} {filter}
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
        kind: Option<MevKind>,
        confidence: Option<Confidence>,
    ) -> anyhow::Result<Vec<ReportRow>> {
        let filter = report_filter_clause(kind, confidence);
        let sql = format!(
            "SELECT date(ts, 'unixepoch') AS label, COUNT(*),
                    COALESCE(SUM(volume_usd), 0),
                    COALESCE(SUM(profit_usd), 0),
                    COALESCE(SUM(gas_cost_usd), 0),
                    COALESCE(SUM(flashloan_fee_usd), 0),
                    COALESCE(SUM(net_profit_usd), 0)
             FROM mev_ops
             WHERE ts >= {since_ts} {filter}
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
        kind: Option<MevKind>,
        confidence: Option<Confidence>,
    ) -> anyhow::Result<Vec<MevOpRow>> {
        let filter = report_filter_clause(kind, confidence);
        let sql = format!(
            "SELECT id, block_number, tx_index, tx_hash, ts, kind, eoa, contract,
                    confidence, canonical_id, profit_token, profit_amount, profit_usd,
                    gas_cost_usd, flashloan_fee_usd, volume_usd, net_profit_usd,
                    route_json, victim_hashes, details_json, detector, created_at
             FROM mev_ops
             WHERE ts >= {since_ts} {filter}
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
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::explorer::types::{Confidence, MevKind};

    #[test]
    fn filter_clause_takes_enums_not_strings() {
        let s = report_filter_clause(Some(MevKind::Sandwich), Some(Confidence::Exact));
        assert!(s.contains("kind = 'sandwich'"));
        assert!(s.contains("confidence = 'exact'"));
    }
}
