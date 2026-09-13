//! ``explorer stats`` / ``explorer top`` - aggregate views over the explorer store.

use super::*;

// ── stats / top ─────────────────────────────────────────────────────────

pub async fn cmd_stats(
    config: &Config,
    since: Option<&str>,
    window: Option<&str>,
    kind: Option<&str>,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let ts = since_ts(since);

    // Validate the --kind value up front so a typo cannot silently filter out
    // every row; the filter then applies to every stats section.
    let kind_filter = kind.and_then(MevKind::parse).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid --kind '{}' (expected one of arb_atomic, sandwich, liquidation, jit, jit_arb, unknown)",
            kind.unwrap_or("")
        )
    })?;
    let kind_str = Some(kind_filter.as_str());

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

    if matches!(window, Some("week") | Some("month") | Some("year")) {
        println!(
            "\n(window {:?} — daily breakdown shown; period rollups use the same store views)",
            window
        );
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

pub async fn cmd_top(
    config: &Config,
    by: &str,
    metric: &str,
    since: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;
    let ts = since_ts(since);
    let rows = match by {
        "sender" => store.top_senders(ts, limit)?,
        "token" => store.top_tokens(ts, limit)?,
        "pool" => store.top_pools(ts, limit)?,
        other => anyhow::bail!("unknown --by '{other}' (sender|token|pool)"),
    };
    let metric_col = if metric == "ops" { "ops" } else { "gross USD" };
    println!(
        "Explorer top — {chain} by {by} ({metric_col}) since={}",
        since.unwrap_or("all")
    );
    print_stats_table(&rows);
    Ok(())
}
