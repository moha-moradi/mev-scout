
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{keccak256, Address};
use clap::Parser;
use comfy_table::Table;
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::EnvFilter;

use mev_scout_core::cache::{SqliteStore, RunManifest};
use mev_scout_core::cli::{Cli, Command};
use mev_scout_core::config::{CliOverrides, Config};
use mev_scout_core::fact_check::{BlockReplayStats, compute_block_summaries, FactCheckReport, verify_opportunities, verify_opportunities_from_chain};
use mev_scout_core::fetch::Fetcher;

use mev_scout_core::pool::state::{PoolManager, PoolState};
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::RangeResolver;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::mev::opportunity::ResultsFile;

use mev_scout_core::run::BacktestRunner;
use mev_scout_core::types::{GasConfig, OutputFormat};
use mev_scout_core::validation;

fn setup_logging(verbose: bool, quiet: bool) {
    let filter = if quiet {
        EnvFilter::new("error")
    } else if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .with_target(false)
        .init();
}

fn build_overrides(cli: &Cli) -> CliOverrides {
    match &cli.command {
        Command::Run(args) => CliOverrides {
            days: args.block_range.days,
            blocks: args.block_range.blocks,
            block: args.block_range.block,
            from_block: args.block_range.from_block,
            to_block: args.block_range.to_block,
            chain: Some(args.chain_args.chain.clone()),
            rpc_url: args.chain_args.rpc_url.clone(),
            rpc_urls: args.chain_args.rpc_urls.clone(),
            rpc_rps: args.chain_args.rpc_rps.clone(),
            rpc_workers: Some(args.chain_args.rpc_workers),
            rps_limit: Some(args.chain_args.rps_limit),
            flash_loan_provider: Some(args.flash_loan_provider.clone()),
            strategies: Some(args.strategies.clone()),
            gas_model: Some(args.gas_model.clone()),
            gas_limit: Some(args.gas_limit),
            priority_fee_gwei: Some(args.priority_fee),
            output: Some(args.output.clone()),
            export_path: Some(args.export_path.clone()),
            db_path: args.db_path.clone(),
            parquet_dir: args.parquet_dir.clone(),
            coingecko_api_key: None,
            pga_enabled: Some(args.pga_enabled),
            pga_mean_competitors: Some(args.pga_mean_competitors),
            pga_intensity: Some(args.pga_intensity),
            price_oracle_mode: Some(args.price_oracle_mode.clone()),
            token_prices: args.token_prices.clone(),
            proximity_window: Some(args.proximity_window),
            capture_pending: Some(args.capture_pending),
            cross_block_window: Some(args.cross_block_window),
        },
        Command::Fetch(args) => CliOverrides {
            days: args.block_range.days,
            blocks: args.block_range.blocks,
            block: args.block_range.block,
            from_block: args.block_range.from_block,
            to_block: args.block_range.to_block,
            chain: Some(args.chain_args.chain.clone()),
            rpc_url: args.chain_args.rpc_url.clone(),
            rpc_urls: args.chain_args.rpc_urls.clone(),
            rpc_rps: args.chain_args.rpc_rps.clone(),
            rpc_workers: Some(args.chain_args.rpc_workers),
            rps_limit: Some(args.chain_args.rps_limit),
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            db_path: args.db_path.clone(),
            parquet_dir: args.parquet_dir.clone(),
            coingecko_api_key: None,
            pga_enabled: None,
            pga_mean_competitors: None,
            pga_intensity: None,
            price_oracle_mode: None,
            token_prices: None,
            proximity_window: None,
            capture_pending: None,
            cross_block_window: None,
        },
        Command::Replay(args) => CliOverrides {
            days: None,
            blocks: None,
            block: Some(args.block),
            from_block: None,
            to_block: None,
            chain: Some(args.chain_args.chain.clone()),
            rpc_url: args.chain_args.rpc_url.clone(),
            rpc_urls: args.chain_args.rpc_urls.clone(),
            rpc_rps: args.chain_args.rpc_rps.clone(),
            rpc_workers: Some(args.chain_args.rpc_workers),
            rps_limit: Some(args.chain_args.rps_limit),
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            db_path: args.db_path.clone(),
            parquet_dir: args.parquet_dir.clone(),
            coingecko_api_key: None,
            pga_enabled: None,
            pga_mean_competitors: None,
            pga_intensity: None,
            price_oracle_mode: None,
            token_prices: None,
            proximity_window: None,
            capture_pending: None,
            cross_block_window: None,
        },
        Command::Report(_args) => CliOverrides {
            days: None,
            blocks: None,
            block: None,
            from_block: None,
            to_block: None,
            chain: None,
            rpc_url: None,
            rpc_urls: None,
            rpc_rps: None,
            rpc_workers: None,
            rps_limit: None,
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            db_path: None,
            parquet_dir: None,
            coingecko_api_key: None,
            pga_enabled: None,
            pga_mean_competitors: None,
            pga_intensity: None,
            price_oracle_mode: None,
            token_prices: None,
            proximity_window: None,
            capture_pending: None,
            cross_block_window: None,
        },
        Command::Config => CliOverrides {
            days: None,
            blocks: None,
            block: None,
            from_block: None,
            to_block: None,
            chain: None,
            rpc_url: None,
            rpc_urls: None,
            rpc_rps: None,
            rpc_workers: None,
            rps_limit: None,
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            db_path: None,
            parquet_dir: None,
            coingecko_api_key: None,
            pga_enabled: None,
            pga_mean_competitors: None,
            pga_intensity: None,
            price_oracle_mode: None,
            token_prices: None,
            proximity_window: None,
            capture_pending: None,
            cross_block_window: None,
        },
        Command::Discover(args) => CliOverrides {
            days: None,
            blocks: None,
            block: None,
            from_block: Some(args.from_block),
            to_block: Some(args.to_block),
            chain: Some(args.chain_args.chain.clone()),
            rpc_url: args.chain_args.rpc_url.clone(),
            rpc_urls: args.chain_args.rpc_urls.clone(),
            rpc_rps: args.chain_args.rpc_rps.clone(),
            rpc_workers: Some(args.chain_args.rpc_workers),
            rps_limit: Some(args.chain_args.rps_limit),
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            db_path: args.db_path.clone(),
            parquet_dir: None,
            coingecko_api_key: None,
            pga_enabled: None,
            pga_mean_competitors: None,
            pga_intensity: None,
            price_oracle_mode: None,
            token_prices: None,
            proximity_window: None,
            capture_pending: None,
            cross_block_window: None,
        },
        Command::FactCheck(_) => CliOverrides {
            days: None,
            blocks: None,
            block: None,
            from_block: None,
            to_block: None,
            chain: None,
            rpc_url: None,
            rpc_urls: None,
            rpc_rps: None,
            rpc_workers: None,
            rps_limit: None,
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            db_path: None,
            parquet_dir: None,
            coingecko_api_key: None,
            pga_enabled: None,
            pga_mean_competitors: None,
            pga_intensity: None,
            price_oracle_mode: None,
            token_prices: None,
            proximity_window: None,
            capture_pending: None,
            cross_block_window: None,
        },
    }
}

fn print_startup_plan(result: &validation::ValidationResult, config: &mev_scout_core::config::Config) {
    let divider = "═".repeat(55);

    println!();
    println!("  ╔{divider}╗");
    println!("  ║        MEV Backtest Engine — Startup Plan        ║");
    println!("  ╚{divider}╝");
    println!();

    let plan = config.plan_summary(
        result.chain_name,
        &result.chain_config,
        &result.range_mode,
        &result.strategies,
        result.flash_loan_provider,
    );

    for line in plan.lines() {
        println!("  {line}");
    }

    println!("  [DRY RUN — no simulation yet]");
    println!();
}

fn save_results_json(
    export_path: &str,
    run_id: &str,
    results_file: &ResultsFile,
) -> anyhow::Result<()> {
    let dir = std::path::Path::new(export_path);
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", run_id));
    let json = serde_json::to_string_pretty(results_file)?;
    std::fs::write(&path, json)?;
    println!("Results saved to {}", path.display());
    Ok(())
}

fn pool_name(pm: &PoolManager, addr: &alloy::primitives::Address) -> String {
    pm.get(addr)
        .map(|ps| match ps {
            PoolState::UniswapV2(s) => &s.info,
            PoolState::UniswapV3(s) => &s.info,
            PoolState::Curve(s) => &s.info,
            PoolState::Balancer(s) => &s.info,
        })
        .and_then(|info| info.name.clone())
        .unwrap_or_else(|| format!("{}", addr))
}

fn render_results_table(all_opportunities: &[mev_scout_core::mev::opportunity::MevOpportunity], pool_manager: Option<&PoolManager>) {
    let mut table = Table::new();
    let has_confidence = all_opportunities.iter().any(|opp| opp.confidence.is_some());

    if pool_manager.is_some() {
        let mut headers = vec![
            "Block", "Tx", "Strategy", "Pool A / Pool B",
            "Input", "Profit (token_out)", "Gas (wei)",
        ];
        if has_confidence {
            headers.push("Confidence");
        }
        table.set_header(headers);

        for opp in all_opportunities {
            let pm = pool_manager.unwrap();
            let name_a = pool_name(pm, &opp.pool_a);
            let name_b = if opp.pool_b == alloy::primitives::Address::ZERO {
                String::new()
            } else {
                pool_name(pm, &opp.pool_b)
            };
            let mut row = vec![
                format!("{}", opp.block_number),
                format!("{}", opp.tx_index),
                format!("{}", opp.strategy),
                if name_b.is_empty() { name_a } else { format!("{} / {}", name_a, name_b) },
                format!("{}", opp.input_amount),
                format!("{}", opp.expected_profit),
                format!("{}", opp.gas_cost_wei),
            ];
            if has_confidence {
                row.push(opp.confidence.map_or("-".to_string(), |c| format!("{:.2}", c)));
            }
            table.add_row(row);
        }
    } else {
        let mut headers = vec![
            "Block", "Tx", "Strategy",
            "Input", "Profit (token_out)", "Gas (wei)",
        ];
        if has_confidence {
            headers.push("Confidence");
        }
        table.set_header(headers);

        for opp in all_opportunities {
            let mut row = vec![
                format!("{}", opp.block_number),
                format!("{}", opp.tx_index),
                format!("{}", opp.strategy),
                format!("{}", opp.input_amount),
                format!("{}", opp.expected_profit),
                format!("{}", opp.gas_cost_wei),
            ];
            if has_confidence {
                row.push(opp.confidence.map_or("-".to_string(), |c| format!("{:.2}", c)));
            }
            table.add_row(row);
        }
    }

    println!("{table}");
}

fn render_block_summary_table(summaries: &[BlockReplayStats]) {
    if summaries.len() <= 1 {
        return;
    }
    let mut table = Table::new();
    let has_pending = summaries.iter().any(|s| s.pending_tx_count > 0);
    if has_pending {
        table.set_header(vec!["Block", "Txs", "DEX txs", "Pending"]);
    } else {
        table.set_header(vec!["Block", "Txs", "DEX txs"]);
    }
    let mut total_tx = 0usize;
    let mut total_dex = 0usize;
    let mut total_pending = 0usize;
    for s in summaries {
        total_tx += s.total_tx_count;
        total_dex += s.dex_tx_count;
        total_pending += s.pending_tx_count;
        if has_pending {
            table.add_row(vec![
                format!("{}", s.block_number),
                format!("{}", s.total_tx_count),
                format!("{}", s.dex_tx_count),
                format!("{}", s.pending_tx_count),
            ]);
        } else {
            table.add_row(vec![
                format!("{}", s.block_number),
                format!("{}", s.total_tx_count),
                format!("{}", s.dex_tx_count),
            ]);
        }
    }
    if has_pending {
        table.add_row(vec![
            format!("{}", "Total"),
            format!("{}", total_tx),
            format!("{}", total_dex),
            format!("{}", total_pending),
        ]);
    } else {
        table.add_row(vec![
            format!("{}", "Total"),
            format!("{}", total_tx),
            format!("{}", total_dex),
        ]);
    }
    println!("\nBlock Summary");
    println!("{table}");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    setup_logging(cli.verbose, cli.quiet);

    // Load config (zero-config by default — only load TOML if --config is given)
    let mut config = match &cli.config {
        Some(path) => Config::load(path).unwrap_or_else(|e| {
            eprintln!("Error: failed to load config '{path}': {e}");
            std::process::exit(1);
        }),
        None => Config::default(),
    };

    // Merge CLI overrides
    let overrides = build_overrides(&cli);
    config.merge_cli(&overrides);

    // Dispatch
    match &cli.command {
        Command::Run(args) => {
            let validation_result = match validation::validate_and_resolve(&config) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            };
            print_startup_plan(&validation_result, &config);

            let provider_configs = config.effective_provider_configs(validation_result.chain_name)?;
            let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
            let rpc = RpcClient::from_urls(&rpc_refs, validation_result.chain_config.chain_id)?;
            rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
            rpc.check_connection(validation_result.chain_config.chain_id).await?;
            let cache = SqliteStore::open(&config.db_path, validation_result.chain_config.chain_id)?;

            // Resolve block range
            let resolver = RangeResolver::new(rpc.clone());
            let resolved = match resolver.resolve(&validation_result.range_mode).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: failed to resolve block range: {}", e);
                    std::process::exit(1);
                }
            };

            // Pool discovery is a separate upfront step.
            // Run `mev-scout discover` before `mev-scout run` to populate the pool cache.

            let run_id = format!(
                "run_{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            );

            let manifest = RunManifest {
                run_id: run_id.clone(),
                chain: validation_result.chain_name.to_string(),
                start_block: resolved.start_block,
                end_block: resolved.end_block,
                resolved_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                range_mode: resolved.mode_string(),
                strategies: validation_result.strategies.iter().map(|s| s.to_string()).collect(),
                flash_loan_provider: validation_result.flash_loan_provider.to_string(),
            };
            cache.put_manifest(&manifest)?;

            println!("Run ID: {}", run_id);
            println!("{}", resolved.summary());
            println!();

            // Pool discovery from DEX events (Swap/Sync/Mint/Burn).
            // Scans all contracts in the block range, discovers pool addresses
            // from event emitters, and saves them to cache. No factory addresses
            // or pre-indexing required — this is the default discovery mode.
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
                    .and_then(|s| s.parse::<alloy::primitives::Address>().ok());

                tracing::info!(
                    "Discovering pools from DEX events in blocks {}..{}...",
                    resolved.start_block,
                    resolved.end_block,
                );

                match mev_scout_core::pool::discovery::discover_and_cache(
                    &rpc,
                    &cache,
                    resolved.start_block,
                    resolved.end_block,
                    batch_size,
                    v2_fee,
                    vault,
                    None, // v2_factories
                    None, // v3_factories
                    None, // v2_factory_fees
                    None, // curve_registry
                )
                .await
                {
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

            // Load pool addresses from cache for log-first fetch optimization
            let pool_addresses: Vec<alloy::primitives::Address> = cache
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

            // Phase 1: Fetch relevant blocks (log-first optimization)
            let mut fetcher = Fetcher::new(rpc.clone(), cache.clone());
            if let Some(pq_dir) = &config.parquet_dir {
                fetcher = fetcher.with_parquet(pq_dir);
            }
            if let Some(workers) = config.rpc_workers {
                fetcher = fetcher.with_parallelism(workers);
            }
            fetcher = fetcher.with_batch_rpc(!args.chain_args.no_batch_rpc);

            let pb = indicatif::ProgressBar::new(resolved.block_count);
            pb.set_style(
                indicatif::ProgressStyle::default_bar()
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

            // Init pool manager (needs cache before it's moved into replayer)
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

            // Build replayer (takes ownership of cache)
            let replayer = BlockReplayer::new(
                tokio::runtime::Handle::current(),
                cache,
                rpc.clone(),
                validation_result.chain_config.chain_id,
            );

            // Run backtest
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
                Some(mev_scout_core::mev::pga::PgaConfig::new(
                    config.pga_mean_competitors,
                    config.pga_intensity,
                ))
            } else {
                None
            };
            let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
                .with_proximity_window(config.proximity_window)
                .with_capture_pending(config.capture_pending);

            // L2: Enable cross-block MEV detection when window is configured
            if config.cross_block_window > 0 {
                runner = runner.with_cross_block(config.cross_block_window);
            }

            // Pre-fetch Aave V3 reserve data for per-asset liquidation parameters (L1).
            // This populates the reserve cache so LiquidationDetector can use real
            // on-chain thresholds and bonuses instead of hardcoded 80%/5% defaults.
            if let Some(aave_pool_str) = &validation_result.chain_config.aave_v3_pool {
                if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
                    runner.prefetch_aave_reserves(aave_pool, resolved.start_block.saturating_sub(1)).await;
                }
            }

            let start = std::time::Instant::now();

            let (all_opportunities, block_stats) = runner.run_range_with_pga(&resolved, pga_cfg)?;
            let elapsed = start.elapsed();

            // L3/L5: Compute USD aggregation for detected opportunities
            // Builds DexMeta from pool manager, resolves native token USD price
            // via the configured PriceOracleMode, and calls aggregate_with_prices.
            let aggregation = if !all_opportunities.is_empty() {
                use mev_scout_core::aggregate::{aggregate_with_prices, DexMeta};
                use mev_scout_core::coingecko::PriceCache;
                use mev_scout_core::types::PriceOracleMode;

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

            // Save results to JSON
            let created_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
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
            };
            if let Err(e) = save_results_json(&config.export_path, &run_id, &results_file) {
                tracing::warn!("Failed to save results: {}", e);
            }

            // Save USD aggregation if computed (L3/L5)
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

            // Print results
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

            // Block summary table
            render_block_summary_table(&block_stats);

            // H8 Phase 3: show mempool-only opportunity count
            let mempool_opps: usize = block_stats.iter().map(|s| s.mempool_opp_count).sum();
            if mempool_opps > 0 {
                let mempool_txs: usize = block_stats.iter().map(|s| s.pending_tx_count).sum();
                println!(
                    "  Mempool: {} pending txs, {} mempool-only opportunities visible",
                    mempool_txs, mempool_opps,
                );
            }

            // Print USD aggregation if computed (L3/L5)
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

            // Fact-check if requested
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

                // Print summary
                println!("  Opportunities: {} total, {} passed, {} failed", report.total_opportunities, report.passed, report.failed);

                // Save report
                let report_path = std::path::Path::new(&config.export_path)
                    .join(format!("{}_factcheck.json", run_id));
                if let Ok(json) = serde_json::to_string_pretty(&report) {
                    let _ = std::fs::write(&report_path, json);
                    println!("  Report saved to {}", report_path.display());
                }
            }
        }
        Command::Fetch(args) => {
            use mev_scout_core::types::ChainName;

            let chain_name: ChainName = match args.chain_args.chain.parse() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };

            let provider_configs = config.effective_provider_configs(chain_name)?;
            let chain_id = chain_name.chain_id();
            let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
            let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
            rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
            rpc.check_connection(chain_id).await?;

            let cache = SqliteStore::open(&config.db_path, chain_id)?;

            let range_mode = match validation::resolve_block_range(
                args.block_range.days,
                args.block_range.blocks,
                args.block_range.block,
                args.block_range.from_block,
                args.block_range.to_block,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };

            let resolver = RangeResolver::new(rpc.clone());
            let resolved = resolver.resolve(&range_mode).await?;

            let run_id = format!(
                "run_{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            );

            let manifest = RunManifest {
                run_id: run_id.clone(),
                chain: chain_name.to_string(),
                start_block: resolved.start_block,
                end_block: resolved.end_block,
                resolved_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                range_mode: resolved.mode_string(),
                strategies: vec![],
                flash_loan_provider: String::new(),
            };
            cache.put_manifest(&manifest)?;

            println!("Run ID: {}", run_id);
            println!("{}", resolved.summary());
            println!();

            let mut fetcher = Fetcher::new(rpc, cache);
            if let Some(pq_dir) = &config.parquet_dir {
                fetcher = fetcher.with_parquet(pq_dir);
            }
            if let Some(workers) = config.rpc_workers {
                fetcher = fetcher.with_parallelism(workers);
            }
            fetcher = fetcher.with_batch_rpc(!args.chain_args.no_batch_rpc);

            let pb = ProgressBar::new(resolved.block_count);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta})")?
                    .progress_chars("=> "),
            );

            let tick = || pb.tick();
            let summary = fetcher.fetch_range(&resolved, Some(&tick)).await?;
            pb.finish_and_clear();

            println!();
            println!("Fetch complete:");
            println!("  Total blocks: {}", summary.total_blocks);
            println!("  Fetched:      {}", summary.fetched);
            println!("  Cached:       {}", summary.cached);
            println!("  Elapsed:      {:.2}s", summary.elapsed_secs);

            if !summary.missing_after_fetch.is_empty() {
                println!(
                    "  Missing:      {} blocks — auto-refetching...",
                    summary.missing_after_fetch.len()
                );
                let refetched = fetcher
                    .auto_refetch_gaps(&summary.missing_after_fetch)
                    .await?;
                println!("  Refetched:    {}", refetched);
            }
        }
        Command::Report(args) => {
            let export_path = args.export_path.as_str();
            let dir = std::path::Path::new(export_path);

            // Determine which run to load
            let run_id = match &args.run_id {
                Some(id) => id.clone(),
                None => {
                    // Find the latest results file
                    if !dir.exists() {
                        eprintln!("Error: export directory '{}' does not exist.", export_path);
                        std::process::exit(1);
                    }
                    let mut entries: Vec<_> = std::fs::read_dir(dir)
                        .unwrap_or_else(|e| {
                            eprintln!("Error reading export directory: {}", e);
                            std::process::exit(1);
                        })
                        .filter_map(|e| e.ok())
                        .filter(|e| {
                            e.path().extension().map(|ext| ext == "json").unwrap_or(false)
                        })
                        .collect();
                    entries.sort_by_key(|e| e.path().metadata().ok().and_then(|m| m.created().ok()));
                    match entries.last() {
                        Some(entry) => {
                            let stem = entry.path().file_stem().unwrap().to_string_lossy().to_string();
                            stem
                        }
                        None => {
                            eprintln!("No results files found in '{}'", export_path);
                            std::process::exit(1);
                        }
                    }
                }
            };

            let path = dir.join(format!("{}.json", run_id));
            if !path.exists() {
                eprintln!("Error: results file not found: {}", path.display());
                std::process::exit(1);
            }

            let json_str = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path.display(), e))?;
            let results_file: ResultsFile = serde_json::from_str(&json_str)
                .map_err(|e| anyhow::anyhow!("Failed to parse '{}': {}", path.display(), e))?;

            let output_format: OutputFormat = args.output.parse().unwrap_or(OutputFormat::Table);

            match output_format {
                OutputFormat::Table => {
                    println!();
                    println!("  Run ID:        {}", results_file.run_id);
                    println!("  Chain:         {}", results_file.chain);
                    println!("  Block range:   {}–{}", results_file.start_block, results_file.end_block);
                    println!("  Mode:          {}", results_file.range_mode);
                    println!("  Strategies:    {}", results_file.strategies.join(", "));
                    println!("  Flash loan:    {}", results_file.flash_loan_provider);
                    println!("  Opportunities: {}", results_file.opportunities.len());
                    println!();

                    if results_file.opportunities.is_empty() {
                        println!("No MEV opportunities in this run.");
                    } else {
                        render_results_table(&results_file.opportunities, None);
                    }
                }
                OutputFormat::Csv => {
                    println!("block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei,confidence");
                    for opp in &results_file.opportunities {
                        println!(
                            "{},{},{},{},{},{},{}",
                            opp.block_number,
                            opp.tx_index,
                            opp.strategy,
                            opp.input_amount,
                            opp.expected_profit,
                            opp.gas_cost_wei,
                            opp.confidence.map_or("".to_string(), |c| format!("{:.2}", c)),
                        );
                    }
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&results_file)?);
                }
            }
        }
        Command::Config => {
            let toml_str = config.to_toml_string()?;
            println!("{}", toml_str);
        }
        Command::Replay(args) => {
            let (chain_name, chain_config) = match validation::validate_replay(&config) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            };

            let provider_configs = config.effective_provider_configs(chain_name)?;
            let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
            let rpc = RpcClient::from_urls(&rpc_refs, chain_config.chain_id)?;
            rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
            rpc.check_connection(chain_config.chain_id).await?;
            let cache = SqliteStore::open(&config.db_path, chain_config.chain_id)?;

            let block_num = args.block;
            let tx_index = args.tx_index.unwrap_or(usize::MAX);

            // Verify block is cached
            if !cache.has_block(block_num)? {
                eprintln!(
                    "Error: Block {} is not cached. Run `mev-scout fetch --block {}` first.",
                    block_num, block_num
                );
                std::process::exit(1);
            }

            // Load pool info for DEX interaction analysis before cache is moved into replayer
            let pool_map: HashMap<Address, mev_scout_core::pool::state::PoolInfo> = if args.analyze {
                let mut map = HashMap::new();
                if let Ok(pools) = cache.list_discovered_pools() {
                    for pool in pools {
                        map.insert(pool.address, pool);
                    }
                }
                tracing::info!("Loaded {} pools for DEX analysis", map.len());
                map
            } else {
                HashMap::new()
            };

            let replayer = BlockReplayer::new(
                tokio::runtime::Handle::current(),
                cache,
                rpc,
                chain_config.chain_id,
            );
            let txs = replayer
                .load_txs(block_num)
                .map_err(|e| anyhow::anyhow!("Failed to load txs for block {}: {}", block_num, e))?;
            let actual_count = txs.len();
            let end_tx = tx_index.min(actual_count.saturating_sub(1));

            println!(
                "Replaying block {} on chain {} ({} txs, replaying 0..{})",
                block_num, chain_name, actual_count, end_tx
            );
            println!();

            let start = std::time::Instant::now();
            let (_snapshot, results) = replayer
                .replay_to(block_num, end_tx)
                .map_err(|e| anyhow::anyhow!("Replay failed for block {}: {}", block_num, e))?;
            let elapsed = start.elapsed();

            println!(
                "  {:<4} {:<66} {:<6} {:<8} receipt",
                "idx", "tx_hash", "status", "gas_used",
            );
            println!("  {}", "─".repeat(100));

            let mut matched = 0u64;
            let mut total = 0u64;

            for r in &results {
                let status_str = if r.status { "ok" } else { "fail" };
                let receipt_str = match &r.error {
                    None => {
                        matched += 1;
                        "✓".to_string()
                    }
                    Some(_) => "✗".to_string(),
                };
                total += 1;

                println!(
                    "  {:<4} {:<66} {:<6} {:<8} {}",
                    r.index, r.tx_hash, status_str, r.gas_used, receipt_str
                );

                if args.analyze {
                    let interactions: Vec<String> = r.logs.iter().filter_map(|log| {
                        pool_map.get(&log.address).map(|info| {
                            let event_type = if log.topics.is_empty() {
                                "Unknown"
                            } else {
                                let t0 = log.topics[0];
                                if t0 == keccak256(b"Swap(address,uint256,uint256,uint256,uint256,address)") {
                                    "Swap"
                                } else if t0 == keccak256(b"Sync(uint112,uint112)") {
                                    "Sync"
                                } else if t0 == keccak256(b"Swap(address,address,int256,int256,uint160,uint128,int24)") {
                                    "Swap"
                                } else if t0 == keccak256(b"Mint(address,address,int24,int24,uint128,uint256,uint256)") {
                                    "Mint"
                                } else if t0 == keccak256(b"Burn(address,address,int24,int24,uint128,uint256,uint256)") {
                                    "Burn"
                                } else {
                                    "Unknown"
                                }
                            };
                            let name = info.name.clone().unwrap_or_else(|| format!("{}", info.address));
                            format!("{} — {}", name, event_type)
                        })
                    }).collect();

                    if interactions.is_empty() {
                        println!("         (no DEX interactions)");
                    } else {
                        println!("         DEX interactions:");
                        for (j, line) in interactions.iter().enumerate() {
                            let prefix = if j == interactions.len() - 1 { "         └ " } else { "         ├ " };
                            println!("{}{}", prefix, line);
                        }
                    }
                }
            }

            println!();
            let pct = if total > 0 {
                (matched as f64 / total as f64) * 100.0
            } else {
                100.0
            };
            println!(
                "  Receipt verification: {}/{} match ({:.1}%) — {:.2}s",
                matched, total, pct, elapsed.as_secs_f64()
            );

            if pct < 99.0 {
                tracing::warn!(
                    "Receipt match rate {:.1}% is below 99% threshold",
                    pct
                );
            }
        }
        Command::Discover(args) => {
            use mev_scout_core::types::ChainName;

            let chain_name: ChainName = match args.chain_args.chain.parse() {
                Ok(c) => c,
                Err(_) => {
                    eprintln!("Error: unknown chain '{}'", args.chain_args.chain);
                    std::process::exit(1);
                }
            };
            let chain_config = config.chains.get(&args.chain_args.chain);
            let chain_id = chain_name.chain_id();
            let provider_configs = config.effective_provider_configs(chain_name)?;
            let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _)| u.as_str()).collect();
            let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
            rpc.with_provider_rps(&provider_configs.iter().map(|(_, r)| r.unwrap_or(1.0)).collect::<Vec<_>>()).await;
            rpc.check_connection(chain_id).await?;

            let from = args.from_block;
            let to = args.to_block;
            let batch_size = args.batch_size;
            let v2_fee = chain_config.and_then(|c| c.uniswap_v2_default_fee);
            let vault = chain_config
                .and_then(|c| c.balancer_vault.as_ref())
                .and_then(|s| s.parse::<alloy::primitives::Address>().ok());
            let registry = chain_config
                .and_then(|c| c.curve_registry.as_ref())
                .and_then(|s| s.parse::<alloy::primitives::Address>().ok());

            // Resolve factory addresses from args, config, or defaults
            let v2_factories: Vec<alloy::primitives::Address> = if let Some(v2_str) = &args.v2_factories {
                v2_str.split(',').filter_map(|s| s.trim().parse().ok()).collect()
            } else if let Some(factories) = chain_config.and_then(|c| c.uniswap_v2_factories.as_ref()) {
                factories.iter().filter_map(|s| s.parse().ok()).collect()
            } else {
                chain_name.default_uniswap_v2_factories().iter().filter_map(|s| s.parse().ok()).collect()
            };
            let v3_factories: Vec<alloy::primitives::Address> = if let Some(v3_str) = &args.v3_factory {
                v3_str.split(',').filter_map(|s| s.trim().parse().ok()).collect()
            } else if let Some(factories) = chain_config.and_then(|c| c.uniswap_v3_factories.as_ref()) {
                factories.iter().filter_map(|s| s.parse().ok()).collect()
            } else {
                chain_name.default_uniswap_v3_factories().iter().filter_map(|s| s.parse().ok()).collect()
            };

            println!();
            println!("  Pool Discovery (unified)");
            println!("  Chain:       {}", args.chain_args.chain);
            println!("  Block range: {from}–{to}");
            println!("  DEX activity scan: yes");
            if !v2_factories.is_empty() || !v3_factories.is_empty() || vault.is_some() || registry.is_some() {
                println!("  Factory event scan: yes ({} V2, {} V3, Balancer: {}, Curve: {})",
                    v2_factories.len(), v3_factories.len(), vault.is_some(), registry.is_some());
            }
            if args.no_save {
                println!("  Save to cache: no");
            }
            println!();

            let cache = SqliteStore::open(&config.db_path, chain_id)?;

            match mev_scout_core::pool::discovery::discover_and_cache(
                &rpc,
                &cache,
                from,
                to,
                batch_size,
                v2_fee,
                vault,
                if v2_factories.is_empty() { None } else { Some(v2_factories.as_slice()) },
                if v3_factories.is_empty() { None } else { Some(v3_factories.as_slice()) },
                None, // v2_factory_fees
                registry,
            )
            .await
            {
                Ok((pools, active_blocks)) => {
                    for p in &pools {
                        match p.dex_type {
                            mev_scout_core::pool::dex_type::DexType::UniswapV2 => {
                                println!("  V2  {}  token0={}  token1={}", p.address, p.token0, p.token1);
                            }
                            mev_scout_core::pool::dex_type::DexType::UniswapV3 => {
                                println!("  V3  {}  token0={}  token1={}  fee={}  tickSpacing={}",
                                    p.address, p.token0, p.token1, p.fee, p.tick_spacing.unwrap_or(0));
                            }
                            mev_scout_core::pool::dex_type::DexType::Balancer => {
                                println!("  Balancer  {}", p.address);
                            }
                            mev_scout_core::pool::dex_type::DexType::Curve => {
                                println!("  Curve  {}", p.address);
                            }
                        }
                    }
                    println!();
                    println!("  Found {} pool(s) in {} active blocks", pools.len(), active_blocks.len());
                    if args.no_save {
                        println!("  (not saved to cache)");
                    } else {
                        println!("  Saved {} pool(s) to cache: {}", pools.len(), config.db_path);
                    }
                }
                Err(e) => {
                    eprintln!("  Pool discovery failed: {e:#}");
                }
            }
        }
        Command::FactCheck(args) => {
            let export_path = &config.export_path;
            let dir = std::path::Path::new(export_path);
            let run_id = &args.run_id;

            let path = dir.join(format!("{}.json", run_id));
            if !path.exists() {
                eprintln!("Error: results file not found: {}", path.display());
                std::process::exit(1);
            }

            let json_str = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path.display(), e))?;
            let results_file: ResultsFile = serde_json::from_str(&json_str)
                .map_err(|e| anyhow::anyhow!("Failed to parse '{}': {}", path.display(), e))?;

            println!();
            println!("  Fact-Check Report for {}", run_id);
            println!("  Chain:         {}", results_file.chain);
            println!("  Block range:   {}–{}", results_file.start_block, results_file.end_block);
            println!("  Opportunities: {}", results_file.opportunities.len());
            println!();

            let checks = verify_opportunities(&results_file.opportunities, None);
            let passed = checks.iter().filter(|c| c.profit_gt_gas).count();
            let failed = checks.len().saturating_sub(passed);

            // Print per-opportunity checks
            let mut check_table = Table::new();
            check_table.set_header(vec!["Block", "Tx", "Strategy", "Profit > Gas", "Victim Tx", "Backrun Tx"]);
            for c in &checks {
                let profit_check = if c.profit_gt_gas { "✓" } else { "✗" };
                let victim_str = c.victim_tx_index.map(|i| i.to_string()).unwrap_or_default();
                let backrun_str = c.backrun_tx_index.map(|i| i.to_string()).unwrap_or_default();
                check_table.add_row(vec![
                    format!("{}", c.block_number),
                    format!("{}", c.tx_index),
                    c.strategy.clone(),
                    profit_check.to_string(),
                    victim_str,
                    backrun_str,
                ]);
            }
            println!("{}", check_table);
            println!();
            println!("  Summary: {} total, {} passed, {} failed", checks.len(), passed, failed);

            // Save report
            let report = FactCheckReport {
                run_id: run_id.clone(),
                chain: results_file.chain.clone(),
                block_count: (results_file.end_block.saturating_sub(results_file.start_block) + 1) as usize,
                total_opportunities: results_file.opportunities.len(),
                passed,
                failed,
                block_summaries: Vec::new(),
                opportunity_checks: checks,
            };
            let report_path = dir.join(format!("{}_factcheck.json", run_id));
            if let Ok(json) = serde_json::to_string_pretty(&report) {
                let _ = std::fs::write(&report_path, json);
                println!("  Report saved to {}", report_path.display());
            }
        }
    }

    Ok(())
}


