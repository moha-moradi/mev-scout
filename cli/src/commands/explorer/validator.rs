//! ``explorer validate`` - cross-check indexed ops against scanner opportunities.

use super::*;
use crate::job_progress::NoopProgress;
use mev_scout_core::jobs::{job_explorer_validate, ExplorerValidateOpts};

pub async fn cmd_validate(config: &Config, args: &ValidateArgs) -> anyhow::Result<()> {
    let opts = ExplorerValidateOpts {
        since: args.since.clone(),
        match_window: args.match_window,
        run_ids: args.run.clone(),
        threshold_sweep: args.threshold_sweep,
        emit_missing_pools: args.emit_missing_pools,
        review_csv: args.review_csv.clone(),
        json: args.json,
    };
    let _ = job_explorer_validate(config, &opts, &NoopProgress).await?;
    Ok(())
}
