//! Explorer store — SQLite persistence for realized-MEV facts.
//!
//! Separate database file from the scanner cache (`explorer_{chain}.sqlite`)
//! so live index writes never contend with replay-path reads. WAL mode,
//! single-writer, batched transactions. Schema is Postgres-portable.
//!
//! Layers:
//! - forensic facts: `blocks`, `txs`, `transfers`, `swaps`, `mev_ops`
//! - results layer: `opportunities` fed from `ResultsFile`
//! - rejection capture: `rejected_candidates`
//! - checkpointing: `sync_state` + `blocks_classified` (gap-safe resume)
/// `(pool, amm, token_in, token_out)` for a realized swap row.
pub(super) type PoolSwapRow = (String, Option<String>, String, String);
/// Persist-block return: `(ops_written, open_jit_positions)`.
pub(super) type PersistBlockStats = (usize, Vec<(Address, Option<Address>, u64)>);

mod facts;
mod jit_positions;
mod labels;
mod pnl;
mod query;
mod schema;
mod sync;

use alloy::primitives::{Address, B256, U256};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::explorer::types::{MevEvent, MevKind};

/// Trace-based reconciliation record for one tx (`explorer show --trace`).
///
/// Measurement-only: compares the classifier's expected USD profit
/// with the trace-observed native balance delta. Never feeds `classify_block`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceVerification {
    /// Classifier-expected USD profit (sum over the tx's ops), if priced.
    pub expected_profit_usd: Option<f64>,
    /// Trace-observed native balance delta converted to USD (signed).
    pub trace_profit_usd: Option<f64>,
    /// `(expected − trace) / expected × 100`; > 0 = classifier over-estimate.
    pub profit_error_pct: Option<f64>,
    /// Raw trace native delta in wei, signed decimal string.
    pub native_delta_wei: String,
    pub note: String,
    /// Gate verdict tag: `pass | fail | unverifiable`.
    pub trace_check: Option<String>,
    /// Human-readable reason for non-pass verdicts.
    pub trace_check_reason: Option<String>,
}

/// One persisted open concentrated-liquidity position (`jit_open_positions`).
///
/// Persists open JIT Mint positions so cross-block JIT detection survives live restarts. A row
/// is inserted on a Mint and deleted when the matching Burn arrives (or pruned
/// once `opened_block` falls outside the block-window cap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPosition {
    pub pool: Address,
    pub owner: Address,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub opened_block: u64,
    pub liquidity: u128,
}

/// One persisted realized-MEV operation row (`mev_ops`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevOpRow {
    pub id: i64,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub tx_hash: String,
    pub ts: u64,
    pub kind: String,
    pub eoa: String,
    pub contract: Option<String>,
    pub confidence: String,
    pub canonical_id: Option<String>,
    pub profit_token: Option<String>,
    pub profit_amount: Option<String>,
    pub profit_usd: Option<f64>,
    pub volume_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub flashloan_fee_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub route_json: Option<String>,
    pub victim_hashes: Option<String>,
    pub details_json: Option<String>,
    pub detector: String,
    pub created_at: u64,
}

impl MevOpRow {
    /// Parse `kind` through the domain enum (SQLite TEXT boundary).
    pub fn kind_enum(&self) -> Option<MevKind> {
        self.kind.parse().ok()
    }

    /// Parse `confidence` through the domain enum (SQLite TEXT boundary).
    pub fn confidence_enum(&self) -> Option<crate::explorer::types::Confidence> {
        self.confidence.parse().ok()
    }
}

/// Aggregate row for stats/leaderboard queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsRow {
    pub label: String,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
}

/// Live-feed row (tail of `mev_ops`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedRow {
    pub ts: u64,
    pub block_number: u64,
    pub kind: String,
    pub profit_token: Option<String>,
    pub profit_amount: Option<String>,
    pub profit_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub eoa: String,
    pub tx_hash: String,
    pub route_json: Option<String>,
    pub details_json: Option<String>,
    /// Spot USD for the chain native / wrapped-native (mevlive Price column).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_price_usd: Option<f64>,
}

pub struct ExplorerStore {
    conn: Connection,
}

/// Merge a set of key/value pairs into a `details` JSON string, preserving any
/// existing fields (used for profit_tokens and FOT/approximate annotations).
pub(super) fn merge_details_json<const N: usize>(
    details_json: &str,
    pairs: [(&str, serde_json::Value); N],
) -> String {
    let mut d: serde_json::Value =
        serde_json::from_str(details_json).unwrap_or(serde_json::json!({}));
    if let Some(map) = d.as_object_mut() {
        for (k, v) in pairs {
            map.insert(k.to_string(), v);
        }
    }
    d.to_string()
}

impl ExplorerStore {
    /// Shared connection for extension modules (e.g. paper tables).
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// A transaction row to persist alongside block facts.
pub struct TxRow {
    pub hash: B256,
    pub tx_index: u64,
    pub from: Address,
    pub to: Option<Address>,
    pub success: bool,
    pub gas_used: u64,
    pub effective_gas_price_gwei: f64,
    pub priority_fee_gwei: f64,
    pub value: U256,
}

/// A swap row to persist.
#[derive(Debug, Clone)]
pub struct SwapRow {
    pub tx_index: u64,
    pub log_index: u64,
    pub pool: Address,
    pub amm: crate::explorer::types::Amm,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
}

/// A transfer row to persist.
pub struct TransferRow {
    pub tx_index: u64,
    pub log_index: u64,
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub amount: U256,
}

/// Everything a single indexed block contributes to the store.
pub struct BlockFactsInput<'a> {
    pub block_number: u64,
    pub block_hash: &'a B256,
    pub ts: u64,
    pub base_fee_gwei: Option<f64>,
    pub tx_count: usize,
    pub txs: &'a [TxRow],
    pub swaps: &'a [SwapRow],
    pub transfers: &'a [TransferRow],
    pub events: &'a [MevEvent],
    pub native_price_usd: Option<f64>,
    pub token_prices: &'a std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd>,
}

/// One scanner opportunity row to insert (`opportunities` results table).
pub struct OpportunityInput<'a> {
    pub run_id: &'a str,
    pub chain: &'a str,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub strategy: &'a str,
    pub pool_a: Option<Address>,
    pub pool_b: Option<Address>,
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    pub input_amount: Option<U256>,
    pub expected_profit: Option<U256>,
    pub gas_cost_wei: Option<U256>,
    pub path: Option<&'a str>,
    pub timestamp: Option<u64>,
    pub mempool_only: bool,
    pub confidence: Option<&'a str>,
    pub sender: Option<Address>,
    pub tx_hash: Option<B256>,
    pub detection_path: Option<&'a str>,
    pub canonical_id: Option<&'a str>,
}

/// One scanner opportunity row (`opportunities` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityRow {
    pub run_id: Option<String>,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub strategy: String,
    pub pool_a: Option<String>,
    pub pool_b: Option<String>,
    pub token_in: Option<String>,
    pub token_out: Option<String>,
    pub expected_profit: Option<String>,
    /// Detector gas estimate in wei (present on retention rows; drives the
    /// detector-vs-realized `expected_net_wei` in the MEV verdict).
    pub gas_cost_wei: Option<String>,
    pub mempool_only: bool,
    pub detection_path: Option<String>,
    pub canonical_id: Option<String>,
    pub tx_hash: Option<String>,
}

/// One rejected-candidate row (`rejected_candidates` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedRow {
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub strategy: String,
    pub pool_a: Option<String>,
    pub pool_b: Option<String>,
    pub token_in: Option<String>,
    pub token_out: Option<String>,
    pub expected_profit: Option<String>,
    pub gas_cost_wei: Option<String>,
    pub reject_reason: String,
    pub detail: Option<String>,
}

/// Window-wide overview stats (`explorer stats` header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewRow {
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
    pub gas_usd: f64,
    pub highest_single_usd: f64,
    pub searchers: i64,
}

/// One cost · profit · volume row for the revenue report (per kind, per day,
/// or the window overview).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRow {
    pub label: String,
    pub ops: i64,
    pub volume_usd: f64,
    pub gross_usd: f64,
    pub gas_usd: f64,
    pub flash_fee_usd: f64,
    pub net_usd: f64,
}

/// Revenue-report window header (bare `explorer` overview).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportOverview {
    pub ops: i64,
    pub volume_usd: f64,
    pub gross_usd: f64,
    pub gas_usd: f64,
    pub flash_fee_usd: f64,
    pub net_usd: f64,
    pub highest_single_usd: f64,
    pub searchers: i64,
    /// Distinct competitor bots (`COALESCE(contract, eoa)`).
    pub competitors: i64,
}

pub(super) fn map_feed_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FeedRow> {
    Ok(FeedRow {
        ts: r.get::<_, i64>(0)? as u64,
        block_number: r.get::<_, i64>(1)? as u64,
        kind: r.get(2)?,
        profit_token: r.get(3)?,
        profit_amount: r.get(4)?,
        profit_usd: r.get(5)?,
        gas_cost_usd: r.get(6)?,
        net_profit_usd: r.get(7)?,
        eoa: r.get(8)?,
        tx_hash: r.get(9)?,
        route_json: r.get(10)?,
        details_json: r.get(11)?,
        native_price_usd: None,
    })
}

pub(super) fn latest_native_price(conn: &rusqlite::Connection) -> anyhow::Result<Option<f64>> {
    // Native keyed at 0x000…000 in the prices table.
    let mut stmt = conn.prepare(
        "SELECT usd FROM prices
         WHERE lower(token) IN ('0x0000000000000000000000000000000000000000', '0x0')
         ORDER BY hour DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, f64>(0)?))
    } else {
        Ok(None)
    }
}

pub(super) fn map_stats_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StatsRow> {
    Ok(StatsRow {
        label: r.get(0)?,
        ops: r.get(1)?,
        gross_usd: r.get(2)?,
        net_usd: r.get(3)?,
    })
}

pub(super) fn map_report_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReportRow> {
    Ok(ReportRow {
        label: r.get(0)?,
        ops: r.get(1)?,
        volume_usd: r.get(2)?,
        gross_usd: r.get(3)?,
        gas_usd: r.get(4)?,
        flash_fee_usd: r.get(5)?,
        net_usd: r.get(6)?,
    })
}

pub(super) fn map_mev_op_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MevOpRow> {
    Ok(MevOpRow {
        id: r.get(0)?,
        block_number: r.get::<_, i64>(1)? as u64,
        tx_index: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        tx_hash: r.get(3)?,
        ts: r.get::<_, i64>(4)? as u64,
        kind: r.get(5)?,
        eoa: r.get(6)?,
        contract: r.get(7)?,
        confidence: r.get(8)?,
        canonical_id: r.get(9)?,
        profit_token: r.get(10)?,
        profit_amount: r.get(11)?,
        profit_usd: r.get(12)?,
        gas_cost_usd: r.get(13)?,
        flashloan_fee_usd: r.get(14)?,
        volume_usd: r.get(15)?,
        net_profit_usd: r.get(16)?,
        route_json: r.get(17)?,
        victim_hashes: r.get(18)?,
        details_json: r.get(19)?,
        detector: r.get(20)?,
        created_at: r.get::<_, i64>(21)? as u64,
    })
}

pub(super) fn stats_grouped(
    conn: &Connection,
    group_col: &str,
    since_ts: u64,
    kind: Option<&str>,
) -> anyhow::Result<Vec<StatsRow>> {
    let kind_clause = kind
        .map(|k| format!("AND kind = '{k}'"))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {group_col} AS label, COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts} {kind_clause}
         GROUP BY label
         ORDER BY gross_usd DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(super) fn top_grouped(
    conn: &Connection,
    group_col: &str,
    since_ts: u64,
    limit: usize,
    kind: Option<&str>,
) -> anyhow::Result<Vec<StatsRow>> {
    let kind_clause = kind
        .map(|k| format!("AND kind = '{k}'"))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {group_col} AS label, COUNT(*) AS ops,
                COALESCE(SUM(profit_usd), 0) AS gross_usd,
                COALESCE(SUM(net_profit_usd), 0) AS net_usd
         FROM mev_ops
         WHERE ts >= {since_ts} {kind_clause} AND {group_col} IS NOT NULL
         GROUP BY label
         ORDER BY gross_usd DESC
         LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_stats_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::pnl::liquidation_pnl;
    use super::*;
    use crate::explorer::types::{Confidence, MevEvent};
    use alloy::primitives::{address, b256};

    fn token_usd(usd: f64, decimals: u32) -> crate::explorer::pricing::TokenUsd {
        crate::explorer::pricing::TokenUsd { usd, decimals }
    }

    struct SeedBlockParams<'a> {
        block_number: u64,
        block_hash: &'a B256,
        ts: u64,
        tx_count: usize,
        swaps: &'a [SwapRow],
        events: &'a [MevEvent],
        token_prices: &'a std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd>,
    }

    /// Insert one block with the fixture defaults shared by these tests
    /// (25 gwei base fee, $0.75 native, no txs/transfers).
    fn seed_block(store: &ExplorerStore, p: SeedBlockParams<'_>) -> usize {
        store
            .insert_block_facts(BlockFactsInput {
                block_number: p.block_number,
                block_hash: p.block_hash,
                ts: p.ts,
                base_fee_gwei: Some(25.0),
                tx_count: p.tx_count,
                txs: &[],
                swaps: p.swaps,
                transfers: &[],
                events: p.events,
                native_price_usd: Some(0.75),
                token_prices: p.token_prices,
            })
            .unwrap()
    }

    fn sample_event(block: u64) -> MevEvent {
        MevEvent {
            block,
            ts: 1_700_000_000,
            tx_index: 3,
            tx_hash: b256!("1111111111111111111111111111111111111111111111111111111111111111"),
            kind: MevKind::ArbAtomic,
            searcher: address!("2222000000000000000000000000000000000002"),
            contract: None,
            pools: vec![address!("3333000000000000000000000000000000000003")],
            profit_token: Some(address!("4444000000000000000000000000000000000004")),
            profit_amount: Some(U256::from(1_000_000u64)),
            profit_tokens: vec![],
            profit_usd: None,
            gas_cost_wei: U256::from(100_000_000_000_000u64),
            flashloan_fee_wei: None,
            flashloan_fee_token: None,
            confidence: Confidence::Exact,
            victim_hashes: vec![],
            victim_swap_size: None,
            details: serde_json::json!({
                "route": [{"pool": "0x3333", "amm": "v3"}]
            }),
        }
    }

    #[test]
    fn insert_and_query_roundtrip() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            token_usd(0.5, 6),
        );
        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 100,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                ts: 1_700_000_000,
                tx_count: 5,
                swaps: &[],
                events: &[sample_event(100)],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 1);
        assert!(store.block_classified(100).unwrap());
        assert!(!store.block_classified(101).unwrap());

        let ops = store.ops_in_range(100, 100, &[]).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].profit_usd.unwrap() > 0.0);
        assert!(ops[0].net_profit_usd.unwrap() < ops[0].profit_usd.unwrap());
        assert!(ops[0]
            .canonical_id
            .as_deref()
            .unwrap()
            .starts_with("ArbAtomic|"));

        let blocks = store
            .blocks_with_kind(100, 100, MevKind::ArbAtomic)
            .unwrap();
        assert!(blocks.contains(&100));

        let gaps = store.unclassified_blocks(99, 102).unwrap();
        assert_eq!(gaps, vec![99, 101, 102]);

        store.unwind_from(100).unwrap();
        assert!(!store.block_classified(100).unwrap());
    }

    #[test]
    fn multi_residual_profit_usd_sums_all_priced_tokens() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let tok_a = address!("4444000000000000000000000000000000000004"); // 6 decimals
        let tok_b = address!("5555000000000000000000000000000000000005"); // 18 decimals
        let mut ev = sample_event(200);
        ev.profit_token = Some(tok_a);
        ev.profit_amount = Some(U256::from(1_000_000u64)); // 1 token @0.5 = 0.5 USD
        ev.profit_tokens = vec![
            (tok_a, U256::from(1_000_000u64)),
            (tok_b, U256::from(1_000_000_000_000_000_000u64)), // 1 token @2.0 = 2 USD
        ];
        let mut prices = std::collections::HashMap::new();
        prices.insert(tok_a, token_usd(0.5, 6));
        prices.insert(tok_b, token_usd(2.0, 18));
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 200,
                block_hash: &b256!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                ts: 1_700_000_000,
                tx_count: 1,
                swaps: &[],
                events: &[ev],
                token_prices: &prices,
            },
        );
        let ops = store.ops_in_range(200, 200, &[]).unwrap();
        assert_eq!(ops.len(), 1);
        let expected = 0.5 + 2.0;
        let profit_usd = ops[0].profit_usd.unwrap();
        assert!((profit_usd - expected).abs() < 1e-6, "got {profit_usd}");
        // Net subtracts gas (100_000_000_000_000 wei native @0.75 = 0.000075 USD).
        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;
        assert!(
            (ops[0].net_profit_usd.unwrap() - (expected - gas_usd)).abs() < 1e-9,
            "net got {}",
            ops[0].net_profit_usd.unwrap()
        );
        // Display-primary pair is persisted unchanged.
        assert_eq!(
            ops[0].profit_token.as_deref(),
            Some("0x4444000000000000000000000000000000000004")
        );
        assert_eq!(ops[0].profit_amount.as_deref(), Some("1000000"));
        // Residual list surfaces in details for forensic display.
        assert!(ops[0]
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"profit_tokens\""));
        // Every residual had an external price, so the sum is complete.
        assert!(!ops[0]
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("MULTI_ASSET_PRICING"));
    }

    #[test]
    fn fot_profit_token_flagged_approximate_and_realized_rate_falls_back() {
        let usdt = address!("dac17f958d2ee523a2206206994597c13d831ec7"); // bundled FOT
        let longtail = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4444000000000000000000000000000000000004");

        let mut prices = std::collections::HashMap::new();
        prices.insert(usdt, token_usd(0.99, 6));
        prices.insert(usdc, token_usd(1.0, 6));

        let store = ExplorerStore::open_in_memory().unwrap();
        let mut fot = sample_event(301);
        fot.profit_token = Some(usdt);
        fot.profit_amount = Some(U256::from(1_000_000u64));
        // Unpriced profit token priced at the realized route rate :
        // 1 longtail token sold for 2 USDC ⇒ each is worth 2 USD.
        let mut realized = sample_event(302);
        realized.profit_token = Some(longtail);
        realized.profit_amount = Some(U256::from(500_000u64));
        realized.details = serde_json::json!({
            "route": [{
                "pool": "0x3333",
                "amm": "v2",
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "registry",
                "amount_in": "1000000",
                "amount_out": "2000000",
            }]
        });

        for (block, ev) in [(301u64, fot), (302, realized)] {
            seed_block(
                &store,
                SeedBlockParams {
                    block_number: block,
                    block_hash: &b256!("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"),
                    ts: 1_700_000_000,
                    tx_count: 1,
                    swaps: &[],
                    events: &[ev],
                    token_prices: &prices,
                },
            );
        }

        let ops = store.ops_in_range(301, 302, &[]).unwrap();
        assert_eq!(ops.len(), 2);

        let fot_row = &ops[0];
        assert!(fot_row
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"usd_approximate\":true"));
        assert!(fot_row
            .details_json
            .as_deref()
            .unwrap_or("")
            .contains("\"approximate_reason\":\"FOT\""));
        // FOT still priced from the external feed.
        assert!((fot_row.profit_usd.unwrap() - 0.99).abs() < 1e-6);

        let realized_row = &ops[1];
        // 0.5 longtail token × realized 2.0 USD/token = 1.0, minus gas.
        let expected_profit = 1.0;
        assert!(
            (realized_row.profit_usd.unwrap() - expected_profit).abs() < 1e-6,
            "got {}",
            realized_row.profit_usd.unwrap()
        );
        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;
        assert_eq!(
            realized_row.net_profit_usd.unwrap(),
            expected_profit - gas_usd
        );

        // Cross-decimal trusted leg: 1e18 raw sold for 2 USDC, so 0.5e18 is $1.
        // Decimals cancel inside the same-token ratio.
        let mut cross = sample_event(303);
        cross.profit_token = Some(longtail);
        cross.profit_amount = Some(U256::from(500_000_000_000_000_000u64));
        cross.details = serde_json::json!({
            "route": [{
                "pool": "0x3333",
                "amm": "v2",
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "registry",
                "amount_in": "1000000000000000000",
                "amount_out": "2000000",
            }]
        });
        // Proximity leg with the block-26059586 ratio must not be stored as a
        // complete USD figure. 3000 raw against amount_in 1 and ~1.3e17 of a
        // 6-decimal token is the $394,674,797,029,045 shape.
        let mut guessed = sample_event(304);
        guessed.profit_token = Some(longtail);
        guessed.profit_amount = Some(U256::from(3000u64));
        guessed.profit_tokens = vec![(longtail, U256::from(3000u64))];
        guessed.details = serde_json::json!({
            "route": [{
                "pool": "0x3333",
                "amm": "v2",
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "proximity",
                "amount_in": "1",
                "amount_out": "131558265676348437",
            }]
        });
        for (block, ev) in [(303u64, cross), (304, guessed)] {
            seed_block(
                &store,
                SeedBlockParams {
                    block_number: block,
                    block_hash: &b256!("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"),
                    ts: 1_700_000_000,
                    tx_count: 1,
                    swaps: &[],
                    events: &[ev],
                    token_prices: &prices,
                },
            );
        }
        let more = store.ops_in_range(303, 304, &[]).unwrap();
        assert_eq!(more.len(), 2);
        assert!(
            (more[0].profit_usd.unwrap() - 1.0).abs() < 1e-6,
            "cross-decimal got {:?}",
            more[0].profit_usd
        );
        assert!(more[1].profit_usd.is_none(), "got {:?}", more[1].profit_usd);
        let guessed_details = more[1].details_json.as_deref().unwrap_or("");
        assert!(guessed_details.contains("MULTI_ASSET_PRICING"));
        assert!(guessed_details.contains("\"usd_approximate\":true"));
    }

    #[test]
    fn clamped_and_native_residuals_are_explicit() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let longtail = address!("0a00000000000000000000000000000000000000");
        let usdc = address!("4444000000000000000000000000000000000004");
        let mut prices = std::collections::HashMap::new();
        prices.insert(usdc, token_usd(1.0, 6));

        let mut clamped = sample_event(410);
        clamped.profit_token = Some(longtail);
        clamped.profit_amount = Some(U256::from(1_000_000u64));
        clamped.details = serde_json::json!({
            "route": [{
                "token_in": format!("{longtail:#x}"),
                "token_out": format!("{usdc:#x}"),
                "token_source": "registry",
                "amount_in": "1",
                "amount_out": "1000000",
            }]
        });

        let mut native = sample_event(411);
        native.profit_token = Some(usdc);
        native.profit_amount = Some(U256::from(1_000_000u64));
        native.profit_tokens = vec![
            (usdc, U256::from(1_000_000u64)),
            (crate::explorer::profit::NATIVE_MARKER, U256::from(1u64)),
        ];

        let mut partial = sample_event(412);
        partial.profit_token = Some(usdc);
        partial.profit_amount = Some(U256::from(1_000_000u64));
        partial.profit_tokens = vec![
            (usdc, U256::from(1_000_000u64)),
            (longtail, U256::from(100u64)),
        ];

        for (block, ev) in [(410u64, clamped), (411, native), (412, partial)] {
            seed_block(
                &store,
                SeedBlockParams {
                    block_number: block,
                    block_hash: &b256!("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"),
                    ts: 1_700_000_000,
                    tx_count: 1,
                    swaps: &[],
                    events: &[ev],
                    token_prices: &prices,
                },
            );
        }
        let ops = store.ops_in_range(410, 412, &[]).unwrap();
        assert_eq!(ops.len(), 3);
        assert!((ops[0].profit_usd.unwrap() - 1.0).abs() < 1e-6);
        let clamped_details = ops[0].details_json.as_deref().unwrap_or("");
        assert!(clamped_details.contains("pricing_clamped"));
        assert!(clamped_details.contains("\"usd_approximate\":true"));
        // USDC residual is $1; the native marker contributes a reason, not $0.
        assert!((ops[1].profit_usd.unwrap() - 1.0).abs() < 1e-6);
        let native_details = ops[1].details_json.as_deref().unwrap_or("");
        assert!(native_details.contains("NATIVE_UNPRICED"));
        assert!(native_details.contains("\"usd_approximate\":true"));
        // Priced USDC is kept; the unpriced residual is not silently folded in as $0.
        assert!((ops[2].profit_usd.unwrap() - 1.0).abs() < 1e-6);
        let partial_details = ops[2].details_json.as_deref().unwrap_or("");
        assert!(partial_details.contains("MULTI_ASSET_PRICING"));
        assert!(partial_details.contains("\"usd_approximate\":true"));
    }

    #[test]
    fn jit_open_positions_roundtrip() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let pool = address!("3333000000000000000000000000000000000003");
        let owner = address!("2222000000000000000000000000000000000002");
        store
            .record_jit_open(&[OpenPosition {
                pool,
                owner,
                tick_lower: -100,
                tick_upper: 100,
                opened_block: 500,
                liquidity: 1000,
            }])
            .unwrap();
        store
            .record_jit_open(&[OpenPosition {
                pool,
                owner,
                tick_lower: -200,
                tick_upper: 200,
                opened_block: 10,
                liquidity: 7,
            }])
            .unwrap();

        let open = store.open_positions(100).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].liquidity, 1000);
        assert_eq!(open[0].opened_block, 500);

        store.close_jit_position(pool, owner, -100, 100).unwrap();
        assert!(store
            .open_positions(0)
            .unwrap()
            .iter()
            .any(|p| p.tick_lower == -200));

        assert_eq!(store.prune_jit_positions(100).unwrap(), 1);
        assert!(store.open_positions(0).unwrap().is_empty());
    }

    fn liquidation_event(collateral: Address, debt: Address) -> MevEvent {
        let mut ev = sample_event(1);
        ev.kind = MevKind::Liquidation;
        ev.profit_token = Some(collateral);
        ev.profit_amount = Some(U256::from(500));
        ev.details = serde_json::json!({
            "collateral_asset": format!("{collateral:#x}"),
            "debt_asset": format!("{debt:#x}"),
            "collateral_amount": "500",
            "debt_to_cover": "300",
            "reconciled": true,
            "reasons": [],
        });
        ev
    }

    #[test]
    fn liquidation_pnl_cross_asset_is_inferred() {
        let collateral = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let debt = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let mut prices = std::collections::HashMap::new();
        prices.insert(collateral, token_usd(2.0, 0));
        prices.insert(debt, token_usd(1.0, 0));
        let ev = liquidation_event(collateral, debt);
        let (usd, conf, details) = liquidation_pnl(&ev, &prices);
        // 500 * $2 − 300 * $1 = 700
        assert!((usd.unwrap() - 700.0).abs() < 1e-9);
        assert_eq!(conf, "inferred");
        assert!(details.contains("LIQ_BONUS_APPROX"));
    }

    #[test]
    fn liquidation_pnl_missing_price_falls_back() {
        let collateral = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let debt = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let prices = std::collections::HashMap::new();
        let ev = liquidation_event(collateral, debt);
        let (usd, conf, details) = liquidation_pnl(&ev, &prices);
        assert!(usd.is_none());
        assert_eq!(conf, "inferred");
        assert!(details.contains("MULTI_ASSET_PRICING"));
        assert!(details.contains("collateral_amount"));
    }

    #[test]
    fn liquidation_pnl_same_asset_is_exact() {
        let asset = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let mut prices = std::collections::HashMap::new();
        prices.insert(asset, token_usd(1.0, 0));
        let ev = liquidation_event(asset, asset);
        let (usd, conf, _) = liquidation_pnl(&ev, &prices);
        assert!((usd.unwrap() - 200.0).abs() < 1e-9);
        assert_eq!(conf, "exact");
    }

    #[test]
    fn sandwich_loss_gate_skips_unprofitable() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            token_usd(0.5, 6),
        );
        // Zero extractable profit cannot cover front+back gas → not realized MEV.
        let mut loss = sample_event(130);
        loss.kind = MevKind::Sandwich;
        loss.profit_amount = Some(U256::ZERO);
        loss.details = serde_json::json!({ "reason": "test" });
        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 130,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaae"),
                ts: 1_700_000_300,
                tx_count: 5,
                swaps: &[],
                events: &[loss],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 0);
        assert!(store.ops_in_range(130, 130, &[]).unwrap().is_empty());
        // ...but the block is still marked classified (0 persisted ops).
        assert!(store.block_classified(130).unwrap());

        // A profitable sandwich (1.0 USD profit > ~0.00008 USD gas) is kept.
        let mut win = sample_event(131);
        win.kind = MevKind::Sandwich;
        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 131,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaf"),
                ts: 1_700_000_400,
                tx_count: 5,
                swaps: &[],
                events: &[win],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 1);
        assert_eq!(store.ops_in_range(131, 131, &[]).unwrap().len(), 1);
    }

    #[test]
    fn stats_kind_filter_matches_only_that_kind() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            token_usd(0.5, 6),
        );
        let mut sand_evt = sample_event(120);
        sand_evt.ts = 1_700_000_101;
        sand_evt.kind = MevKind::Sandwich;
        let mut arb_evt = sample_event(121);
        arb_evt.ts = 1_700_000_200;
        arb_evt.kind = MevKind::ArbAtomic;
        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 120,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab"),
                ts: 1_700_000_100,
                tx_count: 5,
                swaps: &[],
                events: &[sand_evt],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 1);
        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 121,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac"),
                ts: 1_700_000_200,
                tx_count: 5,
                swaps: &[],
                events: &[arb_evt],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 1);

        let all = store.stats_by_kind_filtered(1_699_999_000, None).unwrap();
        assert_eq!(all.len(), 2);

        let arb = store
            .stats_by_kind_filtered(1_699_999_000, Some("arb_atomic"))
            .unwrap();
        assert_eq!(arb.len(), 1);
        assert_eq!(arb[0].label, "arb_atomic");
        assert_eq!(arb[0].ops, 1);

        let sand = store
            .stats_by_kind_filtered(1_699_999_000, Some("sandwich"))
            .unwrap();
        assert_eq!(sand.len(), 1);
        assert_eq!(sand[0].label, "sandwich");

        let none = store
            .stats_by_kind_filtered(1_699_999_000, Some("liquidation"))
            .unwrap();
        assert!(none.is_empty());

        let overview = store
            .stats_overview_filtered(1_699_999_000, Some("sandwich"))
            .unwrap();
        assert_eq!(overview.ops, 1);

        let senders = store
            .top_senders_filtered(1_699_999_000, 5, Some("arb_atomic"))
            .unwrap();
        assert!(!senders.is_empty());

        let senders_sand = store
            .top_senders_filtered(1_699_999_000, 5, Some("sandwich"))
            .unwrap();
        assert!(!senders_sand.is_empty());

        let misc = store
            .top_senders_filtered(1_699_999_000, 5, Some("liquidations"))
            .unwrap();
        assert!(misc.is_empty());
    }

    fn usdc_price() -> std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd> {
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            token_usd(1.0, 6),
        );
        prices
    }

    fn swap_leg(tx_index: u64, log_index: u64, token_in: Address, amount_in: u64) -> SwapRow {
        SwapRow {
            tx_index,
            log_index,
            pool: address!("3333000000000000000000000000000000000003"),
            amm: crate::explorer::types::Amm::V3,
            token_in,
            token_out: address!("5555000000000000000000000000000000000005"),
            amount_in: U256::from(amount_in),
            amount_out: U256::ZERO,
        }
    }

    #[test]
    fn volume_usd_prices_op_swap_legs() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let usdc = address!("4444000000000000000000000000000000000004");
        let mut ev = sample_event(400);
        ev.tx_index = 7;
        let swap_legs = [
            swap_leg(7, 0, usdc, 5_000_000),   // $5
            swap_leg(7, 1, usdc, 2_000_000),   // $2
            swap_leg(9, 0, usdc, 100_000_000), // other tx, not attributed
        ];
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 400,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaadd"),
                ts: 1_700_000_000,
                tx_count: 10,
                swaps: &swap_legs,
                events: &[ev],
                token_prices: &usdc_price(),
            },
        );
        let ops = store.ops_in_range(400, 400, &[]).unwrap();
        assert_eq!(ops.len(), 1);
        let v = ops[0].volume_usd.unwrap();
        assert!((v - 7.0).abs() < 1e-6, "op volume {v}");
        // Unpriced legs yield None volume.
        let mut unpr = sample_event(401);
        unpr.tx_index = 11;
        // token not in prices map
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 401,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaade"),
                ts: 1_700_000_000,
                tx_count: 1,
                swaps: &[swap_leg(
                    11,
                    0,
                    address!("9999000000000000000000000000000000000009"),
                    123,
                )],
                events: &[unpr],
                token_prices: &usdc_price(),
            },
        );
        let ops = store.ops_in_range(401, 401, &[]).unwrap();
        assert!(ops[0].volume_usd.is_none());
    }

    #[test]
    fn report_windows_aggregate_cost_profit_volume() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let usdc = address!("4444000000000000000000000000000000000004");
        let day_a: u64 = 1_700_000_000;
        let day_b: u64 = day_a + 86_400;
        let mut arb = sample_event(510);
        arb.ts = day_a;
        arb.tx_index = 3;
        let mut sand = sample_event(511);
        sand.ts = day_b;
        sand.tx_index = 4;
        sand.kind = MevKind::Sandwich;
        let mut swp_a = swap_leg(3, 0, usdc, 5_000_000);
        swp_a.pool = arb.pools[0];
        let mut swp_b = swap_leg(4, 0, usdc, 10_000_000);
        swp_b.pool = sand.pools[0];
        for (block, ev, swaps, ts) in [
            (510u64, &arb, &[swp_a.clone()][..], day_a),
            (511, &sand, &[swp_b.clone()][..], day_b),
        ] {
            seed_block(
                &store,
                SeedBlockParams {
                    block_number: block,
                    block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaadd"),
                    ts,
                    tx_count: 1,
                    swaps,
                    events: std::slice::from_ref(ev),
                    token_prices: &usdc_price(),
                },
            );
        }

        let ov = store.report_window_overview(0, None).unwrap();
        assert_eq!(ov.ops, 2);
        assert_eq!(ov.searchers, 1);
        assert_eq!(ov.competitors, 1);
        assert!(
            (ov.volume_usd - 15.0).abs() < 1e-6,
            "volume {}",
            ov.volume_usd
        );
        assert!((ov.gross_usd - 2.0).abs() < 1e-6, "gross {}", ov.gross_usd);
        assert!(ov.net_usd < ov.gross_usd, "net subtracts gas");
        assert!(ov.gas_usd > 0.0);
        assert_eq!(ov.flash_fee_usd, 0.0);

        let kinds = store.report_by_kind(0, None).unwrap();
        assert_eq!(kinds.len(), 2);
        let arb_row = kinds.iter().find(|r| r.label == "arb_atomic").unwrap();
        assert!((arb_row.volume_usd - 5.0).abs() < 1e-6);

        let daily = store.report_daily(0, None).unwrap();
        assert_eq!(daily.len(), 2, "two distinct days: {:?}", daily);

        let top = store.top_ops(0, 5, None).unwrap();
        assert_eq!(top.len(), 2);
        assert!(top[0].net_profit_usd.unwrap() >= top[1].net_profit_usd.unwrap());

        assert_eq!(store.blocks_in_window(0).unwrap(), 2);
        assert_eq!(store.blocks_in_window(day_b).unwrap(), 1);

        let ov_k = store.report_window_overview(0, Some("sandwich")).unwrap();
        assert_eq!(ov_k.ops, 1);
        assert_eq!(store.report_by_kind(0, Some("sandwich")).unwrap().len(), 1);
    }

    #[test]
    fn trace_verification_replaces_stale_fields() {
        let store = ExplorerStore::open_in_memory().unwrap();
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 600,
                block_hash: &b256!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
                ts: 1_700_000_000,
                tx_count: 1,
                swaps: &[],
                events: &[sample_event(600)],
                token_prices: &usdc_price(),
            },
        );
        let tx_hash = store.ops_in_range(600, 600, &[]).unwrap()[0]
            .tx_hash
            .clone();

        store
            .mark_trace_verified(
                &tx_hash,
                &TraceVerification {
                    expected_profit_usd: Some(100.0),
                    trace_profit_usd: Some(95.0),
                    profit_error_pct: Some(5.0),
                    native_delta_wei: "123".into(),
                    note: "first".into(),
                    trace_check: Some("pass".into()),
                    trace_check_reason: None,
                },
            )
            .unwrap();
        let first = store.ops_in_range(600, 600, &[]).unwrap()[0]
            .details_json
            .clone()
            .unwrap();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["trace_check"], "pass");
        assert_eq!(first["profit_error_pct"], 5.0);

        store
            .mark_trace_verified(
                &tx_hash,
                &TraceVerification {
                    expected_profit_usd: None,
                    trace_profit_usd: None,
                    profit_error_pct: None,
                    native_delta_wei: String::new(),
                    note: "second".into(),
                    trace_check: Some("unverifiable".into()),
                    trace_check_reason: Some("no price".into()),
                },
            )
            .unwrap();
        let second = store.ops_in_range(600, 600, &[]).unwrap()[0]
            .details_json
            .clone()
            .unwrap();
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["trace_check"], "unverifiable");
        assert_eq!(second["trace_check_reason"], "no price");
        assert!(second.get("expected_profit_usd").is_none());
        assert!(second.get("trace_profit_usd").is_none());
        assert!(second.get("profit_error_pct").is_none());
        assert!(second.get("trace_native_delta_wei").is_none());

        store
            .mark_trace_verified(
                &tx_hash,
                &TraceVerification {
                    expected_profit_usd: Some(100.0),
                    trace_profit_usd: Some(95.0),
                    profit_error_pct: Some(5.0),
                    native_delta_wei: "123".into(),
                    note: "third".into(),
                    trace_check: Some("pass".into()),
                    trace_check_reason: None,
                },
            )
            .unwrap();
        let third = store.ops_in_range(600, 600, &[]).unwrap()[0]
            .details_json
            .clone()
            .unwrap();
        let third: serde_json::Value = serde_json::from_str(&third).unwrap();
        assert_eq!(third["trace_check"], "pass");
        assert!(third.get("trace_check_reason").is_none());
    }

    #[test]
    fn competitors_group_by_shared_contract_and_label() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let contract = address!("5555000000000000000000000000000000000005");
        let eoa_a = address!("2222000000000000000000000000000000000002");
        let eoa_b = address!("222200000000000000000000000000000000000b");
        let mut ev_a = sample_event(700);
        ev_a.searcher = eoa_a;
        ev_a.contract = Some(contract);
        ev_a.tx_index = 1;
        ev_a.tx_hash = b256!("1111111111111111111111111111111111111111111111111111111111111111");
        let mut ev_b = sample_event(701);
        ev_b.searcher = eoa_b;
        ev_b.contract = Some(contract);
        ev_b.tx_index = 2;
        ev_b.tx_hash = b256!("2222222222222222222222222222222222222222222222222222222222222222");
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 700,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa70"),
                ts: 1_700_000_000,
                tx_count: 2,
                swaps: &[],
                events: &[ev_a],
                token_prices: &usdc_price(),
            },
        );
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 701,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa71"),
                ts: 1_700_000_100,
                tx_count: 2,
                swaps: &[],
                events: &[ev_b],
                token_prices: &usdc_price(),
            },
        );

        let ov = store.report_window_overview(0, None).unwrap();
        assert_eq!(ov.ops, 2);
        assert_eq!(ov.searchers, 2);
        assert_eq!(ov.competitors, 1);

        let comps = store.top_competitors_filtered(0, 10, None).unwrap();
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].label, format!("{contract:#x}"));
        assert_eq!(comps[0].ops, 2);

        let bot = format!("{contract:#x}");
        let (name_a, entity_a) = store.get_label(eoa_a).unwrap().expect("eoa labeled");
        assert_eq!(name_a, "unclassified-searcher");
        assert_eq!(entity_a.as_deref(), Some(bot.as_str()));
        let (name_c, entity_c) = store
            .get_label(contract)
            .unwrap()
            .expect("contract labeled");
        assert_eq!(name_c, "searcher-contract");
        assert_eq!(entity_c.as_deref(), Some(bot.as_str()));
    }

    #[test]
    fn backfill_competitor_labels_is_idempotent() {
        let store = ExplorerStore::open_in_memory().unwrap();
        // Seed then wipe labels to simulate a pre-wiring DB.
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 800,
                block_hash: &b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa80"),
                ts: 1_700_000_000,
                tx_count: 1,
                swaps: &[],
                events: &[sample_event(800)],
                token_prices: &usdc_price(),
            },
        );
        store.clear_labels().unwrap();
        let searcher = address!("2222000000000000000000000000000000000002");
        assert!(store.get_label(searcher).unwrap().is_none());
        let n = store.backfill_competitor_labels().unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.backfill_competitor_labels().unwrap(), 0);
        let (name, entity) = store.get_label(searcher).unwrap().expect("backfilled");
        assert_eq!(name, "unclassified-searcher");
        let bot = format!("{searcher:#x}");
        assert_eq!(entity.as_deref(), Some(bot.as_str()));
    }
}
