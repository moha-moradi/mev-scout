//! ``explorer doctor`` - provider capability probe.

use super::*;
use crate::job_progress::NoopProgress;
use mev_scout_core::jobs::job_doctor;

pub async fn cmd_doctor(config: &Config) -> anyhow::Result<()> {
    let _ = job_doctor(config, &NoopProgress).await?;
    Ok(())
}
