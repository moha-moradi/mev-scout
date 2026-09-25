//! ``explorer validate`` — cross-validation report (realized ops vs scanner
//! opportunities). Pure SQL over the store; does no RPC.

use super::*;

use crate::job_progress::JobProgress;
use mev_scout_core::jobs::{job_explorer_validate, ExplorerValidateOpts};

pub async fn cmd_explorer_validate(
    config: &Config,
    a: &crate::cli::ExplorerValidateArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let opts = ExplorerValidateOpts {
        since: a.since.clone(),
        match_window: a.match_window,
        run_ids: a.run_ids.clone(),
        threshold_sweep: a.threshold_sweep,
        emit_missing_pools: a.emit_missing_pools,
        review_csv: a.review_csv.clone(),
        golden_causal: a.golden_causal,
        json: a.json,
    };
    job_explorer_validate(config, &opts, progress).await?;
    Ok(())
}
