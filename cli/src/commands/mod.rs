use crate::cli::{DiscoverArgs, ExplorerArgs, LiveArgs, ReportArgs, RunArgs, TokensArgs};
use crate::job_progress::JobProgress;
use async_trait::async_trait;
use mev_scout_core::config::Config;

mod config;
mod discover;
mod explorer;
mod live;
mod report;
mod run;
mod tokens;

pub use config::cmd_config;
pub use discover::cmd_discover;
pub use explorer::{cmd_index, cmd_show, cmd_stats};
pub use live::cmd_live;
pub use report::cmd_report;
pub use run::cmd_run;
pub use tokens::cmd_tokens;

/// Shared interface for all CLI commands.
/// Uses `?Send` because some commands (e.g. discover) hold non-Send types
/// like `Option<&dyn Fn() -> bool>` across await points; embedding hosts run
/// the returned future via their own `Runtime::block_on`.
#[async_trait(?Send)]
pub trait CliCommand {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()>;
}

#[async_trait(?Send)]
impl CliCommand for RunArgs {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()> {
        cmd_run(config, self, progress).await
    }
}

#[async_trait(?Send)]
impl CliCommand for ReportArgs {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()> {
        let _ = progress;
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
impl CliCommand for TokensArgs {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()> {
        let _ = progress;
        cmd_tokens(config, self).await
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
        match &self.command {
            ExplorerCommand::Index(a) => {
                cmd_index(config, a.duration.as_deref(), progress).await
            }
            ExplorerCommand::Stats(a) => {
                cmd_stats(config, a.since.as_deref(), a.kind.as_deref()).await
            }
            ExplorerCommand::Show(a) => cmd_show(config, &a.tx_hash, a.trace).await,
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
        Run(a) => a.execute(config, progress).await,
        Report(a) => a.execute(config, progress).await,
        Config => cmd_config(config).await,
        Discover(a) => a.execute(config, progress).await,
        Tokens(a) => a.execute(config, progress).await,
        Live(a) => a.execute(config, progress).await,
        Explorer(a) => a.execute(config, progress).await,
    }
}
