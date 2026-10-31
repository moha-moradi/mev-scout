//! Explorer store — SQLite persistence for realized-MEV facts.
//!
//! Separate database file from the scanner cache (`explorer_{chain}.sqlite`)
//! so live index writes never contend with replay-path reads. WAL mode,
//! single-writer. `rusqlite::Connection` is `Send` but not `Sync`, so the
//! store is never shared across threads; the index loop runs each block's
//! synchronous writes inside `tokio::task::block_in_place` on multi-thread
//! runtimes, which keeps the async worker free without a connection pool
//! (a pool would not help a single writer).
//!
//! Layers:
//! - forensic facts: `blocks`, `txs`, `transfers`, `swaps`, `mev_ops`
//! - results layer: `opportunities` fed from `ResultsFile`
//! - rejection capture: `rejected_candidates`
//! - checkpointing: `sync_state` + `blocks_classified` (gap-safe resume)
/// `(pool, amm, token_in, token_out)` for a realized swap row.
pub(super) type PoolSwapRow = (String, Option<String>, String, String);
/// Competitor labels owed by a persisted block: `(eoa, contract, block)`.
pub(super) type PendingLabels = Vec<(Address, Option<Address>, u64)>;

mod facts;
mod jit_positions;
mod labels;
mod pnl;
mod query;
mod reprice;
mod schema;
mod sync;

use alloy::primitives::{Address, B256, U256};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::explorer::types::{MevEvent, MevKind};

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// `keeper` | `bundler` | `claim_and_sell` for a utility op. JSON reports
    /// carry it; `report_by_kind` still groups on `kind` alone.
    pub subkind: Option<String>,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatsRow {
    pub label: String,
    pub ops: i64,
    pub gross_usd: f64,
    pub net_usd: f64,
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

/// One cost · profit · volume row for the revenue report (per kind, per day,
/// or the window overview).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportRow {
    pub label: String,
    pub ops: i64,
    pub volume_usd: f64,
    pub gross_usd: f64,
    pub gas_usd: f64,
    pub flash_fee_usd: f64,
    pub net_usd: f64,
}

/// `explorer_classify` reject count for one reason inside a report window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectCount {
    pub reason: String,
    pub count: i64,
}

/// Pricing / confidence quality slice for one report window.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReportQuality {
    pub ops_priced: i64,
    pub ops_unpriced: i64,
    pub ops_exact: i64,
    pub ops_inferred: i64,
    pub net_usd_exact: f64,
    pub net_usd_inferred: f64,
    /// Ops flagged `usd_approximate` in details_json.
    pub ops_approx: i64,
    /// Gross USD over ops that carry a USD price (`profit_usd IS NOT NULL`).
    /// Fully-unpriced ops (NULL) are excluded from this total, never zeroed.
    pub gross_usd_priced_only: f64,
    /// Fraction of ops with a non-NULL USD price, in percent (0..=100).
    /// `100.0` when the window has no ops.
    pub pricing_coverage_pct: f64,
    /// Ops whose details carry the `NATIVE_UNPRICED` reason (net incomplete).
    pub ops_native_unpriced: i64,
    /// Ops whose details carry the `MULTI_ASSET_PRICING` reason.
    pub ops_multi_asset: i64,
    /// Ops whose details carry the `pricing_clamped` reason.
    pub ops_pricing_clamped: i64,
    /// Ops whose details carry the `FLASH_FEE_UNPRICED` reason: a non-zero
    /// flash-loan fee with no usable quote, not already netted into the ledger.
    pub ops_flash_fee_unpriced: i64,
    /// Sandwich ops loss-gated at persist (priced net `<= 0`): the row is kept
    /// — counts and gross stay truthful — but every *net* sum and `top_ops`
    /// exclude it.
    pub ops_excluded_from_net: i64,
}

/// Revenue-report window header (bare `explorer` overview).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    #[serde(default)]
    pub quality: ReportQuality,
}

impl ReportOverview {
    /// Percentage of window net profit attributable to the single highest op.
    ///
    /// Numerator: `highest_single_usd`. Denominator: window `net_usd`. The
    /// denominator guard (`net_usd > 0`) is required so the warning cannot
    /// divide by zero, and a zero/negative net window (where "dominant share"
    /// is not meaningful) yields `None` instead of a misleading percentage.
    pub fn net_concentration_pct(&self) -> Option<f64> {
        let denom = self.net_usd;
        if denom <= 0.0 || !denom.is_finite() {
            return None;
        }
        let top = self.highest_single_usd;
        if !top.is_finite() {
            return None;
        }
        Some(100.0 * top / denom)
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
        subkind: r.get(22)?,
    })
}

#[cfg(test)]
mod tests {
    use super::pnl::liquidation_pnl;
    use super::*;
    use crate::explorer::types::{Confidence, MevEvent};
    use crate::explorer::RejectedCandidate;
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
        // The natural key is `(canonical_id, tx_hash)`. Fixtures that seed
        // several blocks must not share a hash or the second insert revises
        // the first row.
        let mut raw = [0x11u8; 32];
        raw[24..32].copy_from_slice(&block.to_be_bytes());
        MevEvent {
            block,
            ts: 1_700_000_000,
            tx_index: 3,
            tx_hash: B256::from(raw),
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
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                ),
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

        let gaps = store.unclassified_blocks(99, 102).unwrap();
        assert_eq!(gaps, vec![99, 101, 102]);

        store.unwind_from(100).unwrap();
        assert!(!store.block_classified(100).unwrap());
    }

    #[test]
    fn ops_in_range_ties_are_id_ordered_and_stable_across_runs() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            token_usd(0.5, 6),
        );
        // Same block, tx index and P&L: only the natural key differs, so
        // `(block_number, tx_index)` alone is a tie.
        let a = sample_event(100);
        let mut b = sample_event(100);
        b.tx_hash = B256::from([0x22u8; 32]);
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 100,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                ),
                ts: 1_700_000_000,
                tx_count: 2,
                swaps: &[],
                events: &[a, b],
                token_prices: &prices,
            },
        );

        let ids = |store: &ExplorerStore| -> Vec<i64> {
            store
                .ops_in_range(100, 100, &[])
                .unwrap()
                .into_iter()
                .map(|op| op.id)
                .collect()
        };
        let first = ids(&store);
        assert_eq!(first.len(), 2);
        assert!(first[0] < first[1], "id tiebreak must be ascending");
        assert_eq!(
            first,
            ids(&store),
            "repeated runs must return the same order"
        );
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

    #[test]
    fn jit_positions_batch_close_removes_every_match() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let pool = address!("3333000000000000000000000000000000000003");
        let owner = address!("2222000000000000000000000000000000000002");
        let positions: Vec<OpenPosition> = [(-100, 100), (-200, 200), (-300, 300)]
            .into_iter()
            .map(|(tick_lower, tick_upper)| OpenPosition {
                pool,
                owner,
                tick_lower,
                tick_upper,
                opened_block: 10,
                liquidity: 1,
            })
            .collect();
        store.record_jit_open(&positions).unwrap();
        store
            .close_jit_positions(&[(pool, owner, -100, 100), (pool, owner, -300, 300)])
            .unwrap();
        let open = store.open_positions(0).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].tick_lower, -200);
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
    fn liquidation_pnl_skips_unconverted_qi() {
        let qi = address!("5c0401e81bc07ca70fad469b451682c0d747ef1c");
        let mut prices = std::collections::HashMap::new();
        // Even if a feed somehow prices the qi address, skip when flagged.
        prices.insert(qi, token_usd(10.0, 8));
        let mut ev = liquidation_event(qi, qi);
        ev.details["reasons"] = serde_json::json!(["QI_EXCHANGE_RATE_MISSING"]);
        let (usd, conf, details) = liquidation_pnl(&ev, &prices);
        assert!(usd.is_none());
        assert_eq!(conf, "inferred");
        assert!(details.contains("skipped_qi_unconverted"));
    }

    #[test]
    fn sandwich_loss_gate_persists_lossy_rows_with_flag() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            token_usd(0.5, 6),
        );
        // Zero extractable profit cannot cover front+back gas → net < 0. The
        // sandwich is kept with `excluded_from_net = 1` (forensics survive,
        // counts stay truthful) instead of being dropped.
        let mut loss = sample_event(130);
        loss.kind = MevKind::Sandwich;
        loss.profit_amount = Some(U256::ZERO);
        loss.details = serde_json::json!({ "reason": "test" });
        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 130,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaae"
                ),
                ts: 1_700_000_300,
                tx_count: 5,
                swaps: &[],
                events: &[loss],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 1, "the lossy sandwich is persisted, not dropped");
        let ops = store.ops_in_range(130, 130, &[]).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].net_profit_usd.unwrap() < 0.0);
        let flagged: i64 = store
            .conn
            .query_row(
                "SELECT excluded_from_net FROM mev_ops WHERE block_number = 130",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flagged, 1, "the lossy sandwich carries the flag");
        // ...and the block is still marked classified (1 persisted op).
        assert!(store.block_classified(130).unwrap());

        // A profitable sandwich (1.0 USD profit > ~0.00008 USD gas) is kept
        // unflagged: the gate fires only on priced net <= 0.
        let mut win = sample_event(131);
        win.kind = MevKind::Sandwich;
        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 131,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaf"
                ),
                ts: 1_700_000_400,
                tx_count: 5,
                swaps: &[],
                events: &[win],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 1);
        assert_eq!(store.ops_in_range(131, 131, &[]).unwrap().len(), 1);
        let unflagged: i64 = store
            .conn
            .query_row(
                "SELECT excluded_from_net FROM mev_ops WHERE block_number = 131",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unflagged, 0, "a profitable sandwich is not flagged");
    }

    #[test]
    fn top_senders_kind_filter_matches_only_that_kind() {
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
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab"
                ),
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
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac"
                ),
                ts: 1_700_000_200,
                tx_count: 5,
                swaps: &[],
                events: &[arb_evt],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 1);

        let senders = store
            .top_senders_filtered(1_699_999_000, 5, Some(MevKind::ArbAtomic), None)
            .unwrap();
        assert!(!senders.is_empty());

        let senders_sand = store
            .top_senders_filtered(1_699_999_000, 5, Some(MevKind::Sandwich), None)
            .unwrap();
        assert!(!senders_sand.is_empty());

        let misc = store
            .top_senders_filtered(1_699_999_000, 5, Some(MevKind::Liquidation), None)
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
                    block_hash: &b256!(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaadd"
                    ),
                    ts,
                    tx_count: 1,
                    swaps,
                    events: std::slice::from_ref(ev),
                    token_prices: &usdc_price(),
                },
            );
        }

        let ov = store.report_window_overview(0, None, None).unwrap();
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
        assert_eq!(ov.quality.ops_priced, 2);

        let kinds = store.report_by_kind(0, None, None).unwrap();
        assert_eq!(kinds.len(), 2);
        let arb_row = kinds.iter().find(|r| r.label == "arb_atomic").unwrap();
        assert!((arb_row.volume_usd - 5.0).abs() < 1e-6);

        let daily = store.report_daily(0, None, None).unwrap();
        assert_eq!(daily.len(), 2, "two distinct days: {:?}", daily);

        let top = store.top_ops(0, 5, None, None).unwrap();
        assert_eq!(top.len(), 2);
        assert!(top[0].net_profit_usd.unwrap() >= top[1].net_profit_usd.unwrap());

        assert_eq!(store.blocks_in_window(0).unwrap(), 2);
        assert_eq!(store.blocks_in_window(day_b).unwrap(), 1);

        let ov_k = store
            .report_window_overview(0, Some(MevKind::Sandwich), None)
            .unwrap();
        assert_eq!(ov_k.ops, 1);
        assert_eq!(
            store
                .report_by_kind(0, Some(MevKind::Sandwich), None)
                .unwrap()
                .len(),
            1
        );
    }

    /// The overview must expose every pricing-reason count, `gross_usd_priced_only`,
    /// and a reconciling pricing coverage — and must never silently treat an
    /// unpriced op (NULL `profit_usd`) as $0 in the priced total.
    #[test]
    fn report_window_overview_exposes_reasons_coverage_and_gross_priced() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let usdc = address!("4444000000000000000000000000000000000004");
        let longtail = address!("0a00000000000000000000000000000000000000");
        let mut prices = std::collections::HashMap::new();
        prices.insert(usdc, token_usd(1.0, 6));

        // 1. fully priced, exact, no pricing reason.
        let exact = sample_event(600);

        // 2. realized-rate clamped to the trusted route → `pricing_clamped`.
        let mut clamped = sample_event(610);
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

        // 3. native residual — since A16 it is priced with the in-scope $0.75
        // native price (1 wei ≈ $0), so it is no longer unpriced/approximate.
        let mut native = sample_event(611);
        native.profit_token = Some(usdc);
        native.profit_amount = Some(U256::from(1_000_000u64));
        native.profit_tokens = vec![
            (usdc, U256::from(1_000_000u64)),
            (crate::explorer::profit::NATIVE_MARKER, U256::from(1u64)),
        ];
        native.confidence = Confidence::Inferred;

        // 4. partially priced multi-asset residual → `MULTI_ASSET_PRICING`.
        let mut multi = sample_event(612);
        multi.profit_token = Some(usdc);
        multi.profit_amount = Some(U256::from(1_000_000u64));
        multi.profit_tokens = vec![
            (usdc, U256::from(1_000_000u64)),
            (longtail, U256::from(100u64)),
        ];

        // 5. wholly unpriced (token absent, no route) → NULL profit_usd +
        // `MULTI_ASSET_PRICING`.
        let mut unpriced = sample_event(613);
        unpriced.profit_token = Some(longtail);
        unpriced.profit_tokens = vec![];
        unpriced.details = serde_json::json!({ "route": [] });

        for (block, ev) in [
            (600u64, exact),
            (610, clamped),
            (611, native),
            (612, multi),
            (613, unpriced),
        ] {
            seed_block(
                &store,
                SeedBlockParams {
                    block_number: block,
                    block_hash: &b256!(
                        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    ),
                    ts: 1_700_000_000,
                    tx_count: 1,
                    swaps: &[],
                    events: std::slice::from_ref(&ev),
                    token_prices: &prices,
                },
            );
        }

        let ov = store.report_window_overview(0, None, None).unwrap();
        // A16 shift: case 3 already counted as priced via its USDC leg; what
        // changes is that its native residual now contributes real USD and the
        // op no longer carries NATIVE_UNPRICED / usd_approximate. So priced
        // stays 4, unpriced stays 1 (case 5 is the genuinely unpriced one),
        // approx 4→3, native_unpriced 1→0. Coverage/gross are unchanged.
        assert_eq!(ov.ops, 5);
        assert_eq!(ov.quality.ops_priced, 4);
        assert_eq!(ov.quality.ops_unpriced, 1);
        assert_eq!(ov.quality.ops_exact, 4);
        assert_eq!(ov.quality.ops_inferred, 1);
        assert_eq!(ov.quality.ops_approx, 3);
        assert_eq!(ov.quality.ops_multi_asset, 2);
        assert_eq!(ov.quality.ops_native_unpriced, 0);
        assert_eq!(ov.quality.ops_pricing_clamped, 1);
        // Each priced op nets $1 (gas ~$0.000075 is tiny relative to 4.0).
        assert!((ov.quality.gross_usd_priced_only - 4.0).abs() < 1e-6);
        assert_eq!(
            ov.quality.gross_usd_priced_only, ov.gross_usd,
            "priced-only gross must equal SUM(profit_usd)"
        );
        assert_eq!(ov.quality.pricing_coverage_pct, 80.0);
        assert!(
            ov.net_usd > 0.0,
            "net should be positive for 4 priced $1 ops"
        );
        // No divide-by-zero: one $1 op of $4 net ≈ 25% (< 40%), so no warning path.
        let pct = ov
            .net_concentration_pct()
            .expect("net>0 must yield a percentage");
        assert!(pct > 0.0 && pct < 100.0);
        // The wholly unpriced op (case 5) must stay NULL in the store
        // (excluded, not zeroed).
        let ops = store.ops_in_range(600, 613, &[]).unwrap();
        let null_blocks: Vec<u64> = ops
            .iter()
            .filter(|op| op.profit_usd.is_none())
            .map(|op| op.block_number)
            .collect();
        assert_eq!(
            null_blocks,
            vec![613],
            "only the wholly unpriced op is NULL"
        );
    }

    /// A16: a native-denominated residual is priced with the native price
    /// already in scope for gas. Only a genuinely missing native price keeps
    /// the row NULL and the `NATIVE_UNPRICED` flag — a NULL is a data limit,
    /// never an implicit "no profit".
    #[test]
    fn native_residual_is_priced_with_the_in_scope_native_price() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let one_native = U256::from(1_000_000_000_000_000_000u128);

        fn persist_native(
            store: &ExplorerStore,
            block: u64,
            native_price_usd: Option<f64>,
            amount: U256,
        ) {
            let mut ev = sample_event(block);
            ev.profit_token = None;
            ev.profit_amount = None;
            ev.profit_tokens = vec![(crate::explorer::profit::NATIVE_MARKER, amount)];
            let prices: std::collections::HashMap<Address, crate::explorer::pricing::TokenUsd> =
                Default::default();
            store
                .insert_block_facts(BlockFactsInput {
                    block_number: block,
                    block_hash: &b256!(
                        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    ),
                    ts: 1_700_000_000,
                    base_fee_gwei: Some(25.0),
                    tx_count: 1,
                    txs: &[],
                    swaps: &[],
                    transfers: &[],
                    events: std::slice::from_ref(&ev),
                    native_price_usd,
                    token_prices: &prices,
                })
                .unwrap();
        }

        persist_native(&store, 700, Some(0.75), one_native);
        persist_native(&store, 701, None, one_native);

        let ops = store.ops_in_range(700, 701, &[]).unwrap();
        let priced = ops
            .iter()
            .find(|o| o.block_number == 700)
            .expect("priced native op");
        assert!(
            (priced.profit_usd.expect("priced native profit_usd") - 0.75).abs() < 1e-9,
            "1 native at $0.75 must persist as $0.75, got {:?}",
            priced.profit_usd
        );
        assert!(
            !priced
                .details_json
                .as_deref()
                .unwrap_or_default()
                .contains("NATIVE_UNPRICED"),
            "a priced native residual must not carry NATIVE_UNPRICED"
        );

        let unpriced = ops
            .iter()
            .find(|o| o.block_number == 701)
            .expect("unpriced native op");
        assert_eq!(
            unpriced.profit_usd, None,
            "no native price → NULL, never $0"
        );
        assert!(
            unpriced
                .details_json
                .as_deref()
                .unwrap_or_default()
                .contains("NATIVE_UNPRICED"),
            "a missing native price must stay flagged"
        );

        let ov = store.report_window_overview(0, None, None).unwrap();
        assert_eq!(
            ov.quality.ops_native_unpriced, 1,
            "only the genuinely price-less native op is flagged"
        );
    }

    /// A non-zero flash-loan fee whose token has no quote is *reported*, not
    /// silently netted as $0: the row keeps `flashloan_fee_usd = NULL`, carries
    /// the `FLASH_FEE_UNPRICED` pricing reason (and `usd_approximate`), and is
    /// counted in `ReportQuality`. The fee the ledger already absorbed (A13) is
    /// exempt: its residual is inside the token amount, so the missing quote
    /// does not make `net` incomplete and flagging it would be a false alarm.
    #[test]
    fn unpriced_flashloan_fee_is_flagged_unless_the_ledger_netted_it() {
        let priced = address!("4444000000000000000000000000000000000004"); // 6 decimals
        let unpriced_tok = address!("0a00000000000000000000000000000000000000");
        // Only the profit token is priced — the fee token has no quote at all.
        let mut prices = std::collections::HashMap::new();
        prices.insert(priced, token_usd(0.5, 6));
        let profit_usd = 0.5; // 1_000_000 units @ $0.5
        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;

        // Event A: unpriced fee, no netting signal → must be flagged.
        let mut a = sample_event(700);
        a.profit_token = Some(priced);
        a.profit_amount = Some(U256::from(1_000_000u64));
        a.gas_cost_wei = U256::from(100_000_000_000_000u64);
        a.flashloan_fee_token = Some(unpriced_tok);
        a.flashloan_fee_wei = Some(U256::from(1_000u64));

        // Event B: same inputs, but the ledger already netted the fee → no flag.
        let mut b = a.clone();
        b.block = 701;
        let mut raw = [0x11u8; 32];
        raw[24..32].copy_from_slice(&701u64.to_be_bytes());
        b.tx_hash = B256::from(raw);
        b.details["flashloan_fee_netted_in_ledger"] = serde_json::json!(true);

        let store = ExplorerStore::open_in_memory().unwrap();
        for ev in [a, b] {
            let block = ev.block;
            seed_block(
                &store,
                SeedBlockParams {
                    block_number: block,
                    block_hash: &B256::repeat_byte(block as u8),
                    ts: 1_700_000_000,
                    tx_count: 1,
                    swaps: &[],
                    events: &[ev],
                    token_prices: &prices,
                },
            );
        }

        let ov = store.report_window_overview(0, None, None).unwrap();
        assert_eq!(ov.ops, 2);
        assert_eq!(
            ov.quality.ops_flash_fee_unpriced, 1,
            "only the non-netted fee is flagged"
        );
        // note_pricing_issue also marks the flagged op approximate.
        assert_eq!(
            ov.quality.ops_approx, 1,
            "the flagged op is the only approximate one"
        );

        let ops = store.ops_in_range(700, 701, &[]).unwrap();
        assert_eq!(ops.len(), 2);
        let row = |block: u64| ops.iter().find(|op| op.block_number == block).unwrap();
        let details = |block: u64| row(block).details_json.clone().unwrap_or_default();

        let a_details = details(700);
        assert!(
            a_details.contains("FLASH_FEE_UNPRICED"),
            "flagged op must carry the reason: {a_details}"
        );
        assert!(
            a_details.contains("usd_approximate"),
            "the reason must also mark the op approximate: {a_details}"
        );
        let b_details = details(701);
        assert!(
            !b_details.contains("FLASH_FEE_UNPRICED"),
            "a ledger-netted fee must not be flagged: {b_details}"
        );
        assert!(
            !b_details.contains("usd_approximate"),
            "a ledger-netted fee must not be marked approximate: {b_details}"
        );

        // Neither row invents a $0 quote, and A13's arithmetic is unchanged:
        // the unpriceable fee contributes nothing to `net` for both.
        for block in [700u64, 701] {
            let op = row(block);
            assert!(
                op.flashloan_fee_usd.is_none(),
                "block {block}: unpriced fee must stay NULL, not 0.0"
            );
            let net = op.net_profit_usd.unwrap_or(f64::NAN);
            assert!(
                (net - (profit_usd - gas_usd)).abs() < 1e-9,
                "block {block}: net {net} != {}",
                profit_usd - gas_usd
            );
        }
    }

    /// A loss-gated sandwich (priced net `<= 0`) is persisted with
    /// `excluded_from_net = 1` instead of dropped: op counts and gross stay
    /// inclusive, every *net* sum and `top_ops` skip it, and the report
    /// exposes the count. `net == 0.0` exactly is gated too (the gate is
    /// `<= 0.0`).
    #[test]
    fn loss_gated_sandwich_is_counted_but_excluded_from_net() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            address!("4444000000000000000000000000000000000004"),
            token_usd(0.5, 6),
        );

        // Sandwich: $0.5 gross against $0.75 gas → net = −$0.25.
        let mut loss = sample_event(800);
        loss.kind = MevKind::Sandwich;
        loss.tx_index = 1;
        loss.tx_hash = b256!("8000000000000000000000000000000000000000000000000000000000000001");
        // 1 native @ $0.75.
        loss.gas_cost_wei = U256::from(1_000_000_000_000_000_000u64);
        // Ordinary profitable arb in the same block: $0.5 gross, ~$0.000075 gas.
        let mut arb = sample_event(800);
        arb.tx_index = 2;
        arb.tx_hash = b256!("8000000000000000000000000000000000000000000000000000000000000002");

        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 800,
                block_hash: &B256::repeat_byte(0x80),
                ts: 1_700_000_000,
                tx_count: 5,
                swaps: &[],
                events: &[loss, arb],
                token_prices: &prices,
            },
        );
        // Counts stay truthful: both rows persisted, so the block counted 2.
        assert_eq!(n, 2);

        let gas_usd = 100_000_000_000_000f64 / 1e18 * 0.75;
        let arb_net = 0.5 - gas_usd;

        let ov = store.report_window_overview(0, None, None).unwrap();
        assert_eq!(ov.ops, 2);
        assert!(
            (ov.net_usd - arb_net).abs() < 1e-9,
            "net must be the arb's only, got {}",
            ov.net_usd
        );
        assert!(
            (ov.gross_usd - 1.0).abs() < 1e-9,
            "gross includes both, got {}",
            ov.gross_usd
        );
        assert_eq!(ov.quality.ops_excluded_from_net, 1);

        // The flag itself is persisted on the sandwich row only.
        let (flagged, flagged_net): (i64, f64) = store
            .conn
            .query_row(
                "SELECT excluded_from_net, net_profit_usd FROM mev_ops
             WHERE kind = 'sandwich' AND block_number = 800",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(flagged, 1);
        assert!((flagged_net - (-0.25)).abs() < 1e-9, "got {flagged_net}");

        // `top_ops` is "top by net": the loss-gated sandwich stays out.
        let top = store.top_ops(0, 10, None, None).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].kind, "arb_atomic");

        // The competitor leaderboard nets only the arb too.
        let comps = store.top_competitors_filtered(0, 10, None, None).unwrap();
        assert_eq!(comps.len(), 1);
        assert!(
            (comps[0].net_usd - arb_net).abs() < 1e-9,
            "got {}",
            comps[0].net_usd
        );

        // A sandwich with `net == 0.0` exactly is treated as excluded.
        let mut zero = sample_event(801);
        zero.kind = MevKind::Sandwich;
        zero.tx_index = 1;
        zero.tx_hash = b256!("8000000000000000000000000000000000000000000000000000000000000011");
        zero.profit_amount = Some(U256::ZERO); // priced → profit_usd = Some(0.0)
        zero.gas_cost_wei = U256::ZERO; // gas_usd = Some(0.0) → net = Some(0.0)
        let n = seed_block(
            &store,
            SeedBlockParams {
                block_number: 801,
                block_hash: &B256::repeat_byte(0x81),
                ts: 1_700_000_100,
                tx_count: 5,
                swaps: &[],
                events: &[zero],
                token_prices: &prices,
            },
        );
        assert_eq!(n, 1);
        let ops = store.ops_in_range(801, 801, &[]).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0].net_profit_usd,
            Some(0.0),
            "the gate saw net 0.0, not None"
        );

        let ov = store.report_window_overview(0, None, None).unwrap();
        assert_eq!(ov.ops, 3, "all three rows are counted");
        assert_eq!(ov.quality.ops_excluded_from_net, 2);
        assert!(
            (ov.net_usd - arb_net).abs() < 1e-9,
            "net unchanged by the zero-net row, got {}",
            ov.net_usd
        );
        assert!((ov.gross_usd - 1.0).abs() < 1e-9);

        // Per kind: sandwich counts stay inclusive while its net sum is empty.
        let kinds = store.report_by_kind(0, None, None).unwrap();
        let sand = kinds.iter().find(|r| r.label == "sandwich").unwrap();
        assert_eq!(sand.ops, 2);
        assert!((sand.gross_usd - 0.5).abs() < 1e-9);
        assert_eq!(sand.net_usd, 0.0);
    }

    /// The net-concentration guard must never divide by zero and must only emit
    /// a value for a strictly-positive net window.
    #[test]
    fn net_concentration_pct_boundary_cases() {
        let ov = |net: f64, top: f64| ReportOverview {
            ops: 1,
            volume_usd: 0.0,
            gross_usd: 0.0,
            gas_usd: 0.0,
            flash_fee_usd: 0.0,
            net_usd: net,
            highest_single_usd: top,
            searchers: 0,
            competitors: 0,
            quality: ReportQuality::default(),
        };

        // above and below the 40% threshold both return a value (warning decision is display-side).
        assert_eq!(
            ov(110.0, 100.0).net_concentration_pct(),
            Some((100.0 / 110.0) * 100.0)
        );
        assert_eq!(ov(100.0, 30.0).net_concentration_pct(), Some(30.0));
        assert_eq!(ov(100.0, 40.0).net_concentration_pct(), Some(40.0));
        // zero net → None (no divide-by-zero).
        assert!(ov(0.0, 5.0).net_concentration_pct().is_none());
        // negative net → None (dominant share undefined).
        assert!(ov(-1.0, 3.0).net_concentration_pct().is_none());
        // non-finite numerator → None.
        assert!(ov(10.0, f64::INFINITY).net_concentration_pct().is_none());
        // non-finite denominator → None.
        assert!(ov(f64::NAN, 5.0).net_concentration_pct().is_none());
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
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa70"
                ),
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
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa71"
                ),
                ts: 1_700_000_100,
                tx_count: 2,
                swaps: &[],
                events: &[ev_b],
                token_prices: &usdc_price(),
            },
        );

        let ov = store.report_window_overview(0, None, None).unwrap();
        assert_eq!(ov.ops, 2);
        assert_eq!(ov.searchers, 2);
        assert_eq!(ov.competitors, 1);

        let comps = store.top_competitors_filtered(0, 10, None, None).unwrap();
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
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa80"
                ),
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

    fn sample_reject(block_number: u64, reason: &str, created_at: u64) -> RejectedCandidate {
        RejectedCandidate {
            block_number,
            tx_index: Some(1),
            strategy: "explorer_classify".into(),
            pool_a: None,
            pool_b: None,
            path: None,
            token_in: None,
            token_out: None,
            input_amount: None,
            expected_profit: None,
            expected_profit_usd: None,
            gas_cost_wei: None,
            reject_reason: reason.into(),
            detail: None,
            created_at,
        }
    }

    #[test]
    fn report_classify_rejects_counts_by_reason_in_window() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let since = 1_700_000_000u64;
        // Block row present → window membership via blocks.ts.
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 900,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa90"
                ),
                ts: since + 10,
                tx_count: 0,
                swaps: &[],
                events: &[],
                token_prices: &usdc_price(),
            },
        );
        store
            .insert_rejected_candidate("run", "test", &sample_reject(900, "net_zero_searcher", 0))
            .unwrap();
        // Same block/tx/reason as the row above, different `detail`: this is a
        // distinct candidate and must survive the natural-key index.
        let mut detailed = sample_reject(900, "net_zero_searcher", 0);
        detailed.detail = Some("anchor_profit_zero".into());
        store
            .insert_rejected_candidate("run", "test", &detailed)
            .unwrap();
        store
            .insert_rejected_candidate("run", "test", &sample_reject(900, "unresolved_cycle", 0))
            .unwrap();
        // No block row → falls back to created_at; before the window.
        store
            .insert_rejected_candidate(
                "run",
                "test",
                &sample_reject(901, "open_inventory", since - 1),
            )
            .unwrap();
        // No block row → created_at inside the window.
        store
            .insert_rejected_candidate("run", "test", &sample_reject(902, "builder_tip", since + 5))
            .unwrap();
        // Other strategies never show up in the classify slice.
        let mut scanner_reject = sample_reject(900, "no_pool", since + 10);
        scanner_reject.strategy = "scanner_v1".into();
        store
            .insert_rejected_candidate("run", "test", &scanner_reject)
            .unwrap();

        let rejects = store.report_classify_rejects(since).unwrap();
        assert_eq!(rejects.len(), 3);
        assert_eq!(rejects[0].reason, "net_zero_searcher");
        assert_eq!(rejects[0].count, 2);
        assert_eq!(rejects[1].reason, "builder_tip");
        assert_eq!(rejects[1].count, 1);
        assert_eq!(rejects[2].reason, "unresolved_cycle");
        assert_eq!(rejects[2].count, 1);

        // Older window excludes everything.
        assert!(store
            .report_classify_rejects(since + 100)
            .unwrap()
            .is_empty());
    }

    /// Re-indexing a block re-derives the same rejects with only `created_at`
    /// differing; the natural-key unique index must make that a no-op instead
    /// of doubling every reject denominator.
    #[test]
    fn reject_insert_is_idempotent_across_reindex_passes() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let first = sample_reject(910, "unresolved_cycle", 0);
        let run_id = "explorer-index-910";
        store
            .insert_rejected_candidate(run_id, "test", &first)
            .unwrap();
        // Second pass over the same block: same candidate, later `created_at`.
        let mut again = first.clone();
        again.created_at = 60;
        store
            .insert_rejected_candidate(run_id, "test", &again)
            .unwrap();
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM rejected_candidates WHERE block_number = 910",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "a re-index pass must not duplicate rejects");

        // A different tx in the same block is a different reject.
        let mut other_tx = first.clone();
        other_tx.tx_index = Some(2);
        store
            .insert_rejected_candidate(run_id, "test", &other_tx)
            .unwrap();
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM rejected_candidates WHERE block_number = 910",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    /// A failure part-way through a batch insert must roll back the rows
    /// already written in that transaction: no half-written results set.
    #[test]
    fn insert_opportunities_rolls_back_on_error() {
        let store = ExplorerStore::open_in_memory().unwrap();
        // Fail the second row after the first one has been inserted inside the
        // same transaction.
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_second_opportunity
                 BEFORE INSERT ON opportunities
                 WHEN NEW.block_number = 2
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            )
            .unwrap();
        let row = |block_number: u64| OpportunityInput {
            run_id: "rollback-run",
            chain: "test",
            block_number,
            tx_index: Some(0),
            strategy: "atomic",
            pool_a: None,
            pool_b: None,
            token_in: None,
            token_out: None,
            input_amount: None,
            expected_profit: None,
            gas_cost_wei: None,
            path: None,
            timestamp: None,
            mempool_only: false,
            confidence: None,
            sender: None,
            tx_hash: None,
            detection_path: None,
            canonical_id: None,
        };
        let err = store.insert_opportunities(&[row(1), row(2)]).unwrap_err();
        assert!(
            err.to_string().contains("forced failure"),
            "unexpected error: {err}"
        );
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM opportunities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "the first row must not survive the failed batch");
    }

    #[test]
    fn reindex_keeps_op_id_and_revises_usd() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let token = address!("4444000000000000000000000000000000000004");
        let hash = b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let mut prices = std::collections::HashMap::new();
        prices.insert(token, token_usd(0.5, 6));
        let event = sample_event(100);
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 100,
                block_hash: &hash,
                ts: 1_700_000_000,
                tx_count: 5,
                swaps: &[],
                events: std::slice::from_ref(&event),
                token_prices: &prices,
            },
        );
        let (id, created, profit): (i64, i64, f64) = store
            .conn
            .query_row(
                "SELECT id, created_at, profit_usd FROM mev_ops WHERE block_number = 100",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        prices.insert(token, token_usd(2.0, 6));
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 100,
                block_hash: &hash,
                ts: 1_700_000_000,
                tx_count: 5,
                swaps: &[],
                events: std::slice::from_ref(&event),
                token_prices: &prices,
            },
        );
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM mev_ops", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "re-index must not insert a second op");
        let (id2, created2, profit2): (i64, i64, f64) = store
            .conn
            .query_row(
                "SELECT id, created_at, profit_usd FROM mev_ops WHERE block_number = 100",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(id2, id);
        assert_eq!(created2, created);
        assert!(
            (profit - 0.5).abs() < 1e-9,
            "first quote was 0.5, got {profit}"
        );
        assert!(
            (profit2 - 2.0).abs() < 1e-9,
            "revised quote must replace profit_usd, got {profit2}"
        );
    }

    #[test]
    fn reprice_range_uses_stored_prices_without_rewriting_identity() {
        let store = ExplorerStore::open_in_memory().unwrap();
        let token = address!("4444000000000000000000000000000000000004");
        let mut prices = std::collections::HashMap::new();
        prices.insert(token, token_usd(0.5, 6));
        seed_block(
            &store,
            SeedBlockParams {
                block_number: 100,
                block_hash: &b256!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                ),
                ts: 1_700_000_000,
                tx_count: 1,
                swaps: &[],
                events: &[sample_event(100)],
                token_prices: &prices,
            },
        );
        let hour = crate::explorer::pricing::hour_bucket(1_700_000_000);
        store.put_price(token, hour, 4.0, "test").unwrap();
        let n = store
            .reprice_range(100, 100, |addr| (addr == token).then_some(6))
            .unwrap();
        assert_eq!(n, 1);
        let (profit, kind): (f64, String) = store
            .conn
            .query_row(
                "SELECT profit_usd, kind FROM mev_ops WHERE block_number = 100",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            (profit - 4.0).abs() < 1e-9,
            "stored 4.0 quote, got {profit}"
        );
        assert_eq!(kind, "arb_atomic");
    }
}
