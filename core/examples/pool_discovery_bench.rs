//! Cross-method pool-discovery benchmark harness (Phase 3 of
//! `docs/pool-discovery-benchmark-plan.md`).
//!
//! Every method is driven through the same `Config` + `DiscoverOpts` shape;
//! only the source/RPC assignment changes. Results are written to
//! `bench/pool-discovery-<method>-<window>.json` plus a combined summary.
//!
//! Examples:
//!   cargo run -p mev-scout-core --example pool_discovery_bench -- M1 W1
//!   cargo run -p mev-scout-core --example pool_discovery_bench -- M3 W2 \
//!       --rpc1 https://avalanche-c-chain.publicnode.com \
//!       --rpc2 "$DWELLIR_AVALANCHE_URL"

use std::collections::{BTreeSet, HashSet};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Instant;

use mev_scout_core::config::{Config, DiscoverConfig};
use mev_scout_core::jobs::job_discover;
use mev_scout_core::progress::{JobProgress, ProgressEvent};
use mev_scout_core::rpc::RpcMetricsSnapshot;
use mev_scout_core::types::ChainName;
use serde::Serialize;

const PUBLICNODE: &str = "https://avalanche-c-chain.publicnode.com";

#[derive(Debug, Clone)]
struct Args {
    method: String,
    window: String,
    repeat: usize,
    /// Optional smoke override: scan exactly N blocks instead of the W window.
    blocks_override: Option<u64>,
    /// Optional M3 override for the recent-provider cutoff.
    recent_blocks_override: Option<u64>,
    out_dir: String,
    config_path: String,
    rpc1: String,
    rpc2: Option<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = std::env::args().skip(1);
    let method = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: pool_discovery_bench <M0|M1|M2|M3|M4|M5> <W1|W2|W3> [options]"))?;
    let window = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: pool_discovery_bench <method> <W1|W2|W3> [options]"))?;
    let mut out = Args {
        method,
        window,
        repeat: 1,
        blocks_override: None,
        recent_blocks_override: None,
        out_dir: "bench".to_string(),
        config_path: "mev-scout.local.toml".to_string(),
        rpc1: PUBLICNODE.to_string(),
        rpc2: std::env::var("DWELLIR_AVALANCHE_URL").ok(),
    };
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--repeat" => out.repeat = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--blocks" => out.blocks_override = args.next().and_then(|v| v.parse().ok()),
            "--recent-blocks" => {
                out.recent_blocks_override = args.next().and_then(|v| v.parse().ok())
            }
            "--out" => {
                if let Some(v) = args.next() {
                    out.out_dir = v;
                }
            }
            "--config" => {
                if let Some(v) = args.next() {
                    out.config_path = v;
                }
            }
            "--rpc1" => {
                if let Some(v) = args.next() {
                    out.rpc1 = v;
                }
            }
            "--rpc2" => out.rpc2 = args.next(),
            _ => {}
        }
    }
    anyhow::ensure!(out.repeat > 0, "--repeat must be at least 1");
    Ok(out)
}

struct BenchProgress {
    logs: Mutex<Vec<String>>,
}

impl JobProgress for BenchProgress {
    fn emit(&self, _evt: ProgressEvent) {}

    fn log(&self, line: &str) {
        eprintln!("  {line}");
        if let Ok(mut logs) = self.logs.lock() {
            logs.push(line.to_string());
        }
    }

    fn cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug, Default, Serialize)]
struct ErrorCounts {
    total: usize,
    no_address_logs: usize,
    http_403: usize,
    rate_limit: usize,
    null_receipts: usize,
}

impl ErrorCounts {
    fn from_logs(logs: &[String]) -> Self {
        let mut out = Self::default();
        for line in logs {
            let lower = line.to_ascii_lowercase();
            let is_error = (lower.contains("failed") && !lower.contains("0 failed windows"))
                || lower.contains("error")
                || lower.contains("skipping discovery watermark");
            if is_error {
                out.total += 1;
            }
            if lower.contains("-32701") || lower.contains("specify an address") {
                out.no_address_logs += 1;
            }
            if lower.contains("403") {
                out.http_403 += 1;
            }
            if lower.contains("rate limit") || lower.contains("429") {
                out.rate_limit += 1;
            }
            if lower.contains("receipts not found") || lower.contains("null receipts") {
                out.null_receipts += 1;
            }
        }
        out
    }
}

#[derive(Debug, Serialize)]
struct OutcomeMetrics {
    pools_total: usize,
    dex_families: Vec<String>,
    pools_with_creation_block: usize,
    token_complete: usize,
    fee_complete: usize,
    tick_spacing_complete: usize,
    dex_name_complete: usize,
    tvl_usd_complete: usize,
    active_blocks: usize,
    remote_count: usize,
}

#[derive(Debug, Serialize)]
struct BenchRun {
    method: String,
    window: String,
    iteration: usize,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    source: String,
    rpc_urls: Vec<String>,
    elapsed_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<OutcomeMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc: Option<RpcMetricsSnapshot>,
    errors: ErrorCounts,
    addresses: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overlap_with_remote: Option<f64>,
    logs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BenchFile {
    method: String,
    window: String,
    repeats: usize,
    median_elapsed_ms: u128,
    runs: Vec<BenchRun>,
}

fn median(values: &[u128]) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2
    }
}

fn config_for(args: &Args, iteration: usize) -> anyhow::Result<Config> {
    let mut config = Config::load_or_default(&args.config_path)?;
    config.chain = ChainName::from_str("avalanche")?;

    // A single global RPC list keeps provider indices deterministic. Per-chain
    // overrides from the local config would silently reorder M3 providers.
    if let Some(chain) = config.chains.get_mut("avalanche") {
        chain.rpc = None;
    }
    config.rpc.rpc_url = None;
    config.rpc.rpc_urls = match args.method.as_str() {
        "M3" => {
            let second = args
                .rpc2
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("M3 requires --rpc2"))?;
            vec![args.rpc1.clone(), second.clone()]
        }
        "M2" => vec![args
            .rpc2
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("M2 requires --rpc2"))?
            .clone()],
        _ => vec![args.rpc1.clone()],
    };
    // Provider-level limits are not a discovery setting: publicnode is
    // unlimited, while Dwellir's free tier needs a conservative 1 RPS or the
    // 8 concurrent windows produce rate-limit failures.
    config.rpc.rpc_rps = match args.method.as_str() {
        "M2" => vec![1.0],
        "M3" => vec![0.0, 1.0],
        _ => vec![0.0; config.rpc.rpc_urls.len()],
    };
    config.rpc.rps_limit = 0.0;
    config.days = None;
    config.blocks = None;
    config.block = None;
    config.from_block = None;
    config.to_block = None;
    if let Some(blocks) = args.blocks_override {
        config.blocks = Some(blocks);
    } else {
        match args.window.as_str() {
            "W1" => config.blocks = Some(1000),
            "W2" => config.days = Some(7),
            "W3" => config.days = Some(30),
            other => anyhow::bail!("unknown window '{other}' (expected W1|W2|W3)"),
        }
    }

    // Identical discovery settings for every method (plan §"تنظیمات یکسان").
    config.discover = DiscoverConfig {
        source: match args.method.as_str() {
            "M0" => "hybrid",
            "M5" => "remote",
            _ => "receipts",
        }
        .to_string(),
        enrich: false,
        batch_size: 2000,
        factory_batch_size: Some(10000),
        lookback_days: None,
        rpc_concurrency: 8,
        health_check: false,
        min_tvl: 0.0,
        max_pools: 100_000,
        resolve_remote_metadata: false,
        solidly_fee_bps: None,
        receipt_batch_blocks: 4,
        receipt_cache_blocks: false,
        receipt_recent_blocks: if args.method == "M3" {
            Some(args.recent_blocks_override.unwrap_or_else(|| {
                // Half the window: publicnode serves the recent half, Dwellir
                // the older half. Hardcoding 1000 would make W1 single-peer.
                match args.window.as_str() {
                    "W2" => 302_400 / 2, // 7 days
                    "W3" => 1_296_000 / 2, // 30 days
                    _ => 1000 / 2,
                }
            }))
        } else {
            None
        },
    };

    let db_dir = format!("{}/cache", args.out_dir);
    std::fs::create_dir_all(&db_dir)?;
    config.output.db_path = format!("{db_dir}/{}-{}-run{iteration}.sqlite", args.method, args.window);
    Ok(config)
}

/// Keep credential-like UUID path segments out of benchmark JSON.
fn redact_url(url: &str) -> String {
    url.split('/')
        .map(|part| {
            let compact = part.replace('-', "");
            if compact.len() >= 32 && compact.chars().all(|c| c.is_ascii_hexdigit()) {
                "<redacted>".to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

async fn run_once(args: &Args, iteration: usize) -> BenchRun {
    let rpc_urls = match args.method.as_str() {
        "M3" => vec![args.rpc1.clone(), args.rpc2.clone().unwrap_or_default()],
        "M2" => vec![args.rpc2.clone().unwrap_or_default()],
        _ => vec![args.rpc1.clone()],
    }
    .into_iter()
    .map(|url| redact_url(&url))
    .collect();
    let source = match args.method.as_str() {
        "M0" => "hybrid",
        "M5" => "remote",
        _ => "receipts",
    }
    .to_string();

    if args.method == "M4" {
        return BenchRun {
            method: args.method.clone(),
            window: args.window.clone(),
            iteration,
            status: "skipped".to_string(),
            reason: Some(
                "factory_state fast-path is intentionally not enabled (P4 unverified / no benchmark win)"
                    .to_string(),
            ),
            source: "factory_state".to_string(),
            rpc_urls,
            elapsed_ms: 0,
            outcome: None,
            rpc: None,
            errors: ErrorCounts::default(),
            addresses: Vec::new(),
            overlap_with_remote: None,
            logs: Vec::new(),
        };
    }

    let config = match config_for(args, iteration) {
        Ok(c) => c,
        Err(e) => {
            return BenchRun {
                method: args.method.clone(),
                window: args.window.clone(),
                iteration,
                status: "failed".to_string(),
                reason: Some(format!("{e:#}")),
                source,
                rpc_urls,
                elapsed_ms: 0,
                outcome: None,
                rpc: None,
                errors: ErrorCounts::default(),
                addresses: Vec::new(),
                overlap_with_remote: None,
                logs: Vec::new(),
            }
        }
    };

    let progress = BenchProgress {
        logs: Mutex::new(Vec::new()),
    };
    let started = Instant::now();
    let result = job_discover(&config, &mev_scout_core::jobs::DiscoverOpts::from_config(&config), &progress).await;
    let elapsed_ms = started.elapsed().as_millis();
    let logs = progress.logs.lock().map(|g| g.clone()).unwrap_or_default();
    let errors = ErrorCounts::from_logs(&logs);

    match result {
        Ok(outcome) => {
            let mut dex_families: Vec<String> = outcome
                .pools
                .iter()
                .map(|p| p.dex_type.to_string())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            dex_families.sort_unstable();
            let metrics = OutcomeMetrics {
                pools_total: outcome.pools_found,
                dex_families,
                pools_with_creation_block: outcome.pools.iter().filter(|p| p.creation_block > 0).count(),
                token_complete: outcome
                    .pools
                    .iter()
                    .filter(|p| !p.token0.is_zero() && !p.token1.is_zero())
                    .count(),
                fee_complete: outcome.pools.iter().filter(|p| p.fee > 0).count(),
                tick_spacing_complete: outcome.pools.iter().filter(|p| p.tick_spacing.is_some()).count(),
                dex_name_complete: outcome.pools.iter().filter(|p| p.dex_name.is_some()).count(),
                tvl_usd_complete: outcome.pools.iter().filter(|p| p.tvl_usd.is_some()).count(),
                active_blocks: outcome.active_blocks,
                remote_count: outcome.remote_count,
            };
            let addresses = outcome
                .pools
                .iter()
                .map(|p| format!("{:?}", p.address))
                .collect();
            BenchRun {
                method: args.method.clone(),
                window: args.window.clone(),
                iteration,
                status: "ok".to_string(),
                reason: None,
                source,
                rpc_urls,
                elapsed_ms,
                outcome: Some(metrics),
                rpc: outcome.rpc_metrics,
                errors,
                addresses,
                overlap_with_remote: None,
                logs,
            }
        }
        Err(e) => BenchRun {
            method: args.method.clone(),
            window: args.window.clone(),
            iteration,
            status: "failed".to_string(),
            reason: Some(format!("{e:#}")),
            source,
            rpc_urls,
            elapsed_ms,
            outcome: None,
            rpc: None,
            errors,
            addresses: Vec::new(),
            overlap_with_remote: None,
            logs,
        },
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    anyhow::ensure!(
        matches!(args.method.as_str(), "M0" | "M1" | "M2" | "M3" | "M4" | "M5"),
        "unknown method '{}' (expected M0..M5)",
        args.method
    );
    std::fs::create_dir_all(&args.out_dir)?;

    let mut runs = Vec::new();
    for iteration in 1..=args.repeat {
        eprintln!("== {} {} run {iteration}/{} ==", args.method, args.window, args.repeat);
        runs.push(run_once(&args, iteration).await);
    }

    // M5 is the non-on-chain control; use its addresses to score overlap for
    // every other method without changing their discovery settings.
    let remote_addresses: HashSet<String> = runs
        .iter()
        .filter(|r| r.method == "M5")
        .flat_map(|r| r.addresses.iter().cloned())
        .collect();
    for run in &mut runs {
        if run.method != "M5" && !remote_addresses.is_empty() && !run.addresses.is_empty() {
            let hits = run.addresses.iter().filter(|a| remote_addresses.contains(*a)).count();
            run.overlap_with_remote = Some(hits as f64 / run.addresses.len() as f64);
        }
    }

    let file = BenchFile {
        method: args.method.clone(),
        window: args.window.clone(),
        repeats: args.repeat,
        median_elapsed_ms: median(&runs.iter().map(|r| r.elapsed_ms).collect::<Vec<_>>()),
        runs,
    };
    let out_path = format!(
        "{}/pool-discovery-{}-{}.json",
        args.out_dir, args.method, args.window
    );
    std::fs::write(&out_path, serde_json::to_string_pretty(&file)?)?;

    println!("benchmark written to {out_path}\n");
    println!(
        "| method | coverage (pools / with creation_block) | dex_families | time_ms | rpc_calls | errors |"
    );
    println!("|---|---:|---:|---:|---:|---:|");
    for run in &file.runs {
        let o = run.outcome.as_ref();
        println!(
            "| {} | {} / {} | {} | {} | {} | {} |",
            run.method,
            o.map(|m| m.pools_total).unwrap_or_default(),
            o.map(|m| m.pools_with_creation_block).unwrap_or_default(),
            o.map(|m| m.dex_families.len()).unwrap_or_default(),
            run.elapsed_ms,
            run.rpc.as_ref().map(|m| m.requests).unwrap_or_default(),
            run.errors.total,
        );
    }
    Ok(())
}
