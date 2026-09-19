use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use anyhow::Context;

use crate::cache::{RunManifest, SqliteStore};
use crate::config::validation;
use crate::config::Config;
use crate::explorer::results::{persist_opportunities_to_explorer, persist_rejections_to_explorer};
use crate::fetch::Fetcher;
use crate::pipeline::{BacktestRunner, BlockReplayStats};
use crate::pool::state::PoolManager;
use crate::progress::{JobProgress, ProgressEvent};
use crate::replay::BlockReplayer;
use crate::resolver::RangeResolver;
use crate::types::{GasConfig, MevOpportunity, ResultsFile};
use crate::utils::epoch_secs;

use super::rpc::init_rpc;

#[derive(Debug, Clone, Default)]
pub struct RunOpts {
    pub batch_rpc: bool,
    pub record_rejections: bool,
}

pub struct RunOutcome {
    pub run_id: String,
    pub opportunities: Vec<MevOpportunity>,
    pub block_stats: Vec<BlockReplayStats>,
    pub elapsed: Duration,
}

pub async fn job_run(
    config: &Config,
    opts: &RunOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<RunOutcome> {
    let validation_result =
        validation::validate_and_resolve(config).context("invalid configuration")?;

    let setup = init_rpc(config, validation_result.chain_name, true).await?;
    let provider_configs = setup.provider_configs;
    let rpc = setup.rpc;
    let cache = SqliteStore::open(config.effective_db_path(&validation_result.chain_name))?;

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = resolver.resolve(&validation_result.range_mode).await?;

    let run_id = format!("run_{}", epoch_secs());

    let manifest = RunManifest {
        run_id: run_id.clone(),
        chain: validation_result.chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        resolved_at: epoch_secs(),
        range_mode: resolved.mode_string(),
        strategies: validation_result
            .strategies
            .iter()
            .map(|s| s.to_string())
            .collect(),
        flash_loan_provider: validation_result.flash_loan_provider.to_string(),
    };
    cache.put_manifest(&manifest)?;

    progress.log(&format!("Run ID: {run_id}"));
    progress.emit(ProgressEvent::stage("resolve"));
    progress.log(&resolved.summary());

    let pool_addresses: Vec<Address> = cache
        .list_discovered_pools()
        .unwrap_or_default()
        .iter()
        .map(|p| p.address)
        .collect();

    let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
    fetcher = fetcher.with_parallelism(provider_configs.len());
    fetcher = fetcher.with_batch_rpc(opts.batch_rpc);
    let bc = config.effective_block_concurrency(validation_result.chain_name, &provider_configs);
    fetcher = fetcher.with_block_concurrency(bc);

    let fetch_total = resolved.block_count;
    let fetch_done = Arc::new(AtomicU64::new(0));
    let tick = move || {
        if progress.cancelled() {
            return false;
        }
        let d = fetch_done.fetch_add(1, Ordering::Relaxed) + 1;
        progress.emit(ProgressEvent {
            stage: "fetch".to_string(),
            done: Some(d),
            total: Some(fetch_total),
            run_id: None,
            ops: None,
            elapsed_ms: None,
        });
        true
    };

    let fetch_summary = if !pool_addresses.is_empty() {
        fetcher
            .fetch_relevant(&resolved, &pool_addresses, Some(&tick))
            .await?
    } else {
        fetcher.fetch_range(&resolved, Some(&tick)).await?
    };

    if !fetch_summary.missing_after_fetch.is_empty() {
        let refetched = fetcher
            .auto_refetch_gaps(&fetch_summary.missing_after_fetch)
            .await?;
        progress.log(&format!("Refetched {} missing blocks", refetched));
    }

    let mut pool_manager = PoolManager::new();
    pool_manager.set_max_pairs_per_token(config.backtest.max_pairs_per_token);
    pool_manager.set_concurrency_limit(provider_configs.len() as u32);
    if let Some(vault_addr) = validation_result.chain_config.balancer_vault {
        pool_manager = pool_manager.with_balancer_vault(vault_addr);
    }
    if let Some(native_addr) = validation_result.chain_config.wrapped_native_token {
        pool_manager = pool_manager.with_wrapped_native(native_addr);
    }
    let prev_block = resolved.start_block.saturating_sub(1);

    if !validation_result.strategies.is_empty() {
        if progress.cancelled() {
            anyhow::bail!("job cancelled");
        }
        progress.emit(ProgressEvent::stage("pool_init"));
        BacktestRunner::init_pools(&mut pool_manager, &rpc, prev_block, Some(&cache)).await;
    }

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache,
        rpc.clone(),
        validation_result.chain_config.chain_id,
    );

    let gas_config = GasConfig {
        gas_limit: config.gas.gas_limit,
        gas_model: validation_result.gas_model,
        priority_fee_gwei: config.gas.priority_fee_gwei,
        flash_loan_provider: validation_result.flash_loan_provider,
        winning_bid_premium: 0.0,
        percentile_gas_price: None,
        calibration: Default::default(),
    };
    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
        .with_proximity_window(config.backtest.proximity_window)
        .with_capture_pending(config.backtest.capture_pending)
        .with_min_profit_wei(config.backtest.min_profit_wei)
        .with_max_candidates_per_tx(config.backtest.max_candidates_per_tx)
        .with_record_rejections(opts.record_rejections);

    if let Some(aave_pool) = validation_result.chain_config.aave_v3_pool {
        runner.prefetch_aave_reserves(aave_pool, prev_block).await;
    }

    let start = Instant::now();

    let detect_progress = move |done: u64, total: u64| {
        if progress.cancelled() {
            return false;
        }
        progress.emit(ProgressEvent {
            stage: "detect".to_string(),
            done: Some(done),
            total: Some(total),
            run_id: None,
            ops: None,
            elapsed_ms: None,
        });
        true
    };
    let (all_opportunities, block_stats) = runner.run_range(&resolved, Some(&detect_progress))?;
    let elapsed = start.elapsed();

    progress.emit(ProgressEvent {
        stage: "complete".to_string(),
        done: None,
        total: None,
        run_id: Some(run_id.clone()),
        ops: Some(all_opportunities.len() as u64),
        elapsed_ms: Some(elapsed.as_millis() as u64),
    });

    let results_file = ResultsFile {
        run_id: run_id.clone(),
        chain: validation_result.chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        range_mode: resolved.mode_string(),
        strategies: manifest.strategies.clone(),
        flash_loan_provider: manifest.flash_loan_provider.clone(),
        resolved_at: manifest.resolved_at,
        created_at: epoch_secs(),
        opportunities: all_opportunities.clone(),
    };
    persist_opportunities_to_explorer(config, validation_result.chain_name, &run_id, &results_file);
    let rejections = runner.take_rejections();
    persist_rejections_to_explorer(config, validation_result.chain_name, &run_id, &rejections);

    if all_opportunities.is_empty() {
        progress.log("No MEV opportunities detected in the specified range.");
    } else {
        progress.log(&format!(
            "Detected {} MEV opportunity(ies) in {:.2}s",
            all_opportunities.len(),
            elapsed.as_secs_f64()
        ));
    }

    let blocks_scanned = block_stats.len();
    let mempool_opps: usize = block_stats.iter().map(|s| s.mempool_opp_count).sum();
    progress.log(&format!(
        "  blocks scanned: {blocks_scanned}, mempool-only opportunities: {mempool_opps}"
    ));

    Ok(RunOutcome {
        run_id,
        opportunities: all_opportunities,
        block_stats,
        elapsed,
    })
}
