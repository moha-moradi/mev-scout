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
#[cfg(feature = "validate")]
pub use explorer::cmd_explorer_validate;
pub use explorer::{cmd_backfill, cmd_explorer_report, cmd_index, cmd_show};
pub use live::cmd_live;
pub use report::cmd_report;

/// Shared interface for all CLI commands.
/// Uses `?Send` because some commands (e.g. discover) hold non-Send types
/// like `Option<&dyn Fn() -> bool>` across await points; embedding hosts run
/// the returned future via their own `Runtime::block_on`.
#[async_trait(?Send)]
pub trait CliCommand {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()>;
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
impl CliCommand for LiveArgs {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()> {
        cmd_live(config, self, progress).await
    }
}

#[async_trait(?Send)]
impl CliCommand for ExplorerArgs {
    async fn execute(&self, config: &Config, progress: &dyn JobProgress) -> anyhow::Result<()> {
        use crate::cli::ExplorerCommand;

        if self.command.is_some() && self.report_flags_set() {
            anyhow::bail!(
                "--windows / --kind / --top belong on bare `explorer` (the revenue report)"
            );
        }

        match &self.command {
            None => {
                let _ = progress;
                cmd_explorer_report(
                    config,
                    &self.resolved_windows(),
                    self.kind.as_deref(),
                    self.resolved_top(),
                )
                .await
            }
            Some(ExplorerCommand::Index(a)) => {
                if a.r#loop {
                    if a.days.is_some() || a.from_block.is_some() || a.to_block.is_some() {
                        anyhow::bail!(
                            "--days / --from-block / --to-block cannot be combined with --loop"
                        );
                    }
                    cmd_index(config, a.duration.as_deref(), progress).await
                } else {
                    if a.duration.is_some() {
                        anyhow::bail!("--duration requires --loop");
                    }
                    cmd_backfill(config, a.days, a.from_block, a.to_block).await?;
                    Ok(())
                }
            }
            Some(ExplorerCommand::Show(a)) => cmd_show(config, &a.tx_hash, a.trace).await,
            #[cfg(feature = "validate")]
            Some(ExplorerCommand::Validate(a)) => cmd_explorer_validate(config, a, progress).await,
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
