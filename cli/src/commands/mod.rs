use crate::cli::{DiscoverArgs, ExplorerArgs, LiveArgs, ReportArgs};
use crate::job_progress::JobProgress;
use async_trait::async_trait;
use mev_scout_core::config::Config;

mod config;
mod discover;
mod explorer;
mod live;
mod report;

pub use config::cmd_config;
pub use discover::cmd_discover;
pub use explorer::{cmd_backfill, cmd_explorer_report, run_dashboard};
pub use live::cmd_live;
pub use report::cmd_report;

/// Shared interface for all CLI commands.
/// Uses `?Send` because some commands (e.g. discover) hold non-Send types
/// like `Option<&dyn Fn -> bool>` across await points; embedding hosts run
/// the returned future via their own `Runtime::block_on`.
#[allow(clippy::double_must_use)] // async_trait Future is already must_use
#[async_trait(?Send)]
pub trait CliCommand {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()>;
}

#[async_trait(?Send)]
impl CliCommand for ReportArgs {
    async fn execute(&self, config: &Config, _progress: &dyn JobProgress) -> anyhow::Result<()> {
        cmd_report(config, self).await
    }
}

#[async_trait(?Send)]
impl CliCommand for DiscoverArgs {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()> {
        cmd_discover(config, self, progress).await
    }
}

#[async_trait(?Send)]
impl CliCommand for LiveArgs {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()> {
        cmd_live(config, self, progress).await
    }
}

#[async_trait(?Send)]
impl CliCommand for ExplorerArgs {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()> {
        use crate::cli::ExplorerCommand;

        if self.command.is_some() && self.bare_flags_set() {
            anyhow::bail!("--kind / --confidence / dashboard options belong on bare `explorer`");
        }

        match &self.command {
            None => {
                let _ = progress;
                if self.dashboard.dashboard {
                    run_dashboard(config, &self.dashboard).await
                } else {
                    cmd_explorer_report(config, self.kind.as_deref(), self.resolved_confidence())
                        .await
                }
            }
            Some(ExplorerCommand::Index(a)) => {
                cmd_backfill(
                    config,
                    a.window.as_deref(),
                    a.from_block,
                    a.to_block,
                    progress,
                )
                .await?;
                if a.dashboard {
                    run_dashboard(config, &crate::cli::DashboardArgs::default()).await?;
                }
                Ok(())
            }
        }
    }
}

/// Dispatch a clap `Command` to its trait implementation.
pub async fn execute(
    cmd: &crate::cli::Command,
    config: &Config,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    use crate::cli::Command::*;
    match cmd {
        Report(a) => a.execute(config, progress).await,
        Config => cmd_config(config).await,
        Discover(a) => a.execute(config, progress).await,
        Live(a) => a.execute(config, progress).await,
        Explorer(a) => a.execute(config, progress).await,
    }
}
