use anyhow::Context;
use alloy::primitives::Address;
use std::time::Duration;

use crate::cli::LiveArgs;
use crate::display::{render_results_table, save_results_json};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::{RunManifest, SqliteStore};
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::{RangeResolver, ResolvedRange};
use mev_scout_core::types::{GasConfig, GasModel, RangeMode, ResultsFile};
use mev_scout_core::utils::epoch_secs;

pub async fn cmd_live(config: &Config, args: &LiveArgs) -> anyhow::Result<()> {
    let validation = validation::validate_and_resolve(config)
        .context("invalid configuration")?;

    let setup = init_rpc(config, validation.chain_name, true).await?;
    let provider_configs = setup.provider_configs;
    let rpc = setup.rpc;
    let cache = SqliteStore::open(&config.effective_db_path(&validation.chain_name))?;

    let run_id = format!("live_{}", epoch_secs());

    let manifest = RunManifest {
        run_id: run_id.clone(),
        chain: validation.chain_name.to_string(),
        start_block: 0,
        end_block: 0,
        resolved_at: epoch_secs(),
        range_mode: "live".to_string(),
        strategies: validation.strategies.iter().map(|s| s.to_string()).collect(),
        flash_loan_provider: validation.flash_loan_provider.to_string(),
    };
    cache.put_manifest(&manifest)?;

    println!("Run ID: {}", run_id);
    println!("Live mode — polling every {}ms", args.poll_interval_ms);
    println!();

    let pool_addresses: Vec<Address> = cache
        .list_discovered_pools()
        .unwrap_or_default()
        .iter()
        .map(|p| p.address)
        .collect();

    let mut pool_manager = PoolManager::new();
    pool_manager.set_max_pairs_per_token(config.backtest.max_pairs_per_token);
    pool_manager.set_concurrency_limit(provider_configs.len() as u32);
    pool_manager.set_use_latest(true);
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

    let tip = rpc.get_block_number().await.context("failed to get chain tip")?;
    let prev_block = tip.saturating_sub(1);

    if !validation.strategies.is_empty() {
        BacktestRunner::init_pools(
            &mut pool_manager,
            &rpc,
            prev_block,
            Some(&cache),
        ).await;
    }

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache.clone(),
        rpc.clone(),
        validation.chain_config.chain_id,
    );

    let gas_config = GasConfig {
        gas_limit: args.gas_limit,
        gas_model: args.gas_model.parse().unwrap_or(GasModel::Live),
        priority_fee_gwei: args.priority_fee,
        flash_loan_provider: validation.flash_loan_provider,
        winning_bid_premium: 0.0,
        percentile_gas_price: None,
    };
    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
        .with_proximity_window(args.proximity_window)
        .with_min_profit_wei(args.min_profit_wei);

    if let Some(aave_pool_str) = &validation.chain_config.aave_v3_pool {
        if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
            runner.prefetch_aave_reserves(aave_pool, prev_block).await;
        }
    }

    let mut last_block = tip;
    let mut total_opps = 0u64;
    let mut blocks_processed = 0u64;
    let start_time = std::time::Instant::now();

    println!("Starting from block {} — Ctrl+C to stop\n", last_block);

    loop {
        tokio::time::sleep(Duration::from_millis(args.poll_interval_ms)).await;

        let current_tip = match rpc.get_block_number().await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("Failed to get block number: {}", e);
                continue;
            }
        };

        if current_tip <= last_block {
            continue;
        }

        let from_block = last_block + 1;
        let to_block = current_tip;

        let resolved = ResolvedRange {
            start_block: from_block,
            end_block: to_block,
            block_count: to_block - from_block + 1,
            mode: RangeMode::Range(from_block, to_block),
        };

        let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
        fetcher = fetcher.with_parallelism(provider_configs.len());

        if !pool_addresses.is_empty() {
            if let Err(e) = fetcher.fetch_relevant(&resolved, &pool_addresses, None::<&fn()>).await {
                tracing::warn!("Fetch failed for blocks {}–{}: {}", from_block, to_block, e);
                continue;
            }
        } else if let Err(e) = fetcher.fetch_range(&resolved, None::<&fn()>).await {
            tracing::warn!("Fetch failed for blocks {}–{}: {}", from_block, to_block, e);
            continue;
        }

        let (opps, stats, _modes) = match runner.run_range_hybrid(&resolved, 0) {
            // state_horizon=0 means everything >=0 is full-replay, but since we're live
            // and pool state is at latest, all blocks are effectively within state window
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Backtest failed for blocks {}–{}: {}", from_block, to_block, e);
                last_block = to_block;
                continue;
            }
        };

        let opp_count = opps.len();
        total_opps += opp_count as u64;
        blocks_processed += resolved.block_count;

        if opp_count > 0 {
            println!(
                "Block {}–{}: {} opportunities detected",
                from_block, to_block, opp_count,
            );
            render_results_table(&opps, Some(&runner.pool_manager));
        } else {
            println!(
                "Block {}–{}: no opportunities ({} txs processed)",
                from_block, to_block,
                stats.iter().map(|s| s.total_tx_count).sum::<usize>(),
            );
        }

        runner.last_processed_block = to_block;
        last_block = to_block;

        let elapsed = start_time.elapsed().as_secs_f64();
        let blocks_per_sec = if elapsed > 0.0 { blocks_processed as f64 / elapsed } else { 0.0 };
        tracing::debug!(
            "Live stats: {} blocks processed, {} total opportunities, {:.1} blocks/s",
            blocks_processed, total_opps, blocks_per_sec,
        );
    }
}
