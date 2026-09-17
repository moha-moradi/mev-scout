//! validate-pools — quantified discovery accuracy vs off-chain references.

use crate::cli::ValidatePoolsArgs;
use crate::job_progress::NoopProgress;
use mev_scout_core::config::Config;
use mev_scout_core::jobs::{job_validate_pools, ValidatePoolsOpts};

pub async fn cmd_validate_pools(config: &Config, args: &ValidatePoolsArgs) -> anyhow::Result<()> {
    let source = match args.source {
        crate::cli::ValidationSource::All => "all",
        crate::cli::ValidationSource::Gecko => "gecko",
    };
    let opts = ValidatePoolsOpts {
        days: args.days,
        source: source.to_string(),
        json: args.json,
        markdown_out: args.markdown_out.clone(),
    };
    let _ = job_validate_pools(config, &opts, &NoopProgress).await?;
    Ok(())
}
