
mod cli;
mod commands;
mod display;
mod overrides;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};
use mev_scout_core::config::Config;

/// Guard that keeps the non-blocking file writer alive for the process lifetime.
static mut _LOG_GUARD: Option<tracing_appender::non_blocking::WorkerGuard> = None;

fn setup_logging(verbose: bool, quiet: bool, log_file: Option<PathBuf>) {
    let filter = if quiet {
        EnvFilter::new("error")
    } else if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .with_target(false);

    if let Some(path) = log_file {
        // Create directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let file = std::fs::File::create(&path).expect("Failed to create log file");
        let (non_blocking, guard) = tracing_appender::non_blocking(file);
        // Store guard in a static to keep it alive
        unsafe { _LOG_GUARD = Some(guard); }
        subscriber.with_writer(non_blocking).init();
    } else {
        subscriber.init();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Determine log file path for live mode + verbose
    let log_file = if cli.verbose && matches!(&cli.command, Command::Live(_)) {
        let run_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Some(PathBuf::from(format!("live_{}.log", run_id)))
    } else {
        None
    };

    setup_logging(cli.verbose, cli.quiet, log_file);

    let mut config = match &cli.config {
        Some(path) => Config::load(path).unwrap_or_else(|e| {
            eprintln!("Error: failed to load config '{path}': {e}");
            std::process::exit(1);
        }),
        None => {
            let default_path = "mev-scout.toml";
            if std::path::Path::new(default_path).exists() {
                Config::load(default_path).unwrap_or_else(|e| {
                    eprintln!("Error: failed to load config '{default_path}': {e}");
                    std::process::exit(1);
                })
            } else {
                Config::default()
            }
        }
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
        Command::Live(args) => commands::cmd_live(&config, args).await,
        Command::Audit(args) => commands::cmd_audit(&config, args).await,
        Command::DuneCheck(args) => commands::cmd_dune_check(&config, args).await,
    }
}