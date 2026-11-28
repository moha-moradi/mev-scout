//! Stage 1 capacity benchmark for `docs/POOL_DISCOVERY_REDESIGN_PLAN.md`.
//!
//! This harness compares only the three acquisition paths selected in the plan:
//!   1. address/topic-filtered `eth_getLogs` (PublicNode and Dwellir),
//!   2. the existing batched `eth_getBlockReceipts` scanner,
//!   3. a minimal block-polling + per-block receipt path.
//!
//! It freezes one canonical head before the runs, scans fixed block ranges,
//! and records provider/quota/batch accounting fields that the older
//! `pool_discovery_bench` harness did not capture.
//!
//! Examples:
//!   cargo run -p mev-scout-core --example stage1_avalanche_bench -- all 1000
//!   cargo run -p mev-scout-core --example stage1_avalanche_bench -- receipts_dwellir 10000 \
//!       --rpc2 "$DWELLIR_AVALANCHE_URL" --quota-tier "dwellir-20-rps"
//!   cargo run -p mev-scout-core --example stage1_avalanche_bench -- logs_publicnode 30d

use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use mev_scout_core::cache::{SqliteStore, TokenCache};
use mev_scout_core::chain::timing::blocks_per_day;
use mev_scout_core::config::{ChainConfig, Config, DiscoverConfig};
use mev_scout_core::jobs::{job_discover, DiscoverOpts};
use mev_scout_core::pool::discovery::{
    creation_event_topics, decode_factory_creation_log, discover_pools_from_receipts,
    DiscoveredPool, DiscoveryConfig, DiscoveryRuntimeOpts, ReceiptScanOptions, ResolvedFactories,
};
use mev_scout_core::progress::{JobProgress, ProgressEvent};
use mev_scout_core::rpc::RpcMetricsSnapshot;
use mev_scout_core::types::ChainName;
use serde::Serialize;

const CHAIN_ID: u64 = 43_114;
const PUBLICNODE: &str = "https://avalanche-c-chain.publicnode.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    PublicNode,
    Dwellir,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Provider::PublicNode => "publicnode",
            Provider::Dwellir => "dwellir",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    FilteredLogs,
    Receipts,
    PollReceipts,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::FilteredLogs => "filtered_logs",
            Kind::Receipts => "batched_receipts",
            Kind::PollReceipts => "poll_receipts",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Method {
    name: &'static str,
    provider: Provider,
    kind: Kind,
}

const METHODS: [Method; 6] = [
    Method {
        name: "logs_publicnode",
        provider: Provider::PublicNode,
        kind: Kind::FilteredLogs,
    },
    Method {
        name: "logs_dwellir",
        provider: Provider::Dwellir,
        kind: Kind::FilteredLogs,
    },
    Method {
        name: "receipts_publicnode",
        provider: Provider::PublicNode,
        kind: Kind::Receipts,
    },
    Method {
        name: "receipts_dwellir",
        provider: Provider::Dwellir,
        kind: Kind::Receipts,
    },
    Method {
        name: "poll_publicnode",
        provider: Provider::PublicNode,
        kind: Kind::PollReceipts,
    },
    Method {
        name: "poll_dwellir",
        provider: Provider::Dwellir,
        kind: Kind::PollReceipts,
    },
];

#[derive(Debug, Clone)]
struct Args {
    /// `all` or one of the six method names.
    method: String,
    /// `1000`, `10000`, or `30d`.
    range: String,
    repeat: usize,
    publicnode_url: String,
    dwellir_url: Option<String>,
    out_dir: String,
    config_path: String,
    publicnode_rps: f64,
    dwellir_rps: f64,
    concurrency: usize,
    receipt_batch_blocks: u64,
    logs_batch_blocks: u64,
    /// PublicNode rejects filters with more than 10 addresses.
    log_address_limit: usize,
    quota_tier: Option<String>,
    credits_used: Option<f64>,
    head: Option<u64>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = std::env::args().skip(1);
    let method = args.next().ok_or_else(|| {
        anyhow::anyhow!(
            "usage: stage1_avalanche_bench <all|logs_publicnode|logs_dwellir|receipts_publicnode|receipts_dwellir|poll_publicnode|poll_dwellir> <1000|10000|30d> [options]"
        )
    })?;
    let range = args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: stage1_avalanche_bench <method> <1000|10000|30d> [options]")
    })?;
    let mut out = Args {
        method,
        range,
        repeat: 1,
        publicnode_url: PUBLICNODE.to_string(),
        dwellir_url: std::env::var("DWELLIR_AVALANCHE_URL").ok(),
        out_dir: "bench".to_string(),
        config_path: "mev-scout.local.toml".to_string(),
        publicnode_rps: 0.0,
        dwellir_rps: 20.0,
        concurrency: 8,
        receipt_batch_blocks: 4,
        logs_batch_blocks: 1_000,
        log_address_limit: 10,
        quota_tier: std::env::var("DWELLIR_QUOTA_TIER").ok(),
        credits_used: None,
        head: None,
    };
    while let Some(flag) = args.next() {
        let value = args.next();
        match flag.as_str() {
            "--repeat" => out.repeat = value.and_then(|v| v.parse().ok()).unwrap_or(1),
            "--rpc1" | "--publicnode" => {
                if let Some(v) = value {
                    out.publicnode_url = v;
                }
            }
            "--rpc2" | "--dwellir" => out.dwellir_url = value,
            "--out" => {
                if let Some(v) = value {
                    out.out_dir = v;
                }
            }
            "--config" => {
                if let Some(v) = value {
                    out.config_path = v;
                }
            }
            "--publicnode-rps" => {
                out.publicnode_rps = value.and_then(|v| v.parse().ok()).unwrap_or(0.0)
            }
            "--dwellir-rps" => out.dwellir_rps = value.and_then(|v| v.parse().ok()).unwrap_or(20.0),
            "--concurrency" => out.concurrency = value.and_then(|v| v.parse().ok()).unwrap_or(8),
            "--receipt-batch-blocks" => {
                out.receipt_batch_blocks = value.and_then(|v| v.parse().ok()).unwrap_or(4)
            }
            "--logs-batch-blocks" => {
                out.logs_batch_blocks = value.and_then(|v| v.parse().ok()).unwrap_or(1_000)
            }
            "--log-address-limit" => {
                out.log_address_limit = value.and_then(|v| v.parse().ok()).unwrap_or(10)
            }
            "--quota-tier" => out.quota_tier = value,
            "--credits-used" => out.credits_used = value.and_then(|v| v.parse().ok()),
            "--head" => out.head = value.and_then(|v| v.parse().ok()),
            other => anyhow::bail!("unknown flag '{other}'"),
        }
    }
    anyhow::ensure!(out.repeat > 0, "--repeat must be at least 1");
    anyhow::ensure!(out.concurrency > 0, "--concurrency must be at least 1");
    anyhow::ensure!(
        out.receipt_batch_blocks > 0,
        "--receipt-batch-blocks must be at least 1"
    );
    anyhow::ensure!(
        out.logs_batch_blocks > 0,
        "--logs-batch-blocks must be at least 1"
    );
    anyhow::ensure!(
        out.log_address_limit > 0,
        "--log-address-limit must be at least 1"
    );
    Ok(out)
}

fn methods_for(args: &Args) -> anyhow::Result<Vec<Method>> {
    if args.method == "all" {
        return Ok(METHODS.to_vec());
    }
    METHODS
        .iter()
        .find(|m| m.name == args.method)
        .copied()
        .map(|m| vec![m])
        .ok_or_else(|| anyhow::anyhow!("unknown method '{}'", args.method))
}

fn endpoint_for(args: &Args, provider: Provider) -> anyhow::Result<String> {
    match provider {
        Provider::PublicNode => Ok(args.publicnode_url.clone()),
        Provider::Dwellir => args.dwellir_url.clone().ok_or_else(|| {
            anyhow::anyhow!("Dwellir method requires --rpc2 or DWELLIR_AVALANCHE_URL")
        }),
    }
}

fn rps_for(args: &Args, provider: Provider) -> f64 {
    match provider {
        Provider::PublicNode => args.publicnode_rps,
        Provider::Dwellir => args.dwellir_rps,
    }
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

#[derive(Debug, Serialize)]
struct Stage1Progress {
    logs: Mutex<Vec<String>>,
}

impl JobProgress for Stage1Progress {
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

impl Stage1Progress {
    fn new() -> Self {
        Self {
            logs: Mutex::new(Vec::new()),
        }
    }

    fn lines(&self) -> Vec<String> {
        self.logs.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Default, Serialize)]
struct ErrorCounts {
    total: usize,
    rate_limit: usize,
    http_403: usize,
    getlogs_unsupported: usize,
    null_receipts: usize,
    failed_window_lines: usize,
    failed_window_count: usize,
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
            if lower.contains("429") || lower.contains("rate limit") {
                out.rate_limit += 1;
            }
            if lower.contains("403") {
                out.http_403 += 1;
            }
            if lower.contains("-32701") || lower.contains("specify an address") {
                out.getlogs_unsupported += 1;
            }
            if lower.contains("receipts not found") || lower.contains("null receipts") {
                out.null_receipts += 1;
            }
            if lower.contains("failed windows [") && !lower.contains("0 failed windows") {
                out.failed_window_lines += 1;
            }
            let n = parse_failed_window_count(&[line.to_string()]);
            out.failed_window_count = out.failed_window_count.max(n);
        }
        out
    }
}

fn parse_failed_window_count(logs: &[String]) -> usize {
    let mut found = 0usize;
    for line in logs {
        let lower = line.to_ascii_lowercase();
        let Some(end) = lower.find(" failed windows)") else {
            continue;
        };
        let Some(start) = lower[..end].rfind('(') else {
            continue;
        };
        if let Ok(n) = lower[start + 1..end].trim().parse::<usize>() {
            found = found.max(n);
        }
    }
    found
}

#[derive(Debug, Clone, Serialize)]
struct ProbeResult {
    supported: bool,
    detail: String,
}

#[derive(Debug, Clone, Serialize)]
struct Capability {
    filtered_logs_one_block: ProbeResult,
    filtered_logs_range: ProbeResult,
    block_receipts: ProbeResult,
}

#[derive(Debug, Clone, Serialize)]
struct Accounting {
    provider: &'static str,
    endpoint: String,
    quota_tier: Option<String>,
    credits_used: Option<f64>,
    rps_limit: f64,
    log_filter_chunks: usize,
    http_requests: u64,
    json_rpc_calls: u64,
    json_rpc_calls_inside_batches: u64,
    response_bytes: Option<u64>,
    batch_accounting_note: String,
}

#[derive(Debug, Serialize)]
struct MissedComparison {
    reference: String,
    reference_pools: usize,
    missed_pools: usize,
    missed_rate: f64,
    missed_addresses: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BenchRun {
    method: String,
    kind: &'static str,
    provider: &'static str,
    range: String,
    iteration: usize,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    source: String,
    endpoint: String,
    canonical_head_number: u64,
    canonical_head_hash: String,
    from_block: u64,
    to_block: u64,
    blocks: u64,
    elapsed_ms: u128,
    blocks_per_sec: f64,
    pools_total: usize,
    dex_families: Vec<String>,
    addresses: Vec<String>,
    capability: Capability,
    accounting: Accounting,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc: Option<RpcMetricsSnapshot>,
    errors: ErrorCounts,
    failed_windows: Vec<(u64, u64)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    missed_vs_filtered_logs: Option<MissedComparison>,
    logs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BenchFile {
    method: String,
    range: String,
    repeats: usize,
    median_elapsed_ms: u128,
    canonical_head_number: u64,
    canonical_head_hash: String,
    accounting_note: String,
    runs: Vec<BenchRun>,
}

fn percentile(samples: &[u128], pct: usize) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let idx = (sorted.len() * pct) / 100;
    sorted[idx.min(sorted.len() - 1)]
}

fn median(samples: &[u128]) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2
    }
}

fn manual_rpc_snapshot(
    requests: u64,
    errors: u64,
    bytes: u64,
    latencies_ms: &[u128],
) -> RpcMetricsSnapshot {
    RpcMetricsSnapshot {
        requests,
        errors,
        retryable_errors: 0,
        bytes,
        p50_ms: percentile(latencies_ms, 50) as u64,
        p95_ms: percentile(latencies_ms, 95) as u64,
        p99_ms: percentile(latencies_ms, 99) as u64,
    }
}

fn range_bounds(args: &Args, head: u64) -> anyhow::Result<(u64, u64)> {
    let blocks = match args.range.as_str() {
        "1000" => 1_000,
        "10000" => 10_000,
        "30d" => blocks_per_day(ChainName::Avalanche).saturating_mul(30),
        other => anyhow::bail!("unknown range '{other}' (expected 1000, 10000, or 30d)"),
    };
    anyhow::ensure!(
        head >= blocks,
        "head {head} is older than requested range {blocks}"
    );
    Ok((head - blocks + 1, head))
}

fn chain_config(args: &Args) -> anyhow::Result<ChainConfig> {
    let config = Config::load_or_default(&args.config_path)?;
    Ok(config
        .chains
        .get("avalanche")
        .cloned()
        .unwrap_or_else(ChainConfig::default))
}

fn resolved_factories(chain_config: &ChainConfig) -> ResolvedFactories {
    ResolvedFactories::from_chain_config(chain_config, ChainName::Avalanche)
}

fn factory_addresses(factories: &ResolvedFactories) -> Vec<Address> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |addr: Option<Address>| {
        if let Some(a) = addr {
            if seen.insert(a) {
                out.push(a);
            }
        }
    };
    push(factories.vault);
    push(factories.registry);
    push(factories.v4_pool_manager);
    push(factories.infinity_cl_pool_manager);
    push(factories.metric_factory);
    push(factories.fluid_factory);
    push(factories.pendle_factory);
    for list in [
        &factories.v2_factories,
        &factories.v3_factories,
        &factories.curve_factories,
        &factories.solidly_factories,
        &factories.camelot_factories,
        &factories.trader_joe_factories,
    ] {
        for &addr in list.iter() {
            if seen.insert(addr) {
                out.push(addr);
            }
        }
    }
    out
}

fn address_chunks(addresses: &[Address], limit: usize) -> Vec<Vec<Address>> {
    addresses.chunks(limit.max(1)).map(|c| c.to_vec()).collect()
}

fn discovery_config<'a>(
    factories: &'a ResolvedFactories,
    batch_blocks: u64,
    concurrency: usize,
    token_cache: Option<&'a TokenCache>,
    pool_cache: Option<&'a SqliteStore>,
) -> DiscoveryConfig<'a> {
    factories.discovery_config(DiscoveryRuntimeOpts {
        batch_size: batch_blocks,
        factory_batch_size: batch_blocks,
        solidly_fee_bps: None,
        rpc_concurrency: concurrency,
        token_cache,
        pool_cache,
    })
}

fn neutral_discover_config(
    source: &str,
    args: &Args,
    from: u64,
    to: u64,
    endpoint: &str,
    rps: f64,
    iteration: usize,
    out_dir: &str,
) -> anyhow::Result<Config> {
    let mut config = Config::load_or_default(&args.config_path)?;
    config.chain = ChainName::Avalanche;
    if let Some(chain) = config.chains.get_mut("avalanche") {
        chain.rpc = None;
    }
    config.rpc.rpc_url = None;
    config.rpc.rpc_urls = vec![endpoint.to_string()];
    config.rpc.rpc_rps = vec![rps];
    config.rpc.rps_limit = 0.0;
    config.days = None;
    config.blocks = None;
    config.block = None;
    config.from_block = Some(from);
    config.to_block = Some(to);
    config.discover = DiscoverConfig {
        source: source.to_string(),
        enrich: false,
        batch_size: args.logs_batch_blocks,
        factory_batch_size: Some(args.logs_batch_blocks),
        lookback_days: None,
        rpc_concurrency: args.concurrency,
        health_check: false,
        min_tvl: 0.0,
        max_pools: 100_000,
        resolve_remote_metadata: false,
        solidly_fee_bps: None,
        receipt_batch_blocks: args.receipt_batch_blocks,
        receipt_cache_blocks: false,
        receipt_recent_blocks: None,
    };
    std::fs::create_dir_all(format!("{out_dir}/cache"))?;
    config.output.db_path = format!(
        "{out_dir}/cache/stage1-{}-{from}-{to}-run{iteration}.sqlite",
        args.method
    );
    Ok(config)
}

async fn freeze_head(args: &Args) -> anyhow::Result<(u64, String)> {
    let rpc = mev_scout_core::rpc::RpcClient::from_urls(&[args.publicnode_url.as_str()], CHAIN_ID)?;
    let head = rpc.get_block_number().await?;
    let hash = rpc.get_block_hash(head).await?;
    Ok((head, format!("{hash:?}")))
}

async fn probe_capabilities(
    args: &Args,
    method: Method,
    head: u64,
    factories: &ResolvedFactories,
) -> anyhow::Result<(Capability, String)> {
    let endpoint = endpoint_for(args, method.provider)?;
    let rpc = mev_scout_core::rpc::RpcClient::from_urls(&[endpoint.as_str()], CHAIN_ID)?;
    rpc.with_provider_rps(&[rps_for(args, method.provider)])
        .await;

    let addresses = factory_addresses(factories);
    let chunks = address_chunks(&addresses, args.log_address_limit);
    let probe_addresses = chunks.first().cloned().unwrap_or_default();
    let probe_address_count = probe_addresses.len();
    let topics = creation_event_topics();
    let one_block = Filter::new()
        .address(probe_addresses.clone())
        .event_signature(topics.clone())
        .from_block(head)
        .to_block(head);
    let probe_blocks = if args.range == "1000" { 1_000 } else { 10_000 };
    let from = head.saturating_sub(probe_blocks - 1);
    let multi_block = Filter::new()
        .address(probe_addresses)
        .event_signature(topics)
        .from_block(from)
        .to_block(head);

    let logs_one = match rpc.get_logs_pinned_strict(0, &one_block).await {
        Ok(logs) => ProbeResult {
            supported: true,
            detail: format!(
                "{} matching log(s) in one block; probe chunk {probe_address_count} address(es)",
                logs.len()
            ),
        },
        Err(e) => ProbeResult {
            supported: false,
            detail: format!("{e:#}"),
        },
    };
    let logs_range = match rpc.get_logs_pinned_strict(0, &multi_block).await {
        Ok(logs) => ProbeResult {
            supported: true,
            detail: format!(
                "{} matching log(s) in {probe_blocks} blocks; probe chunk {probe_address_count} address(es)",
                logs.len()
            ),
        },
        Err(e) => ProbeResult { supported: false, detail: format!("{e:#}") },
    };
    let receipts = match rpc.get_block_and_receipts_batch_for(0, head).await {
        Ok((_block, _txs, receipts)) => ProbeResult {
            supported: true,
            detail: format!("block {head} with {} receipt(s)", receipts.len()),
        },
        Err(e) => ProbeResult {
            supported: false,
            detail: format!("{e:#}"),
        },
    };

    Ok((
        Capability {
            filtered_logs_one_block: logs_one,
            filtered_logs_range: logs_range,
            block_receipts: receipts,
        },
        endpoint,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_filtered_logs(
    args: &Args,
    method: Method,
    iteration: usize,
    head: u64,
    head_hash: &str,
    from: u64,
    to: u64,
    factories: &ResolvedFactories,
    capability: Capability,
    endpoint: &str,
    quota: &Accounting,
) -> BenchRun {
    let started = Instant::now();
    let mut logs = Vec::new();
    let mut pools: Vec<DiscoveredPool> = Vec::new();
    let mut seen_pools = HashSet::new();
    let mut failed_windows = Vec::new();
    let mut latencies = Vec::new();
    let mut requests = 0u64;
    let mut errors = 0u64;

    let rpc = match mev_scout_core::rpc::RpcClient::from_urls(&[endpoint], CHAIN_ID) {
        Ok(rpc) => rpc,
        Err(e) => {
            return failed_run(
                args,
                method,
                iteration,
                head,
                head_hash,
                from,
                to,
                capability,
                quota,
                format!("{e:#}"),
            )
        }
    };
    rpc.with_provider_rps(&[rps_for(args, method.provider)])
        .await;
    let disc_config = discovery_config(
        factories,
        args.logs_batch_blocks,
        args.concurrency,
        None,
        None,
    );
    let chunks = address_chunks(&factory_addresses(factories), args.log_address_limit);
    let topics = creation_event_topics();
    let mut quota = quota.clone();
    quota.log_filter_chunks = chunks.len();

    for (chunk_idx, addresses) in chunks.iter().enumerate() {
        let mut cursor = from;
        while cursor <= to {
            let end = cursor.saturating_add(args.logs_batch_blocks - 1).min(to);
            let filter = Filter::new()
                .address(addresses.clone())
                .event_signature(topics.clone())
                .from_block(cursor)
                .to_block(end);
            let t0 = Instant::now();
            requests += 1;
            match rpc.get_logs_pinned_strict(0, &filter).await {
                Ok(entries) => {
                    latencies.push(t0.elapsed().as_millis());
                    for log in &entries {
                        if let Some((_addr, pool)) = decode_factory_creation_log(log, &disc_config)
                        {
                            if seen_pools.insert(pool.address) {
                                pools.push(pool);
                            }
                        }
                    }
                    logs.push(format!(
                        "getLogs chunk {} {cursor}..{end}: {} log(s)",
                        chunk_idx + 1,
                        entries.len()
                    ));
                }
                Err(e) => {
                    errors += 1;
                    failed_windows.push((cursor, end));
                    logs.push(format!(
                        "getLogs chunk {} {cursor}..{end} failed: {e:#}",
                        chunk_idx + 1
                    ));
                }
            }
            if end == to {
                break;
            }
            cursor = end + 1;
        }
    }

    let elapsed_ms = started.elapsed().as_millis();
    finish_run(
        args,
        method,
        iteration,
        head,
        head_hash,
        from,
        to,
        elapsed_ms,
        pools,
        capability,
        &quota,
        requests,
        Some(manual_rpc_snapshot(requests, errors, 0, &latencies)),
        &logs,
        failed_windows,
    )
}

#[allow(clippy::too_many_arguments)]
async fn run_receipts(
    args: &Args,
    method: Method,
    iteration: usize,
    head: u64,
    head_hash: &str,
    from: u64,
    to: u64,
    capability: Capability,
    endpoint: &str,
    quota: &Accounting,
) -> BenchRun {
    let config = match neutral_discover_config(
        "receipts",
        args,
        from,
        to,
        endpoint,
        rps_for(args, method.provider),
        iteration,
        &args.out_dir,
    ) {
        Ok(c) => c,
        Err(e) => {
            return failed_run(
                args,
                method,
                iteration,
                head,
                head_hash,
                from,
                to,
                capability,
                quota,
                format!("{e:#}"),
            )
        }
    };
    let progress = Stage1Progress::new();
    let started = Instant::now();
    let result = job_discover(&config, &DiscoverOpts::from_config(&config), &progress).await;
    let elapsed_ms = started.elapsed().as_millis();
    let logs = progress.lines();
    match result {
        Ok(outcome) => {
            let http_requests = outcome
                .rpc_metrics
                .as_ref()
                .map(|m| m.requests)
                .unwrap_or(0);
            finish_run(
                args,
                method,
                iteration,
                head,
                head_hash,
                from,
                to,
                elapsed_ms,
                outcome.pools,
                capability,
                quota,
                http_requests,
                outcome.rpc_metrics,
                &logs,
                parse_failed_windows(&logs),
            )
        }
        Err(e) => finish_run(
            args,
            method,
            iteration,
            head,
            head_hash,
            from,
            to,
            elapsed_ms,
            Vec::new(),
            capability,
            quota,
            0,
            None,
            &logs,
            parse_failed_windows(&logs),
        )
        .with_reason(format!("{e:#}")),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_poll_receipts(
    args: &Args,
    method: Method,
    iteration: usize,
    head: u64,
    head_hash: &str,
    from: u64,
    to: u64,
    factories: &ResolvedFactories,
    capability: Capability,
    endpoint: &str,
    quota: &Accounting,
) -> BenchRun {
    let started = Instant::now();
    let mut logs = Vec::new();
    logs.push("poll path: historical replay of newly observed heads, batch_blocks=1".to_string());
    let mut pools: Vec<DiscoveredPool> = Vec::new();
    let mut failed_windows = Vec::new();
    let mut requests = 0u64;

    let rpc = match mev_scout_core::rpc::RpcClient::from_urls(&[endpoint], CHAIN_ID) {
        Ok(rpc) => rpc,
        Err(e) => {
            return failed_run(
                args,
                method,
                iteration,
                head,
                head_hash,
                from,
                to,
                capability,
                quota,
                format!("{e:#}"),
            )
        }
    };
    rpc.with_provider_rps(&[rps_for(args, method.provider)])
        .await;
    let db_path = format!(
        "{}/cache/stage1-{}-{from}-{to}-run{iteration}-poll.sqlite",
        args.out_dir, method.name
    );
    let cache = match SqliteStore::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            return failed_run(
                args,
                method,
                iteration,
                head,
                head_hash,
                from,
                to,
                capability,
                quota,
                format!("{e:#}"),
            )
        }
    };
    let token_cache = TokenCache::warm(CHAIN_ID);
    let disc_config = discovery_config(factories, 1, 1, Some(&token_cache), Some(&cache));

    for block in from..=to {
        requests += 1;
        let result = discover_pools_from_receipts(
            &rpc,
            &cache,
            block,
            block,
            &disc_config,
            ReceiptScanOptions {
                batch_blocks: 1,
                cache_blocks: false,
                provider_idx: Some(0),
                mixed_recent: None,
                concurrency: 1,
                on_window: None,
            },
        )
        .await;
        match result {
            Ok(scan) => {
                pools.extend(scan.pools);
                failed_windows.extend(scan.failed_windows);
            }
            Err(e) => {
                failed_windows.push((block, block));
                logs.push(format!("poll receipt {block} failed: {e:#}"));
            }
        }
    }

    let elapsed_ms = started.elapsed().as_millis();
    let rpc_snapshot = rpc.metrics_snapshot();
    finish_run(
        args,
        method,
        iteration,
        head,
        head_hash,
        from,
        to,
        elapsed_ms,
        pools,
        capability,
        quota,
        requests.max(rpc_snapshot.requests),
        Some(rpc_snapshot),
        &logs,
        failed_windows,
    )
}

fn parse_failed_windows(logs: &[String]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for line in logs {
        let lower = line.to_ascii_lowercase();
        let Some(pos) = lower.find("failed windows [") else {
            continue;
        };
        let rest = &line[pos + "failed windows ".len()..];
        let Some(start) = rest.find('[') else {
            continue;
        };
        let Some(end) = rest.find(']') else { continue };
        for pair in rest[start + 1..end].split("), (") {
            let cleaned = pair.trim_matches(|c| c == '(' || c == ')' || c == ' ');
            let mut parts = cleaned.split(',');
            if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<u64>(), b.trim().parse::<u64>()) {
                    out.push((a, b));
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn finish_run(
    args: &Args,
    method: Method,
    iteration: usize,
    head: u64,
    head_hash: &str,
    from: u64,
    to: u64,
    elapsed_ms: u128,
    pools: Vec<DiscoveredPool>,
    capability: Capability,
    quota: &Accounting,
    http_requests: u64,
    rpc: Option<RpcMetricsSnapshot>,
    logs: &[String],
    failed_windows: Vec<(u64, u64)>,
) -> BenchRun {
    let (json_rpc_calls, calls_inside_batches) = match method.kind {
        Kind::FilteredLogs => (http_requests, 0),
        Kind::Receipts | Kind::PollReceipts => (
            http_requests.saturating_mul(2),
            http_requests.saturating_mul(2),
        ),
    };
    let mut accounting = quota.clone();
    accounting.http_requests = http_requests;
    accounting.json_rpc_calls = json_rpc_calls;
    accounting.json_rpc_calls_inside_batches = calls_inside_batches;
    accounting.response_bytes = rpc.as_ref().filter(|m| m.bytes > 0).map(|m| m.bytes);
    accounting.endpoint = redact_url(accounting.endpoint.as_str());
    let blocks = to - from + 1;
    let blocks_per_sec = if elapsed_ms > 0 {
        blocks as f64 / (elapsed_ms as f64 / 1_000.0)
    } else {
        0.0
    };
    let dex_families = pools
        .iter()
        .map(|p| p.dex_type.to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let addresses = pools.iter().map(|p| format!("{:?}", p.address)).collect();
    let mut errors = ErrorCounts::from_logs(logs);
    errors.failed_window_count = errors.failed_window_count.max(failed_windows.len());
    BenchRun {
        method: method.name.to_string(),
        kind: method.kind.label(),
        provider: method.provider.label(),
        range: args.range.clone(),
        iteration,
        status: "ok".to_string(),
        reason: None,
        source: method.kind.label().to_string(),
        endpoint: redact_url(quota.endpoint.as_str()),
        canonical_head_number: head,
        canonical_head_hash: head_hash.to_string(),
        from_block: from,
        to_block: to,
        blocks,
        elapsed_ms,
        blocks_per_sec,
        pools_total: pools.len(),
        dex_families,
        addresses,
        capability,
        accounting,
        rpc,
        errors,
        failed_windows,
        missed_vs_filtered_logs: None,
        logs: logs.to_vec(),
    }
}

#[allow(clippy::too_many_arguments)]
fn failed_run(
    args: &Args,
    method: Method,
    iteration: usize,
    head: u64,
    head_hash: &str,
    from: u64,
    to: u64,
    capability: Capability,
    quota: &Accounting,
    reason: String,
) -> BenchRun {
    BenchRun {
        method: method.name.to_string(),
        kind: method.kind.label(),
        provider: method.provider.label(),
        range: args.range.clone(),
        iteration,
        status: "failed".to_string(),
        reason: Some(reason),
        source: method.kind.label().to_string(),
        endpoint: redact_url(quota.endpoint.as_str()),
        canonical_head_number: head,
        canonical_head_hash: head_hash.to_string(),
        from_block: from,
        to_block: to,
        blocks: to.saturating_sub(from) + 1,
        elapsed_ms: 0,
        blocks_per_sec: 0.0,
        pools_total: 0,
        dex_families: Vec::new(),
        addresses: Vec::new(),
        capability,
        accounting: Accounting {
            endpoint: redact_url(quota.endpoint.as_str()),
            ..quota.clone()
        },
        rpc: None,
        errors: ErrorCounts::default(),
        failed_windows: Vec::new(),
        missed_vs_filtered_logs: None,
        logs: Vec::new(),
    }
}

impl BenchRun {
    fn with_reason(mut self, reason: String) -> Self {
        self.status = "failed".to_string();
        self.reason = Some(reason);
        self
    }
}

async fn run_once(
    args: &Args,
    method: Method,
    iteration: usize,
    head: u64,
    head_hash: &str,
    chain_config: &ChainConfig,
) -> anyhow::Result<BenchRun> {
    let endpoint = endpoint_for(args, method.provider)?;
    let factories = resolved_factories(chain_config);
    let (capability, _) = probe_capabilities(args, method, head, &factories).await?;
    let (from, to) = range_bounds(args, head)?;
    let quota = Accounting {
        provider: method.provider.label(),
        endpoint: endpoint.clone(),
        quota_tier: args.quota_tier.clone(),
        credits_used: args.credits_used,
        rps_limit: rps_for(args, method.provider),
        log_filter_chunks: 0,
        http_requests: 0,
        json_rpc_calls: 0,
        json_rpc_calls_inside_batches: 0,
        response_bytes: None,
        batch_accounting_note:
            ("RpcClient rate limit is applied per HTTP POST. Verify whether the provider counts \
             individual JSON-RPC calls inside batches; 20 RPS is not assumed to mean 20 HTTP \
             batch requests/sec.")
                .to_string(),
    };

    Ok(match method.kind {
        Kind::FilteredLogs => {
            run_filtered_logs(
                args, method, iteration, head, head_hash, from, to, &factories, capability,
                &endpoint, &quota,
            )
            .await
        }
        Kind::Receipts => {
            run_receipts(
                args, method, iteration, head, head_hash, from, to, capability, &endpoint, &quota,
            )
            .await
        }
        Kind::PollReceipts => {
            run_poll_receipts(
                args, method, iteration, head, head_hash, from, to, &factories, capability,
                &endpoint, &quota,
            )
            .await
        }
    })
}

fn attach_comparisons(runs: &mut [BenchRun]) {
    let mut by_range: std::collections::BTreeMap<String, HashSet<String>> =
        std::collections::BTreeMap::new();
    for run in runs.iter() {
        if run.kind == Kind::FilteredLogs.label() && run.status == "ok" {
            by_range
                .entry(run.range.clone())
                .or_default()
                .extend(run.addresses.iter().cloned());
        }
    }
    for run in runs.iter_mut() {
        let Some(reference) = by_range.get(&run.range) else {
            continue;
        };
        if reference.is_empty() || run.kind == Kind::FilteredLogs.label() {
            continue;
        }
        let found: HashSet<String> = run.addresses.iter().cloned().collect();
        let missed: Vec<String> = reference.difference(&found).cloned().collect();
        let missed_rate = missed.len() as f64 / reference.len() as f64;
        run.missed_vs_filtered_logs = Some(MissedComparison {
            reference: "union of filtered eth_getLogs runs".to_string(),
            reference_pools: reference.len(),
            missed_pools: missed.len(),
            missed_rate,
            missed_addresses: missed,
        });
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let methods = methods_for(&args)?;
    std::fs::create_dir_all(&args.out_dir)?;
    let chain_config = chain_config(&args)?;

    let (head, head_hash) = match args.head {
        Some(h) => (h, "provided by --head".to_string()),
        None => freeze_head(&args).await?,
    };
    eprintln!("Stage 1 canonical head: {head} ({head_hash})");

    let mut runs = Vec::new();
    for method in &methods {
        for iteration in 1..=args.repeat {
            eprintln!(
                "== {} {} run {iteration}/{} ==",
                method.name, args.range, args.repeat
            );
            runs.push(run_once(&args, *method, iteration, head, &head_hash, &chain_config).await?);
        }
    }
    attach_comparisons(&mut runs);

    let file = BenchFile {
        method: args.method.clone(),
        range: args.range.clone(),
        repeats: args.repeat,
        median_elapsed_ms: median(&runs.iter().map(|r| r.elapsed_ms).collect::<Vec<_>>()),
        canonical_head_number: head,
        canonical_head_hash: head_hash,
        accounting_note:
            ("Credits/units are provider-accounting inputs, not client-observable values. \
             Pass --credits-used and --quota-tier from the provider dashboard.")
                .to_string(),
        runs,
    };
    let out_path = format!(
        "{}/stage1-{}-{}.json",
        args.out_dir, args.method, args.range
    );
    std::fs::write(&out_path, serde_json::to_string_pretty(&file)?)?;

    println!("Stage 1 benchmark written to {out_path}\n");
    println!("| method | status | pools | blocks/s | time_ms | rpc POSTs | failed windows |");
    println!("|---|---|---:|---:|---:|---:|---:|");
    for run in &file.runs {
        println!(
            "| {} | {} | {} | {:.2} | {} | {} | {} |",
            run.method,
            run.status,
            run.pools_total,
            run.blocks_per_sec,
            run.elapsed_ms,
            run.accounting.http_requests,
            run.failed_windows.len(),
        );
    }
    Ok(())
}
