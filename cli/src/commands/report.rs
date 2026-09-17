use mev_scout_core::config::Config;
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::jobs::{job_report, ReportOpts};
use mev_scout_core::progress::NoopProgress;
use mev_scout_core::types::{OutputFormat, ResultsFile};

use crate::cli::ReportArgs;
use crate::display::render_results_table;

/// Re-render terminal tables for a recorded run. Execution history is read
/// from SQLite only: run metadata from the cache store's `run_manifests`,
/// opportunities from the explorer store's `opportunities` table.
pub async fn cmd_report(config: &Config, args: &ReportArgs) -> anyhow::Result<()> {
    let opts = ReportOpts {
        run_id: args.run_id.clone(),
    };
    // Core's discarding sink: the CLI renders the payload itself, and JSON/CSV
    // must stay machine-parseable (job_report also logs human-readable lines).
    let outcome = job_report(config, &opts, &NoopProgress).await?;
    let manifest = outcome.manifest;
    let output_format = config.output.output;

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
        opportunities: outcome.opportunities,
    };

    match output_format {
        OutputFormat::Table => {
            println!();
            println!("  Run ID:        {}", results_file.run_id);
            println!("  Chain:         {}", results_file.chain);
            println!(
                "  Block range:   {}–{}",
                results_file.start_block, results_file.end_block
            );
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

            // Explorer recall/miss metrics are human-readable; only append in table mode.
            if let Some(section) = explorer_validation_section(
                config,
                &results_file.chain,
                results_file.start_block,
                results_file.end_block,
                &results_file.run_id,
            ) {
                println!("\n{section}");
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
                    opp.confidence
                        .map_or("".to_string(), |c| format!("{:.2}", c)),
                );
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&results_file)?);
        }
    }

    Ok(())
}

/// Explorer cross-validation section for the weekly report: computes the
/// Explorer cross-validation report over the run's block window when the store has realized data.
/// Failures here are logged and the section is skipped — the report still
/// renders — but a broken store must not vanish silently.
fn explorer_validation_section(
    config: &Config,
    chain_str: &str,
    start_block: u64,
    end_block: u64,
    run_id: &str,
) -> Option<String> {
    use mev_scout_core::explorer::validate;
    use mev_scout_core::types::ChainName;

    let chain: ChainName = match chain_str.parse() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Explorer section skipped: invalid chain '{chain_str}': {e}");
            return None;
        }
    };
    let store = match ExplorerStore::open(config.effective_explorer_db_path(&chain)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Explorer section skipped: cannot open store for {chain}: {e:#}");
            return None;
        }
    };
    let empty = match store.ops_in_range(start_block, end_block, &[]) {
        Ok(ops) => ops.is_empty(),
        Err(e) => {
            tracing::warn!("Explorer section skipped: ops_in_range failed: {e:#}");
            return None;
        }
    };
    if empty {
        return None; // no realized data in-window yet
    }
    let run_ids = [run_id.to_string()];
    match validate::compute_validation(
        &store,
        validate::ValidationQuery {
            chain,
            from_block: start_block,
            to_block: end_block,
            match_window: 0,
            run_filter: Some(&run_ids),
            threshold_sweep: false,
        },
    ) {
        Ok(report) => Some(validate::render_validation_report(&report)),
        Err(e) => {
            tracing::warn!("Explorer section skipped: validation compute failed: {e:#}");
            None
        }
    }
}
