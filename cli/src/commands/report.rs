use anyhow::Context;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::{validation, Config};
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::types::{OutputFormat, ResultsFile};

use crate::cli::ReportArgs;
use crate::display::render_results_table;

/// Re-render terminal tables for a recorded run. Execution history is read
/// from SQLite only: run metadata from the cache store's `run_manifests`,
/// opportunities from the explorer store's `opportunities` table.
pub async fn cmd_report(config: &Config, args: &ReportArgs) -> anyhow::Result<()> {
    let (chain_name, _) = validation::resolve_chain(config).context("invalid configuration")?;

    let cache_db = config.effective_db_path(&chain_name);
    let cache = SqliteStore::open(&cache_db)
        .with_context(|| format!("failed to open run-history db '{cache_db}'"))?;

    let run_id = match &args.run_id {
        Some(id) => id.clone(),
        None => {
            let latest = cache.latest_manifest()?
                .context("no runs recorded in the run-history db — execute `mev-scout run` first")?;
            latest.run_id
        }
    };

    let manifest = cache
        .get_manifest(&run_id)?
        .with_context(|| format!("run '{run_id}' not found in '{cache_db}'"))?;

    let explorer_db = config.effective_explorer_db_path(&chain_name);
    let store = ExplorerStore::open(&explorer_db)
        .with_context(|| format!("failed to open explorer db '{explorer_db}'"))?;
    let opportunities = store.opportunities_by_run(&run_id)?;

    let output_format: OutputFormat = config.output.output.parse().unwrap_or(OutputFormat::Table);

    let results_file = ResultsFile {
        run_id: manifest.run_id.clone(),
        chain: manifest.chain.clone(),
        start_block: manifest.start_block,
        end_block: manifest.end_block,
        range_mode: manifest.range_mode.clone(),
        strategies: manifest.strategies.clone(),
        flash_loan_provider: manifest.flash_loan_provider.clone(),
        resolved_at: manifest.resolved_at,
        created_at: manifest.resolved_at,
        opportunities,
    };

    // Weekly-report explorer section (plan §11.1/Phase 5): when the explorer
    // store has data overlapping this run, append recall/miss metrics.
    let explorer_section = explorer_validation_section(
        config,
        &results_file.chain,
        results_file.start_block,
        results_file.end_block,
        &results_file.run_id,
    );

    match output_format {
        OutputFormat::Table => {
            println!();
            println!("  Run ID:        {}", results_file.run_id);
            println!("  Chain:         {}", results_file.chain);
            println!("  Block range:   {}–{}", results_file.start_block, results_file.end_block);
            println!("  Mode:          {}", results_file.range_mode);
            println!("  Strategies:    {}", results_file.strategies.join(", "));
            println!("  Flash loan:    {}", results_file.flash_loan_provider);
            println!("  Opportunities: {}", results_file.opportunities.len());
            println!();

            if results_file.opportunities.is_empty() {
                println!("No MEV opportunities in this run.");
            } else {
                render_results_table(&results_file.opportunities, None);
            }
        }
        OutputFormat::Csv => {
            println!("block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei,confidence");
            for opp in &results_file.opportunities {
                println!(
                    "{},{},{},{},{},{},{}",
                    opp.block_number,
                    opp.tx_index,
                    opp.strategy,
                    opp.input_amount,
                    opp.expected_profit,
                    opp.gas_cost_wei,
                    opp.confidence.map_or("".to_string(), |c| format!("{:.2}", c)),
                );
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&results_file)?);
        }
    }

    if let Some(section) = explorer_section {
        println!("\n{section}");
    }

    Ok(())
}

/// Explorer cross-validation section for the weekly report: computes the
/// §11 report over the run's block window when the store has realized data.
fn explorer_validation_section(
    config: &Config,
    chain_str: &str,
    start_block: u64,
    end_block: u64,
    run_id: &str,
) -> Option<String> {
    use mev_scout_core::explorer::validate;
    use mev_scout_core::types::ChainName;

    let chain: ChainName = chain_str.parse().ok()?;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain)).ok()?;
    let has_ops = store
        .ops_in_range(start_block, end_block, &[])
        .ok()?
        .is_empty();
    if has_ops {
        return None; // no realized data in-window yet
    }
    let report = validate::compute_validation(
        &store,
        chain,
        start_block,
        end_block,
        0,
        Some(&[run_id.to_string()]),
        false,
    )
    .ok()?;
    Some(validate::render_validation_report(&report))
}
