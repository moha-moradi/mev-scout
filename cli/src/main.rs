
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use comfy_table::Table;
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::EnvFilter;

use mev_scout_core::cache::{CacheStore, RunManifest};
use mev_scout_core::cli::{Cli, Command};
use mev_scout_core::config::{CliOverrides, Config};
use mev_scout_core::fetch::Fetcher;

use mev_scout_core::pool::state::PoolManager;
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
            flash_loan_provider: Some(args.flash_loan_provider.clone()),
            strategies: Some(args.strategies.clone()),
            gas_model: Some(args.gas_model.clone()),
            gas_limit: Some(args.gas_limit),
            priority_fee_gwei: Some(args.priority_fee),
            output: Some(args.output.clone()),
            export_path: Some(args.export_path.clone()),
            cache_dir: Some(args.cache_dir.clone()),
            coingecko_api_key: None,
        },
        Command::Fetch(args) => CliOverrides {
            days: args.block_range.days,
            blocks: args.block_range.blocks,
            block: args.block_range.block,
            from_block: args.block_range.from_block,
            to_block: args.block_range.to_block,
            chain: Some(args.chain_args.chain.clone()),
            rpc_url: args.chain_args.rpc_url.clone(),
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            cache_dir: Some(args.cache_dir.clone()),
            coingecko_api_key: None,
        },
        Command::Replay(args) => CliOverrides {
            days: None,
            blocks: None,
            block: Some(args.block),
            from_block: None,
            to_block: None,
            chain: Some(args.chain_args.chain.clone()),
            rpc_url: args.chain_args.rpc_url.clone(),
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            cache_dir: Some(args.cache_dir.clone()),
            coingecko_api_key: None,
        },
        Command::Report(args) => CliOverrides {
            days: None,
            blocks: None,
            block: None,
            from_block: None,
            to_block: None,
            chain: None,
            rpc_url: None,
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: Some(args.output.clone()),
            export_path: Some(args.export_path.clone()),
            cache_dir: None,
            coingecko_api_key: None,
        },
        Command::Config => CliOverrides {
            days: None,
            blocks: None,
            block: None,
            from_block: None,
            to_block: None,
            chain: None,
            rpc_url: None,
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            cache_dir: None,
            coingecko_api_key: None,
        },
        Command::Discover(args) => CliOverrides {
            days: None,
            blocks: None,
            block: None,
            from_block: Some(args.from_block),
            to_block: Some(args.to_block),
            chain: Some(args.chain_args.chain.clone()),
            rpc_url: args.chain_args.rpc_url.clone(),
            flash_loan_provider: None,
            strategies: None,
            gas_model: None,
            gas_limit: None,
            priority_fee_gwei: None,
            output: None,
            export_path: None,
            cache_dir: Some(args.cache_dir.clone()),
            coingecko_api_key: None,
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

fn render_results_table(all_opportunities: &[mev_scout_core::mev::opportunity::MevOpportunity]) {
    let mut table = Table::new();
    table.set_header(vec![
        "Block", "Tx", "Strategy",
        "Input", "Profit (token_out)", "Gas (wei)",
    ]);

    for opp in all_opportunities {
        table.add_row(vec![
            format!("{}", opp.block_number),
            format!("{}", opp.tx_index),
            format!("{}", opp.strategy),
            format!("{}", opp.input_amount),
            format!("{}", opp.expected_profit),
            format!("{}", opp.gas_cost_wei),
        ]);
    }

    println!("{table}");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    setup_logging(cli.verbose, cli.quiet);

    // Load config
    let config_path = cli.config.as_deref().unwrap_or("mev-scout.toml");
    let mut config = Config::load_or_default(config_path);

    // Merge CLI overrides
    let overrides = build_overrides(&cli);
    config.merge_cli(&overrides);

    // Dispatch
    match &cli.command {
        Command::Run(_) => {
            let validation_result = match validation::validate_and_resolve(&config) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            };
            print_startup_plan(&validation_result, &config);

            let rpc_url = config.effective_rpc_url(validation_result.chain_name);
            let rpc = RpcClient::new(&rpc_url, validation_result.chain_config.chain_id)?;
            rpc.check_connection(validation_result.chain_config.chain_id).await?;
            let cache = CacheStore::open(&config.cache_dir, validation_result.chain_config.chain_id)?;

            // Resolve block range
            let resolver = RangeResolver::new(rpc.clone());
            let resolved = match resolver.resolve(&validation_result.range_mode).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: failed to resolve block range: {}", e);
                    std::process::exit(1);
                }
            };

            // Run pool discovery if configured
            if let Some(start_block) = validation_result.chain_config.pool_discovery_start_block {
                let mut v2_addrs = Vec::new();
                if let Some(factories) = &validation_result.chain_config.uniswap_v2_factories {
                    for s in factories {
                        if let Ok(addr) = s.parse::<alloy::primitives::Address>() {
                            v2_addrs.push(addr);
                        }
                    }
                }
                let mut v3_addrs = Vec::new();
                if let Some(factory) = &validation_result.chain_config.uniswap_v3_factory {
                    if let Ok(addr) = factory.parse::<alloy::primitives::Address>() {
                        v3_addrs.push(addr);
                    }
                }
                let batch_size = validation_result.chain_config.pool_discovery_batch_size.unwrap_or(10);
                if !v2_addrs.is_empty() || !v3_addrs.is_empty() {
                    let discovered = mev_scout_core::pool::discovery::discover_pools(
                        &rpc, &cache, &v2_addrs, &v3_addrs, start_block,
                        resolved.start_block.saturating_sub(1), batch_size,
                    ).await?;
                    if discovered > 0 {
                        tracing::info!("Discovered {} new pools", discovered);
                    }
                }
            }

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

            // Init pool manager (needs cache before it's moved into replayer)
            let mut pool_manager = PoolManager::new();
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
            let gas_config = GasConfig {
                gas_limit: config.gas_limit,
                gas_model: validation_result.gas_model,
                priority_fee_gwei: config.priority_fee_gwei,
            };
            let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config);
            let start = std::time::Instant::now();
            let all_opportunities = runner.run_range(&resolved)?;
            let elapsed = start.elapsed();

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

            // Print results
            if all_opportunities.is_empty() {
                println!("No MEV opportunities detected in the specified range.");
            } else {
                println!(
                    "\nDetected {} MEV opportunity(ies) in {:.2}s:\n",
                    all_opportunities.len(),
                    elapsed.as_secs_f64()
                );
                render_results_table(&all_opportunities);
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

            let rpc_url = match &args.chain_args.rpc_url {
                Some(url) if !url.trim().is_empty() && (url.starts_with("http://") || url.starts_with("https://")) => url.clone(),
                Some(url) => {
                    eprintln!("Error: --rpc URL '{}' must be non-empty and start with http:// or https://.", url);
                    std::process::exit(1);
                }
                None => chain_name.public_rpc_url().to_string(),
            };

            let chain_id = chain_name.chain_id();
            let rpc = RpcClient::new(&rpc_url, chain_id)?;
            rpc.check_connection(chain_id).await?;

            let cache = CacheStore::open(&args.cache_dir, chain_id)?;

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

            let fetcher = Fetcher::new(rpc, cache);

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
                        render_results_table(&results_file.opportunities);
                    }
                }
                OutputFormat::Csv => {
                    println!("block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei");
                    for opp in &results_file.opportunities {
                        println!(
                            "{},{},{},{},{},{}",
                            opp.block_number,
                            opp.tx_index,
                            opp.strategy,
                            opp.input_amount,
                            opp.expected_profit,
                            opp.gas_cost_wei,
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

            let rpc_url = config.effective_rpc_url(chain_name);
            let rpc = RpcClient::new(&rpc_url, chain_config.chain_id)?;
            rpc.check_connection(chain_config.chain_id).await?;
            let cache = CacheStore::open(&config.cache_dir, chain_config.chain_id)?;

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
            use alloy::primitives::Address;
            use mev_scout_core::pool::discovery::{discover_v2_pools, discover_v3_pools};
            use mev_scout_core::types::ChainName;

            let chain_name: ChainName = match args.chain_args.chain.parse() {
                Ok(c) => c,
                Err(_) => {
                    eprintln!("Error: unknown chain '{}'", args.chain_args.chain);
                    std::process::exit(1);
                }
            };
            let rpc_url = config.effective_rpc_url(chain_name);
            let chain_config = config.chains.get(&args.chain_args.chain);
            let chain_id = chain_name.chain_id();
            let rpc = RpcClient::new(&rpc_url, chain_id)?;
            rpc.check_connection(chain_id).await?;

            let mut v2_addrs = Vec::new();
            if let Some(v2_str) = &args.v2_factories {
                for s in v2_str.split(',') {
                    if let Ok(addr) = s.trim().parse::<Address>() {
                        v2_addrs.push(addr);
                    }
                }
            } else if let Some(factories) = chain_config.and_then(|c| c.uniswap_v2_factories.as_ref()) {
                for s in factories {
                    if let Ok(addr) = s.parse::<Address>() {
                        v2_addrs.push(addr);
                    }
                }
            } else {
                for s in chain_name.default_uniswap_v2_factories() {
                    if let Ok(addr) = s.parse::<Address>() {
                        v2_addrs.push(addr);
                    }
                }
            }
            let mut v3_addrs = Vec::new();
            if let Some(v3_str) = &args.v3_factory {
                if let Ok(addr) = v3_str.trim().parse::<Address>() {
                    v3_addrs.push(addr);
                }
            } else if let Some(factory) = chain_config.and_then(|c| c.uniswap_v3_factory.as_ref()) {
                if let Ok(addr) = factory.parse::<Address>() {
                    v3_addrs.push(addr);
                }
            } else if let Some(factory) = chain_name.default_uniswap_v3_factory() {
                if let Ok(addr) = factory.parse::<Address>() {
                    v3_addrs.push(addr);
                }
            }

            if v2_addrs.is_empty() && v3_addrs.is_empty() {
                eprintln!("Error: no factory addresses found for chain '{}'", args.chain_args.chain);
                std::process::exit(1);
            }

            let from = args.from_block;
            let to = args.to_block;
            let batch_size = args.batch_size;

            println!();
            println!("  Pool Discovery");
            println!("  Chain:       {}", args.chain_args.chain);
            println!("  Block range: {}–{}", from, to);
            println!("  V2 factories: {}", v2_addrs.len());
            println!("  V3 factories: {}", v3_addrs.len());
            if args.save {
                println!("  Save to cache: yes");
            }
            println!();

            let mut all_pools = Vec::new();

            // V2 discovery batched
            for &factory in &v2_addrs {
                let mut current = from;
                while current <= to {
                    let end = (current + batch_size - 1).min(to);
                    match discover_v2_pools(&rpc, factory, current, end).await {
                        Ok(pools) => {
                            for p in &pools {
                                println!(
                                    "  V2  {}  token0={}  token1={}",
                                    p.address, p.token0, p.token1
                                );
                            }
                            all_pools.extend(pools);
                        }
                        Err(e) => {
                            eprintln!("  Error scanning V2 factory {factory} blocks {current}..{end}: {e}");
                        }
                    }
                    if end == to { break; }
                    current = end + 1;
                }
            }

            // V3 discovery batched
            for &factory in &v3_addrs {
                let mut current = from;
                while current <= to {
                    let end = (current + batch_size - 1).min(to);
                    match discover_v3_pools(&rpc, factory, current, end).await {
                        Ok(pools) => {
                            for p in &pools {
                                println!(
                                    "  V3  {}  token0={}  token1={}  fee={}  tickSpacing={}",
                                    p.address, p.token0, p.token1, p.fee,
                                    p.tick_spacing.unwrap_or(0),
                                );
                            }
                            all_pools.extend(pools);
                        }
                        Err(e) => {
                            eprintln!("  Error scanning V3 factory {factory} blocks {current}..{end}: {e}");
                        }
                    }
                    if end == to { break; }
                    current = end + 1;
                }
            }

            println!();
            println!("  Found {} pool(s)", all_pools.len());

            // Save to cache if requested
            if args.save {
                let cache = CacheStore::open(&args.cache_dir, chain_id)?;
                for pool in &all_pools {
                    let info: mev_scout_core::pool::state::PoolInfo = pool.clone().into();
                    let _ = cache.put_discovered_pool(&info);
                }
                // Save cursors
                for &factory in &v2_addrs {
                    let _ = cache.put_discovery_cursor(&factory, to);
                }
                for &factory in &v3_addrs {
                    let _ = cache.put_discovery_cursor(&factory, to);
                }
                println!("  Saved to cache: {}", args.cache_dir);
            }
        }
    }

    Ok(())
}


