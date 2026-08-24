use anyhow::Context;
use alloy::primitives::Address;
use std::time::Duration;

use crate::cli::LiveArgs;
use crate::display::{render_results_table, save_results_json};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::validation::{self, ValidationResult};
use mev_scout_core::config::Config;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::ResolvedRange;
use mev_scout_core::types::{GasConfig, GasModel, RangeMode, ResultsFile};
use mev_scout_core::utils::epoch_secs;

pub async fn cmd_live(config: &Config, args: &LiveArgs) -> anyhow::Result<()> {
    let validation = validation::validate_and_resolve(config)
        .context("invalid configuration")?;

    let setup = init_rpc(config, validation.chain_name, true).await?;
    let provider_configs = setup.provider_configs;
    let rpc = setup.rpc;
    let cache = SqliteStore::open(&config.effective_db_path(&validation.chain_name))?;

    let pool_addresses: Vec<Address> = cache
        .list_discovered_pools()
        .unwrap_or_default()
        .iter()
        .map(|p| p.address)
        .collect();

    let gas_config = GasConfig {
        gas_limit: args.gas_limit,
        gas_model: args.gas_model.parse().unwrap_or(GasModel::Live),
        priority_fee_gwei: args.priority_fee,
        flash_loan_provider: validation.flash_loan_provider,
        winning_bid_premium: 0.0,
        percentile_gas_price: None,
        calibration: Default::default(),
    };

    let mode_label = if args.r#loop { "continuous" } else { "one-shot" };
    println!("Live mode ({}) — polling every {}ms", mode_label, args.poll_interval_ms);

    if args.r#loop {
        run_loop(config, &validation, &rpc, &provider_configs, &cache, &pool_addresses, &args, gas_config).await
    } else {
        run_once(config, &validation, &rpc, &provider_configs, &cache, &pool_addresses, &args, gas_config).await
    }
}

async fn run_once(
    config: &Config,
    validation: &ValidationResult,
    rpc: &mev_scout_core::rpc::RpcClient,
    provider_configs: &[(String, Option<f64>, bool)],
    cache: &SqliteStore,
    pool_addresses: &[Address],
    args: &LiveArgs,
    gas_config: GasConfig,
) -> anyhow::Result<()> {
    let tip = rpc.get_block_number().await.context("failed to get chain tip")?;
    println!("Latest block: {}", tip);

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

    if !validation.strategies.is_empty() {
        let prev_block = tip.saturating_sub(1);
        BacktestRunner::init_pools(&mut pool_manager, rpc, prev_block, Some(cache)).await;
    }

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache.clone(),
        rpc.clone(),
        validation.chain_config.chain_id,
    );
    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
        .with_proximity_window(args.proximity_window)
        .with_min_profit_wei(args.min_profit_wei);

    let prev_block = tip.saturating_sub(1);
    if let Some(aave_pool_str) = &validation.chain_config.aave_v3_pool {
        if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
            runner.prefetch_aave_reserves(aave_pool, prev_block).await;
        }
    }

    let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
    fetcher = fetcher.with_parallelism(provider_configs.len());
    let resolved = ResolvedRange {
        start_block: tip,
        end_block: tip,
        block_count: 1,
        mode: RangeMode::Single(tip),
    };
    if !pool_addresses.is_empty() {
        fetcher.fetch_relevant(&resolved, pool_addresses, None::<&fn()>).await?;
    } else {
        fetcher.fetch_range(&resolved, None::<&fn()>).await?;
    }

    let state_horizon = rpc.detect_state_horizon(tip).await;
    let (opps, stats, _modes) = runner.run_range_hybrid(&resolved, state_horizon)?;

    let run_id = format!("live_{}", epoch_secs());
    let results_file = ResultsFile {
        run_id: run_id.clone(),
        chain: validation.chain_name.to_string(),
        start_block: tip,
        end_block: tip,
        range_mode: "live".to_string(),
        strategies: validation.strategies.iter().map(|s| s.to_string()).collect(),
        flash_loan_provider: validation.flash_loan_provider.to_string(),
        resolved_at: epoch_secs(),
        created_at: epoch_secs(),
        opportunities: opps.clone(),
    };
    let _ = save_results_json(&config.output.export_path, &run_id, &results_file);

    println!("\nBlock {} — {} opportunity(ies) detected", tip, opps.len());
    if opps.is_empty() {
        println!("No MEV opportunities in this block.");
    } else {
        render_results_table(&opps, Some(&runner.pool_manager));
    }

    if !stats.is_empty() {
        let s = &stats[0];
        println!(
            "  {} txs scanned, {} DEX, {} pending",
            s.total_tx_count, s.dex_tx_count, s.pending_tx_count,
        );
    }

    Ok(())
}

async fn run_loop(
    config: &Config,
    validation: &ValidationResult,
    rpc: &mev_scout_core::rpc::RpcClient,
    provider_configs: &[(String, Option<f64>, bool)],
    cache: &SqliteStore,
    pool_addresses: &[Address],
    args: &LiveArgs,
    gas_config: GasConfig,
) -> anyhow::Result<()> {
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
    if !validation.strategies.is_empty() {
        let prev_block = tip.saturating_sub(1);
        BacktestRunner::init_pools(&mut pool_manager, rpc, prev_block, Some(cache)).await;
    }

    let replayer = BlockReplayer::new(
        tokio::runtime::Handle::current(),
        cache.clone(),
        rpc.clone(),
        validation.chain_config.chain_id,
    );
    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
        .with_proximity_window(args.proximity_window)
        .with_min_profit_wei(args.min_profit_wei);

    let prev_block = tip.saturating_sub(1);
    if let Some(aave_pool_str) = &validation.chain_config.aave_v3_pool {
        if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
            runner.prefetch_aave_reserves(aave_pool, prev_block).await;
        }
    }

    let mut last_block = tip;
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

        let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
        fetcher = fetcher.with_parallelism(provider_configs.len());
        let resolved = ResolvedRange {
            start_block: from_block,
            end_block: current_tip,
            block_count: current_tip - from_block + 1,
            mode: RangeMode::Range(from_block, current_tip),
        };

        if !pool_addresses.is_empty() {
            if let Err(e) = fetcher.fetch_relevant(&resolved, pool_addresses, None::<&fn()>).await {
                tracing::warn!("Fetch failed for blocks {}–{}: {}", from_block, current_tip, e);
                last_block = current_tip;
                continue;
            }
        } else if let Err(e) = fetcher.fetch_range(&resolved, None::<&fn()>).await {
            tracing::warn!("Fetch failed for blocks {}–{}: {}", from_block, current_tip, e);
            last_block = current_tip;
            continue;
        }

        let state_horizon = rpc.detect_state_horizon(current_tip).await;
        let (opps, stats, _modes) = match runner.run_range_hybrid(&resolved, state_horizon) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Backtest failed for blocks {}–{}: {}", from_block, current_tip, e);
                last_block = current_tip;
                continue;
            }
        };

        if opps.is_empty() {
            println!(
                "Block {}–{}: no opportunities ({} txs)",
                from_block, current_tip,
                stats.iter().map(|s| s.total_tx_count).sum::<usize>(),
            );
        } else {
            println!(
                "Block {}–{}: {} opportunity(ies)",
                from_block, current_tip, opps.len(),
            );
            render_results_table(&opps, Some(&runner.pool_manager));
        }

        runner.last_processed_block = current_tip;
        last_block = current_tip;
    }
}
