//! ``explorer validate`` - cross-check indexed ops against scanner opportunities.

use super::*;
use crate::job_progress::NoopProgress;
use mev_scout_core::jobs::{job_explorer_validate, ExplorerValidateOpts};

pub async fn cmd_validate(config: &Config, args: &ValidateArgs) -> anyhow::Result<()> {
    // Phase 0.5: labeled causal set can run without an indexed store.
    if args.golden_causal
        && args.since.is_none()
        && !args.threshold_sweep
        && !args.emit_missing_pools
        && args.review_csv.is_none()
        && args.run.is_empty()
    {
        let score = mev_scout_core::explorer::score_embedded_causal_set();
        if args.json {
            println!("{}", serde_json::to_string_pretty(&score)?);
        } else {
            print!("{}", score.render());
            let gate = if score.passes(1.0, 1.0) {
                "PASS"
            } else {
                "FAIL"
            };
            println!("Causal golden ship gate: {gate}");
        }
        if !score.passes(1.0, 1.0) {
            anyhow::bail!("causal labeled golden set failed ship gate");
        }
        return Ok(());
    }

    let opts = ExplorerValidateOpts {
        since: args.since.clone(),
        match_window: args.match_window,
        run_ids: args.run.clone(),
        threshold_sweep: args.threshold_sweep,
        emit_missing_pools: args.emit_missing_pools,
        review_csv: args.review_csv.clone(),
        golden_causal: args.golden_causal,
        json: args.json,
    };
    let outcome = job_explorer_validate(config, &opts, &NoopProgress).await?;
    if args.golden_causal {
        if let Some(score) = &outcome.report.causal_golden {
            if !score.passes(1.0, 1.0) {
                anyhow::bail!("causal labeled golden set failed ship gate");
            }
        }
    }
    Ok(())
}
