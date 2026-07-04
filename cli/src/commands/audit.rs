use comfy_table::Table;
use mev_scout_core::config::Config;
use mev_scout_core::dune::audit::run_audit;
use mev_scout_core::dune::DuneClient;
use mev_scout_core::types::ResultsFile;

use crate::cli::AuditArgs;

pub async fn cmd_audit(config: &Config, args: &AuditArgs) -> anyhow::Result<()> {
    let dune_api_key = match &config.dune_api_key {
        Some(k) => k,
        None => anyhow::bail!("Dune API key not configured. Set dune_api_key in config."),
    };

    let client = DuneClient::new(dune_api_key.clone());
    let chain = args.chain_args.chain.clone();
    let from_block = args.from_block;
    let to_block = args.to_block;

    // Load MEV Scout opportunities if a previous run is specified
    let scout_opportunities = if let Some(run_id) = &args.run_id {
        let path = std::path::Path::new(&config.export_path).join(format!("{run_id}.json"));
        if !path.exists() {
            anyhow::bail!("Results file not found: {}", path.display());
        }
        let json_str = std::fs::read_to_string(&path)?;
        let results: ResultsFile = serde_json::from_str(&json_str)?;
        tracing::info!("Loaded {} opportunities from run '{run_id}'", results.opportunities.len());
        results.opportunities
    } else if let Some(path) = &args.results_file {
        let json_str = std::fs::read_to_string(path)?;
        let results: ResultsFile = serde_json::from_str(&json_str)?;
        tracing::info!("Loaded {} opportunities from {}", results.opportunities.len(), path);
        results.opportunities
    } else {
        Vec::new()
    };

    println!();
    println!("  Dune Audit Report");
    println!("  Chain:       {chain}");
    println!("  Block range: {from_block}–{to_block}");
    println!("  Scout opps:  {}", scout_opportunities.len());
    println!();

    let report = run_audit(&client, &scout_opportunities, &chain, from_block, to_block).await?;

    // ── Summary ──
    println!("  ┌──────────────────────────────────────────┐");
    println!("  │              AUDIT SUMMARY               │");
    println!("  ├─────────────────────────────┬────────────┤");
    println!("  │ MEV Scout opportunities     │ {:>10} │", report.scout_total);
    println!("  │ Dune sandwiches             │ {:>10} │", report.dune_sandwiches);
    println!("  │ Dune arbitrages             │ {:>10} │", report.dune_arbitrages);
    println!("  │ Dune flash loans            │ {:>10} │", report.dune_flash_loans);
    println!("  ├─────────────────────────────┼────────────┤");
    println!("  │ Confirmed by both           │ {:>10} │", report.confirmed_by_both);
    println!("  │ Only in MEV Scout           │ {:>10} │", report.only_in_scout);
    println!("  │ Only in Dune                │ {:>10} │", report.only_in_dune);
    println!("  └─────────────────────────────┴────────────┘");
    println!();

    // ── Comparison table ──
    if !report.comparisons.is_empty() {
        println!("  Per-Opportunity Comparison:");
        let mut table = Table::new();
        table.set_header(vec!["Block", "Strategy", "Pools", "Confirmed"]);
        for c in &report.comparisons {
            let pools: Vec<String> = c.pool_addresses.iter().map(|a| {
                let s = a.to_string();
                format!("{}..{}", &s[..6], &s[s.len()-4..])
            }).collect();
            table.add_row(vec![
                c.block_number.to_string(),
                c.strategy.clone(),
                pools.join(",\n"),
                if c.confirmed_by_dune { "✓ Dune" } else { "—" }.to_string(),
            ]);
        }
        println!("{table}");
        println!();
    }

    // ── Dune-only events ──
    if !report.unmatched_dune_events.is_empty() {
        println!("  Events found by Dune but NOT by MEV Scout:");
        let mut ue_table = Table::new();
        ue_table.set_header(vec!["Block", "Type", "Pool"]);
        for ue in &report.unmatched_dune_events {
            let pool = ue.pool_address.map(|a| {
                let s = a.to_string();
                format!("{}..{}", &s[..6], &s[s.len()-4..])
            }).unwrap_or_default();
            ue_table.add_row(vec![
                ue.block_number.to_string(),
                ue.event_type.clone(),
                pool,
            ]);
        }
        println!("{ue_table}");
        println!();
        println!("  Tip: These blocks may contain MEV events MEV Scout missed.");
        println!("  Re-run with these blocks to investigate: {}",
            report.unmatched_dune_events.iter()
                .map(|e| e.block_number.to_string())
                .collect::<Vec<_>>()
                .join(", "));
    }

    println!("  Done.");
    Ok(())
}
