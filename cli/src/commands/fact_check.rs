use comfy_table::Table;

use crate::cli::FactCheckArgs;
use mev_scout_core::config::Config;
use mev_scout_core::mev::verify::{FactCheckReport, verify_opportunities};
use mev_scout_core::types::ResultsFile;

pub async fn cmd_factcheck(config: &Config, args: &FactCheckArgs) -> anyhow::Result<()> {
    let export_path = &config.export_path;
    let dir = std::path::Path::new(export_path);
    let run_id = &args.run_id;

    let path = dir.join(format!("{}.json", run_id));
    if !path.exists() {
        anyhow::bail!("Error: results file not found: {}", path.display());
    }

    let json_str = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path.display(), e))?;
    let results_file: ResultsFile = serde_json::from_str(&json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse '{}': {}", path.display(), e))?;

    println!();
    println!("  Fact-Check Report for {}", run_id);
    println!("  Chain:         {}", results_file.chain);
    println!("  Block range:   {}–{}", results_file.start_block, results_file.end_block);
    println!("  Opportunities: {}", results_file.opportunities.len());
    println!();

    let checks = verify_opportunities(&results_file.opportunities, None);
    let passed = checks.iter().filter(|c| c.profit_gt_gas).count();
    let failed = checks.len().saturating_sub(passed);

    let mut check_table = Table::new();
    check_table.set_header(vec!["Block", "Tx", "Strategy", "Profit > Gas", "Victim Tx", "Backrun Tx"]);
    for c in &checks {
        let profit_check = if c.profit_gt_gas { "✓" } else { "✗" };
        let victim_str = c.victim_tx_index.map(|i| i.to_string()).unwrap_or_default();
        let backrun_str = c.backrun_tx_index.map(|i| i.to_string()).unwrap_or_default();
        check_table.add_row(vec![
            format!("{}", c.block_number),
            format!("{}", c.tx_index),
            c.strategy.clone(),
            profit_check.to_string(),
            victim_str,
            backrun_str,
        ]);
    }
    println!("{}", check_table);
    println!();
    println!("  Summary: {} total, {} passed, {} failed", checks.len(), passed, failed);

    let report = FactCheckReport {
        run_id: run_id.clone(),
        chain: results_file.chain.clone(),
        block_count: (results_file.end_block.saturating_sub(results_file.start_block) + 1) as usize,
        total_opportunities: results_file.opportunities.len(),
        passed,
        failed,
        block_summaries: Vec::new(),
        opportunity_checks: checks,
    };
    let report_path = dir.join(format!("{}_factcheck.json", run_id));
    if let Ok(json) = serde_json::to_string_pretty(&report) {
        let _ = std::fs::write(&report_path, json);
        println!("  Report saved to {}", report_path.display());
    }

    Ok(())
}
