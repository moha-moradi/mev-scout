use anyhow::Context;

use crate::cli::FetchArgs;
use crate::job_progress::JobProgress;
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{job_fetch, FetchOpts};

pub async fn cmd_fetch(
    config: &Config,
    args: &FetchArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let _ = config;
    let opts = FetchOpts {
        batch_rpc: args.batch_rpc,
        no_sig_resolve: args.no_sig_resolve,
    };
    // Range flags are already merged into `config` by main via CliOverrides.
    let outcome = job_fetch(config, &opts, progress)
        .await
        .context("fetch job failed")?;
    let _ = outcome;
    Ok(())
}
