//! ``explorer export`` - dump indexed opportunities to CSV/JSON.

use super::*;
use crate::job_progress::NoopProgress;
use mev_scout_core::jobs::{job_export, ExportOpts};

pub async fn cmd_export(
    config: &Config,
    since: Option<&str>,
    kinds: Option<&str>,
    format: &str,
    out: Option<&str>,
) -> anyhow::Result<()> {
    let opts = ExportOpts {
        format: format.to_string(),
        since: since.map(str::to_string),
        kinds: kinds.map(str::to_string),
        out: out.map(str::to_string),
    };
    let _ = job_export(config, &opts, &NoopProgress).await?;
    Ok(())
}
