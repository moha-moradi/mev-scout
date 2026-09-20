//! ``explorer stats`` — aggregate views over the explorer store.

use super::*;

pub async fn cmd_stats(
    config: &Config,
    since: Option<&str>,
    window: Option<&str>,
    kind: Option<&str>,
    arb_shape: Option<&str>,
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
    if let Some(shape) = arb_shape {
        println!("  (arb_shape filter: {shape} — applied to per-op listings below)");
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

    if matches!(window, Some("week") | Some("month") | Some("year")) {
        println!(
            "\n(window {:?} — daily breakdown shown; period rollups use the same store views)",
            window
        );
    }

    if let Some(shape) = arb_shape {
        println!("\nArb shape sample ({shape}):");
        let ops = store.ops_since(ts)?;
        let mut shown = 0usize;
        for o in ops.iter().rev() {
            if o.kind != "arb_atomic" {
                continue;
            }
            let matches = o
                .details_json
                .as_deref()
                .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
                .and_then(|v| {
                    v.pointer("/arb_meta/arb_shape")
                        .and_then(|x| x.as_str())
                        .map(|s| s.eq_ignore_ascii_case(shape))
                })
                .unwrap_or(false);
            if !matches {
                continue;
            }
            println!(
                "  blk {} {} ${:.2}",
                o.block_number,
                short_addr(&o.tx_hash),
                o.net_profit_usd.or(o.profit_usd).unwrap_or(0.0)
            );
            shown += 1;
            if shown >= 20 {
                break;
            }
        }
        if shown == 0 {
            println!("  (no matching arb ops in window)");
        }
    }

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
