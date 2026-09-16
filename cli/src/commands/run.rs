use anyhow::Context;
use mev_scout_core::utils::epoch_secs;

use alloy::primitives::Address;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cli::RunArgs;
use crate::display::{
    persist_opportunities_to_explorer, persist_rejections_to_explorer, print_startup_plan,
    render_block_summary_table, render_results_table,
};
use crate::job_progress::{JobProgress, ProgressEvent};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::{RunManifest, SqliteStore};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::types::{GasConfig, ResultsFile};

pub async fn cmd_run(
    config: &Config,
    args: &RunArgs,
    progress: &dyn JobProgress,
) -> anyhow::Result<()> {
    if let Some(format) = args.progress.as_deref() {
        if format != "json" {
            anyhow::bail!("unsupported --progress format '{format}' (only 'json')");
        }
    }
    let validation_result =
        validation::validate_and_resolve(config).context("invalid configuration")?;
    print_startup_plan(&validation_result, config);

    let setup = init_rpc(config, validation_result.chain_name, true).await?;
    let provider_configs = setup.provider_configs;
    let rpc = setup.rpc;
    let cache = SqliteStore::open(config.effective_db_path(&validation_result.chain_name))?;

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = match resolver.resolve(&validation_result.range_mode).await {
        Ok(r) => r,
        Err(e) => anyhow::bail!("{e}"),
    };

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
    progress.log("");

    let pool_addresses: Vec<Address> = cache
        .list_discovered_pools()
        .unwrap_or_default()
        .iter()
        .map(|p| p.address)
        .collect();

    if !pool_addresses.is_empty() {
        tracing::info!(
            "Using log-first fetch with {} known pool addresses",
            pool_addresses.len()
        );
    } else {
        tracing::info!("No known pool addresses, fetching all blocks");
    }

    let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
    fetcher = fetcher.with_parallelism(provider_configs.len());
    fetcher = fetcher.with_batch_rpc(args.batch_rpc);
    let bc = config.effective_block_concurrency(&provider_configs);
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

    if fetch_summary.skipped > 0 {
        tracing::info!(
            "Fetch optimization: skipped {} blocks with no DEX activity (fetched {} of {} scanned)",
            fetch_summary.skipped,
            fetch_summary.fetched,
            fetch_summary.scanned,
        );
    }

    if !fetch_summary.missing_after_fetch.is_empty() {
        tracing::warn!(
            "{} blocks missing after fetch, auto-refetching...",
            fetch_summary.missing_after_fetch.len()
        );
        let refetched = fetcher
            .auto_refetch_gaps(&fetch_summary.missing_after_fetch)
            .await?;
        tracing::info!("Refetched {} blocks", refetched);
    }

    let mut pool_manager = PoolManager::new();
    pool_manager.set_max_pairs_per_token(config.backtest.max_pairs_per_token);
    pool_manager.set_concurrency_limit(provider_configs.len() as u32);
    if let Some(vault_str) = &validation_result.chain_config.balancer_vault {
        if let Ok(vault_addr) = vault_str.parse::<Address>() {
            pool_manager = pool_manager.with_balancer_vault(vault_addr);
        }
    }
    if let Some(native_str) = &validation_result.chain_config.wrapped_native_token {
        if let Ok(native_addr) = native_str.parse::<Address>() {
            pool_manager = pool_manager.with_wrapped_native(native_addr);
        }
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
        .with_record_rejections(args.record_rejections);

    if let Some(aave_pool_str) = &validation_result.chain_config.aave_v3_pool {
        if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
            runner
                .prefetch_aave_reserves(aave_pool, resolved.start_block.saturating_sub(1))
                .await;
        }
    }

    let start = std::time::Instant::now();

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
    let (all_opportunities, block_stats) =
        runner.run_range(&resolved, Some(&detect_progress))?;
    let elapsed = start.elapsed();

    progress.emit(ProgressEvent {
        stage: "complete".to_string(),
        done: None,
        total: None,
        run_id: Some(run_id.clone()),
        ops: Some(all_opportunities.len() as u64),
        elapsed_ms: Some(elapsed.as_millis() as u64),
    });

    // Execution history lives only in SQLite: run metadata in the cache
    // store's `run_manifests`, opportunities/rejections in the explorer store.
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

    // Results layer: persist into the explorer opportunities table.
    persist_opportunities_to_explorer(config, validation_result.chain_name, &run_id, &results_file);
    // Rejection capture when --record-rejections.
    let rejections = runner.take_rejections();
    persist_rejections_to_explorer(config, validation_result.chain_name, &run_id, &rejections);

    if all_opportunities.is_empty() {
        progress.log("No MEV opportunities detected in the specified range.");
    } else {
        progress.log(&format!(
            "\nDetected {} MEV opportunity(ies) in {:.2}s:\n",
            all_opportunities.len(),
            elapsed.as_secs_f64()
        ));
        render_results_table(&all_opportunities, Some(runner.pool_manager()));
    }

    render_block_summary_table(&block_stats);

    let mempool_opps: usize = block_stats.iter().map(|s| s.mempool_opp_count).sum();
    if mempool_opps > 0 {
        let mempool_txs: usize = block_stats.iter().map(|s| s.pending_tx_count).sum();
        progress.log(&format!(
            "  Mempool: {} pending txs, {} mempool-only opportunities visible",
            mempool_txs, mempool_opps,
        ));
    }

    Ok(())
}