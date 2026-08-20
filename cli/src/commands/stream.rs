use anyhow::Context;
use alloy::primitives::Address;
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::StreamArgs;
use crate::display::{print_startup_plan, render_block_summary_table, render_results_table, save_results_json};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::{RunManifest, SqliteStore};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::pipeline::BlockMode;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::types::{GasConfig, ResultsFile};
use mev_scout_core::utils::epoch_secs;

pub async fn cmd_stream(config: &Config, args: &StreamArgs) -> anyhow::Result<()> {
    let mut validation = validation::validate_and_resolve(config)
        .context("invalid configuration")?;
    print_startup_plan(&validation, config);

    let setup = init_rpc(config, validation.chain_name, true).await?;
    let provider_configs = setup.provider_configs;
    let rpc = setup.rpc;
    let cache = SqliteStore::open(&config.effective_db_path(&validation.chain_name))?;

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = match resolver.resolve(&validation.range_mode).await {
        Ok(r) => r,
        Err(e) => anyhow::bail!("{e}"),
    };

    let state_horizon = rpc.detect_state_horizon(resolved.end_block).await;
    println!(
        "State horizon: block {} (blocks >= horizon get full EVM replay, older blocks use log-only)",
        state_horizon
    );

    let run_id = format!("stream_{}", epoch_secs());

    let manifest = RunManifest {
        run_id: run_id.clone(),
        chain: validation.chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        resolved_at: epoch_secs(),
        range_mode: resolved.mode_string(),
        strategies: validation.strategies.iter().map(|s| s.to_string()).collect(),
        flash_loan_provider: validation.flash_loan_provider.to_string(),
    };
    cache.put_manifest(&manifest)?;

    println!("Run ID: {}", run_id);
    println!("{}", resolved.summary());
    println!();

    let pool_addresses: Vec<Address> = cache
        .list_discovered_pools()
        .unwrap_or_default()
        .iter()
        .map(|p| p.address)
        .collect();

    let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
    fetcher = fetcher.with_parallelism(provider_configs.len());
    fetcher = fetcher.with_batch_rpc(args.batch_rpc);
    let bc = config.effective_block_concurrency(&provider_configs);
    fetcher = fetcher.with_block_concurrency(bc);

    let pb = ProgressBar::new(resolved.block_count);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta})")?
            .progress_chars("=> "),
    );
    let tick = || pb.inc(1);

    let fetch_summary = if !pool_addresses.is_empty() {
        fetcher.fetch_relevant(&resolved, &pool_addresses, Some(&tick)).await?
    } else {
        fetcher.fetch_range(&resolved, Some(&tick)).await?
    };
    pb.finish_and_clear();

    if fetch_summary.skipped > 0 {
        tracing::info!(
            "Fetch optimization: skipped {} blocks with no DEX activity (fetched {} of {} scanned)",
            fetch_summary.skipped, fetch_summary.fetched, fetch_summary.scanned,
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
    if let Some(vault_str) = &validation.chain_config.balancer_vault {
        if let Ok(vault_addr) = vault_str.parse::<Address>() {
            pool_manager = pool_manager.with_balancer_vault(vault_addr);
        }
    }
    if let Some(native_str) = &validation.chain_config.wrapped_native_token {
        if let Ok(native_addr) = native_str.parse::<Address>() {
            pool_manager = pool_manager.with_wrapped_native(native_addr);
        }
    }

    let init_block = state_horizon.max(resolved.start_block);
    let prev_block = init_block.saturating_sub(1);

    if !validation.strategies.is_empty() {
        if init_block > resolved.start_block {
            tracing::info!(
                "Initializing pool state at block {} (state horizon) for range starting at {}",
                init_block, resolved.start_block,
            );
            pool_manager.set_use_latest(true);
        }
        BacktestRunner::init_pools(
            &mut pool_manager,
            &rpc,
            prev_block,
            Some(&cache),
        ).await;
    }

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache,
        rpc.clone(),
        validation.chain_config.chain_id,
    );

    let gas_config = GasConfig {
        gas_limit: config.gas.gas_limit,
        gas_model: validation.gas_model,
        priority_fee_gwei: config.gas.priority_fee_gwei,
        flash_loan_provider: validation.flash_loan_provider,
        winning_bid_premium: 0.0,
        percentile_gas_price: None,
    };
    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
        .with_proximity_window(config.backtest.proximity_window)
        .with_capture_pending(config.backtest.capture_pending)
        .with_min_profit_wei(config.backtest.min_profit_wei)
        .with_max_candidates_per_tx(config.backtest.max_candidates_per_tx);

    if let Some(aave_pool_str) = &validation.chain_config.aave_v3_pool {
        if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
            runner.prefetch_aave_reserves(aave_pool, prev_block).await;
        }
    }

    let start = std::time::Instant::now();

    let (all_opportunities, block_stats, block_modes) = runner.run_range_hybrid(&resolved, state_horizon)?;
    let elapsed = start.elapsed();

    let results_file = ResultsFile {
        run_id: run_id.clone(),
        chain: validation.chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        range_mode: resolved.mode_string(),
        strategies: manifest.strategies.clone(),
        flash_loan_provider: manifest.flash_loan_provider.clone(),
        resolved_at: manifest.resolved_at,
        created_at: epoch_secs(),
        opportunities: all_opportunities.clone(),
    };
    if let Err(e) = save_results_json(&config.output.export_path, &run_id, &results_file) {
        tracing::warn!("Failed to save results: {}", e);
    }

    let full_count = block_modes.iter().filter(|m| **m == BlockMode::FullReplay).count();
    let log_only_count = block_modes.iter().filter(|m| **m == BlockMode::LogOnly).count();
    println!(
        "\nStream complete: {} blocks processed ({}, {} log-only) in {:.2}s",
        block_modes.len(), full_count, log_only_count, elapsed.as_secs_f64(),
    );

    if all_opportunities.is_empty() {
        println!("No MEV opportunities detected in the specified range.");
    } else {
        println!(
            "Detected {} MEV opportunity(ies):\n",
            all_opportunities.len(),
        );
        render_results_table(&all_opportunities, Some(&runner.pool_manager));
    }

    render_block_summary_table(&block_stats);

    let mempool_opps: usize = block_stats.iter().map(|s| s.mempool_opp_count).sum();
    if mempool_opps > 0 {
        let mempool_txs: usize = block_stats.iter().map(|s| s.pending_tx_count).sum();
        println!(
            "  Mempool: {} pending txs, {} mempool-only opportunities visible",
            mempool_txs, mempool_opps,
        );
    }

    Ok(())
}
