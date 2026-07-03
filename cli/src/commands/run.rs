use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use comfy_table::Table;
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::RunArgs;
use crate::display::{print_startup_plan, render_block_summary_table, render_results_table, save_results_json, render_competition_table};
use mev_scout_core::cache::{RunManifest, SqliteStore};
use mev_scout_core::coingecko::PriceCache;
use mev_scout_core::config::validation;
use mev_scout_core::config::Config;
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::mev::verify::{
    compute_block_summaries, FactCheckReport, verify_opportunities,
    verify_opportunities_from_chain,
};
use mev_scout_core::pipeline::{aggregate_with_prices, BacktestRunner, DexMeta};
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::{GasConfig, PriceOracleMode, ResultsFile};

pub async fn cmd_run(config: &Config, args: &RunArgs) -> anyhow::Result<()> {
    let validation_result = match validation::validate_and_resolve(config) {
        Ok(r) => r,
        Err(e) => anyhow::bail!("{}", e),
    };
    print_startup_plan(&validation_result, config);

    let provider_configs = config.effective_provider_configs(validation_result.chain_name)?;
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, validation_result.chain_config.chain_id)?;
    rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
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

    {
        let batch_size = validation_result
            .chain_config
            .pool_discovery_batch_size
            .unwrap_or(200);
        let v2_fee = validation_result.chain_config.uniswap_v2_default_fee;
        let vault = validation_result
            .chain_config
            .balancer_vault
            .as_ref()
            .and_then(|s| s.parse::<Address>().ok());

        tracing::info!(
            "Discovering pools from DEX events in blocks {}..{}...",
            resolved.start_block,
            resolved.end_block,
        );

        let discovery_result = {
            let dune_enabled = config.dune_api_key.is_some() && config.dune_primary_pool_discovery;
            if dune_enabled {
                mev_scout_core::pool::discovery::discover_pools_with_sources(
                    &rpc, &cache, config,
                    validation_result.chain_name,
                    resolved.start_block, resolved.end_block,
                    batch_size, v2_fee, vault,
                    None, None, None, None,
                ).await
            } else {
                mev_scout_core::pool::discovery::discover_and_cache(
                    &rpc, &cache,
                    resolved.start_block, resolved.end_block,
                    batch_size, v2_fee, vault,
                    None, None, None, None,
                ).await
            }
        };
        match discovery_result {
            Ok((discovered, active)) => {
                if !discovered.is_empty() {
                    tracing::info!(
                        "Discovered {} pools from DEX events in {} active blocks",
                        discovered.len(),
                        active.len(),
                    );
                } else {
                    tracing::info!("No DEX activity found in block range");
                }
            }
            Err(e) => {
                tracing::warn!("Pool discovery from DEX events failed: {e:#}");
            }
        }
    }

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
    if let Some(pq_dir) = &config.parquet_dir {
        fetcher = fetcher.with_parquet(pq_dir);
    }
    if let Some(workers) = config.rpc_workers {
        fetcher = fetcher.with_parallelism(workers);
    }
    fetcher = fetcher.with_batch_rpc(!args.chain_args.no_batch_rpc);
    match mev_scout_core::sigs::ensure_signature_db(None).await {
        Ok(sig_db_path) => {
            match mev_scout_core::sigs::SignatureResolver::new(&sig_db_path) {
                Ok(resolver) => {
                    fetcher = fetcher.with_sig_resolver(resolver);
                    tracing::info!("Signature resolution enabled");
                }
                Err(e) => tracing::warn!("Failed to load signature DB: {e} — continuing without sig resolution"),
            }
        }
        Err(e) => tracing::warn!("Failed to ensure signature DB: {e} — continuing without sig resolution"),
    }

    let pb = ProgressBar::new(resolved.block_count);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta})")?
            .progress_chars("=> "),
    );
    let tick = || pb.tick();

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
    if let Some(workers) = config.rpc_workers {
        pool_manager.set_concurrency_limit(workers as u32);
    }
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

    let mut gas_config = GasConfig {
        gas_limit: config.gas_limit,
        gas_model: validation_result.gas_model,
        priority_fee_gwei: config.priority_fee_gwei,
        flash_loan_provider: validation_result.flash_loan_provider,
        winning_bid_premium: 0.0,
        percentile_gas_price: None,
    };
    let pga_cfg = if config.pga_enabled {
        gas_config = gas_config.with_winning_bid_premium(
            config.pga_mean_competitors,
            config.pga_intensity,
        );
        Some(mev_scout_core::mev::detectors::pga::PgaConfig::new(
            config.pga_mean_competitors,
            config.pga_intensity,
        ))
    } else {
        None
    };
    let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
        .with_proximity_window(config.proximity_window)
        .with_capture_pending(config.capture_pending);
    if args.competition {
        runner = runner.with_competition();
    }

    if config.cross_block_window > 0 {
        runner = runner.with_cross_block(config.cross_block_window);
    }

    if let Some(aave_pool_str) = &validation_result.chain_config.aave_v3_pool {
        if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
            runner.prefetch_aave_reserves(aave_pool, resolved.start_block.saturating_sub(1)).await;
        }
    }

    let start = std::time::Instant::now();

    let (all_opportunities, block_stats) = runner.run_range_with_pga(&resolved, pga_cfg)?;
    let elapsed = start.elapsed();

    let aggregation = if !all_opportunities.is_empty() {
        let oracle_mode: PriceOracleMode = match config.price_oracle_mode.parse() {
            Ok(m) => m,
            Err(_) => {
                tracing::warn!(
                    "Invalid price_oracle_mode '{}', falling back to coingecko",
                    config.price_oracle_mode,
                );
                PriceOracleMode::CoinGeckoOnly
            }
        };

        let mut token_prices = config.parse_token_prices();
        let mut price_cache = PriceCache::new(config.coingecko_api_key.clone());

        let native_price = price_cache.resolve_native_price(
            oracle_mode,
            validation_result.chain_name,
            &runner.pool_manager,
            resolved.start_block,
        ).await;
        if let Some(price) = native_price {
            token_prices.insert(Address::ZERO, price);
        }

        let dexes: Vec<DexMeta> = runner.pool_manager.all_pools().map(|pool| {
            let info = pool.info();
            DexMeta {
                name: info.name.clone().unwrap_or_else(|| format!("{:#x}", info.address)),
                fork: format!("{:?}", info.dex_type),
                tx_count: 0,
                pool_addresses: vec![info.address],
            }
        }).collect();

        let agg = aggregate_with_prices(&all_opportunities, &dexes, &token_prices);
        tracing::info!(
            "USD aggregation: {} opportunities, net_profit_usd=${:.2}",
            agg.summary.total,
            agg.summary.net_profit_usd,
        );
        Some(agg)
    } else {
        None
    };

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System clock went backwards")
        .as_secs();
    let competition_report = if args.competition {
        runner.build_competition_report()
    } else {
        None
    };

    // Persist competitor profiles to competition-db if provided
    if let Some(ref comp_db_path) = args.competition_db {
        if let Some(ref comp) = competition_report {
            match SqliteStore::open(comp_db_path, validation_result.chain_config.chain_id) {
                Ok(comp_store) => {
                    let _ = comp_store.put_competitor_profiles(&comp.top_searchers);
                }
                Err(e) => tracing::warn!("Failed to open competition DB: {}", e),
            }
        }
    }

    // Save PGA calibration to file if requested
    if let Some(ref cal_path) = args.pga_calibration_file {
        if let Some(ref comp) = competition_report {
            if let Ok(json) = serde_json::to_string_pretty(&comp.pga_calibration) {
                if let Err(e) = std::fs::write(cal_path, &json) {
                    tracing::warn!("Failed to save PGA calibration: {}", e);
                } else {
                    tracing::info!("PGA calibration saved to {}", cal_path);
                }
            }
        }
    }

    let results_file = ResultsFile {
        run_id: run_id.clone(),
        chain: validation_result.chain_name.to_string(),
        start_block: resolved.start_block,
        end_block: resolved.end_block,
        range_mode: resolved.mode_string(),
        strategies: manifest.strategies.clone(),
        flash_loan_provider: manifest.flash_loan_provider.clone(),
        resolved_at: manifest.resolved_at,
        created_at,
        opportunities: all_opportunities.clone(),
        competition: competition_report.clone(),
    };
    if let Err(e) = save_results_json(&config.export_path, &run_id, &results_file) {
        tracing::warn!("Failed to save results: {}", e);
    }

    if let Some(ref agg) = aggregation {
        let agg_path = std::path::Path::new(&config.export_path)
            .join(format!("{}_aggregation.json", run_id));
        if let Ok(json) = serde_json::to_string_pretty(agg) {
            if let Err(e) = std::fs::write(&agg_path, json) {
                tracing::warn!("Failed to save aggregation: {}", e);
            } else {
                tracing::info!("Aggregation saved to {}", agg_path.display());
            }
        }
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

    // Render competition analysis results
    if let Some(ref comp) = competition_report {
        render_competition_table(comp);

        // --calibrate-pga: print PGA calibration prominently
        if args.calibrate_pga && !comp.pga_calibration.mean_competitors.is_empty() {
            println!("\nPGA Calibration (--calibrate-pga):");
            println!("  Blocks analyzed: {}", comp.pga_calibration.blocks_analyzed);
            println!("  Total extractions: {}", comp.pga_calibration.total_extractions);
            println!("  Per-strategy parameters:");
            let mut strategies: Vec<String> = comp.pga_calibration.mean_competitors.keys().cloned().collect();
            strategies.sort();
            for strategy in &strategies {
                let mean = comp.pga_calibration.mean_competitors.get(strategy).copied().unwrap_or(0.0);
                let intensity = comp.pga_calibration.bid_to_value_ratio
                    .get(strategy).copied().unwrap_or(0.5);
                println!("    {}: mean_competitors={:.2}, intensity={:.3}", strategy, mean, intensity);
            }
            println!("  Suggested PGA config:");
            for strategy in &strategies {
                let mean = comp.pga_calibration.mean_competitors.get(strategy).copied().unwrap_or(0.0);
                let intensity = comp.pga_calibration.bid_to_value_ratio
                    .get(strategy).copied().unwrap_or(0.5);
                println!("    --pga-mean-competitors {:.1} --pga-intensity {:.3}  # {}", mean, intensity, strategy);
            }
        }
    }

    // Load PGA calibration from file if provided (overrides CLI defaults)
    if let Some(ref cal_path) = args.pga_calibration_file {
        if competition_report.is_none() && std::path::Path::new(cal_path).exists() {
            match std::fs::read_to_string(cal_path) {
                Ok(json) => {
                    if let Ok(cal) = serde_json::from_str::<mev_scout_core::mev::competition::PgaCalibration>(&json) {
                        println!("\nLoaded PGA calibration from {} ({} blocks, {} extractions)",
                            cal_path, cal.blocks_analyzed, cal.total_extractions);
                    }
                }
                Err(e) => tracing::warn!("Failed to read PGA calibration file: {}", e),
            }
        }
    }

    let mempool_opps: usize = block_stats.iter().map(|s| s.mempool_opp_count).sum();
    if mempool_opps > 0 {
        let mempool_txs: usize = block_stats.iter().map(|s| s.pending_tx_count).sum();
        println!(
            "  Mempool: {} pending txs, {} mempool-only opportunities visible",
            mempool_txs, mempool_opps,
        );
    }

    if let Some(ref agg) = aggregation {
        println!("\nUSD Aggregation:");
        let mut agg_table = Table::new();
        agg_table.set_header(vec!["Metric".to_string(), "Value".to_string()]);
        agg_table.add_row(vec!["Total".to_string(), agg.summary.total.to_string()]);
        agg_table.add_row(vec!["Profitable".to_string(), agg.summary.profitable.to_string()]);
        agg_table.add_row(vec!["Gross (ETH)".to_string(), format!("{:.6}", agg.summary.gross_revenue)]);
        agg_table.add_row(vec!["Gas (ETH)".to_string(), format!("{:.6}", agg.summary.total_cost)]);
        agg_table.add_row(vec!["Net (ETH)".to_string(), format!("{:.6}", agg.summary.net_profit)]);
        agg_table.add_row(vec!["Net (USD)".to_string(), format!("${:.2}", agg.summary.net_profit_usd)]);
        if let Some(ref best) = agg.summary.best_strategy {
            agg_table.add_row(vec!["Best strategy".to_string(), best.clone()]);
        }
        println!("{agg_table}");
    }

    let fact_check_mode = if args.evm_fact_check { "EVM" } else { "structural" };
    if args.fact_check && !all_opportunities.is_empty() {
        println!("\nFact-Check Report ({}):", fact_check_mode);
        let checks = if args.evm_fact_check {
            let rpc = RpcClient::from_urls(&rpc_refs, validation_result.chain_config.chain_id)?;
            rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
            verify_opportunities_from_chain(&all_opportunities, &runner.pool_manager, &rpc).await
        } else {
            verify_opportunities(&all_opportunities, Some(&runner.pool_manager))
        };
        let passed = checks.iter().filter(|c| c.profit_gt_gas).count();
        let failed = checks.len().saturating_sub(passed);
        let summaries = compute_block_summaries(&all_opportunities, &block_stats);
        let report = FactCheckReport {
            run_id: run_id.clone(),
            chain: validation_result.chain_name.to_string(),
            block_count: block_stats.len(),
            total_opportunities: all_opportunities.len(),
            passed,
            failed,
            block_summaries: summaries,
            opportunity_checks: checks,
        };

        println!("  Opportunities: {} total, {} passed, {} failed", report.total_opportunities, report.passed, report.failed);

        let report_path = std::path::Path::new(&config.export_path)
            .join(format!("{}_factcheck.json", run_id));
        if let Ok(json) = serde_json::to_string_pretty(&report) {
            let _ = std::fs::write(&report_path, json);
            println!("  Report saved to {}", report_path.display());
        }
    }

    Ok(())
}
