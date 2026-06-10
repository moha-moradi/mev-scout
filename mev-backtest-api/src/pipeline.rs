//! Async simulation pipeline — the central execution engine for the API server.
//!
//! This module bridges the HTTP API to the core backtest engine. It runs the
//! full 6-stage pipeline (RPC fetch → tx filter → EVM replay → opportunity
//! scan → profitability check → aggregation) as a background tokio task,
//! streaming progress and results to connected clients via SSE.
//!
//! Key design decisions:
//! - Stages 0-2 run async (RPC, fetching, pool init)
//! - Stage 3 (opportunity scan) runs sync inside `block_in_place` because
//!   `BacktestRunner::run_range()` is synchronous and CPU-bound
//! - SSE events are emitted via `broadcast::channel` so multiple clients
//!   can subscribe to the same run's status

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, RwLock};
use tracing::info;

use mev_backtest_core::aggregate;
use mev_backtest_core::cache::CacheStore;
use mev_backtest_core::config::ChainConfig;
use mev_backtest_core::fetch::Fetcher;
use mev_backtest_core::pool::state::PoolManager;
use mev_backtest_core::replay::BlockReplayer;
use mev_backtest_core::resolver::RangeResolver;
use mev_backtest_core::rpc::RpcClient;
use mev_backtest_core::run::BacktestRunner;
use mev_backtest_core::types::{ChainName, GasConfig, RangeMode, Strategy};

use crate::state::{
    LogEntry, RunResult, RunState, RunStatus, SseEvent, StageStatus,
};

pub struct PipelineParams {
    pub chain: String,
    pub rpc_url: String,
    pub range_mode: RangeMode,
    pub strategies: Vec<Strategy>,
    pub flash_loan_provider: String,
    pub gas_model: String,
    pub priority_fee_gwei: f64,
    pub gas_limit: u64,
    pub cache_dir: String,
}

/// Run the full 6-stage MEV backtest pipeline for a single simulation request.
///
/// This function is spawned as a background tokio task by the API route
/// handler. It owns the execution for the lifetime of the run, updating
/// `run_state` and emitting SSE events via `sse_tx` at each stage boundary.
///
/// # Stages
/// 0. RPC fetch — connect, verify chain, resolve block range
/// 1. TX filter — fetch blocks, cache with integrity check
/// 2. REVM replay — init pools, build replayer
/// 3. Opportunity scan — per-block replay + detection (runs sync)
/// 4. Profitability — filter `expected_profit > 0`
/// 5. Aggregation — compute metrics, save results to disk
///
/// On success, emits a `complete` SSE event with the run duration.
/// On failure at any stage, emits an `error` SSE event and sets the run
/// status to `Error`. The run state and results remain accessible via the
/// API for debugging.
pub async fn run_pipeline(
    params: PipelineParams,
    chain_config: ChainConfig,
    run_id: String,
    sse_tx: broadcast::Sender<SseEvent>,
    run_state: Arc<RwLock<RunState>>,
    results_dir: String,
) {
    let start_time = Instant::now();

    {
        if let Ok(mut s) = run_state.try_write() {
            s.status = RunStatus::Running;
        }
    }

    // Stage 0: RPC FETCH
    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[0].status = StageStatus::Running;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_start".into(),
        data: serde_json::json!({"stage": 0, "id": "rpc_fetch", "label": "RPC FETCH", "sub": format!("Connecting to {}", params.chain)}),
    });

    let rpc = match RpcClient::new(&params.rpc_url, chain_config.chain_id) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Failed to create RPC client: {}", e);
            pipeline_error(&run_state, &sse_tx, 0, "rpc_fetch", &msg).await;
            return;
        }
    };

    match rpc.check_connection(chain_config.chain_id).await {
        Ok(_) => pipeline_log(&run_state, &sse_tx, "RPC", &format!("Connected to {} (chain {})", params.chain, chain_config.chain_id)).await,
        Err(e) => {
            let msg = format!("RPC connection failed: {}", e);
            pipeline_error(&run_state, &sse_tx, 0, "rpc_fetch", &msg).await;
            return;
        }
    }

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = match resolver.resolve(&params.range_mode).await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Range resolution failed: {}", e);
            pipeline_error(&run_state, &sse_tx, 0, "rpc_fetch", &msg).await;
            return;
        }
    };

    pipeline_log(&run_state, &sse_tx, "RPC", &format!("Resolved range: blocks {}–{} ({} blocks)", resolved.start_block, resolved.end_block, resolved.block_count)).await;

    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[0].status = StageStatus::Completed;
            s.blocks_total = resolved.block_count;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_end".into(),
        data: serde_json::json!({"stage": 0, "id": "rpc_fetch", "result": "OK"}),
    });

    // Stage 1: TX FILTER (fetch blocks, cache-first)
    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[1].status = StageStatus::Running;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_start".into(),
        data: serde_json::json!({"stage": 1, "id": "tx_filter", "label": "TX FILTER"}),
    });

    let cache = match CacheStore::open(&params.cache_dir, chain_config.chain_id) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Failed to open cache: {}", e);
            pipeline_error(&run_state, &sse_tx, 1, "tx_filter", &msg).await;
            return;
        }
    };

    let progress_fn = || {};
    let fetcher = Fetcher::new(rpc.clone(), cache.clone());
    match fetcher.fetch_range(&resolved, Some(&progress_fn)).await {
        Ok(summary) => {
            pipeline_log(&run_state, &sse_tx, "FETCH", &format!("Fetched: {} total, {} new, {} cached (chain {})", summary.total_blocks, summary.fetched, summary.cached, chain_config.chain_id)).await;
            if !summary.missing_after_fetch.is_empty() {
                pipeline_log(&run_state, &sse_tx, "FETCH", &format!("{} blocks missing after fetch, auto-refetching...", summary.missing_after_fetch.len())).await;
                let refetch = Fetcher::new(rpc.clone(), cache.clone());
                if let Err(e) = refetch.auto_refetch_gaps(&summary.missing_after_fetch).await {
                    pipeline_log(&run_state, &sse_tx, "FETCH", &format!("Refetch error: {}", e)).await;
                }
            }
        }
        Err(e) => {
            let msg = format!("Fetch failed: {}", e);
            pipeline_error(&run_state, &sse_tx, 1, "tx_filter", &msg).await;
            return;
        }
    }

    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[1].status = StageStatus::Completed;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_end".into(),
        data: serde_json::json!({"stage": 1, "id": "tx_filter", "result": "OK"}),
    });

    // Stage 2: REVM REPLAY (init pools + build replayer)
    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[2].status = StageStatus::Running;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_start".into(),
        data: serde_json::json!({"stage": 2, "id": "revm_replay", "label": "REVM REPLAY"}),
    });

    let mut pool_manager = PoolManager::new();
    let prev_block = resolved.start_block.saturating_sub(1);
    let registry_path = chain_config.pools_registry_path.as_deref();

    if !params.strategies.is_empty() {
        BacktestRunner::init_pools(
            &mut pool_manager,
            registry_path,
            &rpc,
            prev_block,
            Some(&cache),
        ).await;
    }

    pipeline_log(&run_state, &sse_tx, "POOLS", &format!("Loaded {} pools", pool_manager.pool_count())).await;

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache,
        rpc.clone(),
        chain_config.chain_id,
    );

    let gas_model = match params.gas_model.parse() {
        Ok(m) => m,
        Err(_) => {
            pipeline_log(&run_state, &sse_tx, "GAS", &format!("Unknown gas model '{}', using historical_exact", params.gas_model)).await;
            mev_backtest_core::types::GasModel::HistoricalExact
        }
    };

    let gas_config = GasConfig {
        gas_limit: params.gas_limit,
        gas_model,
        priority_fee_gwei: params.priority_fee_gwei,
    };

    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config);

    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[2].status = StageStatus::Completed;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_end".into(),
        data: serde_json::json!({"stage": 2, "id": "revm_replay", "result": "OK"}),
    });

    // Stage 3: OPPORTUNITY SCAN (per-block synchronous loop)
    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[3].status = StageStatus::Running;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_start".into(),
        data: serde_json::json!({"stage": 3, "id": "opportunity_scan", "label": "OPPORTUNITY SCAN"}),
    });

    let sse_scan = sse_tx.clone();
    let state_scan = run_state.clone();
    let block_count = resolved.block_count;
    let start_block = resolved.start_block;
    let end_block = resolved.end_block;

    let all_opportunities = tokio::task::block_in_place(move || {
        let mut all = Vec::new();
        for block_num in start_block..=end_block {
            match runner.run_block(block_num) {
                Ok(opps) => {
                    let count = opps.len();
                    if count > 0 {
                        info!("Block {}: {} opportunities", block_num, count);
                    }
                    all.extend(opps);

                    if let Ok(mut s) = state_scan.try_write() {
                        s.blocks_processed += 1;
                        s.progress = (s.blocks_processed as f64 / block_count as f64) * 100.0;
                    }
                    let _ = sse_scan.send(SseEvent {
                        event_type: "progress".into(),
                        data: serde_json::json!({
                            "stage": 3,
                            "block": block_num,
                            "blocks_processed": all.len(),
                            "total_blocks": block_count,
                        }),
                    });
                }
                Err(e) => {
                    info!("Block {} failed: {}", block_num, e);
                    if let Ok(mut s) = state_scan.try_write() {
                        s.blocks_processed += 1;
                        s.progress = (s.blocks_processed as f64 / block_count as f64) * 100.0;
                        s.logs.push(LogEntry {
                            ts: chrono::Utc::now().format("%H:%M:%S%.3f").to_string(),
                            tag: "BLOCK".into(),
                            text: format!("Block {} error: {}", block_num, e),
                        });
                    }
                }
            }
        }
        all
    });

    pipeline_log(&run_state, &sse_tx, "SCAN", &format!("Scanned {} blocks, found {} opportunities", block_count, all_opportunities.len())).await;

    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[3].status = StageStatus::Completed;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_end".into(),
        data: serde_json::json!({"stage": 3, "id": "opportunity_scan", "result": format!("{} opportunities", all_opportunities.len())}),
    });

    // Stage 4: PROFITABILITY CHECK
    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[4].status = StageStatus::Running;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_start".into(),
        data: serde_json::json!({"stage": 4, "id": "profitability", "label": "PROFITABILITY CHECK"}),
    });

    let profitable: Vec<_> = all_opportunities
        .into_iter()
        .filter(|o| o.expected_profit > 0)
        .collect();

    let skipped_count = resolved.block_count as usize - profitable.len();

    pipeline_log(&run_state, &sse_tx, "PROFIT", &format!("{} profitable opportunities (skipped {} non-profitable)", profitable.len(), skipped_count)).await;

    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[4].status = StageStatus::Completed;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_end".into(),
        data: serde_json::json!({"stage": 4, "id": "profitability", "result": format!("{} profitable", profitable.len())}),
    });

    // Stage 5: AGGREGATION + PERSISTENCE
    {
        if let Ok(mut s) = run_state.try_write() {
            s.stages[5].status = StageStatus::Running;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_start".into(),
        data: serde_json::json!({"stage": 5, "id": "aggregation", "label": "AGGREGATION"}),
    });

    let api_key = run_state
        .try_read()
        .ok()
        .and_then(|s| s.config.coingecko_api_key.clone());
    let mut price_cache = mev_backtest_core::coingecko::PriceCache::new(api_key);
    let chain_name: ChainName = params.chain.parse().ok().unwrap_or(ChainName::Ethereum);
    let usd_price = price_cache.usd_price(chain_name).await.unwrap_or(0.0);

    let agg = aggregate::aggregate(&profitable, &[], usd_price);
    let ui_opportunities = crate::mapping::map_opportunities(&profitable, usd_price);

    let elapsed = start_time.elapsed();
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let run_result = RunResult {
        run_id: run_id.clone(),
        chain: params.chain.clone(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        range_mode: resolved.mode_string(),
        strategies: params.strategies.iter().map(|s| s.to_string()).collect(),
        opportunities: ui_opportunities,
        summary: Some(agg.summary),
        by_strategy: Some(agg.by_strategy),
        by_dex: Some(agg.by_dex),
        duration_ms: elapsed.as_millis() as u64,
        created_at: now_secs,
    };

    // Save to disk
    let results_path = std::path::Path::new(&results_dir);
    if let Err(e) = std::fs::create_dir_all(results_path) {
        pipeline_log(&run_state, &sse_tx, "SAVE", &format!("Failed to create results dir: {}", e)).await;
    } else {
        let file_path = results_path.join(format!("{}.json", run_id));
        match serde_json::to_string_pretty(&run_result) {
            Ok(json) => {
                match std::fs::write(&file_path, &json) {
                    Ok(_) => pipeline_log(&run_state, &sse_tx, "SAVE", &format!("Results saved to {}", file_path.display())).await,
                    Err(e) => pipeline_log(&run_state, &sse_tx, "SAVE", &format!("Failed to write results: {}", e)).await,
                }
            }
            Err(e) => pipeline_log(&run_state, &sse_tx, "SAVE", &format!("Failed to serialize results: {}", e)).await,
        }
    }

    {
        if let Ok(mut s) = run_state.try_write() {
            s.status = RunStatus::Done;
            s.result = Some(run_result);
            s.elapsed_ms = elapsed.as_millis() as u64;
            s.stages[5].status = StageStatus::Completed;
        }
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "stage_end".into(),
        data: serde_json::json!({"stage": 5, "id": "aggregation", "result": "OK"}),
    });
    let _ = sse_tx.send(SseEvent {
        event_type: "complete".into(),
        data: serde_json::json!({"run_id": run_id, "duration_ms": elapsed.as_millis() as u64}),
    });
}

/// Send a log entry to both the run state (for API polling) and the SSE
/// stream (for live status subscribers).
///
/// Logs are tagged with a category (e.g., "RPC", "FETCH", "SCAN") for
/// filtering in the frontend.
async fn pipeline_log(
    run_state: &Arc<RwLock<RunState>>,
    sse_tx: &broadcast::Sender<SseEvent>,
    tag: &str,
    text: &str,
) {
    let entry = LogEntry {
        ts: chrono::Utc::now().format("%H:%M:%S%.3f").to_string(),
        tag: tag.to_string(),
        text: text.to_string(),
    };
    if let Ok(mut s) = run_state.try_write() {
        s.logs.push(entry.clone());
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "log".into(),
        data: serde_json::json!({"ts": entry.ts, "tag": entry.tag, "text": entry.text}),
    });
}

/// Transition the run to an error state and emit a terminal `error` SSE event.
///
/// This is called when any pipeline stage fails irrecoverably (e.g., RPC
/// connection failure, fetch error). The run status is set to `Error` with
/// the message, the failed stage is marked `Failed`, and the error is
/// broadcast to all SSE subscribers.
async fn pipeline_error(
    run_state: &Arc<RwLock<RunState>>,
    sse_tx: &broadcast::Sender<SseEvent>,
    stage: usize,
    stage_id: &str,
    message: &str,
) {
    if let Ok(mut s) = run_state.try_write() {
        s.status = RunStatus::Error(message.to_string());
        s.stages[stage].status = StageStatus::Failed;
        s.logs.push(LogEntry {
            ts: chrono::Utc::now().format("%H:%M:%S%.3f").to_string(),
            tag: "ERROR".to_string(),
            text: message.to_string(),
        });
    }
    let _ = sse_tx.send(SseEvent {
        event_type: "error".into(),
        data: serde_json::json!({"stage": stage, "id": stage_id, "error": message}),
    });
}
