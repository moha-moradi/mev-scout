//! Monthly per-strategy MEV revenue report generator.
//!
//! Runs a curated set of Dune queries — one per measurable strategy from
//! `mev_strategies_analysis_summary.md` — for a chain and block range, then
//! renders the results as Markdown, a self-contained HTML dashboard, or JSON.
//!
//! # Measurement contract
//! Every query returns the same 6 columns (in order):
//! `opportunity_count`(0), `avg_profit_usd`(1), `total_profit_usd`(2),
//! `period_start`(3), `period_end`(4), `period_days`(5)
//!
//! # Methodology
//! Dune reports the *total addressable market* (value extracted by all bots,
//! plus victims' losses), not revenue from a specific bot. Each strategy row
//! states its profit basis (e.g. "trade volume × 0.3%") so estimates carry
//! error bars. Strategies without decoded Dune tables are listed separately.
//!
//! # Rate limits
//! Queries run sequentially with a 5s delay (Dune free tier: ~1 execution/5s).
//! A full report (~20 queries) takes ~2–4 minutes per chain.

use serde::Serialize;

use super::client::DuneClient;
use super::queries;
use super::util::{approx_block_month_min, chain_timing, dune_chain_label};

/// Status of a single strategy measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStatus {
    /// Query succeeded and returned a positive opportunity count.
    Ok,
    /// Query succeeded but returned no rows / zero count.
    NoData,
    /// Query failed because the decoded table does not exist on this chain.
    TableMissing,
    /// Query failed for another reason.
    Error,
}

impl ReportStatus {
    pub fn label(self) -> &'static str {
        match self {
            ReportStatus::Ok => "ok",
            ReportStatus::NoData => "no-data",
            ReportStatus::TableMissing => "table-missing",
            ReportStatus::Error => "error",
        }
    }
}

/// One strategy's measurement for the report.
#[derive(Debug, Clone, Serialize)]
pub struct StrategyReportItem {
    pub strategy_id: u32,
    pub strategy: String,
    pub query: String,
    pub source: String,
    pub status: String,
    pub opportunity_count: u64,
    pub avg_profit_usd: f64,
    pub total_profit_usd: f64,
    pub period_start: String,
    pub period_end: String,
    pub period_days: u64,
    pub est_monthly_usd: f64,
    pub claim_low_usd: Option<f64>,
    pub claim_high_usd: Option<f64>,
    pub verdict: String,
    pub note: String,
}

/// Report metadata: chain, block range, approximate time window.
#[derive(Debug, Clone, Serialize)]
pub struct ReportMeta {
    pub chain: String,
    pub chain_label: String,
    pub from_block: u64,
    pub to_block: u64,
    pub from_time: String,
    pub to_time: String,
    pub generated_at: String,
    pub query_count: usize,
    pub measured_count: usize,
    pub total_est_monthly_usd: f64,
}

/// The full report: metadata + per-strategy rows.
#[derive(Debug, Clone, Serialize)]
pub struct StrategyReport {
    pub meta: ReportMeta,
    pub items: Vec<StrategyReportItem>,
}

/// A single report query definition.
struct ReportQuery {
    strategy_id: u32,
    strategy: &'static str,
    query: &'static str,
    source: &'static str,
    /// (monthly low, monthly high) USD income claim from the analysis doc.
    claim: Option<(f64, f64)>,
    /// Profit basis description shown in the report.
    note: &'static str,
    sql: String,
}

/// Strategies whose revenue is approximated by one of the report queries
/// rather than measured directly.
pub const COVERED_BY_PROXY: &[(&str, &str)] = &[
    ("#9 Flash swap arbitrage", "flash-loan volume × 0.5%"),
    ("#16 Interest accrual liq", "liquidation volume × 5% bonus"),
    ("#20 AAVE partial liq", "liquidation volume × 5% bonus"),
    ("#29 Statistical arb", "backrun / long-tail proxies"),
    ("#30 Oracle-latency liq", "liquidation volume × 5% bonus"),
    ("#40 Cross-chain arb", "bridge flow volume × 0.3%"),
    ("#41 Bridge MEV", "bridge flow volume × 0.3%"),
    ("#42 Solver / intent MEV", "aggregator trade volume × 0.2%"),
    ("#51 JIT + arb combo", "JIT query (same events)"),
    ("#53 Cascading liq eng.", "liquidation volume × 5% bonus"),
];

/// Strategies that cannot be measured on Dune at all (off-chain / private data).
pub const NOT_MEASURABLE: &[(&str, &str)] = &[
    ("#7 Airdrop MEV", "claim contracts unknown; off-chain discovery"),
    ("#8 GMX v1 keeper", "Vault_evt_Liquidation not decoded on any chain"),
    ("#13 Perp protocol keeper", "Gains Network / dYdX data not on Dune"),
    ("#23 L2 sequencer MEV", "sequencer-internal ordering data"),
    ("#24 NFT floor arbitrage", "no NFT floor-price feed"),
    ("#25 ERC-4337 bundler MEV", "bundler reward / alt-mempool data"),
    ("#32 NFT collateral liq", "NFTfi/BendDAO tables absent or sparse"),
    ("#33 Governance MEV", "off-chain forum / snapshot data"),
    ("#44 CEX–DEX arbitrage", "no CEX order-book data"),
    ("#46 TWAP manipulation", "deprioritized (adversarial)"),
    ("#50 PBS / MEV-Boost", "builder-side revenue, not on Dune"),
    ("#52 Multi-block MEV", "validator relationship data"),
];

/// Strategies that are measurable in principle but have no query template yet.
pub const DEFERRED_MEASURABLE: &[&str] = &[
    "#12 V3 range order snipe",
    "#15 Liquity stability pool front-run",
    "#21 Bad debt prevention",
    "#26 Velodrome/Aerodrome epoch",
    "#27 Trader Joe V2 Liquidity Book",
    "#34 Pendle PT/YT yield spread",
    "#35 Balancer rate provider staleness",
    "#37 Lido oracle report front-run",
    "#38 Convex/Curve gauge vote epoch",
    "#48 Morpho Blue market transition",
    "#49 Uniswap V4 hook MEV",
];

/// Approximate the timestamp of a block number (for time-based queries).
fn approx_block_timestamp(block: u64, chain: &str) -> chrono::NaiveDateTime {
    let p = chain_timing(chain);
    let ts = p.genesis_ts + (block as f64 * p.secs_per_block) as i64;
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap_or_default()
        .naive_utc()
}

/// Group a number's integer digits with thousands separators.
fn group_thousands(num: &str) -> String {
    let (int, frac) = match num.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (num, None),
    };
    let mut out = String::new();
    let digits: Vec<char> = int.chars().collect();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

fn fmt_usd(v: f64) -> String {
    group_thousands(&format!("{:.0}", v))
}

fn fmt_usd_2(v: f64) -> String {
    group_thousands(&format!("{:.2}", v))
}

fn fmt_count(v: u64) -> String {
    group_thousands(&v.to_string())
}

/// The list of report queries for a chain, in doc order.
fn strategy_queries(chain: &str) -> Vec<ReportQuery> {
    let chain_label = dune_chain_label(chain);
    let v3_factory_table = match chain_label.as_str() {
        "polygon" => "uniswap_v3_polygon.factory_polygon_evt_PoolCreated".to_string(),
        other => format!("uniswap_v3_{other}.Factory_evt_PoolCreated"),
    };

    vec![
        // #1 sync() race / #2 skim() capture — V2 Sync events without swaps.
        ReportQuery {
            strategy_id: 1,
            strategy: "sync() race".into(),
            query: "VALIDATE_SYNC_RACE".into(),
            source: "uniswap_v2_{chain}.Pair_evt_Sync".into(),
            claim: Some((0.0, 50.0)),
            note: "opportunity count only; defensive sync() carries no direct revenue".into(),
            sql: queries::VALIDATE_SYNC_RACE.to_string(),
        },
        ReportQuery {
            strategy_id: 2,
            strategy: "skim() capture".into(),
            query: "VALIDATE_SKIM_CAPTURE".into(),
            source: "uniswap_v2_{chain}.Pair_evt_Sync".into(),
            claim: Some((50.0, 500.0)),
            note: "reserve delta on Sync-without-Swap (excess token drift)".into(),
            sql: queries::VALIDATE_SKIM_CAPTURE.to_string(),
        },
        // #3 Init price snipe — new V3 pools whose first swap volume is proxy for snipe value.
        ReportQuery {
            strategy_id: 3,
            strategy: "Init price snipe".into(),
            query: "VALIDATE_INIT_PRICE_SNIPE (chain-aware)".into(),
            source: "uniswap_v3_{chain} PoolCreated + first trade".into(),
            claim: Some((100.0, 2000.0)),
            note: "first-swap USD volume after pool creation (opportunity size, not capture)".into(),
            sql: format!(
                r#"WITH new_pools AS (
  SELECT
    p.evt_block_number AS block_number,
    p.evt_tx_hash AS tx_hash,
    p.contract_address AS pool_address,
    p.token0,
    p.token1,
    p.evt_block_time AS block_time
  FROM {f} p
  WHERE p.evt_block_number >= {{from_block}}
    AND p.evt_block_number <= {{to_block}}
),
first_swaps AS (
  SELECT
    t.project_contract_address AS pool_address,
    t.block_number,
    t.amount_usd,
    t.block_time,
    ROW_NUMBER() OVER (PARTITION BY t.project_contract_address ORDER BY t.block_number, t.tx_hash) AS rn
  FROM dex.trades t
  WHERE t.blockchain = '{{chain}}'
    AND t.block_month >= DATE '{{block_month_min}}'
    AND t.project = 'uniswap_v3'
    AND t.project_contract_address IN (SELECT pool_address FROM new_pools)
)
SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(fs.amount_usd), 0) AS avg_profit_usd,
  COALESCE(SUM(fs.amount_usd), 0) AS total_profit_usd,
  MIN(fs.block_time) AS period_start,
  MAX(fs.block_time) AS period_end,
  DATE_DIFF('day', MIN(fs.block_time), MAX(fs.block_time)) AS period_days
FROM first_swaps fs
JOIN new_pools np ON np.pool_address = fs.pool_address
WHERE fs.rn = 1
  AND fs.amount_usd > 0"#,
                f = v3_factory_table,
            ),
        },
        // #4 Backrunning — multi-pool txs following a >$10K swap.
        ReportQuery {
            strategy_id: 4,
            strategy: "Backrunning".into(),
            query: "VALIDATE_BACKRUN".into(),
            source: "dex.trades".into(),
            claim: Some((500.0, 5000.0)),
            note: "multi-pool tx volume × 0.3% fee-rate proxy".into(),
            sql: queries::VALIDATE_BACKRUN.to_string(),
        },
        // #10 Long-tail token arb.
        ReportQuery {
            strategy_id: 10,
            strategy: "Long-tail token arb".into(),
            query: "VALIDATE_LONG_TAIL_ARB".into(),
            source: "dex.trades".into(),
            claim: Some((300.0, 2000.0)),
            note: "low-volume-token multi-pool tx volume × 0.5%".into(),
            sql: queries::VALIDATE_LONG_TAIL_ARB.to_string(),
        },
        // #11 Sandwich — curated attacker-side dataset.
        ReportQuery {
            strategy_id: 11,
            strategy: "Sandwich attack".into(),
            query: "REPORT_SANDWICH_VOLUME".into(),
            source: "dex.sandwiches".into(),
            claim: Some((1000.0, 10000.0)),
            note: "sandwich trade volume × 0.5% extraction proxy".into(),
            sql: r#"SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(s.amount_usd * 0.005), 0) AS avg_profit_usd,
  COALESCE(SUM(s.amount_usd * 0.005), 0) AS total_profit_usd,
  MIN(s.block_time) AS period_start,
  MAX(s.block_time) AS period_end,
  DATE_DIFF('day', MIN(s.block_time), MAX(s.block_time)) AS period_days
FROM dex.sandwiches s
WHERE s.blockchain = '{chain}'
  AND s.block_month >= DATE '{block_month_min}'
  AND s.block_number >= {from_block}
  AND s.block_number <= {to_block}"#
                .into(),
        },
        // #9 Flash swap arbitrage — flash-loan volume proxy.
        ReportQuery {
            strategy_id: 9,
            strategy: "Flash swap arbitrage".into(),
            query: "REPORT_FLASHLOAN_VOLUME".into(),
            source: "lending.flashloans".into(),
            claim: Some((500.0, 5000.0)),
            note: "flash-loan volume × 0.5% arb-capture proxy".into(),
            sql: r#"SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(f.amount_usd * 0.005), 0) AS avg_profit_usd,
  COALESCE(SUM(f.amount_usd * 0.005), 0) AS total_profit_usd,
  MIN(f.block_time) AS period_start,
  MAX(f.block_time) AS period_end,
  DATE_DIFF('day', MIN(f.block_time), MAX(f.block_time)) AS period_days
FROM lending.flashloans f
WHERE f.blockchain = '{chain}'
  AND f.block_month >= DATE '{block_month_min}'
  AND f.block_number >= {from_block}
  AND f.block_number <= {to_block}"#
                .into(),
        },
        // #18 Flash loan atomic liquidation.
        ReportQuery {
            strategy_id: 18,
            strategy: "Flash loan atomic liq".into(),
            query: "VALIDATE_FLASH_LIQ_PROFIT".into(),
            source: "lending.flashloans ∩ lending.borrow".into(),
            claim: Some((500.0, 3000.0)),
            note: "flash loan amount in txs that also liquidate (volume proxy)".into(),
            sql: queries::VALIDATE_FLASH_LIQ_PROFIT.to_string(),
        },
        // #16/#20/#30/#53 Liquidation cluster — all borrow_liquidations.
        ReportQuery {
            strategy_id: 16,
            strategy: "Liquidations (all protocols)".into(),
            query: "REPORT_LIQUIDATION_VOLUME".into(),
            source: "lending.borrow (borrow_liquidation)".into(),
            claim: Some((200.0, 10000.0)),
            note: "liquidation volume × 5% bonus; covers #16, #20, #30, #53 cluster".into(),
            sql: r#"SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(l.amount_usd * 0.05), 0) AS avg_profit_usd,
  COALESCE(SUM(l.amount_usd * 0.05), 0) AS total_profit_usd,
  MIN(l.block_time) AS period_start,
  MAX(l.block_time) AS period_end,
  DATE_DIFF('day', MIN(l.block_time), MAX(l.block_time)) AS period_days
FROM lending.borrow l
WHERE l.blockchain = '{chain}'
  AND l.transaction_type = 'borrow_liquidation'
  AND l.block_month >= DATE '{block_month_min}'
  AND l.block_number >= {from_block}
  AND l.block_number <= {to_block}"#
                .into(),
        },
        // #17 Stablecoin depeg arbitrage.
        ReportQuery {
            strategy_id: 17,
            strategy: "Stablecoin depeg arb".into(),
            query: "VALIDATE_STABLECOIN_DEPEG".into(),
            source: "dex.trades (project='curve')".into(),
            claim: Some((5000.0, 50000.0)),
            note: "stablecoin curve-trade volume × 1% depeg-wedge proxy".into(),
            sql: queries::VALIDATE_STABLECOIN_DEPEG.to_string(),
        },
        // #22 Curve pool imbalance.
        ReportQuery {
            strategy_id: 22,
            strategy: "Curve pool imbalance".into(),
            query: "REPORT_CURVE_VOLUME".into(),
            source: "dex.trades (project='curve')".into(),
            claim: Some((500.0, 3000.0)),
            note: "curve trade volume × 0.3% imbalance-capture proxy".into(),
            sql: r#"SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(t.amount_usd * 0.003), 0) AS avg_profit_usd,
  COALESCE(SUM(t.amount_usd * 0.003), 0) AS total_profit_usd,
  MIN(t.block_time) AS period_start,
  MAX(t.block_time) AS period_end,
  DATE_DIFF('day', MIN(t.block_time), MAX(t.block_time)) AS period_days
FROM dex.trades t
WHERE t.blockchain = '{chain}'
  AND t.project = 'curve'
  AND t.block_month >= DATE '{block_month_min}'
  AND t.block_number >= {from_block}
  AND t.block_number <= {to_block}"#
                .into(),
        },
        // #28 JIT liquidity (V3).
        ReportQuery {
            strategy_id: 28,
            strategy: "JIT liquidity (V3)".into(),
            query: "VALIDATE_JIT_FEE_CAPTURE".into(),
            source: "uniswap_v3_{chain} Mint/Burn + dex.trades".into(),
            claim: Some((2000.0, 10000.0)),
            note: "Mint+Swap+Burn bundles; $1,000/event placeholder in template".into(),
            sql: queries::VALIDATE_JIT_FEE_CAPTURE.to_string(),
        },
        // #31 LST depeg collateral liq.
        ReportQuery {
            strategy_id: 31,
            strategy: "LST depeg collateral liq".into(),
            query: "VALIDATE_LST_DEPEG_LIQ".into(),
            source: "aave_v3_{chain}.Pool_evt_LiquidationCall".into(),
            claim: Some((2000.0, 20000.0)),
            note: "LST-collateral liquidation amount (raw, no bonus). Ethereum-native".into(),
            sql: queries::VALIDATE_LST_DEPEG_LIQ.to_string(),
        },
        // #19 MakerDAO Clip Dutch auction.
        ReportQuery {
            strategy_id: 19,
            strategy: "MakerDAO Clip auction".into(),
            query: "VALIDATE_MAKERDAO_CLIP".into(),
            source: "maker_{chain}.Clipper_evt_Take".into(),
            claim: Some((500.0, 3000.0)),
            note: "lot size (collateral per take); Ethereum only, sparse".into(),
            sql: queries::VALIDATE_MAKERDAO_CLIP.to_string(),
        },
        // #45 MakerDAO OSM preview + kick().
        ReportQuery {
            strategy_id: 45,
            strategy: "MakerDAO OSM kick()".into(),
            query: "VALIDATE_MAKERDAO_KICK".into(),
            source: "lending.borrow (project='maker')".into(),
            claim: Some((500.0, 5000.0)),
            note: "maker liquidation USD volume; kicker reward ≈ small % — volume is upper bound".into(),
            sql: queries::VALIDATE_MAKERDAO_KICK.to_string(),
        },
        // #39 Liquity recovery mode cascade.
        ReportQuery {
            strategy_id: 39,
            strategy: "Liquity recovery cascade".into(),
            query: "VALIDATE_LIQUITY_RECOVERY".into(),
            source: "liquity_{chain}.TroveManager_evt_TroveLiquidated".into(),
            claim: Some((2000.0, 20000.0)),
            note: "trove debt liquidated (USD proxy); Ethereum only, event-driven".into(),
            sql: queries::VALIDATE_LIQUITY_RECOVERY.to_string(),
        },
        // #14 Synthetix flag + delayed liq.
        ReportQuery {
            strategy_id: 14,
            strategy: "Synthetix flag + delayed liq".into(),
            query: "VALIDATE_SYNTHETIX_LIQ".into(),
            source: "synthetix_v3_{chain}.core_evt_liquidation".into(),
            claim: Some((100.0, 500.0)),
            note: "debtLiquidated (USD proxy); data from 2025-04-03, Ethereum".into(),
            sql: queries::VALIDATE_SYNTHETIX_LIQ.to_string(),
        },
        // #36 GMX V2 ADL front-run.
        ReportQuery {
            strategy_id: 36,
            strategy: "GMX V2 ADL front-run".into(),
            query: "VALIDATE_GMX_V2_ADL".into(),
            source: "gmx_v2_{chain}.liquidationhandler/adlhandler".into(),
            claim: Some((1000.0, 5000.0)),
            note: "oracle-error events; tables exist but empty on Arbitrum".into(),
            sql: queries::VALIDATE_GMX_V2_ADL.to_string(),
        },
        // #8 GMX v1 keeper — table missing on Dune.
        ReportQuery {
            strategy_id: 8,
            strategy: "GMX v1 keeper race".into(),
            query: "VALIDATE_GMX_V1_KEEPER".into(),
            source: "gmx_{chain}.Vault_evt_Liquidation".into(),
            claim: Some((200.0, 1000.0)),
            note: "not decoded on any Dune chain — expected to fail".into(),
            sql: queries::VALIDATE_GMX_V1_KEEPER.to_string(),
        },
        // #13 Perp protocol keeper — placeholder, always empty.
        ReportQuery {
            strategy_id: 13,
            strategy: "Perp protocol keeper".into(),
            query: "VALIDATE_PERP_KEEPER".into(),
            source: "(placeholder — no table)".into(),
            claim: Some((200.0, 1000.0)),
            note: "Gains Network / dYdX liquidation data not on Dune".into(),
            sql: queries::VALIDATE_PERP_KEEPER.to_string(),
        },
        // #40/#41 Cross-chain arb / bridge MEV — bridge flow volume proxy.
        ReportQuery {
            strategy_id: 40,
            strategy: "Cross-chain arb / Bridge MEV".into(),
            query: "REPORT_BRIDGE_FLOWS".into(),
            source: "bridges_evms.flows".into(),
            claim: Some((1000.0, 10000.0)),
            note: "bridged volume × 0.3% price-dislocation proxy; covers #40, #41".into(),
            sql: r#"SELECT
  COUNT(DISTINCT b.tx_hash) AS opportunity_count,
  COALESCE(AVG(b.amount_usd * 0.003), 0) AS avg_profit_usd,
  COALESCE(SUM(b.amount_usd * 0.003), 0) AS total_profit_usd,
  MIN(b.block_time) AS period_start,
  MAX(b.block_time) AS period_end,
  DATE_DIFF('day', MIN(b.block_time), MAX(b.block_time)) AS period_days
FROM bridges_evms.flows b
WHERE b.source_blockchain = '{chain}'
  AND b.block_time >= TIMESTAMP '{from_time}'
  AND b.block_time < TIMESTAMP '{to_time}'"#
                .into(),
        },
        // #42 Solver / intent MEV — aggregator trade volume proxy.
        ReportQuery {
            strategy_id: 42,
            strategy: "Solver / intent MEV".into(),
            query: "REPORT_AGGREGATOR_VOLUME".into(),
            source: "dex_aggregator.trades".into(),
            claim: Some((2000.0, 10000.0)),
            note: "aggregator-routed volume × 0.2% solver-fee proxy".into(),
            sql: r#"SELECT
  COUNT(*) AS opportunity_count,
  COALESCE(AVG(a.amount_usd * 0.002), 0) AS avg_profit_usd,
  COALESCE(SUM(a.amount_usd * 0.002), 0) AS total_profit_usd,
  MIN(a.block_time) AS period_start,
  MAX(a.block_time) AS period_end,
  DATE_DIFF('day', MIN(a.block_time), MAX(a.block_time)) AS period_days
FROM dex_aggregator.trades a
WHERE a.blockchain = '{chain}'
  AND a.block_month >= DATE '{block_month_min}'
  AND a.block_number >= {from_block}
  AND a.block_number <= {to_block}"#
                .into(),
        },
    ]
}

fn render_query_sql(
    template: &str,
    chain: &str,
    block_month_min: &str,
    from_block: u64,
    to_block: u64,
    from_time: &str,
    to_time: &str,
) -> String {
    template
        .replace("{chain}", chain)
        .replace("{block_month_min}", block_month_min)
        .replace("{from_block}", &from_block.to_string())
        .replace("{to_block}", &to_block.to_string())
        .replace("{from_time}", from_time)
        .replace("{to_time}", to_time)
}

/// Parse a numeric value from a Dune row that may be number or string.
fn row_f64(row: &super::types::DuneRow, key: &str) -> f64 {
    row.get(key)
        .and_then(|v| {
            if let Some(n) = v.as_f64() {
                return Some(n);
            }
            if let Some(n) = v.as_i64() {
                return Some(n as f64);
            }
            if let Some(n) = v.as_u64() {
                return Some(n as f64);
            }
            if let Some(s) = v.as_str() {
                return s.trim().parse::<f64>().ok();
            }
            None
        })
        .unwrap_or(0.0)
}

fn row_u64(row: &super::types::DuneRow, key: &str) -> u64 {
    row.get(key)
        .and_then(|v| {
            if let Some(n) = v.as_u64() {
                return Some(n);
            }
            if let Some(n) = v.as_i64() {
                return Some(n.max(0) as u64);
            }
            if let Some(s) = v.as_str() {
                return s.trim().parse::<u64>().ok();
            }
            None
        })
        .unwrap_or(0)
}

fn row_str(row: &super::types::DuneRow, key: &str) -> String {
    row.get(key)
        .map(|v| v.to_string())
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Build an item from the first row of a successful result.
fn item_from_row(rq: &ReportQuery, row: &super::types::DuneRow) -> StrategyReportItem {
    let count = row_u64(row, "opportunity_count");
    let total = row_f64(row, "total_profit_usd");
    let days = row_u64(row, "period_days");
    let est_monthly = if days >= 1 { total / days as f64 * 30.0 } else { total };

    let verdict = match rq.claim {
        Some((lo, hi)) => {
            if est_monthly < lo {
                "below claim"
            } else if est_monthly > hi {
                "above claim"
            } else {
                "in claim range"
            }
        }
        None => "",
    };

    StrategyReportItem {
        strategy_id: rq.strategy_id,
        strategy: rq.strategy.to_string(),
        query: rq.query.to_string(),
        source: rq.source.to_string(),
        status: if count > 0 {
            ReportStatus::Ok.label().to_string()
        } else {
            ReportStatus::NoData.label().to_string()
        },
        opportunity_count: count,
        avg_profit_usd: row_f64(row, "avg_profit_usd"),
        total_profit_usd: total,
        period_start: row_str(row, "period_start"),
        period_end: row_str(row, "period_end"),
        period_days: days,
        est_monthly_usd: est_monthly,
        claim_low_usd: rq.claim.map(|(lo, _)| lo),
        claim_high_usd: rq.claim.map(|(_, hi)| hi),
        verdict: verdict.to_string(),
        note: rq.note.to_string(),
    }
}

/// Build an empty item (no data, missing table, or error).
fn item_empty(rq: &ReportQuery, status: ReportStatus, err: Option<String>) -> StrategyReportItem {
    StrategyReportItem {
        strategy_id: rq.strategy_id,
        strategy: rq.strategy.to_string(),
        query: rq.query.to_string(),
        source: rq.source.to_string(),
        status: status.label().to_string(),
        opportunity_count: 0,
        avg_profit_usd: 0.0,
        total_profit_usd: 0.0,
        period_start: String::new(),
        period_end: String::new(),
        period_days: 0,
        est_monthly_usd: 0.0,
        claim_low_usd: rq.claim.map(|(lo, _)| lo),
        claim_high_usd: rq.claim.map(|(_, hi)| hi),
        verdict: String::new(),
        note: err.unwrap_or_else(|| rq.note.to_string()),
    }
}

impl StrategyReport {
    /// Run all measurable strategy queries for a chain and block range.
    ///
    /// Queries run sequentially with a 5s delay between executions to stay
    /// within Dune free-tier rate limits (~1 execution / 5s).
    pub async fn run(
        client: &DuneClient,
        chain: &str,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Self> {
        let chain_label = dune_chain_label(chain);
        let block_month_min = approx_block_month_min(from_block, &chain_label);
        let from_time =
            approx_block_timestamp(from_block, &chain_label).format("%Y-%m-%d %H:%M:%S").to_string();
        let to_time =
            approx_block_timestamp(to_block, &chain_label).format("%Y-%m-%d %H:%M:%S").to_string();

        let queries = strategy_queries(chain);
        let mut items = Vec::with_capacity(queries.len());

        for rq in &queries {
            eprintln!(
                "  [#{:>2}] {:<32} ...",
                rq.strategy_id, rq.strategy
            );
            let sql = render_query_sql(
                &rq.sql,
                &chain_label,
                &block_month_min,
                from_block,
                to_block,
                &from_time,
                &to_time,
            );

            let item = match client.execute_raw_sql(&sql).await {
                Ok(res) => match res.result {
                    Some(r) if !r.rows.is_empty() => item_from_row(rq, &r.rows[0]),
                    _ => item_empty(rq, ReportStatus::NoData, None),
                },
                Err(e) => {
                    let msg = format!("{e}");
                    let status = if msg.contains("does not exist")
                        || msg.contains("not found")
                        || msg.contains("Unsupported")
                    {
                        ReportStatus::TableMissing
                    } else {
                        ReportStatus::Error
                    };
                    item_empty(rq, status, Some(msg))
                }
            };
            eprintln!(
                "       -> {} ({} opps, ${:.0} est monthly)",
                item.status,
                item.opportunity_count,
                item.est_monthly_usd
            );
            items.push(item);

            // Dune free tier: ~1 execution per 5 seconds.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        let measured_count = items
            .iter()
            .filter(|i| i.status == ReportStatus::Ok.label())
            .count();
        let total_est_monthly_usd: f64 = items.iter().map(|i| i.est_monthly_usd).sum();

        Ok(StrategyReport {
            meta: ReportMeta {
                chain: chain.to_string(),
                chain_label,
                from_block,
                to_block,
                from_time,
                to_time,
                generated_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                query_count: items.len(),
                measured_count,
                total_est_monthly_usd,
            },
            items,
        })
    }

    /// Render the report as Markdown (the `mev_strategies_analysis_summary.md` §8 style).
    pub fn render_markdown(&self) -> String {
        let m = &self.meta;
        let mut out = String::new();
        out.push_str(&format!(
            "# MEV Strategy Monthly Revenue Report — {}\n\n",
            m.chain
        ));
        out.push_str(&format!(
            "> Generated: {} — Blocks {}–{} ({} → {})\n\n",
            m.generated_at, m.from_block, m.to_block, m.from_time, m.to_time
        ));
        out.push_str(&format!(
            "> **Measured strategies: {}/{}** — total est. monthly value **${}**\n",
            m.measured_count, m.query_count, fmt_usd(m.total_est_monthly_usd)
        ));
        out.push_str(
            "\n> **Methodology**: Dune reports the *total addressable market* (value extracted by all bots, \
             plus victims' losses), not revenue from a specific bot. Each row states its profit basis; \
             estimates carry large error bars. Strategies whose decoded tables do not exist on Dune are listed at the end.\n\n",
        );
        out.push_str("| # | Strategy | Status | Opps | Avg $ | Total $ | Est. /mo | Claim /mo | Verdict | Basis |\n");
        out.push_str("|---|----------|--------|-----:|-------:|--------:|---------:|-----------|---------|-------|\n");
        for i in &self.items {
            let claim = match (i.claim_low_usd, i.claim_high_usd) {
                (Some(lo), Some(hi)) => format!("${}–${}", fmt_usd(lo), fmt_usd(hi)),
                _ => "—".to_string(),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | ${} | ${} | ${} | {} | {} | {} |\n",
                i.strategy_id,
                i.strategy,
                i.status,
                fmt_count(i.opportunity_count),
                fmt_usd_2(i.avg_profit_usd),
                fmt_usd(i.total_profit_usd),
                fmt_usd(i.est_monthly_usd),
                claim,
                if i.verdict.is_empty() { "—" } else { &i.verdict },
                i.note,
            ));
        }

        out.push_str("\n---\n\n## Covered by proxy (no dedicated query)\n\n");
        for (s, basis) in COVERED_BY_PROXY {
            out.push_str(&format!("- **{s}** — via {basis}\n"));
        }

        out.push_str("\n## Measurable but deferred (no query template yet)\n\n");
        for s in DEFERRED_MEASURABLE {
            out.push_str(&format!("- {s}\n"));
        }

        out.push_str("\n## Not measurable on Dune\n\n");
        out.push_str("| Strategy | Reason |\n|---|---|\n");
        for (s, reason) in NOT_MEASURABLE {
            out.push_str(&format!("| {s} | {reason} |\n"));
        }
        out
    }

    /// Render the report as a self-contained HTML dashboard (no external deps).
    pub fn render_html(&self) -> String {
        let m = &self.meta;
        let items_json = serde_json::to_string(&self.items).unwrap_or_else(|_| "[]".to_string());
        let max_val = self
            .items
            .iter()
            .map(|i| i.est_monthly_usd)
            .fold(1.0_f64, f64::max);

        let mut rows = String::new();
        let mut sorted: Vec<&StrategyReportItem> = self.items.iter().collect();
        sorted.sort_by(|a, b| b.est_monthly_usd.partial_cmp(&a.est_monthly_usd).unwrap_or(std::cmp::Ordering::Equal));
        for i in sorted {
            let pct = (i.est_monthly_usd / max_val * 100.0) as u32;
            let claim = match (i.claim_low_usd, i.claim_high_usd) {
                (Some(lo), Some(hi)) => format!("${}–${}", fmt_usd(lo), fmt_usd(hi)),
                _ => "—".to_string(),
            };
            let badge_class = match i.status.as_str() {
                "ok" => "badge-ok",
                "no-data" => "badge-muted",
                "table-missing" => "badge-warn",
                _ => "badge-err",
            };
            let verdict_class = match i.verdict.as_str() {
                "above claim" => "verdict-up",
                "below claim" => "verdict-down",
                "in claim range" => "verdict-in",
                _ => "",
            };
            rows.push_str(&format!(
                "<tr>\
                 <td class='mono'>{}</td><td>{}</td>\
                 <td><span class='badge {}'>{}</span></td>\
                 <td class='num'>{}</td><td class='num'>{}</td><td class='num'>{}</td>\
                 <td><div class='bar-wrap'><div class='bar' style='width:{}%'></div><span class='bar-val'>{}</span></div></td>\
                 <td class='num muted'>{}</td>\
                 <td><span class='verdict {}'>{}</span></td>\
                 <td class='basis'>{}</td></tr>\n",
                i.strategy_id,
                i.strategy,
                badge_class,
                i.status,
                fmt_count(i.opportunity_count),
                fmt_usd(i.avg_profit_usd),
                fmt_usd(i.total_profit_usd),
                pct.max(1),
                fmt_usd(i.est_monthly_usd),
                claim,
                verdict_class,
                i.verdict,
                i.note,
            ));
        }

        format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>MEV Strategy Revenue — {chain}</title>
<style>
  :root {{ color-scheme: dark; }}
  body {{ font-family: ui-monospace, "Cascadia Mono", Consolas, monospace; background: #0d1117; color: #e6edf3; margin: 0; padding: 24px; }}
  h1 {{ font-size: 20px; margin: 0 0 4px; }}
  .sub {{ color: #8b949e; font-size: 12px; margin-bottom: 20px; }}
  .cards {{ display: flex; gap: 12px; flex-wrap: wrap; margin-bottom: 20px; }}
  .card {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 12px 16px; min-width: 160px; }}
  .card .label {{ color: #8b949e; font-size: 11px; text-transform: uppercase; letter-spacing: .05em; }}
  .card .value {{ font-size: 20px; font-weight: 600; margin-top: 4px; }}
  table {{ border-collapse: collapse; width: 100%; font-size: 12px; background: #161b22; border-radius: 8px; overflow: hidden; }}
  th {{ text-align: left; color: #8b949e; font-size: 11px; text-transform: uppercase; letter-spacing: .05em; padding: 8px 10px; border-bottom: 1px solid #30363d; }}
  td {{ padding: 7px 10px; border-bottom: 1px solid #21262d; vertical-align: middle; }}
  tr:last-child td {{ border-bottom: none; }}
  .num {{ text-align: right; }}
  .mono {{ color: #8b949e; }}
  .muted {{ color: #8b949e; }}
  .basis {{ color: #8b949e; max-width: 340px; }}
  .badge {{ display: inline-block; padding: 2px 8px; border-radius: 10px; font-size: 10px; }}
  .badge-ok {{ background: #1f6f3f33; color: #3fb950; border: 1px solid #1f6f3f66; }}
  .badge-muted {{ background: #30363d33; color: #8b949e; border: 1px solid #30363d; }}
  .badge-warn {{ background: #9e6a031f; color: #d29922; border: 1px solid #9e6a0366; }}
  .badge-err {{ background: #f851491f; color: #f85149; border: 1px solid #f8514966; }}
  .verdict {{ font-size: 11px; }}
  .verdict-up {{ color: #3fb950; }}
  .verdict-down {{ color: #f85149; }}
  .verdict-in {{ color: #d29922; }}
  .bar-wrap {{ position: relative; background: #21262d; border-radius: 4px; height: 16px; min-width: 120px; }}
  .bar {{ background: linear-gradient(90deg, #1f6f3f, #2ea043); border-radius: 4px; height: 100%; }}
  .bar-val {{ position: absolute; right: 6px; top: 1px; font-size: 10px; color: #e6edf3; }}
  .note {{ color: #8b949e; font-size: 11px; margin-top: 16px; line-height: 1.6; }}
  @media (max-width: 900px) {{ table {{ font-size: 11px; }} }}
</style>
</head>
<body>
<h1>MEV Strategy Revenue — {chain}</h1>
<div class="sub">Generated {generated} &middot; Blocks {from_block}–{to_block} &middot; {from_time} &rarr; {to_time}</div>
<div class="cards">
  <div class="card"><div class="label">Total est. monthly</div><div class="value">${total}</div></div>
  <div class="card"><div class="label">Strategies measured</div><div class="value">{measured}/{queries}</div></div>
  <div class="card"><div class="label">Opportunities</div><div class="value" id="opps">–</div></div>
  <div class="card"><div class="label">Period</div><div class="value" id="period">–</div></div>
</div>
<table>
<thead>
<tr><th>#</th><th>Strategy</th><th>Status</th><th class="num">Opps</th><th class="num">Avg $</th><th class="num">Total $</th><th>Est. monthly</th><th class="num">Claim /mo</th><th>Verdict</th><th>Basis</th></tr>
</thead>
<tbody>{rows}</tbody>
</table>
<div class="note">
Methodology: Dune reports the total addressable market (value extracted by all bots plus victims' losses),
not revenue from a specific bot. Each row states its profit basis; estimates carry large error bars.
Raw data embedded below for scripting.
</div>
<script>
const ITEMS = {items_json};
const opps = ITEMS.reduce((a, i) => a + (Number(i.opportunity_count) || 0), 0);
const days = new Set(ITEMS.filter(i => i.period_start).map(i => i.period_start.slice(0, 10)));
document.getElementById('opps').textContent = opps.toLocaleString();
document.getElementById('period').textContent = days.size ? [...days].sort().slice(0, 1) + ' → ' + [...days].sort().slice(-1) : '–';
</script>
</body>
</html>"#,
            chain = m.chain,
            generated = m.generated_at,
            from_block = m.from_block,
            to_block = m.to_block,
            from_time = m.from_time,
            to_time = m.to_time,
            total = fmt_usd(m.total_est_monthly_usd),
            measured = m.measured_count,
            queries = m.query_count,
            rows = rows,
            items_json = items_json,
        )
    }

    /// Render the report as JSON.
    pub fn render_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_item(strategy_id: u32, strategy: &str, total: f64, days: u64) -> StrategyReportItem {
        StrategyReportItem {
            strategy_id,
            strategy: strategy.to_string(),
            query: "TEST".to_string(),
            source: "test".to_string(),
            status: "ok".to_string(),
            opportunity_count: 100,
            avg_profit_usd: total / 100.0,
            total_profit_usd: total,
            period_start: "2026-07-01 00:00:00".to_string(),
            period_end: "2026-07-30 00:00:00".to_string(),
            period_days: days,
            est_monthly_usd: if days >= 1 { total / days as f64 * 30.0 } else { total },
            claim_low_usd: Some(500.0),
            claim_high_usd: Some(5000.0),
            verdict: "in claim range".to_string(),
            note: "test basis".to_string(),
        }
    }

    fn sample_report() -> StrategyReport {
        StrategyReport {
            meta: ReportMeta {
                chain: "ethereum".to_string(),
                chain_label: "ethereum".to_string(),
                from_block: 100,
                to_block: 200,
                from_time: "2026-07-01 00:00:00".to_string(),
                to_time: "2026-07-30 00:00:00".to_string(),
                generated_at: "2026-08-02 00:00:00 UTC".to_string(),
                query_count: 2,
                measured_count: 2,
                total_est_monthly_usd: 3000.0,
            },
            items: vec![
                sample_item(4, "Backrunning", 1000.0, 30),
                sample_item(10, "Long-tail arb", 2000.0, 30),
            ],
        }
    }

    #[test]
    fn markdown_renders_table() {
        let md = sample_report().render_markdown();
        assert!(md.contains("Backrunning"));
        assert!(md.contains("| # | Strategy"));
        assert!(md.contains("Not measurable on Dune"));
    }

    #[test]
    fn html_renders_dashboard() {
        let html = sample_report().render_html();
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("MEV Strategy Revenue — ethereum"));
        assert!(html.contains("Long-tail arb"));
    }

    #[test]
    fn json_renders() {
        let json = sample_report().render_json();
        assert!(json.contains("\"strategy_id\": 4"));
    }
}
