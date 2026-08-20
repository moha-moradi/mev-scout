mod config;
mod discover;
mod fetch;
mod live;
mod replay;
mod report;
mod run;
mod scan;
mod stream;
mod tokens;

pub use config::cmd_config;
pub use discover::cmd_discover;
pub use fetch::cmd_fetch;
pub use live::cmd_live;
pub use replay::cmd_replay;
pub use report::cmd_report;
pub use run::cmd_run;
pub use scan::cmd_scan;
pub use stream::cmd_stream;
pub use tokens::cmd_tokens;

use async_trait::async_trait;
use mev_scout_core::config::Config;
use crate::cli::{DiscoverArgs, FetchArgs, LiveArgs, ReplayArgs, ReportArgs, RunArgs, ScanArgs, StreamArgs, TokensArgs};

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
impl CliCommand for StreamArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_stream(config, self).await }
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
        Stream(a) => a.execute(config).await,
    }
}
