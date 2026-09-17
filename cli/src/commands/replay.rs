use anyhow::Context;

use crate::cli::ReplayArgs;
use crate::job_progress::JobProgress;
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{job_replay, ReplayOpts};

pub async fn cmd_replay(
    config: &Config,
    args: &ReplayArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let opts = ReplayOpts {
        block: args.block,
        tx_index: args.tx_index,
        analyze: args.analyze,
    };
    let outcome = job_replay(config, &opts, progress)
        .await
        .context("replay job failed")?;
    let _ = outcome;
    Ok(())
}
