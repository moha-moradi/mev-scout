//! ``explorer report`` — revenue report: cost, profit, and volume per time
//! window (1d/7d/30d default), broken out per MEV kind, with a daily trend,
//! top searchers/pools, and a top-op detail list. Pure SQL over the store —
//! requires history (see ``explorer backfill``).

use super::*;

use serde::Serialize;

use mev_scout_core::chain::timing::block_time_secs;
use mev_scout_core::explorer::store::{ReportOverview, ReportRow, StatsRow};
use mev_scout_core::types::OutputFormat;

/// One window's worth of report data, ready for table / JSON / CSV rendering.
#[derive(Debug, Clone, Serialize)]
struct WindowReport {
    window: String,
    /// Fraction of expected blocks indexed in the window (0..=1); `None` for
    /// the `all` window where expected is unbounded.
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage_pct: Option<f64>,
    blocks: i64,
    overview: ReportOverview,
    by_kind: Vec<ReportRow>,
    daily: Vec<ReportRow>,
    top_senders: Vec<StatsRow>,
    top_pools: Vec<StatsRow>,
    top_ops: Vec<MevOpRow>,
}

/// Expected blocks in a window using the chain's authoritative block time.
fn expected_blocks(chain: ChainName, window: &str) -> Option<u64> {
    let secs = match window {
        "1d" => 86_400,
        "7d" => 7 * 86_400,
        "30d" => 30 * 86_400,
        _ => return None, // "all" and invalid windows have no expected bound
    };
    Some(secs / block_time_secs(chain) as u64)
}

fn usd(x: f64) -> String {
    format!("{x:.2}")
}

fn row_fields(r: &ReportRow) -> Vec<String> {
    vec![
        r.ops.to_string(),
        usd(r.volume_usd),
        usd(r.gross_usd),
        usd(r.gas_usd),
        usd(r.flash_fee_usd),
        usd(r.net_usd),
    ]
}

/// label + report row fields, joined for a single table row.
fn row_cells(label: &str, fields: &[String]) -> Vec<String> {
    let mut v = vec![label.to_string()];
    v.extend_from_slice(fields);
    v
}

fn print_report_table(rows: &[ReportRow], first_col: &str) {
    let mut table = Table::new();
    table.set_header(vec![
        first_col,
        "ops",
        "volume USD",
        "gross USD",
        "gas USD",
        "flash USD",
        "net USD",
    ]);
    if rows.is_empty() {
        table.add_row(vec!["(no data)", "-", "-", "-", "-", "-", "-"]);
    }
    for r in rows {
        table.add_row(row_cells(&r.label, &row_fields(r)));
    }
    println!("{table}");
}

fn print_stats_table(rows: &[StatsRow]) {
    let mut table = Table::new();
    table.set_header(vec!["label", "ops", "gross USD", "net USD"]);
    if rows.is_empty() {
        table.add_row(vec!["(no data)", "-", "-", "-"]);
    }
    for r in rows {
        table.add_row(vec![
            short_addr(&r.label),
            r.ops.to_string(),
            usd(r.gross_usd),
            usd(r.net_usd),
        ]);
    }
    println!("{table}");
}

fn print_top_ops(rows: &[MevOpRow]) {
    let mut table = Table::new();
    table.set_header(vec![
        "tx",
        "block",
        "kind",
        "volume USD",
        "gross USD",
        "gas USD",
        "net USD",
    ]);
    if rows.is_empty() {
        table.add_row(vec!["(no data)", "-", "-", "-", "-", "-", "-"]);
    }
    for r in rows {
        table.add_row(vec![
            short_addr(&r.tx_hash),
            r.block_number.to_string(),
            r.kind.clone(),
            usd(r.volume_usd.unwrap_or(0.0)),
            usd(r.profit_usd.unwrap_or(0.0)),
            usd(r.gas_cost_usd.unwrap_or(0.0)),
            usd(r.net_profit_usd.unwrap_or(0.0)),
        ]);
    }
    println!("{table}");
}

pub async fn cmd_explorer_report(
    config: &Config,
    windows: &[String],
    kind: Option<&str>,
    top: usize,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;

    let kind_str: Option<&str> = match kind {
        None => None,
        Some(k) => {
            let parsed = MevKind::parse(k).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid --kind '{k}' (expected one of arb_atomic, sandwich, frontrun, backrun, liquidation, jit, jit_arb, unknown)"
                )
            })?;
            Some(parsed.as_str())
        }
    };

    // `--windows` is comma-delimited; also tolerate spaces after commas.
    let mut window_list: Vec<String> = Vec::new();
    for w in windows {
        for part in w.split(',') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            if !matches!(p, "1d" | "7d" | "30d" | "all") {
                anyhow::bail!("invalid --windows '{p}' (expected 1d, 7d, 30d, all)");
            }
            window_list.push(p.to_string());
        }
    }
    window_list.dedup();
    if window_list.is_empty() {
        anyhow::bail!("no --windows selected (expected 1d, 7d, 30d, or all)");
    }
    let top = top.min(100);

    let now = epoch_secs();
    let mut reports: Vec<WindowReport> = Vec::with_capacity(window_list.len());
    for window in &window_list {
        let since = since_ts(Some(window));
        let overview = store.report_window_overview(since, kind_str)?;
        let by_kind = store.report_by_kind(since, kind_str)?;
        let daily = store.report_daily(since, kind_str)?;
        let top_senders = store.top_senders_filtered(since, 10, kind_str)?;
        let top_pools = store.top_pools_filtered(since, 10, kind_str)?;
        let top_ops = store.top_ops(since, top, kind_str)?;
        let blocks = store.blocks_in_window(since)?;
        let coverage_pct = expected_blocks(chain, window).map(|expected| {
            if expected == 0 {
                1.0
            } else {
                (blocks as f64 / expected as f64).min(1.0)
            }
        });
        reports.push(WindowReport {
            window: window.clone(),
            coverage_pct,
            blocks,
            overview,
            by_kind,
            daily,
            top_senders,
            top_pools,
            top_ops,
        });
    }

    println!(
        "Explorer report — {chain} windows: {}",
        window_list.join(", ")
    );
    if let Some(k) = kind_str {
        println!("  (kind filter: {k})");
    }

    match config.output.output {
        OutputFormat::Json => {
            let mut out = serde_json::Map::new();
            out.insert("chain".into(), serde_json::json!(chain.to_string()));
            out.insert("generated_at".into(), serde_json::json!(now));
            let mut wins = serde_json::Map::new();
            for r in &reports {
                wins.insert(r.window.clone(), serde_json::to_value(r)?);
            }
            out.insert("windows".into(), serde_json::Value::Object(wins));
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        OutputFormat::Csv => {
            println!("window,kind,ops,volume_usd,gross_usd,gas_usd,flash_fee_usd,net_usd");
            for r in &reports {
                print_csv_row(&r.window, "overview", &row_overview_fields(&r.overview));
                for k in &r.by_kind {
                    print_csv_row(&r.window, &k.label, &row_fields(k));
                }
            }
        }
        OutputFormat::Table => {
            for r in &reports {
                render_window(r);
            }
        }
    }

    Ok(())
}

fn row_overview_fields(o: &ReportOverview) -> Vec<String> {
    vec![
        o.ops.to_string(),
        usd(o.volume_usd),
        usd(o.gross_usd),
        usd(o.gas_usd),
        usd(o.flash_fee_usd),
        usd(o.net_usd),
    ]
}

fn print_csv_row(window: &str, label: &str, cols: &[String]) {
    let mut parts: Vec<String> = vec![window.to_string(), label.to_string()];
    parts.extend_from_slice(cols);
    println!("{}", parts.join(","));
}

fn render_window(r: &WindowReport) {
    let o = &r.overview;
    println!();
    match r.coverage_pct {
        Some(c) => println!(
            "── {} window ({} blocks indexed, {:.0}% coverage) ──────────",
            r.window,
            r.blocks,
            c * 100.0
        ),
        None => println!(
            "── {} window (all-time, {} blocks indexed) ─────",
            r.window, r.blocks
        ),
    }
    if let Some(c) = r.coverage_pct {
        if c < 0.7 {
            eprintln!(
                "  warning: {} window is under-filled ({:.0}% coverage). Run \
                 `mev-scout explorer backfill --days 30` first.",
                r.window,
                c * 100.0
            );
        }
    }
    println!(
        "  ops: {} | searchers: {} | volume: ${} | gross: ${} | gas: ${} | \
         flash fees: ${} | net: ${} | highest single: ${}",
        o.ops,
        o.searchers,
        usd(o.volume_usd),
        usd(o.gross_usd),
        usd(o.gas_usd),
        usd(o.flash_fee_usd),
        usd(o.net_usd),
        usd(o.highest_single_usd),
    );

    println!("\n  Per kind:");
    let mut t = Table::new();
    t.set_header(vec![
        "kind",
        "ops",
        "volume USD",
        "gross USD",
        "gas USD",
        "flash USD",
        "net USD",
    ]);
    if r.by_kind.is_empty() {
        t.add_row(vec!["(no data)", "-", "-", "-", "-", "-", "-"]);
    }
    for k in &r.by_kind {
        t.add_row(row_cells(&k.label, &row_fields(k)));
    }
    println!("{t}");

    println!("\n  Daily trend:");
    print_report_table(&r.daily, "date");

    println!("\n  Top searchers:");
    print_stats_table(&r.top_senders);

    println!("\n  Top pools:");
    print_stats_table(&r.top_pools);

    println!("\n  Top ops (by net profit):");
    print_top_ops(&r.top_ops);
}
