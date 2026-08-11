
mod cli;
mod commands;
mod display;
mod overrides;
mod rpc_setup;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::Cli;
use mev_scout_core::config::Config;

fn setup_logging(verbose: bool, quiet: bool) {
    let filter = if quiet {
        EnvFilter::new("error")
    } else if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .with_target(false)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    setup_logging(cli.verbose, cli.quiet);

    let mut config = match &cli.config {
        Some(path) => Config::load_or_default(path),
        None => {
            let default_path = "mev-scout.toml";
            if std::path::Path::new(default_path).exists() {
                Config::load_or_default(default_path)
            } else {
                Config::default()
            }
        }
    };

    let overrides = overrides::build_overrides(&cli);
    config.merge_cli(&overrides);

    // `--chain auto` resolves the chain from the configured RPC endpoint's
    // chain ID, silently falling back to the default chain when that fails.
    if config.chain.eq_ignore_ascii_case("auto") {
        rpc_setup::resolve_auto_chain(&mut config).await?;
    }

    commands::execute(&cli.command, &config).await
}