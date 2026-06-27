
mod cli;
mod commands;
mod display;
mod overrides;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};
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
        Some(path) => Config::load(path).unwrap_or_else(|e| {
            eprintln!("Error: failed to load config '{path}': {e}");
            std::process::exit(1);
        }),
        None => Config::default(),
    };

    let overrides = overrides::build_overrides(&cli);
    config.merge_cli(&overrides);

    match &cli.command {
        Command::Run(args) => commands::cmd_run(&config, args).await,
        Command::Fetch(args) => commands::cmd_fetch(&config, args).await,
        Command::Report(args) => commands::cmd_report(&config, args).await,
        Command::Config => commands::cmd_config(&config).await,
        Command::Replay(args) => commands::cmd_replay(&config, args).await,
        Command::Discover(args) => commands::cmd_discover(&config, args).await,
        Command::FactCheck(args) => commands::cmd_factcheck(&config, args).await,
        Command::Live(args) => commands::cmd_live(&config, args).await,
    }
}