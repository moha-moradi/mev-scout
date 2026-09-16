//! In-process job execution: rebuild the CLI argv for a job, let clap parse
//! it, then run the command through `commands::execute` with the API's
//! [`JobShared`] as the progress sink — no subprocess.
//!
//! Also hosts the `MEV_SCOUT_JOB_STUB=1` fake-job mode that the
//! `/api/jobs*` integration tests use to exercise the manager
//! (spawn/stop/progress/log) without a live RPC backend.

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use mev_scout_cli::cli::Cli;
use mev_scout_cli::commands;
use mev_scout_cli::JobProgress;
use mev_scout_core::config::Config;

use crate::jobs::JobShared;

/// Run a real job: parse argv, build config, dispatch the command.
async fn run_request(
    config_path: &str,
    command_words: &[String],
    args: &[String],
    shared: &Arc<JobShared>,
) -> anyhow::Result<()> {
    let mut argv: Vec<String> = vec!["mev-scout".to_string()];
    argv.extend_from_slice(command_words);
    argv.extend_from_slice(args);

    let cli = Cli::try_parse_from(argv)
        .map_err(|e| anyhow::anyhow!("job args failed to parse: {e}"))?;

    let config = Config::load_or_default(config_path)
        .context("failed to load config for job")?;

    commands::execute(&cli.command, &config, shared.as_ref()).await
}

/// `MEV_SCOUT_JOB_STUB=1` fake job: behavior driven by marker args tests
/// pass (the CLI being invoked is simulated, matching the old `stub_main`).
async fn stub_job(
    command_words: &[String],
    args: &[String],
    shared: &Arc<JobShared>,
) -> anyhow::Result<()> {
    let command = command_words.join(" ");
    let rest = args;
    match command.as_str() {
        "run" | "live" | "discover" | "tokens" | "scan" | "report" | "explorer index" => {
            if rest.iter().any(|a| a == "emit-progress") {
                // Emit a Run ID + stage events, then stay alive until
                // cancelled (progress + stop tests).
                shared.log("Run ID: live_1700000001");
                shared.emit(mev_scout_cli::ProgressEvent::stage("resolve"));
                shared.emit(mev_scout_cli::ProgressEvent {
                    stage: "fetch".to_string(),
                    done: Some(5),
                    total: Some(10),
                    run_id: None,
                    ops: None,
                    elapsed_ms: None,
                });
                shared.emit(mev_scout_cli::ProgressEvent {
                    stage: "detect".to_string(),
                    done: Some(8),
                    total: Some(10),
                    run_id: None,
                    ops: None,
                    elapsed_ms: None,
                });
                loop {
                    if shared.cancelled() {
                        return Ok(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            } else if rest.iter().any(|a| a == "fail") {
                shared.log("stub failed: boom");
                anyhow::bail!("stub failure requested");
            } else if rest.iter().any(|a| a == "sleep-secs") {
                let idx = rest.iter().position(|a| a == "sleep-secs").unwrap();
                let secs: u64 = rest[idx + 1..]
                    .first()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1);
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                shared.log(&format!("done sleeping {secs}s"));
                Ok(())
            } else {
                shared.log(&format!("hello from stub; args={}", rest.join(" ")));
                shared.log("Run ID: run_1700000000");
                Ok(())
            }
        }
        other => anyhow::bail!("stub: unknown command '{other}'"),
    }
}

/// Entry point called by the job thread inside its own runtime: run the
/// command (or the stub), then resolve final status on [`JobShared`].
pub(crate) async fn run_job(
    config_path: &str,
    command_words: &[String],
    args: &[String],
    shared: &Arc<JobShared>,
) {
    let result: anyhow::Result<()> = if std::env::var("MEV_SCOUT_JOB_STUB") == Ok("1".to_string())
    {
        stub_job(command_words, args, shared).await
    } else {
        run_request(config_path, command_words, args, shared).await
    };

    let killed = shared.cancelled();
    match (&result, killed) {
        (Ok(_), _) => shared.finish(Some(0), killed),
        (Err(_), true) => {
            shared.log("job cancelled");
            shared.finish(None, true);
        }
        (Err(e), false) => {
            shared.log(&format!("job failed: {e:#}"));
            shared.finish(Some(1), false);
        }
    }
}