//! ``explorer stats`` — aggregate views over the explorer store.

use super::*;

pub async fn cmd_stats(
    config: &Config,
    since: Option<&str>,
    kind: Option<&str>,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let ts = since_ts(since);

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

    let overview = store.stats_overview_filtered(ts, kind_str)?;
    println!("Explorer stats — {chain} since={}", since.unwrap_or("all"));
    if let Some(k) = kind_str {
        println!("  (filtered to kind: {k})");
    }
    println!(
        "  ops: {} | searchers: {} | gross: ${:.2} | gas: ${:.2} | net: ${:.2} | highest single: ${:.2}",
        overview.ops,
        overview.searchers,
        overview.gross_usd,
        overview.gas_usd,
        overview.net_usd,
        overview.highest_single_usd,
    );

    println!("\nPer-kind:");
    let rows = store.stats_by_kind_filtered(ts, kind_str)?;
    print_stats_table(&rows);

    println!("\nDaily breakdown:");
    let rows = store.stats_daily_filtered(ts, kind_str)?;
    print_stats_table(&rows);

    println!("\nTop searchers (7d):");
    let rows = store.top_senders_filtered(since_ts(Some("7d")), 10, kind_str)?;
    print_stats_table(&rows);

    println!("\nTop pools:");
    let rows = store.top_pools_filtered(ts, 10, kind_str)?;
    print_stats_table(&rows);
    Ok(())
}

fn print_stats_table(rows: &[mev_scout_core::explorer::store::StatsRow]) {
    let mut table = Table::new();
    table.set_header(vec!["label", "ops", "gross USD", "net USD"]);
    if rows.is_empty() {
        table.add_row(vec!["(no data)", "-", "-", "-"]);
    }
    for r in rows {
        table.add_row(vec![
            short_addr(&r.label),
            r.ops.to_string(),
            format!("{:.2}", r.gross_usd),
            format!("{:.2}", r.net_usd),
        ]);
    }
    println!("{table}");
}
