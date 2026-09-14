use alloy::primitives::Address;
use anyhow::Context;
use std::time::{Duration, Instant};

use crate::cli::LiveArgs;
use crate::display::{
    persist_opportunities_to_explorer, persist_rejections_to_explorer, render_results_table,
};
use crate::rpc_setup::init_rpc;
use mev_scout_core::cache::{RunManifest, SqliteStore};
use mev_scout_core::config::validation::{self, ValidationResult};
use mev_scout_core::config::{Config, ProviderConfig};
use mev_scout_core::fetch::Fetcher;
use mev_scout_core::pipeline::BacktestRunner;
use mev_scout_core::pool::state::PoolManager;
use mev_scout_core::replay::BlockReplayer;
use mev_scout_core::resolver::ResolvedRange;
use mev_scout_core::types::{GasConfig, RangeMode, ResultsFile};
use mev_scout_core::utils::epoch_secs;

pub fn parse_duration_str(s: &str) -> anyhow::Result<Duration> {
    humantime::parse_duration(s)
        .with_context(|| format!("invalid --duration '{s}' (expected e.g. 90s, 15m, 1h)"))
}

pub fn deadline_from(
    loop_enabled: bool,
    duration: Option<&str>,
    now: Instant,
) -> anyhow::Result<Option<Instant>> {
    match duration {
        Some(d) => {
            if !loop_enabled {
                anyhow::bail!("--duration requires --loop");
            }
            Ok(Some(now + parse_duration_str(d)?))
        }
        // No deadline: `live --loop` runs until stopped; one-shot `live` runs a
        // single pass (its duration is bounded by the chain head itself).
        None => Ok(None),
    }
}

pub async fn cmd_live(config: &Config, args: &LiveArgs) -> anyhow::Result<()> {
    let progress_json = match args.progress.as_deref() {
        None => false,
        Some("json") => true,
        Some(other) => anyhow::bail!("unsupported --progress format '{other}' (only 'json')"),
    };
    if args.max_blocks.is_some() && !args.r#loop {
        anyhow::bail!("--max-blocks requires --loop");
    }
    let deadline = deadline_from(args.r#loop, args.duration.as_deref(), Instant::now())?;
    let validation = validation::validate_live(config).context("invalid configuration")?;

    let setup = init_rpc(config, validation.chain_name, true).await?;
    let cache = SqliteStore::open(config.effective_db_path(&validation.chain_name))?;

    let pool_addresses: Vec<Address> = cache
        .list_discovered_pools()
        .unwrap_or_default()
        .iter()
        .map(|p| p.address)
        .collect();

    let gas_config = GasConfig {
        gas_limit: config.gas.gas_limit,
        gas_model: validation.gas_model,
        priority_fee_gwei: config.gas.priority_fee_gwei,
        flash_loan_provider: validation.flash_loan_provider,
        winning_bid_premium: 0.0,
        percentile_gas_price: None,
        calibration: Default::default(),
    };

    let mode_label = if args.r#loop {
        "continuous"
    } else {
        "one-shot"
    };
    println!(
        "Live mode ({}) — polling every {}ms",
        mode_label, args.poll_interval_ms
    );

    let mut ctx = LiveContext::new(
        config,
        &validation,
        setup,
        cache,
        pool_addresses,
        args,
        gas_config,
    )
    .await?;

    if progress_json {
        println!("{}", serde_json::json!({ "stage": "resolve" }));
    }

    if args.r#loop {
        run_loop(&mut ctx, deadline).await
    } else {
        run_once(&mut ctx).await
    }
}

/// Session state shared by both `live` modes: RPC plumbing, pool bootstrap,
/// and the backtest runner are built once here so `run_once`/`run_loop` stay
/// thin orchestration over the same context (no 9-positional-parameter
/// signatures, no duplicated setup blocks).
struct LiveContext<'a> {
    config: &'a Config,
    validation: &'a ValidationResult,
    rpc: mev_scout_core::rpc::RpcClient,
    provider_configs: Vec<ProviderConfig>,
    cache: SqliteStore,
    pool_addresses: Vec<Address>,
    args: &'a LiveArgs,
    runner: BacktestRunner,
    /// Chain tip captured at context construction.
    tip: u64,
}

impl<'a> LiveContext<'a> {
    async fn new(
        config: &'a Config,
        validation: &'a ValidationResult,
        setup: crate::rpc_setup::RpcSetup,
        cache: SqliteStore,
        pool_addresses: Vec<Address>,
        args: &'a LiveArgs,
        gas_config: GasConfig,
    ) -> anyhow::Result<LiveContext<'a>> {
        let crate::rpc_setup::RpcSetup {
            rpc,
            provider_configs,
        } = setup;
        let tip = rpc
            .get_block_number()
            .await
            .context("failed to get chain tip")?;

        let mut pool_manager = PoolManager::new();
        pool_manager.set_max_pairs_per_token(config.backtest.max_pairs_per_token);
        pool_manager.set_concurrency_limit(provider_configs.len() as u32);
        pool_manager.use_latest();
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
            if args.progress.as_deref() == Some("json") {
                println!("{}", serde_json::json!({ "stage": "pool_init" }));
            }
            BacktestRunner::init_pools(
                &mut pool_manager,
                &rpc,
                tip.saturating_sub(1),
                Some(&cache),
            )
            .await;
        }

        let replayer = BlockReplayer::new(
            tokio::runtime::Handle::current(),
            cache.clone(),
            rpc.clone(),
            validation.chain_config.chain_id,
        );
        let mut runner = BacktestRunner::new(replayer, pool_manager, gas_config)
            .with_proximity_window(config.backtest.proximity_window)
            .with_min_profit_wei(config.backtest.min_profit_wei)
            .with_record_rejections(args.record_rejections);

        if let Some(aave_pool_str) = &validation.chain_config.aave_v3_pool {
            if let Ok(aave_pool) = aave_pool_str.parse::<Address>() {
                runner
                    .prefetch_aave_reserves(aave_pool, tip.saturating_sub(1))
                    .await;
            }
        }

        Ok(LiveContext {
            config,
            validation,
            rpc,
            provider_configs,
            cache,
            pool_addresses,
            args,
            runner,
            tip,
        })
    }

/// Fetch `resolved`'s blocks into the cache (relevant-only when the pool
    /// set is already known, full-range otherwise).
    async fn fetch_blocks<F: Fn() + Sync>(
        &self,
        resolved: &ResolvedRange,
        tick: Option<&F>,
    ) -> anyhow::Result<()> {
        let mut fetcher = Fetcher::new(self.rpc.clone(), self.cache.clone());
        fetcher = fetcher.with_parallelism(self.provider_configs.len());
        if !self.pool_addresses.is_empty() {
            fetcher
                .fetch_relevant(resolved, &self.pool_addresses, tick)
                .await
                .map(|_| ())
        } else {
            fetcher.fetch_range(resolved, tick).await.map(|_| ())
        }
    }

    /// Backtest the resolved snapshot at the RPC-detected state horizon.
    async fn run_blocks(
        &mut self,
        resolved: &ResolvedRange,
        progress: Option<&dyn Fn(u64, u64)>,
    ) -> anyhow::Result<(
        Vec<mev_scout_core::types::MevOpportunity>,
        Vec<mev_scout_core::pipeline::BlockReplayStats>,
    )> {
        let state_horizon = self.rpc.detect_state_horizon(resolved.end_block).await;
        let (opps, stats, _modes) = self.runner.run_range_hybrid(resolved, state_horizon, progress)?;
        Ok((opps, stats))
    }

    /// Persist a pass to SQLite (manifest) and the explorer store
    /// (opportunities + rejections).
    fn persist_results(
        &mut self,
        resolved: &ResolvedRange,
        opps: &[mev_scout_core::types::MevOpportunity],
        run_id: &str,
    ) {
        let results_file = ResultsFile {
            run_id: run_id.to_string(),
            chain: self.validation.chain_name.to_string(),
            start_block: resolved.start_block,
            end_block: resolved.end_block,
            range_mode: "live".to_string(),
            strategies: self
                .validation
                .strategies
                .iter()
                .map(|s| s.to_string())
                .collect(),
            flash_loan_provider: self.validation.flash_loan_provider.to_string(),
            resolved_at: epoch_secs(),
            created_at: epoch_secs(),
            opportunities: opps.to_vec(),
        };
        // Execution history lives only in SQLite: manifest in the cache
        // store's `run_manifests`, opportunities/rejections in the explorer
        // store.
        let manifest = RunManifest {
            run_id: run_id.to_string(),
            chain: self.validation.chain_name.to_string(),
            start_block: resolved.start_block,
            end_block: resolved.end_block,
            resolved_at: epoch_secs(),
            range_mode: "live".to_string(),
            strategies: results_file.strategies.clone(),
            flash_loan_provider: results_file.flash_loan_provider.clone(),
        };
        if let Err(e) = self.cache.put_manifest(&manifest) {
            tracing::warn!("run-manifest persist failed: {e}");
        }
        persist_opportunities_to_explorer(
            self.config,
            self.validation.chain_name,
            run_id,
            &results_file,
        );
        let rejections = self.runner.take_rejections();
        persist_rejections_to_explorer(
            self.config,
            self.validation.chain_name,
            run_id,
            &rejections,
        );
    }
}

async fn run_once(ctx: &mut LiveContext<'_>) -> anyhow::Result<()> {
    let progress_json = ctx.args.progress.as_deref() == Some("json");
    let run_id = format!("live_{}", epoch_secs());
    let tip = ctx.tip;
    println!("Run ID: {run_id}");
    println!("Latest block: {tip}");

    let resolved = ResolvedRange {
        start_block: tip,
        end_block: tip,
        block_count: 1,
        mode: RangeMode::Single(tip),
    };

    let start = std::time::Instant::now();
    let fetch_done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let tick = move || {
        if progress_json {
            let d = fetch_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            println!(
                "{}",
                serde_json::json!({ "stage": "fetch", "done": d, "total": 1 })
            );
        }
    };
    ctx.fetch_blocks(&resolved, Some(&tick)).await?;
    let detect_progress: Option<Box<dyn Fn(u64, u64)>> = if progress_json {
        Some(Box::new(|done, total| {
            println!(
                "{}",
                serde_json::json!({ "stage": "detect", "done": done, "total": total })
            );
        }))
    } else {
        None
    };
    let (opps, stats) = ctx
        .run_blocks(&resolved, detect_progress.as_deref())
        .await?;
    let elapsed = start.elapsed();
    ctx.persist_results(&resolved, &opps, &run_id);

    if progress_json {
        println!(
            "{}",
            serde_json::json!({
                "stage": "complete",
                "run_id": run_id,
                "ops": opps.len(),
                "elapsed_ms": elapsed.as_millis(),
            })
        );
    }

    println!("\nBlock {} — {} opportunity(ies) detected", tip, opps.len());
    if opps.is_empty() {
        println!("No MEV opportunities in this block.");
    } else {
        render_results_table(&opps, Some(ctx.runner.pool_manager()));
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

async fn run_loop(ctx: &mut LiveContext<'_>, deadline: Option<Instant>) -> anyhow::Result<()> {
    let progress_json = ctx.args.progress.as_deref() == Some("json");
    let max_blocks = ctx.args.max_blocks;
    let mut last_block = ctx.tip;
    println!("Starting from block {} — Ctrl+C to stop\n", last_block);

    const MAX_CONSECUTIVE_FAILURES: u32 = 5;
    let mut consecutive_failures: u32 = 0;
    let mut blocks_processed: u64 = 0;
    let mut total_txs_scanned: usize = 0;
    let mut total_opportunities: usize = 0;

    loop {
        tokio::time::sleep(Duration::from_millis(ctx.args.poll_interval_ms)).await;

        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                break;
            }
        }
        if let Some(mb) = max_blocks {
            if blocks_processed >= mb {
                break;
            }
        }

        let current_tip = match ctx.rpc.get_block_number().await {
            Ok(n) => n,
            Err(e) => {
                consecutive_failures += 1;
                tracing::warn!(
                    "Failed to get block number ({}/{}): {}",
                    consecutive_failures,
                    MAX_CONSECUTIVE_FAILURES,
                    e
                );
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    anyhow::bail!(
                        "giving up after {MAX_CONSECUTIVE_FAILURES} consecutive RPC failures"
                    );
                }
                continue;
            }
        };

        if current_tip <= last_block {
            continue;
        }

        let from_block = last_block + 1;
        let block_count = current_tip - from_block + 1;
        let resolved = ResolvedRange {
            start_block: from_block,
            end_block: current_tip,
            block_count,
            mode: RangeMode::Range(from_block, current_tip),
        };

        let run_id = format!("live_{}", epoch_secs());
        println!("Run ID: {run_id}");
        let pass_start = std::time::Instant::now();

        let fetch_done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tick = move || {
            if progress_json {
                let d = fetch_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                println!(
                    "{}",
                    serde_json::json!({ "stage": "fetch", "done": d, "total": block_count })
                );
            }
        };
        if let Err(e) = ctx.fetch_blocks(&resolved, Some(&tick)).await {
            consecutive_failures += 1;
            tracing::warn!(
                "Fetch failed for blocks {}–{} ({}/{}): {} — will retry same range",
                from_block,
                current_tip,
                consecutive_failures,
                MAX_CONSECUTIVE_FAILURES,
                e
            );
            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                anyhow::bail!(
                    "giving up after {MAX_CONSECUTIVE_FAILURES} consecutive fetch/backtest failures"
                );
            }
            continue;
        }

        let detect_progress: Option<Box<dyn Fn(u64, u64)>> = if progress_json {
            Some(Box::new(move |done, total| {
                println!(
                    "{}",
                    serde_json::json!({ "stage": "detect", "done": done, "total": total })
                );
            }))
        } else {
            None
        };
        let (opps, stats) = match ctx
            .run_blocks(&resolved, detect_progress.as_deref())
            .await
        {
            Ok(r) => r,
            Err(e) => {
                consecutive_failures += 1;
                tracing::warn!(
                    "Backtest failed for blocks {}–{} ({}/{}): {} — will retry same range",
                    from_block,
                    current_tip,
                    consecutive_failures,
                    MAX_CONSECUTIVE_FAILURES,
                    e
                );
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    anyhow::bail!(
                        "giving up after {MAX_CONSECUTIVE_FAILURES} consecutive fetch/backtest failures"
                    );
                }
                continue;
            }
        };
        let pass_elapsed = pass_start.elapsed();

        let txs_scanned = stats.iter().map(|s| s.total_tx_count).sum::<usize>();

        if opps.is_empty() {
            println!(
                "Block {}–{}: no opportunities ({} txs)",
                from_block, current_tip, txs_scanned,
            );
        } else {
            println!(
                "Block {}–{}: {} opportunity(ies)",
                from_block,
                current_tip,
                opps.len(),
            );
            render_results_table(&opps, Some(ctx.runner.pool_manager()));
        }

        ctx.persist_results(&resolved, &opps, &run_id);
        if progress_json {
            println!(
                "{}",
                serde_json::json!({
                    "stage": "complete",
                    "run_id": run_id,
                    "ops": opps.len(),
                    "elapsed_ms": pass_elapsed.as_millis(),
                })
            );
        }
        ctx.runner.advance_to(current_tip);
        last_block = current_tip;
        blocks_processed += resolved.block_count;
        total_txs_scanned += txs_scanned;
        total_opportunities += opps.len();
        consecutive_failures = 0;
        if let Some(mb) = max_blocks {
            if blocks_processed >= mb {
                break;
            }
        }
    }

    println!();
    println!("Session summary:");
    println!("  Blocks processed: {}", blocks_processed);
    println!("  Txs scanned:      {}", total_txs_scanned);
    println!("  Opportunities:    {}", total_opportunities);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_humantime_suffixes() {
        assert_eq!(parse_duration_str("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration_str("15m").unwrap(), Duration::from_secs(900));
        assert_eq!(parse_duration_str("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(
            parse_duration_str("1h30m").unwrap(),
            Duration::from_secs(5400)
        );
        assert_eq!(
            parse_duration_str("2m 30s").unwrap(),
            Duration::from_secs(150)
        );
    }

    #[test]
    fn rejects_invalid_duration() {
        assert!(parse_duration_str("abc").is_err());
        assert!(parse_duration_str("").is_err());
        assert!(parse_duration_str("-5m").is_err());
    }

    #[test]
    fn duration_requires_loop() {
        let now = Instant::now();
        assert!(deadline_from(false, Some("30s"), now).is_err());
        assert!(deadline_from(true, None, now).unwrap().is_none());
        let dl = deadline_from(true, Some("30s"), now).unwrap().unwrap();
        assert!(dl > now);
    }
}
