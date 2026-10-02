use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, U256};
use anyhow::Context;

use crate::cache::{RunManifest, SqliteStore};
use crate::config::validation::{self, ValidationResult};
use crate::config::{Config, ProviderConfig};
use crate::explorer::pricing;
use crate::explorer::results::{persist_opportunities_to_explorer, persist_rejections_to_explorer};
use crate::explorer::store::ExplorerStore;
use crate::fetch::Fetcher;
use crate::paper::{
    FillSkipReason, LedgerPolicy, LedgerResult, PaperFill, PaperMode, HARD_MAX_FILLS_PER_BLOCK,
};
use crate::pipeline::{aggregate_fills, BacktestRunner, BlockReplayStats};
use crate::pool::state::PoolManager;
use crate::progress::{JobProgress, ProgressEvent};
use crate::replay::BlockReplayer;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;
use crate::types::{GasConfig, MevOpportunity, RangeMode, ResultsFile};
use crate::utils::epoch_secs;

use super::rpc::init_rpc;

#[derive(Debug, Clone)]
pub struct LiveOpts {
    pub loop_enabled: bool,
    pub duration: Option<String>,
    pub poll_interval_ms: u64,
    pub record_rejections: bool,
    pub max_blocks: Option<u64>,
    /// Write the `paper_sessions` row for this session.
    ///
    /// `true` for `mev-scout live`, which owns its session end to end.
    /// `false` for callers that already persist their own row — notably
    /// `jobs::paper`, which calls this job and then applies and stores its own
    /// ledger. Leaving it on there wrote two rows per paper session and made
    /// `report` resolve the wrong one.
    pub persist_session: bool,
    /// Ledger overrides. `None` falls back to the `[paper]` config section.
    pub initial_balance_wei: Option<u128>,
    pub initial_balance_usd: Option<f64>,
    pub reserve_wei: Option<u128>,
    pub max_fills_per_block: Option<usize>,
    pub native_usd: Option<f64>,
}

impl Default for LiveOpts {
    fn default() -> Self {
        Self {
            loop_enabled: false,
            duration: None,
            poll_interval_ms: 0,
            record_rejections: false,
            max_blocks: None,
            persist_session: true,
            initial_balance_wei: None,
            initial_balance_usd: None,
            reserve_wei: None,
            max_fills_per_block: None,
            native_usd: None,
        }
    }
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
    pub total_txs_scanned: usize,
    pub total_opportunities: usize,
    /// Per-block stats accumulated across every pass of the session, so the
    /// CLI can render the same summary table the one-shot path produces.
    pub block_stats: Vec<BlockReplayStats>,
    /// Cumulative ledger over the whole session. `None` only if persistence
    /// failed, which is already logged.
    pub ledger: Option<LedgerResult>,
    pub session_id: String,
}

/// Resolve the ledger policy for a live session from `[paper]` plus CLI overrides.
///
/// Precedence: `--initial-balance` (exact wei) over `--initial-balance-usd`
/// (needs a price). Supplying both is an error rather than a silent pick —
/// the two numbers usually disagree and guessing which one the user meant
/// would silently change a financial figure.
fn resolve_ledger_policy(
    config: &Config,
    opts: &LiveOpts,
    native_usd: Option<f64>,
) -> anyhow::Result<LedgerPolicy> {
    let mut policy = config.paper.ledger_policy()?;

    if opts.initial_balance_wei.is_some() && opts.initial_balance_usd.is_some() {
        anyhow::bail!(
            "--initial-balance and --initial-balance-usd are mutually exclusive; \
             pass the wallet in wei (exact, no price lookup) or in USD (needs a price)"
        );
    }

    if let Some(wei) = opts.initial_balance_wei {
        policy.starting_gas_wei = wei;
    } else if let Some(usd) = opts.initial_balance_usd {
        if !usd.is_finite() || usd <= 0.0 {
            anyhow::bail!("--initial-balance-usd must be a positive finite number, got {usd}");
        }
        let price = native_usd.ok_or_else(|| {
            anyhow::anyhow!(
                "cannot resolve --initial-balance-usd {usd}: no native token USD price \
                 available for this chain. Pass --native-usd <price>, or use --initial-balance \
                 <wei> which needs no price."
            )
        })?;
        let wei = pricing::usd_to_wei(usd, price).ok_or_else(|| {
            anyhow::anyhow!(
                "--initial-balance-usd {usd} at {price} USD/native does not convert to a \
                 representable wei amount"
            )
        })?;
        policy.starting_gas_wei = wei;
    }

    if let Some(reserve) = opts.reserve_wei {
        if reserve > policy.starting_gas_wei {
            anyhow::bail!(
                "--reserve {reserve} exceeds the starting wallet {} wei",
                policy.starting_gas_wei
            );
        }
        policy.reserve_wei = reserve;
    }

    if let Some(max_fills) = opts.max_fills_per_block {
        policy.max_fills_per_block = max_fills.clamp(1, HARD_MAX_FILLS_PER_BLOCK);
    }

    Ok(policy)
}

/// Resolve the native USD price once per session.
///
/// Order mirrors `job_trace_op` (`jobs/trace.rs:374-388`): cached ZERO-keyed
/// price, then the wrapped-native cache, then DefiLlama, then CoinGecko.
/// Failure is not an error unless the caller needs the price, because a
/// wei-only session is fully valid without it.
async fn resolve_native_price(
    config: &Config,
    validation: &ValidationResult,
    override_price: Option<f64>,
) -> anyhow::Result<Option<f64>> {
    if let Some(price) = override_price {
        if price <= 0.0 || !price.is_finite() {
            anyhow::bail!("--native-usd must be a positive finite number, got {price}");
        }
        return Ok(Some(price));
    }
    let chain = validation.chain_name;
    let store = ExplorerStore::open(config.effective_explorer_db_path(&chain))?;
    let hour = pricing::hour_bucket(epoch_secs());
    if let Some((price, _)) = store.price_at(Address::ZERO, hour)? {
        return Ok(Some(price));
    }
    let wrapped = validation
        .chain_config
        .wrapped_native_token
        .unwrap_or(Address::ZERO);
    if let Some((price, _)) = store.price_at(wrapped, hour)? {
        return Ok(Some(price));
    }
    if !wrapped.is_zero() {
        if let Ok(price) = pricing::fetch_native_price_llama(chain, wrapped).await {
            return Ok(Some(price));
        }
    }
    match pricing::fetch_native_price_coingecko(chain).await {
        Ok(price) => Ok(Some(price)),
        Err(e) => {
            tracing::debug!("native USD price unavailable for {chain}: {e}");
            Ok(None)
        }
    }
}

/// Ledger inputs resolved once at session start.
struct ResolvedLedger {
    policy: LedgerPolicy,
    native_usd: Option<f64>,
    persist_session: bool,
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
    /// Latest tip observed during this session, refreshed on every loop pass.
    /// One-shot reads it here instead of reusing the value captured at startup.
    current_tip: u64,
    /// Stable for the whole session. One-shot is a single pass and generates
    /// one id; the loop reuses this for every pass.
    session_run_id: String,
    /// Growing session-wide block range, widened by [`Self::extend_session`].
    /// The manifest is rewritten on each pass with these bounds so `report`
    /// and `explorer validate --run-id` see the whole session as one run.
    session_start_block: u64,
    session_end_block: u64,
    /// Ledger applied over every opportunity seen so far this session.
    /// Recomputed from scratch each pass — `LedgerPolicy::apply` is pure and
    /// re-derives block grouping itself, so no incremental state to thread.
    policy: LedgerPolicy,
    session_opps: Vec<MevOpportunity>,
    /// Native price resolved once at startup; `None` when unavailable and the
    /// session does not need it (P&L stays in wei).
    native_usd: Option<f64>,
    /// Persisted once at session end — `paper_sessions` keys on a plain
    /// `session_id`, so re-inserting the same id would violate the constraint.
    session_id: String,
    /// Whether this context owns the `paper_sessions` row. False when the
    /// caller persists its own ledger row after borrowing this context.
    persist_session: bool,
}

impl<'a> LiveContext<'a> {
    async fn new(
        config: &'a Config,
        validation: ValidationResult,
        rpc: RpcClient,
        provider_configs: Vec<ProviderConfig>,
        record_rejections: bool,
        progress: &'a dyn JobProgress,
        ledger: ResolvedLedger,
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
            winning_bid_premium: config.gas.winning_bid_premium,
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
            .with_capture_pending(config.backtest.capture_pending)
            .with_min_profit_wei(config.backtest.min_profit_wei)
            .with_max_candidates_per_tx(config.backtest.max_candidates_per_tx)
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
            current_tip: tip,
            session_run_id: format!("live_{}", epoch_secs()),
            session_start_block: tip,
            session_end_block: tip,
            policy: ledger.policy,
            session_opps: Vec::new(),
            native_usd: ledger.native_usd,
            session_id: format!("paper_live_{}", epoch_secs()),
            persist_session: ledger.persist_session,
        })
    }

    async fn fetch_blocks<F: Fn() -> bool + Sync>(
        &self,
        resolved: &ResolvedRange,
        tick: Option<&F>,
    ) -> anyhow::Result<()> {
        let mut fetcher = Fetcher::new(self.rpc.clone(), self.cache.clone());
        fetcher = fetcher.with_parallelism(self.provider_configs.len());
        let summary = if !self.pool_addresses.is_empty() {
            fetcher
                .fetch_relevant(resolved, &self.pool_addresses, tick)
                .await?
        } else {
            fetcher.fetch_range(resolved, tick).await?
        };

        // A block at the tip can come back incomplete — the node may not have
        // indexed its receipts yet (`eth_getBlockReceipts` null). Without a
        // retry the gap is permanent here: the loop advances `last_block` past
        // it and never revisits, leaving a hole in the opportunity record and
        // desynced pool state. One retry only runs when a gap is real.
        if !summary.missing_after_fetch.is_empty() {
            let refetched = fetcher
                .auto_refetch_gaps(&summary.missing_after_fetch)
                .await?;
            self.progress.log(&format!(
                "Refetched {refetched} of {} missing block(s)",
                summary.missing_after_fetch.len()
            ));
        }

        Ok(())
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

    /// Widen the session-wide block range to cover `resolved`.
    fn extend_session(&mut self, resolved: &ResolvedRange) {
        if resolved.start_block < self.session_start_block {
            self.session_start_block = resolved.start_block;
        }
        if resolved.end_block > self.session_end_block {
            self.session_end_block = resolved.end_block;
        }
    }

    /// Persist one pass's opportunities plus the session manifest.
    ///
    /// `opps` holds only the current pass's findings — it is inserted verbatim,
    /// so per-pass rows stay distinct. The manifest, by contrast, is keyed on
    /// `run_id` and rewritten with `INSERT OR REPLACE`
    /// (`cache/store/manifests.rs:7`), so it always carries the full session
    /// range rather than just this pass.
    fn persist_results(&mut self, opps: &[MevOpportunity]) {
        let chain_name = self.validation.chain_name;
        let run_id = self.session_run_id.clone();
        let strategies: Vec<String> = self
            .validation
            .strategies
            .iter()
            .map(|s| s.to_string())
            .collect();
        let flash_loan_provider = self.validation.flash_loan_provider.to_string();

        let results_file = ResultsFile {
            run_id: run_id.clone(),
            chain: chain_name.to_string(),
            start_block: self.session_start_block,
            end_block: self.session_end_block,
            range_mode: "live".to_string(),
            strategies: strategies.clone(),
            flash_loan_provider: flash_loan_provider.clone(),
            resolved_at: epoch_secs(),
            created_at: epoch_secs(),
            opportunities: opps.to_vec(),
        };
        let manifest = RunManifest {
            run_id: run_id.clone(),
            chain: chain_name.to_string(),
            start_block: self.session_start_block,
            end_block: self.session_end_block,
            resolved_at: epoch_secs(),
            range_mode: "live".to_string(),
            strategies,
            flash_loan_provider,
        };
        if let Err(e) = self.cache.put_manifest(&manifest) {
            tracing::warn!("run-manifest persist failed: {e}");
        }
        persist_opportunities_to_explorer(self.config, chain_name, &run_id, &results_file);
        let rejections = self.runner.take_rejections();
        persist_rejections_to_explorer(self.config, chain_name, &run_id, &rejections);
    }

    /// Fold this pass's opportunities into the session ledger and return it.
    ///
    /// `apply` regroups by `block_number` and recomputes the wallet from
    /// `starting_gas_wei` every time, so the growing `session_opps` list is
    /// the only state needed. Cost is O(n) per pass in session opportunities.
    fn update_ledger(&mut self, opps: &[MevOpportunity]) -> LedgerResult {
        self.session_opps.extend_from_slice(opps);
        self.policy.apply(&self.session_opps)
    }

    /// Persist the finished session ledger. Called once — `insert_paper_session`
    /// is a plain `INSERT` on a `session_id` primary key
    /// (`paper/store.rs:57,69`), so a second call would violate the constraint.
    fn persist_ledger_session(&self, ledger: &LedgerResult) -> anyhow::Result<String> {
        let chain = self.validation.chain_name;
        let store = ExplorerStore::open(self.config.effective_explorer_db_path(&chain))?;
        store.insert_paper_session(
            &self.session_id,
            &chain.to_string(),
            PaperMode::Live,
            Some(&self.session_run_id),
            ledger,
        )?;
        Ok(self.session_id.clone())
    }
}

/// Render one ledger line set for progress output.
///
/// `not_native_unit` is counted and labelled rather than lumped in with real
/// skips: `is_native_eligible` rejects liquidation outright, so with the
/// ledger always on this counter is routinely non-zero and an unlabelled count
/// reads like a problem.
pub fn render_ledger_summary(ledger: &LedgerResult, native_usd: Option<f64>) -> String {
    let mut out = String::new();
    // Net P&L is signed (a losing session must read as negative), so it cannot
    // go through `wei_to_usd`, which only accepts `U256`.
    let usd_of = |w: u128| match native_usd {
        Some(price) => format!(" (${:.4})", pricing::wei_to_usd(U256::from(w), price)),
        None => String::new(),
    };
    let usd_net = match native_usd {
        Some(price) => format!(
            " (${:.4})",
            pricing::signed_wei_to_usd(ledger.net_profit_wei, price)
        ),
        None => String::new(),
    };
    out.push_str(&format!(
        "  ledger: {} fill(s), {} skipped | net {} wei{usd_net}\n",
        ledger.fills_count(),
        ledger.skips_count(),
        ledger.net_profit_wei,
    ));
    out.push_str(&format!(
        "  wallet: {}{usd_start} → {}{usd_end} | reserve {} | max drawdown {} wei",
        ledger.starting_gas_wei,
        ledger.ending_gas_wei,
        ledger.reserve_wei,
        ledger.max_drawdown_wei,
        usd_start = usd_of(ledger.starting_gas_wei),
        usd_end = usd_of(ledger.ending_gas_wei),
    ));
    out.push_str(&render_by_strategy(&ledger.fills, native_usd));
    let not_native = ledger
        .skips
        .iter()
        .filter(|s| s.reason == FillSkipReason::NotNativeUnit)
        .count();
    if not_native > 0 {
        out.push_str(&format!(
            "\n  note: {not_native} candidate(s) skipped as not_native_unit \
             (profit not native-denominated, e.g. liquidation)"
        ));
    }
    out
}

/// Per-strategy attribution of the session's realized fills.
///
/// `aggregate_fills` reuses the same rollup as `report`, so the numbers here
/// cannot drift from `core::pipeline::aggregate`'s strategy table. Ordered by
/// absolute net so the dominant winner/loser reads first rather than in
/// hash-map order.
fn render_by_strategy(fills: &[PaperFill], native_usd: Option<f64>) -> String {
    if fills.is_empty() {
        return String::new();
    }
    // `aggregate_fills` needs a price only to populate USD fields. Passing 0.0
    // when unknown keeps them at zero; the wei columns stay exact regardless.
    let agg = aggregate_fills(fills, native_usd.unwrap_or(0.0));

    let mut rows: Vec<_> = agg.by_strategy.into_values().collect();
    rows.sort_by(|a, b| {
        b.net_profit_wei
            .unsigned_abs()
            .cmp(&a.net_profit_wei.unsigned_abs())
            .then_with(|| a.strategy.cmp(&b.strategy))
    });

    let widest = rows
        .iter()
        .map(|r| r.strategy.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let fills_col = rows
        .iter()
        .map(|r| r.count)
        .max()
        .unwrap_or(0)
        .to_string()
        .len();

    let mut out = String::from("\n  by strategy (fills accepted):\n");
    for r in &rows {
        let usd = match native_usd {
            Some(_) => format!("  ${:>9.4}", r.net_profit_usd),
            None => String::new(),
        };
        // Signed so the column reads uniformly. In practice every accepted fill
        // has net > 0 — `LedgerPolicy::apply` skips `net <= 0` as
        // `NonPositiveNet` — so this is a guard, not a currently-exercised path.
        let net = r.net_profit_wei as f64 / 1e18;
        out.push_str(&format!(
            "    {:<widest$}  {:>fills_col$} fill{}  gas {:>9.6}  net {net:>+14.6} wei{usd}\n",
            r.strategy,
            r.count,
            if r.count == 1 { "" } else { "s" },
            r.total_gas_cost_wei as f64 / 1e18,
        ));
    }
    out
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

    // Resolved once per session: a live loop can run for hours, and re-pricing
    // every pass would both cost RPC calls and make the wallet drift mid-session.
    let native_usd = resolve_native_price(config, &validation, opts.native_usd).await?;
    let policy = resolve_ledger_policy(config, opts, native_usd)?;
    let ledger = ResolvedLedger {
        policy,
        native_usd,
        persist_session: opts.persist_session,
    };

    let setup = init_rpc(config, validation.chain_name, true).await?;
    let mut ctx = LiveContext::new(
        config,
        validation,
        setup.rpc,
        setup.provider_configs,
        opts.record_rejections,
        progress,
        ledger,
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
    let run_id = ctx.session_run_id.clone();
    // Re-read the tip: `ctx.tip` was captured during setup, and pool
    // initialization + validation can take long enough that a newer block has
    // already landed. One-shot should scan the freshest block, not a stale one.
    let tip = match ctx.rpc.get_block_number().await {
        Ok(n) => n,
        Err(e) => {
            // Fall back to the setup tip rather than failing the job — the
            // block may still be fetchable even if the tip probe fails.
            tracing::warn!("Failed to refresh tip ({}), using {}", e, ctx.tip);
            ctx.tip
        }
    };
    ctx.current_tip = tip;
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
    ctx.extend_session(&resolved);
    ctx.persist_results(&opps);
    let ledger = ctx.update_ledger(&opps);
    if ctx.persist_session {
        if let Err(e) = ctx.persist_ledger_session(&ledger) {
            // Not fatal: the run results and manifest are already written, so
            // the session is still usable via `report`. Losing only the P&L row
            // should not discard a completed scan.
            tracing::warn!("paper-session persist failed: {e}");
        }
    }
    progress.log(&render_ledger_summary(&ledger, ctx.native_usd));

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
    let run_id = ctx.session_run_id.clone();
    progress.log(&format!("Run ID: {run_id}"));
    progress.log(&format!(
        "Starting from block {last_block} — stop from the UI to halt\n"
    ));

    const MAX_CONSECUTIVE_FAILURES: u32 = 5;
    let mut consecutive_failures: u32 = 0;
    let mut blocks_processed: u64 = 0;
    let mut total_txs_scanned: usize = 0;
    let mut total_dex_txs: usize = 0;
    let mut total_pending_txs: usize = 0;
    let mut total_opportunities: usize = 0;
    let mut block_stats: Vec<BlockReplayStats> = Vec::new();

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
        let (opps, pass_stats) = match ctx.run_blocks(&resolved, Some(&detect_progress)).await {
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

        for s in &pass_stats {
            block_stats.push(s.clone());
            total_txs_scanned += s.total_tx_count;
            total_dex_txs += s.dex_tx_count;
            total_pending_txs += s.pending_tx_count;
        }

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

        ctx.extend_session(&resolved);
        ctx.persist_results(&opps);
        let ledger = ctx.update_ledger(&opps);
        progress.log(&render_ledger_summary(&ledger, ctx.native_usd));
        progress.emit(ProgressEvent {
            stage: "complete".to_string(),
            done: None,
            total: None,
            run_id: Some(run_id.clone()),
            ops: Some(opps.len() as u64),
            elapsed_ms: Some(pass_elapsed.as_millis() as u64),
        });
        ctx.runner.advance_to(current_tip);
        ctx.current_tip = current_tip;
        last_block = current_tip;
        blocks_processed += resolved.block_count;
        total_opportunities += opps.len();
        consecutive_failures = 0;
    }

    progress.log("");
    progress.log("Session summary:");
    progress.log(&format!("  Blocks processed: {blocks_processed}"));
    progress.log(&format!("  Txs scanned:      {total_txs_scanned}"));
    progress.log(&format!("    DEX txs:        {total_dex_txs}"));
    if total_pending_txs > 0 {
        progress.log(&format!("    Pending txs:    {total_pending_txs}"));
    }
    progress.log(&format!("  Opportunities:    {total_opportunities}"));

    // Recomputed from the full accumulated list rather than reusing the last
    // pass's result: passes with no opportunities skip `update_ledger` output,
    // so the in-loop value can be stale by one pass.
    let ledger = ctx.update_ledger(&[]);
    let session_id = if ctx.persist_session {
        match ctx.persist_ledger_session(&ledger) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("paper-session persist failed: {e}");
                ctx.session_id.clone()
            }
        }
    } else {
        ctx.session_id.clone()
    };
    if ctx.persist_session {
        progress.log(&format!("  Ledger session:   {session_id}"));
    }
    progress.log("");

    Ok(LiveLoopOutcome {
        blocks_processed,
        total_txs_scanned,
        total_opportunities,
        block_stats,
        ledger: Some(ledger),
        session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> LiveOpts {
        LiveOpts::default()
    }

    #[test]
    fn config_defaults_apply_when_no_overrides() {
        let config = Config::default();
        let policy = resolve_ledger_policy(&config, &opts(), None).unwrap();
        assert_eq!(
            policy.starting_gas_wei,
            config.paper.starting_gas_wei_u128().unwrap()
        );
        assert_eq!(policy.reserve_wei, 0);
        assert_eq!(policy.max_fills_per_block, HARD_MAX_FILLS_PER_BLOCK);
    }

    #[test]
    fn wei_balance_overrides_config_exactly() {
        let config = Config::default();
        let o = LiveOpts {
            initial_balance_wei: Some(42),
            ..Default::default()
        };
        // No price needed — wei is the exact primitive.
        let policy = resolve_ledger_policy(&config, &o, None).unwrap();
        assert_eq!(policy.starting_gas_wei, 42);
    }

    #[test]
    fn usd_balance_converts_via_price() {
        let config = Config::default();
        let o = LiveOpts {
            initial_balance_usd: Some(10.0),
            ..Default::default()
        };
        let policy = resolve_ledger_policy(&config, &o, Some(2.0)).unwrap();
        assert_eq!(policy.starting_gas_wei, 5 * 10u128.pow(18));
    }

    #[test]
    fn both_balance_forms_is_an_error() {
        let config = Config::default();
        let o = LiveOpts {
            initial_balance_wei: Some(1),
            initial_balance_usd: Some(10.0),
            ..Default::default()
        };
        let err = resolve_ledger_policy(&config, &o, Some(1.0)).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn usd_balance_without_price_is_a_hard_error() {
        let config = Config::default();
        let o = LiveOpts {
            initial_balance_usd: Some(10.0),
            ..Default::default()
        };
        let err = resolve_ledger_policy(&config, &o, None).unwrap_err();
        let msg = err.to_string();
        // Must point at both escapes, not silently substitute a wallet.
        assert!(msg.contains("--native-usd"), "msg was: {msg}");
        assert!(msg.contains("--initial-balance"), "msg was: {msg}");
    }

    #[test]
    fn non_positive_usd_balance_rejected() {
        let config = Config::default();
        for bad in [0.0, -1.0, f64::NAN] {
            let o = LiveOpts {
                initial_balance_usd: Some(bad),
                ..Default::default()
            };
            assert!(resolve_ledger_policy(&config, &o, Some(1.0)).is_err());
        }
    }

    #[test]
    fn reserve_above_wallet_rejected() {
        let config = Config::default();
        let o = LiveOpts {
            initial_balance_wei: Some(100),
            reserve_wei: Some(101),
            ..Default::default()
        };
        let err = resolve_ledger_policy(&config, &o, None).unwrap_err();
        assert!(err.to_string().contains("exceeds the starting wallet"));
    }

    #[test]
    fn max_fills_clamped_to_hard_cap() {
        let config = Config::default();
        let o = LiveOpts {
            max_fills_per_block: Some(9999),
            ..Default::default()
        };
        let policy = resolve_ledger_policy(&config, &o, None).unwrap();
        assert_eq!(policy.max_fills_per_block, HARD_MAX_FILLS_PER_BLOCK);

        // 0 would otherwise mean "unlimited"; clamp keeps it at least 1.
        let o = LiveOpts {
            max_fills_per_block: Some(0),
            ..Default::default()
        };
        let policy = resolve_ledger_policy(&config, &o, None).unwrap();
        assert_eq!(policy.max_fills_per_block, 1);
    }

    #[test]
    fn zero_wei_balance_is_allowed() {
        let config = Config::default();
        let o = LiveOpts {
            initial_balance_wei: Some(0),
            ..Default::default()
        };
        let policy = resolve_ledger_policy(&config, &o, None).unwrap();
        assert_eq!(policy.starting_gas_wei, 0);
    }

    fn opp(block: u64, tx: usize, profit: u128, gas: u128) -> MevOpportunity {
        use alloy::primitives::U256;
        let mut o = MevOpportunity::new(
            block,
            tx,
            crate::types::Strategy::TwoHopArb,
            Address::repeat_byte((tx + 1) as u8),
            0,
        );
        o.pool_b = Address::repeat_byte((tx + 50) as u8);
        o.expected_profit = U256::from(profit);
        o.gas_cost_wei = gas;
        o.canonical_id = Some(format!("c|{block}|{tx}"));
        o
    }

    /// The loop appends each pass's opportunities and recomputes from scratch.
    /// Guards that the session list actually accumulates — a non-accumulating
    /// list would silently report only the final pass's P&L.
    #[test]
    fn session_ledger_accumulates_across_passes() {
        let policy = LedgerPolicy {
            starting_gas_wei: 1_000_000,
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let mut session: Vec<MevOpportunity> = Vec::new();

        session.extend([opp(10, 0, 500, 100)]);
        let after_first = policy.apply(&session);
        assert_eq!(after_first.fills_count(), 1);
        assert_eq!(after_first.net_profit_wei, 400);

        session.extend([opp(11, 0, 700, 100)]);
        let after_second = policy.apply(&session);
        assert_eq!(after_second.fills_count(), 2);
        assert_eq!(after_second.net_profit_wei, 1000);
        assert_eq!(after_second.ending_gas_wei, 1_001_000);
    }

    /// `apply` groups by `block_number` itself, so passing a pass's opps again
    /// after they are already in the session list must not double-count.
    #[test]
    fn recompute_over_full_list_is_stable() {
        let policy = LedgerPolicy {
            starting_gas_wei: 1_000_000,
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let opps = vec![opp(10, 0, 500, 100), opp(11, 0, 700, 100)];
        let once = policy.apply(&opps);
        let twice = policy.apply(&opps);
        assert_eq!(once.net_profit_wei, twice.net_profit_wei);
        assert_eq!(once.fills_count(), twice.fills_count());
    }

    #[test]
    fn ledger_summary_reports_net_and_drawdown() {
        let policy = LedgerPolicy {
            starting_gas_wei: 1_000_000,
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let ledger = policy.apply(&[opp(10, 0, 500, 100)]);
        let text = render_ledger_summary(&ledger, None);
        assert!(text.contains("400 wei"), "text was: {text}");
        assert!(text.contains("1 fill(s)"));
    }

    #[test]
    fn ledger_summary_adds_usd_when_price_known() {
        let policy = LedgerPolicy {
            starting_gas_wei: 10 * 10u128.pow(18),
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let ledger = policy.apply(&[opp(10, 0, 2 * 10u128.pow(18), 0)]);
        let text = render_ledger_summary(&ledger, Some(2.0));
        assert!(text.contains('$'), "text was: {text}");
        assert!(text.contains("4.0000"), "text was: {text}");
    }

    /// `is_native_eligible` rejects liquidation, so with the ledger always on
    /// this count is routinely non-zero. It must be labelled, not hidden.
    #[test]
    fn summary_labels_not_native_unit_skips() {
        use crate::types::Strategy;
        let mut liq = MevOpportunity::new(10, 0, Strategy::Liquidation, Address::repeat_byte(1), 0);
        liq.expected_profit = alloy::primitives::U256::from(1_000);
        let ledger = LedgerPolicy::default().apply(&[liq]);
        let text = render_ledger_summary(&ledger, None);
        assert!(
            text.contains("not_native_unit"),
            "expected an explicit note, got: {text}"
        );
    }

    // ── per-strategy attribution ────────────────────────────────────────────

    fn strat_opp(
        block: u64,
        tx: usize,
        strategy: crate::types::Strategy,
        profit: u128,
        gas: u128,
    ) -> MevOpportunity {
        use alloy::primitives::U256;
        let mut o =
            MevOpportunity::new(block, tx, strategy, Address::repeat_byte((tx + 1) as u8), 0);
        o.pool_b = Address::repeat_byte((tx + 50) as u8);
        o.expected_profit = U256::from(profit);
        o.gas_cost_wei = gas;
        o.canonical_id = Some(format!("c|{block}|{tx}|{strategy}"));
        o
    }

    #[test]
    fn by_strategy_breaks_down_by_strategy() {
        use crate::types::Strategy;
        let policy = LedgerPolicy {
            starting_gas_wei: 100 * 10u128.pow(18),
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let ledger = policy.apply(&[
            strat_opp(
                10,
                0,
                Strategy::TwoHopArb,
                3 * 10u128.pow(18),
                10u128.pow(17),
            ),
            strat_opp(10, 1, Strategy::TwoHopArb, 10u128.pow(18), 10u128.pow(17)),
            strat_opp(11, 0, Strategy::Jit, 5 * 10u128.pow(18), 10u128.pow(17)),
        ]);
        let text = render_ledger_summary(&ledger, None);
        assert!(text.contains("by strategy"), "text was: {text}");
        // `aggregate_fills` reports short display names via `ui_strategy_name`.
        assert!(text.contains("arb"), "text was: {text}");
        assert!(text.contains("jit"), "text was: {text}");
        // TwoHopArb filed twice; Jit once. Singular/plural follows the count.
        assert!(text.contains("2 fills"), "text was: {text}");
        assert!(text.contains("1 fill "), "text was: {text}");
    }

    /// The per-strategy rows must add up to the headline net. Without this a
    /// change to either renderer could silently disagree with the other.
    #[test]
    fn by_strategy_rows_sum_to_headline_net() {
        use crate::types::Strategy;
        let policy = LedgerPolicy {
            starting_gas_wei: 100 * 10u128.pow(18),
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let ledger = policy.apply(&[
            strat_opp(
                10,
                0,
                Strategy::TwoHopArb,
                3 * 10u128.pow(18),
                10u128.pow(17),
            ),
            strat_opp(11, 0, Strategy::Jit, 5 * 10u128.pow(18), 10u128.pow(17)),
            strat_opp(12, 0, Strategy::Sandwich, 10u128.pow(17), 10u128.pow(17)),
        ]);
        let agg = aggregate_fills(&ledger.fills, 0.0);
        let summed: i128 = agg.by_strategy.values().map(|m| m.net_profit_wei).sum();
        assert_eq!(
            summed, ledger.net_profit_wei,
            "per-strategy net must equal the ledger headline"
        );
        // Every fill must be attributed to exactly one strategy bucket.
        let counted: usize = agg.by_strategy.values().map(|m| m.count).sum();
        assert_eq!(counted, ledger.fills_count());
    }

    /// Gas exceeding profit means the candidate is never filled — it is skipped as
    /// `NonPositiveNet`. So every row in the breakdown is positive by
    /// construction, and a gas-heavy opportunity must not appear as a losing row.
    #[test]
    fn gas_heavy_candidates_are_skipped_not_filled() {
        use crate::types::Strategy;
        let policy = LedgerPolicy {
            starting_gas_wei: 100 * 10u128.pow(18),
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let ledger = policy.apply(&[strat_opp(
            10,
            0,
            Strategy::Sandwich,
            10u128.pow(17),
            5 * 10u128.pow(17),
        )]);
        assert_eq!(ledger.fills.len(), 0, "net <= 0 must not be filled");
        assert_eq!(ledger.skips[0].reason, FillSkipReason::NonPositiveNet);
        assert_eq!(
            render_by_strategy(&ledger.fills, None),
            "",
            "no fills ⇒ no breakdown rows"
        );
    }

    /// USD columns must appear only when a price is known, never guessed.
    #[test]
    fn by_strategy_usd_requires_price() {
        use crate::types::Strategy;
        let policy = LedgerPolicy {
            starting_gas_wei: 100 * 10u128.pow(18),
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let ledger = policy.apply(&[strat_opp(
            10,
            0,
            Strategy::TwoHopArb,
            3 * 10u128.pow(18),
            10u128.pow(17),
        )]);
        assert!(
            !render_by_strategy(&ledger.fills, None).contains('$'),
            "no price ⇒ no USD column"
        );
        assert!(
            render_by_strategy(&ledger.fills, Some(2.0)).contains('$'),
            "known price ⇒ USD column"
        );
    }

    /// Rows are ordered by absolute net so the dominant strategy leads, not
    /// hash-map iteration order.
    #[test]
    fn by_strategy_orders_by_absolute_net() {
        use crate::types::Strategy;
        let policy = LedgerPolicy {
            starting_gas_wei: 100 * 10u128.pow(18),
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let ledger = policy.apply(&[
            strat_opp(10, 0, Strategy::Jit, 10u128.pow(18), 0),
            strat_opp(11, 0, Strategy::Sandwich, 90 * 10u128.pow(17), 0),
        ]);
        let text = render_by_strategy(&ledger.fills, None);
        let sandwich = text.find("sandwich").expect("sandwich row");
        let jit = text.find("jit").expect("jit row");
        assert!(sandwich < jit, "largest |net| must come first:\n{text}");
    }

    #[test]
    fn by_strategy_is_empty_without_fills() {
        assert_eq!(render_by_strategy(&[], None), "");
    }
}
