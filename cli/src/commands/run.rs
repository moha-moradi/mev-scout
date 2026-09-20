use anyhow::Context;

use crate::cli::RunArgs;
use crate::display::{print_startup_plan, render_block_summary_table, render_results_table};
use crate::job_progress::JobProgress;
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{job_run, RunOpts};

pub async fn cmd_run(
    config: &Config,
    args: &RunArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let _ = args;
    let validation_result =
        validation::validate_and_resolve(config).context("invalid configuration")?;
    print_startup_plan(&validation_result, config);

    let opts = RunOpts {
        batch_rpc: config.backtest.batch_rpc,
        record_rejections: config.backtest.record_rejections,
    };
    let outcome = job_run(config, &opts, progress).await?;

    if !outcome.opportunities.is_empty() {
        progress.log(&format!(
            "\nDetected {} MEV opportunity(ies) in {:.2}s:\n",
            outcome.opportunities.len(),
            outcome.elapsed.as_secs_f64()
        ));
        render_results_table(&outcome.opportunities, None);
    }

    render_block_summary_table(&outcome.block_stats);

    let mempool_opps: usize = outcome
        .block_stats
        .iter()
        .map(|s| s.mempool_opp_count)
        .sum();
    if mempool_opps > 0 {
        let mempool_txs: usize = outcome.block_stats.iter().map(|s| s.pending_tx_count).sum();
        progress.log(&format!(
            "  Mempool: {} pending txs, {} mempool-only opportunities visible",
            mempool_txs, mempool_opps,
        ));
    }

    let _ = outcome.run_id;
    Ok(())
}
