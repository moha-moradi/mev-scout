//! ``explorer index`` - live indexing loop.

use super::*;
use crate::job_progress::JobProgress;
use mev_scout_core::jobs::{job_index, IndexOpts};

pub async fn cmd_index(
    config: &Config,
    duration: Option<&str>,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    let opts = IndexOpts {
        duration: duration.map(String::from),
    };
    job_index(config, &opts, progress).await?;
    Ok(())
}
