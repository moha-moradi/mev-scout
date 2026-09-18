use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use anyhow::Context;

use crate::cache::{RunManifest, SqliteStore};
use crate::config::validation::{self, ValidationResult};
use crate::config::{Config, ProviderConfig};
use crate::explorer::results::{persist_opportunities_to_explorer, persist_rejections_to_explorer};
use crate::fetch::Fetcher;
use crate::pipeline::{BacktestRunner, BlockReplayStats};
use crate::pool::state::PoolManager;
use crate::progress::{JobProgress, ProgressEvent};
use crate::replay::BlockReplayer;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;
use crate::types::{GasConfig, MevOpportunity, RangeMode, ResultsFile};
use crate::utils::epoch_secs;

use super::rpc::init_rpc;

#[derive(Debug, Clone, Default)]
pub struct LiveOpts {
    pub loop_enabled: bool,
    pub duration: Option<String>,
    pub poll_interval_ms: u64,
    pub record_rejections: bool,
    pub max_blocks: Option<u64>,
}

pub struct LiveOneShotOutcome {
    pub run_id: String,
    pub tip: u64,
    pub opportunities: Vec<MevOpportunity>,
    pub block_stats: Vec<BlockReplayStats>,
    pub elapsed: Duration,
}

pub struct LiveLoopOutcome {
    pub blocks_processed: u64,
    pub total_opportunities: usize,
}

pub enum LiveOutcome {
    OneShot(LiveOneShotOutcome),
    Loop(LiveLoopOutcome),
}

struct LiveContext<'a> {
    config: &'a Config,
    validation: ValidationResult,
    rpc: RpcClient,
    provider_configs: Vec<ProviderConfig>,
    cache: SqliteStore,
    pool_addresses: Vec<Address>,
    progress: &'a dyn JobProgress,
    runner: BacktestRunner,
    tip: u64,
}

impl<'a> LiveContext<'a> {
    async fn new(
        config: &'a Config,
        validation: ValidationResult,
        rpc: RpcClient,
        provider_configs: Vec<ProviderConfig>,
        record_rejections: bool,
        progress: &'a dyn JobProgress,
    ) -> anyhow::Result<LiveContext<'a>> {
        let chain_name = validation.chain_name;
        let cache = SqliteStore::open(config.effective_db_path(&chain_name))?;
        let tip = rpc
            .get_block_number()
            .await
            .context("failed to get chain tip")?;

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

        let mut pool_manager = PoolManager::new();
        pool_manager.set_max_pairs_per_token(config.backtest.max_pairs_per_token);
        pool_manager.set_concurrency_limit(provider_configs.len() as u32);
        pool_manager.use_latest();
        if let Some(vault_addr) = validation.chain_config.balancer_vault {
            pool_manager = pool_manager.with_balancer_vault(vault_addr);
        }
        if let Some(native_addr) = validation.chain_config.wrapped_native_token {
            pool_manager = pool_manager.with_wrapped_native(native_addr);
        }
        if !validation.strategies.is_empty() {
            progress.emit(ProgressEvent::stage("pool_init"));
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
            .with_record_rejections(record_rejections);

        if let Some(aave_pool) = validation.chain_config.aave_v3_pool {
            runner
                .prefetch_aave_reserves(aave_pool, tip.saturating_sub(1))
                .await;
        }

        Ok(LiveContext {
            config,
            validation,
            rpc,
            provider_configs,
            cache,
            pool_addresses,
            progress,
            runner,
            tip,
        })
    }

    async fn fetch_blocks<F: Fn() -> bool + Sync>(
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

    async fn run_blocks(
        &mut self,
        resolved: &ResolvedRange,
        progress: Option<&dyn Fn(u64, u64) -> bool>,
    ) -> anyhow::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>)> {
        let state_horizon = self.rpc.detect_state_horizon(resolved.end_block).await;
        let (opps, stats, _modes) =
            self.runner
                .run_range_hybrid(resolved, state_horizon, progress)?;
        Ok((opps, stats))
    }

    fn persist_results(&mut self, resolved: &ResolvedRange, opps: &[MevOpportunity], run_id: &str) {
        let chain_name = self.validation.chain_name;
        let results_file = ResultsFile {
            run_id: run_id.to_string(),
            chain: chain_name.to_string(),
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
        let manifest = RunManifest {
            run_id: run_id.to_string(),
            chain: chain_name.to_string(),
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
        persist_opportunities_to_explorer(self.config, chain_name, run_id, &results_file);
        let rejections = self.runner.take_rejections();
        persist_rejections_to_explorer(self.config, chain_name, run_id, &rejections);
    }
}

pub async fn job_live(
    config: &Config,
    opts: &LiveOpts,
    progress: &dyn JobProgress,
) -> anyhow::Result<LiveOutcome> {
    if opts.max_blocks.is_some() && !opts.loop_enabled {
        anyhow::bail!("--max-blocks requires --loop");
    }
    let deadline = match opts.duration.as_deref() {
        Some(d) => {
            if !opts.loop_enabled {
                anyhow::bail!("--duration requires --loop");
            }
            let dur = humantime::parse_duration(d).with_context(|| {
                format!("invalid --duration '{d}' (expected e.g. 90s, 15m, 1h)")
            })?;
            Some(Instant::now() + dur)
        }
        None => None,
    };

    let validation = validation::validate_live(config).context("invalid configuration")?;
    progress.log(&format!(
        "Live mode ({}) — polling every {}ms",
        if opts.loop_enabled {
            "continuous"
        } else {
            "one-shot"
        },
        opts.poll_interval_ms
    ));

    let setup = init_rpc(config, validation.chain_name, true).await?;
    let mut ctx = LiveContext::new(
        config,
        validation,
        setup.rpc,
        setup.provider_configs,
        opts.record_rejections,
        progress,
    )
    .await?;

    progress.emit(ProgressEvent::stage("resolve"));

    if opts.loop_enabled {
        let summary = run_loop(&mut ctx, deadline, opts.poll_interval_ms, opts.max_blocks).await?;
        Ok(LiveOutcome::Loop(summary))
    } else {
        let pass = run_once(&mut ctx).await?;
        Ok(LiveOutcome::OneShot(pass))
    }
}

async fn run_once(ctx: &mut LiveContext<'_>) -> anyhow::Result<LiveOneShotOutcome> {
    let progress = ctx.progress;
    let run_id = format!("live_{}", epoch_secs());
    let tip = ctx.tip;
    progress.log(&format!("Run ID: {run_id}"));
    progress.log(&format!("Latest block: {tip}"));

    let resolved = ResolvedRange {
        start_block: tip,
        end_block: tip,
        block_count: 1,
        mode: RangeMode::Single(tip),
    };

    let start = Instant::now();
    let fetch_done = Arc::new(AtomicU64::new(0));
    let tick = move || {
        if progress.cancelled() {
            return false;
        }
        let d = fetch_done.fetch_add(1, Ordering::Relaxed) + 1;
        progress.emit(ProgressEvent {
            stage: "fetch".to_string(),
            done: Some(d),
            total: Some(1),
            run_id: None,
            ops: None,
            elapsed_ms: None,
        });
        true
    };
    ctx.fetch_blocks(&resolved, Some(&tick)).await?;

    let detect = move |done: u64, total: u64| -> bool {
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
    let (opps, block_stats) = ctx.run_blocks(&resolved, Some(&detect)).await?;
    let elapsed = start.elapsed();
    ctx.persist_results(&resolved, &opps, &run_id);

    progress.emit(ProgressEvent {
        stage: "complete".to_string(),
        done: None,
        total: None,
        run_id: Some(run_id.clone()),
        ops: Some(opps.len() as u64),
        elapsed_ms: Some(elapsed.as_millis() as u64),
    });

    progress.log(&format!(
        "Block {tip} — {} opportunity(ies) detected",
        opps.len()
    ));

    Ok(LiveOneShotOutcome {
        run_id,
        tip,
        opportunities: opps,
        block_stats,
        elapsed,
    })
}

async fn run_loop(
    ctx: &mut LiveContext<'_>,
    deadline: Option<Instant>,
    poll_interval_ms: u64,
    max_blocks: Option<u64>,
) -> anyhow::Result<LiveLoopOutcome> {
    let progress = ctx.progress;
    let mut last_block = ctx.tip;
    progress.log(&format!(
        "Starting from block {last_block} — stop from the UI to halt\n"
    ));

    const MAX_CONSECUTIVE_FAILURES: u32 = 5;
    let mut consecutive_failures: u32 = 0;
    let mut blocks_processed: u64 = 0;
    let mut total_txs_scanned: usize = 0;
    let mut total_opportunities: usize = 0;

    loop {
        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;

        if progress.cancelled() {
            break;
        }
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
        progress.log(&format!("Run ID: {run_id}"));
        let pass_start = Instant::now();

        let fetch_done = Arc::new(AtomicU64::new(0));
        let tick = move || {
            if progress.cancelled() {
                return false;
            }
            let d = fetch_done.fetch_add(1, Ordering::Relaxed) + 1;
            progress.emit(ProgressEvent {
                stage: "fetch".to_string(),
                done: Some(d),
                total: Some(block_count),
                run_id: None,
                ops: None,
                elapsed_ms: None,
            });
            true
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
        let (opps, _stats) = match ctx.run_blocks(&resolved, Some(&detect_progress)).await {
            Ok(o) => o,
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

        if opps.is_empty() {
            progress.log(&format!(
                "Block {}–{}: no opportunities",
                from_block, current_tip
            ));
        } else {
            progress.log(&format!(
                "Block {}–{}: {} opportunity(ies)",
                from_block,
                current_tip,
                opps.len(),
            ));
        }

        ctx.persist_results(&resolved, &opps, &run_id);
        progress.emit(ProgressEvent {
            stage: "complete".to_string(),
            done: None,
            total: None,
            run_id: Some(run_id.clone()),
            ops: Some(opps.len() as u64),
            elapsed_ms: Some(pass_elapsed.as_millis() as u64),
        });
        ctx.runner.advance_to(current_tip);
        last_block = current_tip;
        blocks_processed += resolved.block_count;
        total_opportunities += opps.len();
        total_txs_scanned += 0;
        consecutive_failures = 0;
    }

    progress.log("");
    progress.log("Session summary:");
    progress.log(&format!("  Blocks processed: {blocks_processed}"));
    progress.log(&format!("  Txs scanned:      {total_txs_scanned}"));
    progress.log(&format!("  Opportunities:    {total_opportunities}"));

    Ok(LiveLoopOutcome {
        blocks_processed,
        total_opportunities,
    })
}
