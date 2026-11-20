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
pub use explorer::cmd_backfill;
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
        use mev_scout_core::explorer::dashboard::WindowSpec;

        match &self.command {
            None => {
                explorer::emit_dashboard(config, self.out.clone(), "all", WindowSpec::all()).await
            }
            Some(ExplorerCommand::Index(a)) => {
                let outcome = cmd_backfill(
                    config,
                    a.window.as_deref(),
                    a.from_block,
                    a.to_block,
                    a.reclassify,
                    a.discover_pools,
                    progress,
                )
                .await?;
                let range_label = explorer::index_range_label(
                    a.window.as_deref(),
                    a.from_block.is_some() && a.to_block.is_some(),
                    outcome.from_block,
                    outcome.to_block,
                );
                let window = explorer::index_window(
                    config,
                    &range_label,
                    outcome.from_block,
                    outcome.to_block,
                );
                explorer::emit_dashboard(config, self.out.clone(), &range_label, window).await
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
