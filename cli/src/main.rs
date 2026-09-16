use clap::Parser;
use tracing_subscriber::EnvFilter;

use mev_scout_cli::cli::{Cli, Command};
use mev_scout_cli::commands;
use mev_scout_cli::job_progress::{BarProgress, JobProgress, NoopProgress, StdoutJsonProgress};
use mev_scout_cli::overrides;
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

/// Pick the presentation sink: NDJSON on stdout when `--progress json` was
/// requested by a progress-capable command, an indicatif bar otherwise.
fn make_sink(cmd: &Command) -> Box<dyn JobProgress> {
    let json = match cmd {
        Command::Run(a) => a.progress.as_deref() == Some("json"),
        Command::Live(a) => a.progress.as_deref() == Some("json"),
        _ => false,
    };
    if json {
        Box::new(StdoutJsonProgress)
    } else if matches!(cmd, Command::Run(_) | Command::Live(_)) {
        Box::new(BarProgress::new())
    } else {
        Box::new(NoopProgress)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    setup_logging(cli.verbose, cli.quiet);

    let mut config = match &cli.config {
        // Explicit --config: a missing file is still a fallback to defaults
        // (logged), but a malformed file is a hard error.
        Some(path) => Config::load_or_default(path)?,
        None => {
            let default_path = "mev-scout.toml";
            if std::path::Path::new(default_path).exists() {
                Config::load_or_default(default_path)?
            } else {
                Config::default()
            }
        }
    };

    let overrides = overrides::build_overrides_from_command(&cli.command);
    config.merge_cli(&overrides)?;

    let progress = make_sink(&cli.command);
    commands::execute(&cli.command, &config, progress.as_ref()).await
}