mod config;
mod discover;
mod explorer;
mod fetch;
mod live;
mod replay;
mod report;
mod run;
mod scan;
mod tokens;
mod validate_pools;

pub use config::cmd_config;
pub use discover::cmd_discover;
pub use explorer::{
    cmd_doctor, cmd_explain, cmd_export, cmd_index, cmd_live_feed, cmd_show, cmd_stats, cmd_top,
    cmd_validate,
};
pub use fetch::cmd_fetch;
pub use live::cmd_live;
pub use replay::cmd_replay;
pub use report::cmd_report;
pub use run::cmd_run;
pub use scan::cmd_scan;
pub use tokens::cmd_tokens;
pub use validate_pools::cmd_validate_pools;

use async_trait::async_trait;
use mev_scout_core::config::Config;
use crate::cli::{
    DiscoverArgs, ExplorerArgs, FetchArgs, LiveArgs, ReplayArgs, ReportArgs, RunArgs, ScanArgs,
    TokensArgs, ValidatePoolsArgs,
};

/// Shared interface for all CLI commands.
/// Uses `?Send` because some commands (e.g. discover) hold non-Send types
/// like `Option<&dyn Fn()>` across await points.
#[async_trait(?Send)]
pub trait CliCommand {
    async fn execute(&self, config: &Config) -> anyhow::Result<()>;
}

#[async_trait(?Send)]
impl CliCommand for RunArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_run(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for FetchArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_fetch(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for ReportArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_report(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for ReplayArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_replay(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for DiscoverArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_discover(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for TokensArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_tokens(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for ScanArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_scan(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for LiveArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_live(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for ValidatePoolsArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_validate_pools(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for ExplorerArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> {
        use crate::cli::ExplorerCommand;
        match &self.command {
            ExplorerCommand::Doctor => cmd_doctor(config).await,
            ExplorerCommand::Index(a) => {
                cmd_index(config, a.from, a.to, a.days, a.live, a.duration.as_deref()).await
            }
            ExplorerCommand::LiveFeed(a) => {
                cmd_live_feed(
                    config,
                    a.kinds.as_deref(),
                    a.min_profit_usd,
                    a.poll_interval_ms.unwrap_or(config.explorer.poll_interval_ms),
                    a.duration.as_deref(),
                )
                .await
            }
            ExplorerCommand::Stats(a) => {
                cmd_stats(config, a.since.as_deref(), a.window.as_deref(), a.kind.as_deref()).await
            }
            ExplorerCommand::Top(a) => {
                cmd_top(config, &a.by, &a.metric, a.since.as_deref(), a.limit).await
            }
            ExplorerCommand::Show(a) => cmd_show(config, &a.tx_hash, a.trace).await,
            ExplorerCommand::Explain(a) => cmd_explain(config, &a.tx_hash).await,
            ExplorerCommand::Validate(a) => {
                cmd_validate(
                    config,
                    a.since.as_deref(),
                    a.match_window,
                    if a.run.is_empty() { None } else { Some(a.run.clone()) },
                    a.threshold_sweep,
                    a.emit_missing_pools,
                    a.json,
                )
                .await
            }
            ExplorerCommand::Export(a) => {
                cmd_export(config, a.since.as_deref(), a.kinds.as_deref(), &a.format, a.out.as_deref()).await
            }
        }
    }
}

/// Dispatch a clap `Command` to its trait implementation.
pub async fn execute(cmd: &crate::cli::Command, config: &Config) -> anyhow::Result<()> {
    use crate::cli::Command::*;
    match cmd {
        Run(a) => a.execute(config).await,
        Fetch(a) => a.execute(config).await,
        Report(a) => a.execute(config).await,
        Config => cmd_config(config).await,
        Replay(a) => a.execute(config).await,
        Discover(a) => a.execute(config).await,
        Tokens(a) => a.execute(config).await,
        Scan(a) => a.execute(config).await,
        Live(a) => a.execute(config).await,
        ValidatePools(a) => a.execute(config).await,
        Explorer(a) => a.execute(config).await,
    }
}
