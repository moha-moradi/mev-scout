//! In-process job execution: argv/`--flag` parsing, config merge, and
//! dispatch into [`mev_scout_core::jobs`]. No CLI binary or subprocess.
//!
//! Also hosts the `MEV_SCOUT_JOB_STUB=1` fake-job mode that the
//! `/api/jobs*` integration tests use to exercise the manager
//! (spawn/stop/progress/log) without a live RPC backend.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use mev_scout_core::config::{CliOverrides, Config};
use mev_scout_core::jobs::{
    job_discover, job_doctor, job_explorer_validate, job_export, job_fetch, job_index, job_live,
    job_replay, job_report, job_run, job_scan, job_tokens, job_trace_op, job_validate_pools,
    DiscoverOpts, ExplorerValidateOpts, ExportOpts, FetchOpts, IndexOpts, LiveOpts, ReplayOpts,
    ReportOpts, RunOpts, ScanKind, ScanOpts, TokensOpts, ValidatePoolsOpts,
};
use mev_scout_core::progress::{JobProgress, ProgressEvent};

use crate::jobs::JobShared;

// ── argv parsing (the CLI's clap surface, interpreted locally) ──────────

fn parse_flags(args: &[String]) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(rest) = a.strip_prefix("--") {
            let (key, inline) = match rest.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            let mut vals = Vec::new();
            if let Some(v) = inline {
                vals.push(v);
            } else {
                while let Some(n) = it.peek() {
                    if n.starts_with("--") {
                        break;
                    }
                    vals.push(it.next().expect("peeked value").clone());
                }
            }
            m.entry(key).or_default().extend(vals);
        } else if !a.starts_with('-') {
            // Positional (e.g. tx hash for explorer show / trace).
            m.entry("_".to_string()).or_default().push(a.clone());
        }
    }
    m
}

fn flag_str(f: &HashMap<String, Vec<String>>, key: &str) -> Option<String> {
    f.get(key).and_then(|v| v.first().cloned())
}

fn flag_u64(f: &HashMap<String, Vec<String>>, key: &str) -> Option<u64> {
    flag_str(f, key).and_then(|s| s.parse().ok())
}

fn flag_usize(f: &HashMap<String, Vec<String>>, key: &str) -> Option<usize> {
    flag_str(f, key).and_then(|s| s.parse().ok())
}

fn flag_f64(f: &HashMap<String, Vec<String>>, key: &str) -> Option<f64> {
    flag_str(f, key).and_then(|s| s.parse().ok())
}

fn flag_bool(f: &HashMap<String, Vec<String>>, key: &str) -> bool {
    match f.get(key) {
        None => false,
        Some(vals) if vals.is_empty() => true,
        Some(vals) => vals
            .first()
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true),
    }
}

fn flag_list(f: &HashMap<String, Vec<String>>, key: &str) -> Vec<String> {
    f.get(key).cloned().unwrap_or_default()
}

fn overrides_from_flags(f: &HashMap<String, Vec<String>>, cmd: &str) -> CliOverrides {
    let mut o = CliOverrides::default();
    if matches!(cmd, "run" | "discover" | "scan" | "fetch") {
        o.days = flag_u64(f, "days");
        o.blocks = flag_u64(f, "blocks");
        o.block = flag_u64(f, "block");
        o.from_block = flag_u64(f, "from-block");
        o.to_block = flag_u64(f, "to-block");
    }
    o
}

fn run_opts(f: &HashMap<String, Vec<String>>) -> RunOpts {
    RunOpts {
        batch_rpc: flag_bool(f, "batch-rpc"),
        record_rejections: flag_bool(f, "record-rejections"),
    }
}

fn fetch_opts(f: &HashMap<String, Vec<String>>) -> FetchOpts {
    FetchOpts {
        batch_rpc: flag_bool(f, "batch-rpc"),
        no_sig_resolve: flag_bool(f, "no-sig-resolve"),
    }
}

fn live_opts(f: &HashMap<String, Vec<String>>) -> LiveOpts {
    LiveOpts {
        loop_enabled: flag_bool(f, "loop"),
        duration: flag_str(f, "duration"),
        poll_interval_ms: flag_u64(f, "poll-interval").unwrap_or(2000),
        record_rejections: flag_bool(f, "record-rejections"),
        max_blocks: flag_u64(f, "max-blocks"),
    }
}

fn discover_opts(f: &HashMap<String, Vec<String>>) -> DiscoverOpts {
    DiscoverOpts {
        source: flag_str(f, "source").unwrap_or_else(|| "onchain".to_string()),
        enrich: flag_bool(f, "enrich"),
        min_tvl: flag_f64(f, "min-tvl").filter(|v| *v > 0.0),
        max_pools: flag_usize(f, "max-pools").unwrap_or(1000),
        batch_size: flag_u64(f, "batch-size").unwrap_or(500),
        rpc_concurrency: flag_usize(f, "rpc-concurrency").unwrap_or(8),
        incremental: flag_bool(f, "incremental"),
        health_check: flag_str(f, "health-check").is_none_or(|v| v != "false"),
        json: flag_bool(f, "json"),
        solidly_fee_bps: flag_u64(f, "solidly-fee-bps"),
        resolve_remote_metadata: flag_bool(f, "resolve-remote-metadata"),
    }
}

fn scan_kind(s: &str) -> anyhow::Result<ScanKind> {
    match s {
        "trades" => Ok(ScanKind::Trades),
        "transfers" => Ok(ScanKind::Transfers),
        "flashloans" => Ok(ScanKind::Flashloans),
        "liquidations" => Ok(ScanKind::Liquidations),
        "labels" => Ok(ScanKind::Labels),
        other => anyhow::bail!("unknown scan kind '{other}'"),
    }
}

fn scan_opts(f: &HashMap<String, Vec<String>>) -> anyhow::Result<ScanOpts> {
    let kind = scan_kind(&flag_str(f, "kind").unwrap_or_else(|| "trades".to_string()))?;
    let addresses = f
        .get("address")
        .map(|v| v.iter().filter_map(|s| s.parse().ok()).collect::<Vec<_>>())
        .filter(|v: &Vec<alloy::primitives::Address>| !v.is_empty());
    let min_value =
        flag_str(f, "min-value").and_then(|s| s.parse::<alloy::primitives::U256>().ok());
    Ok(ScanOpts {
        kind,
        addresses,
        batch_size: flag_u64(f, "batch-size").unwrap_or(500),
        limit: flag_usize(f, "limit").unwrap_or(500),
        min_value,
    })
}

fn tokens_opts(f: &HashMap<String, Vec<String>>) -> TokensOpts {
    TokensOpts {
        symbol: flag_str(f, "symbol"),
        decimals: flag_u64(f, "decimals"),
        limit: flag_usize(f, "limit").unwrap_or(100),
        cache_only: flag_bool(f, "cache-only"),
        enrich: flag_bool(f, "enrich"),
    }
}

fn report_opts(f: &HashMap<String, Vec<String>>) -> ReportOpts {
    ReportOpts {
        run_id: flag_str(f, "run-id"),
    }
}

fn index_opts(f: &HashMap<String, Vec<String>>) -> IndexOpts {
    IndexOpts {
        from: flag_u64(f, "from"),
        to: flag_u64(f, "to"),
        days: flag_u64(f, "days"),
        live: flag_bool(f, "live"),
        duration: flag_str(f, "duration"),
    }
}

fn replay_opts(f: &HashMap<String, Vec<String>>) -> anyhow::Result<ReplayOpts> {
    let block = flag_u64(f, "block").context("--block is required for replay")?;
    Ok(ReplayOpts {
        block,
        tx_index: flag_usize(f, "tx-index"),
        analyze: flag_bool(f, "analyze"),
    })
}

fn validate_pools_opts(f: &HashMap<String, Vec<String>>) -> ValidatePoolsOpts {
    ValidatePoolsOpts {
        days: flag_u64(f, "days").unwrap_or(7),
        source: flag_str(f, "source").unwrap_or_else(|| "all".into()),
        json: flag_bool(f, "json"),
        markdown_out: flag_str(f, "markdown-out"),
    }
}

fn export_opts(f: &HashMap<String, Vec<String>>) -> ExportOpts {
    ExportOpts {
        format: flag_str(f, "format").unwrap_or_else(|| "json".into()),
        since: flag_str(f, "since"),
        kinds: flag_str(f, "kinds"),
        out: flag_str(f, "out"),
    }
}

fn explorer_validate_opts(f: &HashMap<String, Vec<String>>) -> ExplorerValidateOpts {
    ExplorerValidateOpts {
        since: flag_str(f, "since"),
        match_window: flag_u64(f, "match-window").unwrap_or(0),
        run_ids: flag_list(f, "run"),
        threshold_sweep: flag_bool(f, "threshold-sweep"),
        emit_missing_pools: flag_bool(f, "emit-missing-pools"),
        review_csv: flag_str(f, "review-csv"),
        json: flag_bool(f, "json"),
    }
}

// ── dispatch ────────────────────────────────────────────────────────────

async fn run_request(
    config_path: &str,
    command_words: &[String],
    args: &[String],
    shared: &Arc<JobShared>,
) -> anyhow::Result<()> {
    let command = command_words.join(" ");
    let flags = parse_flags(args);

    let mut config =
        Config::load_or_default(config_path).context("failed to load config for job")?;
    config
        .merge_cli(&overrides_from_flags(&flags, &command))
        .context("failed to apply job args to config")?;

    let progress: &dyn JobProgress = shared.as_ref();
    match command.as_str() {
        "run" => {
            job_run(&config, &run_opts(&flags), progress).await?;
        }
        "live" => {
            job_live(&config, &live_opts(&flags), progress).await?;
        }
        "discover" => {
            job_discover(&config, &discover_opts(&flags), progress).await?;
        }
        "scan" => {
            job_scan(&config, &scan_opts(&flags)?, progress).await?;
        }
        "tokens" => {
            job_tokens(&config, &tokens_opts(&flags), progress).await?;
        }
        "report" => {
            job_report(&config, &report_opts(&flags), progress).await?;
        }
        "fetch" => {
            job_fetch(&config, &fetch_opts(&flags), progress).await?;
        }
        "replay" => {
            job_replay(&config, &replay_opts(&flags)?, progress).await?;
        }
        "validate-pools" => {
            job_validate_pools(&config, &validate_pools_opts(&flags), progress).await?;
        }
        "explorer index" => {
            job_index(&config, &index_opts(&flags), progress).await?;
        }
        "explorer doctor" => {
            job_doctor(&config, progress).await?;
        }
        "explorer export" => {
            job_export(&config, &export_opts(&flags), progress).await?;
        }
        "explorer validate" => {
            job_explorer_validate(&config, &explorer_validate_opts(&flags), progress).await?;
        }
        "explorer show" => {
            let tx = flags
                .get("_")
                .and_then(|v| v.first().cloned())
                .context("tx hash positional arg required for explorer show")?;
            if flag_bool(&flags, "trace") {
                job_trace_op(&config, &tx, progress).await?;
            } else {
                let path = config.effective_explorer_db_path(&config.chain);
                let store = mev_scout_core::explorer::store::ExplorerStore::open(path)?;
                let ops = store.ops_for_tx(&tx)?;
                if ops.is_empty() {
                    anyhow::bail!("no explorer ops for tx {tx}");
                }
                for op in ops {
                    progress.log(&serde_json::to_string_pretty(&op)?);
                }
            }
        }
        other => anyhow::bail!("unsupported job command '{other}'"),
    }
    Ok(())
}

const STUB_COMMANDS: &[&str] = &[
    "run",
    "live",
    "discover",
    "tokens",
    "scan",
    "report",
    "fetch",
    "replay",
    "validate-pools",
    "explorer index",
    "explorer doctor",
    "explorer export",
    "explorer validate",
    "explorer show",
];

async fn stub_job(
    command_words: &[String],
    args: &[String],
    shared: &Arc<JobShared>,
) -> anyhow::Result<()> {
    let command = command_words.join(" ");
    let rest = args;
    if !STUB_COMMANDS.contains(&command.as_str()) {
        anyhow::bail!("stub: unknown command '{command}'");
    }
    if rest.iter().any(|a| a == "emit-progress") {
        shared.log("Run ID: live_1700000001");
        shared.emit(ProgressEvent::stage("resolve"));
        shared.emit(ProgressEvent {
            stage: "fetch".to_string(),
            done: Some(5),
            total: Some(10),
            run_id: None,
            ops: None,
            elapsed_ms: None,
        });
        shared.emit(ProgressEvent {
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

pub(crate) async fn run_job(
    config_path: &str,
    command_words: &[String],
    args: &[String],
    shared: &Arc<JobShared>,
) {
    let result: anyhow::Result<()> = if std::env::var("MEV_SCOUT_JOB_STUB") == Ok("1".to_string()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_live_flag_is_true() {
        let f = parse_flags(&["--live".into()]);
        assert!(flag_bool(&f, "live"));
        assert!(!flag_bool(&f, "loop"));
    }

    #[test]
    fn live_flag_false_values() {
        let f = parse_flags(&["--live=false".into()]);
        assert!(!flag_bool(&f, "live"));
        let f = parse_flags(&["--live".into(), "0".into()]);
        assert!(!flag_bool(&f, "live"));
    }
}
