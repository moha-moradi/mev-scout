use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::RunArgs;
use crate::display::{print_startup_plan, render_block_summary_table, render_results_table, save_results_json};
use mev_scout_core::cache::{RunManifest, SqliteStore};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::{GasConfig, ResultsFile};

pub async fn cmd_run(config: &Config, args: &RunArgs) -> anyhow::Result<()> {
    let validation_result = match validation::validate_and_resolve(config) {
        Ok(r) => r,
        Err(e) => anyhow::bail!("{}", e),
    };
    print_startup_plan(&validation_result, config);

    let provider_configs = config.effective_provider_configs(validation_result.chain_name)?;
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, validation_result.chain_config.chain_id)?;
    rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(config.rps_limit)).collect::<Vec<_>>()).await;
    rpc.check_connection(validation_result.chain_config.chain_id).await?;
    let cache = SqliteStore::open(&config.effective_db_path(&validation_result.chain_name), validation_result.chain_config.chain_id)?;

    let resolver = RangeResolver::new(rpc.clone());
    let resolved = match resolver.resolve(&validation_result.range_mode).await {
        Ok(r) => r,
        Err(e) => anyhow::bail!("Error: failed to resolve block range: {}", e),
    };

    let run_id = format!(
        "run_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System clock went backwards")
            .as_secs()
    );

    let manifest = RunManifest {
        run_id: run_id.clone(),
        chain: validation_result.chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        resolved_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System clock went backwards")
            .as_secs(),
        range_mode: resolved.mode_string(),
        strategies: validation_result.strategies.iter().map(|s| s.to_string()).collect(),
        flash_loan_provider: validation_result.flash_loan_provider.to_string(),
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
    let bc = config.block_concurrency.unwrap_or(100);
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
    pool_manager.set_max_pairs_per_token(config.max_pairs_per_token);
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
        validation_result.chain_config.chain_id,
    );

    let gas_config = GasConfig {
        gas_limit: config.gas_limit,
        gas_model: validation_result.gas_model,
        priority_fee_gwei: config.priority_fee_gwei,
        flash_loan_provider: validation_result.flash_loan_provider,
        winning_bid_premium: 0.0,
        percentile_gas_price: None,
    };
    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
        .with_proximity_window(config.proximity_window)
        .with_capture_pending(config.capture_pending);

    if config.cross_block_window > 0 {
        runner = runner.with_cross_block(config.cross_block_window);
    }

    if let Some(aave_pool_str) = &validation_result.chain_config.aave_v3_pool {
        if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
            runner.prefetch_aave_reserves(aave_pool, resolved.start_block.saturating_sub(1)).await;
        }
    }

    let start = std::time::Instant::now();

    let (all_opportunities, block_stats) = runner.run_range(&resolved)?;
    let elapsed = start.elapsed();

    let results_file = ResultsFile {
        run_id: run_id.clone(),
        chain: validation_result.chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        range_mode: resolved.mode_string(),
        strategies: manifest.strategies.clone(),
        flash_loan_provider: manifest.flash_loan_provider.clone(),
        resolved_at: manifest.resolved_at,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System clock went backwards")
            .as_secs(),
        opportunities: all_opportunities.clone(),
    };
    if let Err(e) = save_results_json(&config.export_path, &run_id, &results_file) {
        tracing::warn!("Failed to save results: {}", e);
    }

    if all_opportunities.is_empty() {
        println!("No MEV opportunities detected in the specified range.");
    } else {
        println!(
            "\nDetected {} MEV opportunity(ies) in {:.2}s:\n",
            all_opportunities.len(),
            elapsed.as_secs_f64()
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
