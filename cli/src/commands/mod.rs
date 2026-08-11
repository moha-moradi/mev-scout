mod audit;
mod config;
mod discover;
mod dune_check;
mod dune_find_blocks;
mod dune_query;
mod dune_report;
mod fetch;
mod replay;
mod report;
mod run;
mod tokens;

pub use audit::cmd_audit;
pub use config::cmd_config;
pub use discover::cmd_discover;
pub use dune_check::cmd_dune_check;
pub use dune_find_blocks::cmd_dune_find_blocks;
pub use dune_query::cmd_dune_query;
pub use dune_report::cmd_dune_report;
pub use fetch::cmd_fetch;
pub use replay::cmd_replay;
pub use report::cmd_report;
pub use run::cmd_run;
pub use tokens::cmd_tokens;

use async_trait::async_trait;
use mev_scout_core::config::Config;
use crate::cli::{AuditArgs, DiscoverArgs, DuneCheckArgs, DuneFindBlocksArgs, DuneQueryArgs, DuneReportArgs, FetchArgs, ReplayArgs, ReportArgs, RunArgs, TokensArgs};

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
impl CliCommand for AuditArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_audit(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for DuneCheckArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_dune_check(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for DuneFindBlocksArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_dune_find_blocks(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for DuneQueryArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_dune_query(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for DuneReportArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_dune_report(config, self).await }
}

#[async_trait(?Send)]
impl CliCommand for TokensArgs {
    async fn execute(&self, config: &Config) -> anyhow::Result<()> { cmd_tokens(config, self).await }
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
        Audit(a) => a.execute(config).await,
        DuneCheck(a) => a.execute(config).await,
        DuneFindBlocks(a) => a.execute(config).await,
        DuneQuery(a) => a.execute(config).await,
        DuneReport(a) => a.execute(config).await,
        Tokens(a) => a.execute(config).await,
    }
}
