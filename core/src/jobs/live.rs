use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, U256};
use anyhow::Context;

use crate::cache::{RunManifest, SqliteStore};
use crate::config::validation::{self, ValidationResult};
use crate::config::{Config, ProviderConfig};
use crate::exec::{execute_ledger, searcher_address, AnvilNode, ExecContext, ExecMode};
use crate::explorer::pricing::{self, GasQuote};
use crate::explorer::reject::RejectReason;
use crate::explorer::results::{persist_opportunities_to_explorer, persist_rejections_to_explorer};
use crate::explorer::store::ExplorerStore;
use crate::fetch::Fetcher;
use crate::paper::sim::{merge_seeds, SimSeeds};
use crate::paper::{
    reconcile_sim_vs_exec, BacktestMetrics, FillSkipReason, LedgerPolicy, LedgerResult, PaperFill,
    PaperMode, SimMode, SimPolicy, HARD_MAX_FILLS_PER_BLOCK,
};
use crate::pipeline::aggregate::StrategyMetrics;
use crate::pipeline::{aggregate_fills, BacktestRunner, BlockMode, BlockReplayStats};
use crate::pool::state::PoolManager;
use crate::progress::{JobProgress, ProgressEvent};
use crate::replay::BlockReplayer;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;
use crate::types::{FlashLoanProvider, GasConfig, MevOpportunity, RangeMode, ResultsFile};
use crate::utils::{epoch_millis, epoch_secs};

use super::rpc::init_rpc;

#[derive(Debug, Clone, Default)]
pub struct LiveOpts {
    pub loop_enabled: bool,
    pub duration: Option<String>,
    pub poll_interval_ms: u64,
    pub record_rejections: bool,
    pub max_blocks: Option<u64>,
    /// Width of the one-shot window, in blocks ending at the tip. `0` is
    /// normalized to `1`, which preserves the historical "scan the tip block
    /// only" behaviour for callers that never set it. The CLI default-subcommand
    /// path sets a wider window so a bare run yields an immediately useful
    /// result. Ignored when [`Self::loop_enabled`] is set.
    pub single_pass_blocks: u64,
    /// Ledger overrides. `None` falls back to the `[paper]` config section.
    pub initial_balance_wei: Option<u128>,
    pub initial_balance_usd: Option<f64>,
    pub reserve_wei: Option<u128>,
    pub max_fills_per_block: Option<usize>,
    pub native_usd: Option<f64>,
    /// Override `[paper].sim` (`assumed` | `state`).
    pub sim: Option<String>,
    /// Override `[paper].sim_max_candidates_per_block`.
    pub sim_max_candidates_per_block: Option<usize>,
    /// Override `[paper].exec` (`none` | `revm` | `anvil`).
    pub exec: Option<String>,
    /// Override `[backtest].flash_loan_provider` for this live session.
    pub funding_provider: Option<String>,
    /// Override `[live].max_quote_age_ms`.
    pub max_quote_age_ms: Option<u64>,
    /// Override `[live].require_risk_clear`.
    pub require_risk_clear: bool,
    /// Override `[live].pending_backrun`.
    pub pending_backrun: bool,
}

/// Resolved capital-free live gate policy (CLI overrides applied).
#[derive(Debug, Clone, Default)]
pub struct LiveGateConfig {
    pub max_quote_age_ms: Option<u64>,
    pub require_risk_clear: bool,
    pub pending_backrun: bool,
    pub pending_swap_min_wei: u128,
    /// TTL stamped onto re-quoted pending opportunities.
    pub pending_quote_ttl_ms: u64,
}

/// Per-session counters for the gate, used only by the ledger summary.
#[derive(Debug, Clone, Default)]
pub struct LiveGateStats {
    /// All candidates that entered the gate (denominator for quote rates).
    pub quote_gate_attempts: u64,
    pub expired: u64,
    pub drift: u64,
    pub risk_blocked: u64,
    /// Estimated same-block bid premium: `gas * winning_bid_premium`.
    pub competition_loss_wei: u128,
    pub funding_source: Option<String>,
}

/// Default re-quote TTL when pending-backrun is enabled (2 Avalanche blocks).
pub const DEFAULT_PENDING_QUOTE_TTL_MS: u64 = 4_000;

/// Default native-value floor for a pending swap re-quote trigger (1 AVAX).
pub const DEFAULT_PENDING_SWAP_MIN_WEI: u128 = 1_000_000_000_000_000_000;

/// Resolve the live gate policy from `[live]` plus CLI overrides.
fn resolve_gate_config(config: &Config, opts: &LiveOpts) -> anyhow::Result<LiveGateConfig> {
    let live = &config.live;
    let min_wei = match live.pending_backrun_min_wei.as_deref() {
        Some(raw) => raw.parse::<u128>().with_context(|| {
            format!("invalid [live].pending_backrun_min_wei '{raw}': expected wei integer")
        })?,
        None => DEFAULT_PENDING_SWAP_MIN_WEI,
    };
    Ok(LiveGateConfig {
        max_quote_age_ms: opts.max_quote_age_ms.or(live.max_quote_age_ms),
        require_risk_clear: opts.require_risk_clear || live.require_risk_clear,
        pending_backrun: opts.pending_backrun || live.pending_backrun,
        pending_swap_min_wei: min_wei,
        pending_quote_ttl_ms: DEFAULT_PENDING_QUOTE_TTL_MS,
    })
}

/// Split one pass's opportunities at the quote-freshness gate.
///
/// Only the `fresh` slice may reach `persist_results` / `update_ledger`; every
/// gated candidate comes back with the exact reject reason to record (M-07).
/// Deadline expiry takes precedence over age/drift, and risk gating is last so
/// a stale quote is not mislabelled as a generic risk block.
pub fn split_by_quote_freshness(
    opps: Vec<MevOpportunity>,
    now_ms: u64,
    gate: &LiveGateConfig,
) -> (Vec<MevOpportunity>, Vec<(MevOpportunity, RejectReason)>) {
    let mut fresh = Vec::with_capacity(opps.len());
    let mut gated = Vec::new();
    for opp in opps {
        let reason = if opp
            .quote_expires_at
            .is_some_and(|deadline| now_ms >= deadline)
        {
            Some(RejectReason::QuoteExpired)
        } else if opp.risk_flags.iter().any(|f| f == "quote_drift") {
            Some(RejectReason::QuoteDrift)
        } else if let Some(max_age) = gate.max_quote_age_ms {
            // `timestamp` is the block/pending state time in seconds. Pending
            // re-quotes stamp it from wall-clock time, so the same rule covers
            // both replayed and mempool-sourced candidates.
            let created_ms = opp.timestamp.saturating_mul(1000);
            (now_ms.saturating_sub(created_ms) > max_age).then_some(RejectReason::QuoteDrift)
        } else {
            None
        };
        let reason = reason.or_else(|| {
            (gate.require_risk_clear && !opp.risk_flags.is_empty())
                .then_some(RejectReason::RiskFlagBlocked)
        });
        match reason {
            Some(reason) => gated.push((opp, reason)),
            None => fresh.push(opp),
        }
    }
    (fresh, gated)
}

pub struct LiveOneShotOutcome {
    pub opportunities: Vec<MevOpportunity>,
    pub block_stats: Vec<BlockReplayStats>,
}

pub struct LiveLoopOutcome {
    /// Per-block stats accumulated across every pass of the session, so the
    /// CLI can render the same summary table the one-shot path produces.
    pub block_stats: Vec<BlockReplayStats>,
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

/// Resolve sim policy from `[paper]` plus CLI `--sim` override.
fn resolve_sim_policy(
    config: &Config,
    opts: &LiveOpts,
    provider: FlashLoanProvider,
) -> anyhow::Result<SimPolicy> {
    let mut policy = config.paper.sim_policy()?;
    if let Some(raw) = opts.sim.as_deref() {
        policy.mode = SimMode::parse(raw).ok_or_else(|| {
            anyhow::anyhow!("invalid --sim '{raw}': expected 'assumed' or 'state'")
        })?;
    }
    if let Some(n) = opts.sim_max_candidates_per_block {
        policy.max_candidates_per_block = n.max(1);
    }
    // Fee bps from the effective provider: config or `--funding-provider`.
    policy.fee_bps = provider.fee_rate_bps();
    Ok(policy)
}

/// Configured priority fee in wei, saturating instead of wrapping on absurd input.
fn priority_fee_wei(config: &Config) -> u128 {
    let gwei = config.gas.priority_fee_gwei;
    if !gwei.is_finite() || gwei <= 0.0 {
        return 0;
    }
    let wei = gwei * 1_000_000_000.0;
    if !wei.is_finite() || wei >= u128::MAX as f64 {
        u128::MAX
    } else {
        wei.round() as u128
    }
}

fn resolve_exec_context(
    config: &Config,
    opts: &LiveOpts,
    validation: &ValidationResult,
) -> anyhow::Result<ExecContext> {
    let mut mode = config.paper.exec_mode()?;
    if let Some(raw) = opts.exec.as_deref() {
        mode = ExecMode::parse(raw).ok_or_else(|| {
            anyhow::anyhow!("invalid --exec '{raw}': expected 'none', 'revm', or 'anvil'")
        })?;
    }
    let chain_id = validation.chain_config.chain_id;
    let key = if !config.paper.exec_key.trim().is_empty() {
        Some(config.paper.exec_key.trim().to_string())
    } else {
        std::env::var("MEV_SCOUT_EXEC_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let searcher = searcher_address(key.as_deref())?;
    let urls = config.effective_rpc_urls(validation.chain_name)?;
    let fork_url = urls.first().cloned().unwrap_or_default();
    let anvil = if mode == ExecMode::Anvil && !config.paper.anvil_url.trim().is_empty() {
        Some(AnvilNode::attach(
            config.paper.anvil_url.trim().to_string(),
        )?)
    } else {
        None
    };
    Ok(ExecContext {
        mode,
        chain_id,
        vault: validation
            .chain_config
            .balancer_vault
            .unwrap_or(Address::ZERO),
        weth: validation
            .chain_config
            .wrapped_native_token
            .unwrap_or(Address::ZERO),
        searcher,
        provider: validation.flash_loan_provider,
        fork_url,
        exec_key: key,
        anvil_bin: if config.paper.anvil_bin.trim().is_empty() {
            "anvil".to_string()
        } else {
            config.paper.anvil_bin.clone()
        },
        anvil,
        anvil_executor: None,
        anvil_block: None,
        routers: crate::exec::VenueRouters {
            lb_router_v21: validation.chain_config.lb_router_v21,
            lb_router_v22: validation.chain_config.lb_router_v22,
            lb_factory_versions: validation
                .chain_config
                .lb_factory_versions
                .clone()
                .unwrap_or_default(),
            pendle_router: validation.chain_config.pendle_router,
        },
        cache: Default::default(),
    })
}

/// Resolve the native USD price once per session.
///
/// Order: cached ZERO-keyed price, then the wrapped-native cache, then
/// DefiLlama, then CoinGecko.
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

/// Inclusive span of blocks that were actually scanned. An empty `span`
/// means nothing has been scanned yet, so the first range becomes the span
/// instead of being unioned with a tip that was only observed at startup.
fn widen_span(span: Option<(u64, u64)>, start: u64, end: u64) -> (u64, u64) {
    match span {
        None => (start, end),
        Some((lo, hi)) => (start.min(lo), end.max(hi)),
    }
}

/// Ledger inputs resolved once at session start.
struct ResolvedLedger {
    policy: LedgerPolicy,
    sim: SimPolicy,
    native_usd: Option<f64>,
    /// Gas inputs captured once per session. Basefee/block are filled from the
    /// first scanned block in `LiveContext::new`; the native price is the same
    /// AVAX quote used for the ledger's USD display.
    gas_quote: GasQuote,
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
    /// Stable for the whole session. One-shot is a single pass and generates
    /// one id; the loop reuses this for every pass.
    session_run_id: String,
    /// Inclusive span of blocks actually fetched and scanned, widened by
    /// [`Self::extend_session`]. Unset until the first successful pass, so the
    /// tip observed during setup is not recorded as scanned. The manifest is
    /// rewritten on each pass with these bounds so `report` sees the whole
    /// session as one run.
    scanned_span: Option<(u64, u64)>,
    /// Ledger applied over every opportunity seen so far this session.
    /// Recomputed from scratch each pass — `LedgerPolicy::apply` is pure and
    /// re-derives block grouping itself, so no incremental state to thread.
    policy: LedgerPolicy,
    sim: SimPolicy,
    session_opps: Vec<MevOpportunity>,
    /// Pool snapshots accumulated across passes (keyed by canonical_id).
    session_seeds: SimSeeds,
    /// Native price resolved once at startup; `None` when unavailable and the
    /// session does not need it (P&L stays in wei).
    native_usd: Option<f64>,
    /// Basefee/priority/native price tied to the session's simulation block.
    gas_quote: GasQuote,
    /// Persisted once at session end — `paper_sessions` keys on a plain
    /// `session_id`, so re-inserting the same id would violate the constraint.
    session_id: String,
    /// Blocks scanned via full revm vs log-only (session counters for report).
    replay_blocks: u64,
    log_only_blocks: u64,
    /// Seed-budget diagnostics for report (`seed_budget` vs `fills capped`).
    seeds_capped: usize,
    /// Virtual execution. `mode == none` leaves the paper ledger on sim/assumed.
    exec: ExecContext,
    /// Hourly Benqi sAVAX exchange-rate cache (shared with the explorer).
    savax_cache: crate::chain::savax::SavaxRateCache,
    /// Resolved quote/risk/pending gate policy for this session.
    gate: LiveGateConfig,
    /// Gate counters accumulated across passes for the ledger summary.
    gate_stats: LiveGateStats,
}

impl<'a> LiveContext<'a> {
    /// Eight distinct collaborators; grouping them into a struct would only
    /// move the same fields one level down and hide the call-site contract.
    #[allow(clippy::too_many_arguments)]
    async fn new(
        config: &'a Config,
        validation: ValidationResult,
        rpc: RpcClient,
        provider_configs: Vec<ProviderConfig>,
        record_rejections: bool,
        progress: &'a dyn JobProgress,
        ledger: ResolvedLedger,
        exec: ExecContext,
        gate: LiveGateConfig,
    ) -> anyhow::Result<LiveContext<'a>> {
        let chain_name = validation.chain_name;
        let cache = SqliteStore::open(config.effective_db_path(&chain_name))?;
        let tip = rpc
            .get_block_number()
            .await
            .context("failed to get chain tip")?;

        // Gas quote is tied to the block the simulators actually read. Pool
        // initialization uses tip-1, so use the same block here; falling back
        // to zero basefee is explicit and never silently swaps in a stablecoin
        // price. Price lookup failure is non-fatal (P&L stays in wei).
        let mut gas_quote = ledger.gas_quote;
        let quote_block = tip.saturating_sub(1);
        let basefee = cache
            .get_block(quote_block)
            .ok()
            .flatten()
            .and_then(|b| b.base_fee_per_gas);
        let basefee = match basefee {
            Some(v) => Some(v),
            None => rpc
                .get_block(quote_block)
                .await
                .ok()
                .and_then(|(b, _)| b.base_fee_per_gas),
        };
        gas_quote.basefee_wei = basefee.unwrap_or(0);
        gas_quote.sim_block = Some(quote_block);

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

        let wrapped_native = validation
            .chain_config
            .wrapped_native_token
            .unwrap_or(Address::ZERO);
        let mut blue_chips = validation
            .chain_config
            .profit_stable_tokens
            .clone()
            .unwrap_or_default();
        if !wrapped_native.is_zero() {
            blue_chips.push(wrapped_native);
        }
        blue_chips.retain(|a| !a.is_zero());
        let registry = crate::jobs::load_pool_registry(config, &chain_name);
        let tag_policy = crate::mev::detectors::ArbTagPolicy {
            blue_chips,
            wrapped_native,
            savax: validation.chain_config.savax,
            epoch_venue_pools: registry.epoch_venue.into_iter().collect(),
            gmx_market_tokens: validation
                .chain_config
                .gmx_market_tokens
                .clone()
                .unwrap_or_default(),
            gmx_event_emitters: validation
                .chain_config
                .gmx_event_emitters
                .clone()
                .unwrap_or_default(),
            flash_financed: ledger.sim.flash_loan,
            funding_source: Some(validation.flash_loan_provider.funding_source().to_string()),
            funding_vault: validation.chain_config.balancer_vault,
            savax_rate_wad: None,
        };

        let mut pool_manager = PoolManager::new();
        pool_manager.set_max_pairs_per_token(config.backtest.max_pairs_per_token);
        pool_manager.set_tax_quoting(config.live.fot_quote);
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
        let runner = BacktestRunner::new(replayer, pool_manager, gas_config)
            .with_capture_pending(config.backtest.capture_pending || gate.pending_backrun)
            .with_pending_backrun(
                gate.pending_backrun,
                gate.pending_quote_ttl_ms,
                gate.pending_swap_min_wei,
            )
            .with_min_profit_wei(config.backtest.min_profit_wei)
            .with_max_candidates_per_tx(config.backtest.max_candidates_per_tx)
            .with_record_rejections(record_rejections)
            .with_sim_seeds(
                ledger.sim.is_state_sim(),
                ledger.sim.max_candidates_per_block,
            )
            .with_tag_policy(tag_policy);

        let funding_source = validation.flash_loan_provider.funding_source().to_string();
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
            session_run_id: format!("live_{}", epoch_secs()),
            scanned_span: None,
            policy: ledger.policy,
            sim: ledger.sim,
            session_opps: Vec::new(),
            session_seeds: SimSeeds::new(),
            native_usd: ledger.native_usd,
            gas_quote,
            session_id: format!("paper_live_{}", epoch_secs()),
            replay_blocks: 0,
            log_only_blocks: 0,
            seeds_capped: 0,
            exec,
            savax_cache: Default::default(),
            gate,
            gate_stats: LiveGateStats {
                funding_source: Some(funding_source),
                ..Default::default()
            },
        })
    }

    /// Refresh the sAVAX exchange rate at most once per hour and hand it to
    /// the detector/tag policy. A missing rate disables sAVAX emissions.
    async fn refresh_savax_rate(&mut self, block: u64) {
        let rate = crate::chain::savax::fetch_savax_exchange_rate(
            &self.rpc,
            self.validation.chain_config.savax,
            block,
            epoch_secs(),
            &mut self.savax_cache,
        )
        .await;
        match rate {
            Some(rate) => self.runner.set_savax_rate(Some(rate)),
            None => {
                self.runner.set_savax_rate(None);
                tracing::debug!("sAVAX exchangeRate unavailable for this hour");
            }
        }
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
        let (opps, stats, modes) =
            self.runner
                .run_range_hybrid(resolved, state_horizon, progress)?;
        for mode in &modes {
            match mode {
                BlockMode::FullReplay => self.replay_blocks += 1,
                BlockMode::LogOnly => self.log_only_blocks += 1,
            }
        }
        if self.sim.is_state_sim() {
            let incoming = self.runner.take_sim_seeds();
            merge_seeds(&mut self.session_seeds, incoming);
            self.seeds_capped = self
                .seeds_capped
                .saturating_add(self.runner.take_seeds_capped());
        }
        Ok((opps, stats))
    }

    /// Apply the capital-free live gate before anything is persisted or
    /// booked. Gated candidates are recorded as rejects with the full quote
    /// context; only fresh ones are allowed to reach the ledger (M-07).
    fn apply_gate(&mut self, opps: Vec<MevOpportunity>, block_number: u64) -> Vec<MevOpportunity> {
        self.gate_stats.quote_gate_attempts = self
            .gate_stats
            .quote_gate_attempts
            .saturating_add(opps.len() as u64);
        let gate = self.gate.clone();
        let (fresh, gated) = split_by_quote_freshness(opps, epoch_millis(), &gate);
        for (opp, reason) in gated {
            match reason {
                RejectReason::QuoteExpired => self.gate_stats.expired += 1,
                RejectReason::QuoteDrift => self.gate_stats.drift += 1,
                RejectReason::RiskFlagBlocked => self.gate_stats.risk_blocked += 1,
                _ => {}
            }
            self.runner.note_rejection(
                &opp,
                block_number,
                reason,
                Some("live quote gate: not admitted to ledger".into()),
            );
        }
        // Estimated same-block competition cost: the configured PGA premium
        // paid on top of each admitted candidate's simulated gas. This is an
        // estimate, never a realized fill P&L adjustment.
        let premium = self.config.gas.winning_bid_premium;
        if premium.is_finite() && premium > 0.0 {
            let extra: u128 = fresh
                .iter()
                .map(|o| (o.gas_cost_wei as f64 * premium) as u128)
                .sum();
            self.gate_stats.competition_loss_wei =
                self.gate_stats.competition_loss_wei.saturating_add(extra);
        }
        fresh
    }

    /// Widen the scanned span to cover `resolved`. The first call sets the
    /// span; later calls only grow it. Callers must pass blocks that were
    /// actually fetched — a tip observed but not scanned must not be included.
    fn extend_session(&mut self, resolved: &ResolvedRange) {
        self.scanned_span = Some(widen_span(
            self.scanned_span,
            resolved.start_block,
            resolved.end_block,
        ));
    }

    fn scanned_bounds(&self) -> (u64, u64) {
        // Invariant: persist_results / persist_ledger_session only run after at
        // least one successful pass has set `scanned_span`.
        #[allow(clippy::expect_used)]
        {
            self.scanned_span
                .expect("persist called before any block was scanned")
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
        // Canonical funding-layer value (config spellings like `auto`/`lfj`
        // are normalized); `ResultsFile` keeps the original config string.
        let funding_source = self
            .validation
            .flash_loan_provider
            .funding_source()
            .to_string();

        // Stamp the session gas/funding context onto each opportunity before
        // persistence. Detectors leave these fields unset; the quote is the
        // same one the simulator used, so explorer rows can reconstruct the
        // AVAX-denominated cost later without a different price feed.
        let quote = self.gas_quote;
        let mut stamped: Vec<MevOpportunity> = opps.to_vec();
        for opp in &mut stamped {
            if opp.sim_block.is_none() {
                opp.sim_block = quote.sim_block;
            }
            if opp.basefee_wei.is_none() {
                opp.basefee_wei = Some(quote.basefee_wei);
            }
            if opp.priority_fee_wei.is_none() {
                opp.priority_fee_wei = Some(quote.priority_fee_wei);
            }
            if opp.funding_source.is_none() {
                opp.funding_source = Some(funding_source.clone());
            }
        }

        let (start_block, end_block) = self.scanned_bounds();
        let results_file = ResultsFile {
            run_id: run_id.clone(),
            chain: chain_name.to_string(),
            start_block,
            end_block,
            range_mode: "live".to_string(),
            strategies: strategies.clone(),
            flash_loan_provider: flash_loan_provider.clone(),
            resolved_at: epoch_secs(),
            created_at: epoch_secs(),
            opportunities: stamped,
        };
        let manifest = RunManifest {
            run_id: run_id.clone(),
            chain: chain_name.to_string(),
            start_block,
            end_block,
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
    async fn update_ledger(&mut self, opps: &[MevOpportunity]) -> anyhow::Result<LedgerResult> {
        self.session_opps.extend_from_slice(opps);
        let mut ledger = if self.exec.mode.is_off() {
            if self.sim.is_state_sim() {
                self.policy.apply_simulated(
                    &self.session_opps,
                    &self.session_seeds,
                    &self.sim,
                    Some(self.runner.pool_manager()),
                )
            } else {
                self.policy.apply(&self.session_opps)
            }
        } else {
            self.ensure_exec_ready().await?;
            let replayer = self.runner.replayer();
            let pools = self.runner.pool_manager();
            execute_ledger(
                &self.policy,
                &self.session_opps,
                self.sim.mode,
                replayer,
                pools,
                &mut self.exec,
            )?
        };
        if !self.exec.mode.is_off() && self.sim.is_state_sim() {
            let sim_ledger = self.policy.apply_simulated(
                &self.session_opps,
                &self.session_seeds,
                &self.sim,
                Some(self.runner.pool_manager()),
            );
            reconcile_sim_vs_exec(&mut ledger, &sim_ledger);
        }
        ledger.sim_mode = self.sim.mode;
        ledger.replay_blocks = self.replay_blocks;
        ledger.log_only_blocks = self.log_only_blocks;
        ledger.seed_budget = self.sim.max_candidates_per_block;
        ledger.seeds_recorded = self.session_seeds.len();
        ledger.seeds_capped = self.seeds_capped;

        // Same quote that priced the simulation: no spot/stablecoin substitution.
        let quote = self.gas_quote;
        ledger.basefee_wei = Some(quote.basefee_wei);
        ledger.priority_fee_wei = Some(quote.priority_fee_wei);
        ledger.sim_block = quote.sim_block;
        ledger.native_usd = quote.native_usd;
        ledger.manipulation_risk = quote.manipulation_risk;
        tracing::debug!(
            "gas quote: block={:?} basefee={} priority={} effective={} one_gas_wei={} one_gas_usd={:?} native_usd={:?} risk={}",
            quote.sim_block,
            quote.basefee_wei,
            quote.priority_fee_wei,
            quote.gas_price_wei(),
            quote.gas_cost_wei(1),
            quote.gas_cost_usd(1),
            quote.native_usd,
            quote.manipulation_risk,
        );
        Ok(ledger)
    }

    async fn ensure_exec_ready(&mut self) -> anyhow::Result<()> {
        match self.exec.mode {
            ExecMode::None | ExecMode::Revm => Ok(()),
            ExecMode::Anvil => {
                if self.exec.anvil.is_some() {
                    return Ok(());
                }
                let bin = self.exec.anvil_bin.clone();
                let fork = self.exec.fork_url.clone();
                if fork.is_empty() {
                    anyhow::bail!("--exec anvil needs an RPC url to fork");
                }
                let mut node = AnvilNode::spawn(&bin, &fork, self.tip)?;
                node.wait_ready().await.map_err(|e| {
                    anyhow::anyhow!(
                        "{e}\n--exec anvil forks `{fork}` at block {}. \
                         Public/free RPCs often 408 here; use a local tip node \
                         or set [paper].anvil_url to a ready loopback fork.",
                        self.tip
                    )
                })?;
                self.progress
                    .log(&format!("exec: anvil fork at {}", node.url));
                self.exec.anvil = Some(node);
                Ok(())
            }
        }
    }

    /// Persist the finished session ledger. Called once — `insert_paper_session`
    /// is a plain `INSERT` on a `session_id` primary key
    /// (`paper/store.rs:57,69`), so a second call would violate the constraint.
    fn persist_ledger_session(&self, ledger: &LedgerResult) -> anyhow::Result<String> {
        let chain = self.validation.chain_name;
        let store = ExplorerStore::open(self.config.effective_explorer_db_path(&chain))?;
        // `insert_paper_session` derives the block range from the ledger
        // (`paper/store.rs:66`), but those fields come from the *fills* and are
        // `None` when a session finds nothing — which would record the session
        // as blocks 0-0 despite having scanned a real range. The context
        // tracks the true session span via `extend_session`, so prefer it and
        // keep the ledger's value only as a fallback.
        let mut ledger = ledger.clone();
        if let Some((start, end)) = self.scanned_span {
            ledger.start_block = Some(start);
            ledger.end_block = Some(end);
        }
        let metrics = session_metrics(&ledger, &self.gate_stats);
        store.insert_paper_session(
            &self.session_id,
            &chain.to_string(),
            PaperMode::Live,
            Some(&self.session_run_id),
            &ledger,
            &metrics,
        )?;
        Ok(self.session_id.clone())
    }
}

/// WP6 metrics for one finished session. Quote failures are a share of all
/// gate candidates; gas shares use accepted-fill gas as their basis.
fn session_metrics(ledger: &LedgerResult, gate: &LiveGateStats) -> BacktestMetrics {
    BacktestMetrics::from_parts(
        gate.quote_gate_attempts,
        gate.drift,
        gate.expired,
        ledger.gas_wei,
        ledger.funding_fee_wei,
        ledger.revert_gas_wei,
        gate.competition_loss_wei,
    )
}

/// Render one ledger line set for progress output.
///
/// `not_native_unit` is counted and labelled rather than lumped in with real
/// skips. `is_native_eligible` only admits native-normalized strategies, so a
/// row in another profit unit must still read as a labeled skip, not as a
/// silent failure.
pub fn render_ledger_summary(ledger: &LedgerResult, native_usd: Option<f64>) -> String {
    render_ledger_summary_with_gate(ledger, native_usd, None)
}

/// Same summary plus the three capital-free live lines (M-09). Existing
/// `exec:`/`recon:`/`caps:`/`blocks:` lines are deliberately not duplicated.
pub fn render_ledger_summary_with_gate(
    ledger: &LedgerResult,
    native_usd: Option<f64>,
    gate: Option<&LiveGateStats>,
) -> String {
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
    let state_fills = ledger
        .fills
        .iter()
        .filter(|f| f.sim_mode == SimMode::StateSim)
        .count();
    let assumed_fills = ledger.fills.len().saturating_sub(state_fills);
    let sim_skips = ledger
        .skips
        .iter()
        .filter(|s| {
            matches!(
                s.reason,
                FillSkipReason::SimNotQuotable
                    | FillSkipReason::SimPriceConsumed
                    | FillSkipReason::SimRepayShortfall
                    | FillSkipReason::SimPathUnavailable
                    | FillSkipReason::SimStrategyUnsupported
            )
        })
        .count();
    out.push_str(&format!(
        "  sim: {} | state-sim {state_fills} fill / {sim_skips} skipped | assumed {assumed_fills}\n",
        ledger.sim_mode.as_str(),
    ));
    if ledger.exec_mode != "none" {
        let mut exec_line = format!(
            "  exec: {} {} ok / {} revert",
            ledger.exec_mode, ledger.exec_ok, ledger.exec_revert,
        );
        if ledger.revert_gas_wei > 0 {
            exec_line.push_str(&format!(" | revert gas {}", ledger.revert_gas_wei));
        }
        if ledger.exec_loss_wei > 0 {
            exec_line.push_str(&format!(" | loss {}", ledger.exec_loss_wei));
        }
        if ledger.setup_gas_wei > 0 {
            exec_line.push_str(&format!(" | setup {}", ledger.setup_gas_wei));
        }
        exec_line.push('\n');
        out.push_str(&exec_line);
    }
    if let (Some(pass), Some(fail), Some(unverifiable)) = (
        ledger.recon_pass,
        ledger.recon_fail,
        ledger.recon_unverifiable,
    ) {
        out.push_str(&format!(
            "  recon: state vs exec {pass} pass / {fail} fail / {unverifiable} unverifiable\n",
        ));
    }
    if ledger.sim_mode == SimMode::StateSim
        || ledger.seed_budget > 0
        || ledger.seeds_recorded > 0
        || ledger.seeds_capped > 0
    {
        out.push_str(&format!(
            "  caps: fill_slots≤{HARD_MAX_FILLS_PER_BLOCK} | seed_budget={} (recorded {}, capped {})\n",
            ledger.seed_budget, ledger.seeds_recorded, ledger.seeds_capped,
        ));
    }
    if ledger.replay_blocks > 0 || ledger.log_only_blocks > 0 {
        out.push_str(&format!(
            "  blocks: {} replay / {} log-only\n",
            ledger.replay_blocks, ledger.log_only_blocks
        ));
    }
    out.push_str(&format!(
        "  wallet: {}{usd_start} → {}{usd_end} | reserve {} | max drawdown {} wei",
        ledger.starting_gas_wei,
        ledger.ending_gas_wei,
        ledger.reserve_wei,
        ledger.max_drawdown_wei,
        usd_start = usd_of(ledger.starting_gas_wei),
        usd_end = usd_of(ledger.ending_gas_wei),
    ));
    if let Some(gate) = gate {
        let source = gate.funding_source.as_deref().unwrap_or("unspecified");
        out.push_str(&format!(
            "\n  funding: {source} | fee {} wei{}\n",
            ledger.funding_fee_wei,
            usd_of(ledger.funding_fee_wei),
        ));
        out.push_str(&format!(
            "  quotes: {} expired / {} drift | {} risk-blocked (not in ledger)\n",
            gate.expired, gate.drift, gate.risk_blocked,
        ));
        out.push_str(&format!(
            "  competition: est. {} wei{} same-block bid premium\n",
            gate.competition_loss_wei,
            usd_of(gate.competition_loss_wei),
        ));
        let m = session_metrics(ledger, gate);
        out.push_str(&format!(
            "  metrics: drift {:.2}% | expiry {:.2}% | competition {:.2}% | funding/gas {:.2}% | revert/gas {:.2}%\n",
            m.quote_drift_failure_rate * 100.0,
            m.quote_expiry_failure_rate * 100.0,
            m.same_block_competition_loss_rate * 100.0,
            m.funding_fee_share_of_gas * 100.0,
            m.revert_gas_share * 100.0,
        ));
    }
    out.push_str(&render_by_strategy(&ledger.fills, native_usd));
    out.push_str(&render_by_tag(&ledger.fills, native_usd));
    let not_native = ledger
        .skips
        .iter()
        .filter(|s| s.reason == FillSkipReason::NotNativeUnit)
        .count();
    if not_native > 0 {
        out.push_str(&format!(
            "\n  note: {not_native} candidate(s) skipped as not_native_unit \
             (profit not native-denominated)"
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
    render_metric_table(
        "by strategy (fills accepted):",
        agg.by_strategy.into_values().collect(),
        native_usd,
    )
}

/// Per-tag attribution. Tag buckets overlap: one fill may appear under several
/// tags, so these rows do not add up to the strategy table above.
fn render_by_tag(fills: &[PaperFill], native_usd: Option<f64>) -> String {
    if fills.is_empty() {
        return String::new();
    }
    let agg = aggregate_fills(fills, native_usd.unwrap_or(0.0));
    if agg.by_tag.is_empty() {
        return String::new();
    }
    render_metric_table(
        "by tag (fills accepted, overlapping):",
        agg.by_tag.into_values().collect(),
        native_usd,
    )
}

fn render_metric_table(
    title: &str,
    mut rows: Vec<StrategyMetrics>,
    native_usd: Option<f64>,
) -> String {
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

    let mut out = format!("\n  {title}\n");
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
    // Fail fast on unknown --exec before any RPC / price work.
    if let Some(raw) = opts.exec.as_deref() {
        ExecMode::parse(raw).ok_or_else(|| {
            anyhow::anyhow!("invalid --exec '{raw}': expected 'none', 'revm', or 'anvil'")
        })?;
    }
    // Before any price lookup or RPC: both flags describe the same wallet,
    // and resolving a price first would hit the network for a request that
    // cannot succeed.
    if opts.initial_balance_wei.is_some() && opts.initial_balance_usd.is_some() {
        anyhow::bail!(
            "--initial-balance and --initial-balance-usd are mutually exclusive; \
             pass the wallet in wei (exact, no price lookup) or in USD (needs a price)"
        );
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

    let mut validation = validation::validate_live(config).context("invalid configuration")?;
    // A CLI funding override must be parsed before any RPC work so a typo fails
    // locally. `Auto` keeps the resolver's cheapest-provider behaviour.
    if let Some(raw) = opts.funding_provider.as_deref() {
        validation.flash_loan_provider = FlashLoanProvider::from_str(raw).map_err(|_| {
            anyhow::anyhow!(
                "invalid --funding-provider '{raw}': expected auto, balancer, aave, uniswap, lfj, or v2_flash_swap"
            )
        })?;
    }
    let gate = resolve_gate_config(config, opts)?;
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
    let sim = resolve_sim_policy(config, opts, validation.flash_loan_provider)?;
    let ledger = ResolvedLedger {
        policy,
        sim,
        native_usd,
        gas_quote: GasQuote {
            basefee_wei: 0,
            priority_fee_wei: priority_fee_wei(config),
            native_usd,
            sim_block: None,
            manipulation_risk: false,
        },
    };
    let exec = resolve_exec_context(config, opts, &validation)?;
    if !exec.mode.is_off() {
        progress.log(&format!("exec: {}", exec.mode));
    }

    let setup = init_rpc(config, validation.chain_name, true).await?;
    let mut ctx = LiveContext::new(
        config,
        validation,
        setup.rpc,
        setup.provider_configs,
        opts.record_rejections,
        progress,
        ledger,
        exec,
        gate,
    )
    .await?;

    progress.emit(ProgressEvent::stage("resolve"));

    if opts.loop_enabled {
        let summary = run_loop(&mut ctx, deadline, opts.poll_interval_ms, opts.max_blocks).await?;
        Ok(LiveOutcome::Loop(summary))
    } else {
        let pass = run_once(&mut ctx, opts.single_pass_blocks).await?;
        Ok(LiveOutcome::OneShot(pass))
    }
}

async fn run_once(
    ctx: &mut LiveContext<'_>,
    single_pass_blocks: u64,
) -> anyhow::Result<LiveOneShotOutcome> {
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

    // `0` means "unspecified" for `LiveOpts::default`, so clamp to the tip
    // block rather than emitting a zero-width range.
    let width = single_pass_blocks.max(1);
    let start_block = tip.saturating_sub(width.saturating_sub(1));
    let span_label = if width == 1 {
        format!("Latest block: {tip}")
    } else {
        format!("Latest {width} blocks: {start_block}-{tip}")
    };
    progress.log(&format!("Run ID: {run_id}"));
    progress.log(&span_label);

    let resolved = ResolvedRange {
        start_block,
        end_block: tip,
        block_count: tip - start_block + 1,
        mode: if width == 1 {
            RangeMode::Single(tip)
        } else {
            RangeMode::Range(start_block, tip)
        },
    };

    let start = Instant::now();
    ctx.refresh_savax_rate(tip).await;
    let fetch_done = Arc::new(AtomicU64::new(0));
    let total_blocks = resolved.block_count;
    let tick = move || {
        if progress.cancelled() {
            return false;
        }
        let d = fetch_done.fetch_add(1, Ordering::Relaxed) + 1;
        progress.emit(ProgressEvent {
            stage: "fetch".to_string(),
            done: Some(d),
            total: Some(total_blocks),
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
    let (raw_opps, block_stats) = ctx.run_blocks(&resolved, Some(&detect)).await?;
    let opps = ctx.apply_gate(raw_opps, tip);
    let elapsed = start.elapsed();
    ctx.extend_session(&resolved);
    ctx.persist_results(&opps);
    let ledger = ctx.update_ledger(&opps).await?;
    if let Err(e) = ctx.persist_ledger_session(&ledger) {
        // Not fatal: the run results and manifest are already written, so
        // the session is still usable via `report`. Losing only the P&L row
        // should not discard a completed scan.
        tracing::warn!("paper-session persist failed: {e}");
    }
    progress.log(&render_ledger_summary_with_gate(
        &ledger,
        ctx.native_usd,
        Some(&ctx.gate_stats),
    ));

    progress.emit(ProgressEvent {
        stage: "complete".to_string(),
        done: None,
        total: None,
        run_id: Some(run_id.clone()),
        ops: Some(opps.len() as u64),
        elapsed_ms: Some(elapsed.as_millis() as u64),
    });

    progress.log(&format!(
        "Block {start_block}-{tip} — {} opportunity(ies) detected",
        opps.len()
    ));

    Ok(LiveOneShotOutcome {
        opportunities: opps,
        block_stats,
    })
}

struct LoopStats {
    blocks_processed: u64,
    total_txs_scanned: usize,
    total_dex_txs: usize,
    total_pending_txs: usize,
    total_opportunities: usize,
    block_stats: Vec<BlockReplayStats>,
    consecutive_failures: u32,
}

fn should_stop(
    progress: &dyn JobProgress,
    deadline: Option<Instant>,
    max_blocks: Option<u64>,
    stats: &LoopStats,
) -> bool {
    if progress.cancelled() {
        return true;
    }
    if let Some(dl) = deadline {
        if Instant::now() >= dl {
            return true;
        }
    }
    if let Some(mb) = max_blocks {
        if stats.blocks_processed >= mb {
            return true;
        }
    }
    false
}

async fn poll_tip_with_retry(
    ctx: &LiveContext<'_>,
    consecutive_failures: &mut u32,
    max_consecutive_failures: u32,
) -> anyhow::Result<Option<u64>> {
    match ctx.rpc.get_block_number().await {
        Ok(n) => Ok(Some(n)),
        Err(e) => {
            *consecutive_failures += 1;
            tracing::warn!(
                "Failed to get block number ({}/{}): {}",
                *consecutive_failures,
                max_consecutive_failures,
                e
            );
            if *consecutive_failures >= max_consecutive_failures {
                anyhow::bail!(
                    "giving up after {max_consecutive_failures} consecutive RPC failures"
                );
            }
            Ok(None)
        }
    }
}

/// Fetch and backtest one tip range. Returns `Ok(true)` when the pass completed;
/// `Ok(false)` when fetch/backtest failed under the failure limit (caller should retry).
async fn process_pass(
    ctx: &mut LiveContext<'_>,
    last_block: &mut u64,
    stats: &mut LoopStats,
    current_tip: u64,
    run_id: &str,
    max_consecutive_failures: u32,
) -> anyhow::Result<bool> {
    let progress = ctx.progress;
    let from_block = *last_block + 1;
    let block_count = current_tip - from_block + 1;
    let resolved = ResolvedRange {
        start_block: from_block,
        end_block: current_tip,
        block_count,
        mode: RangeMode::Range(from_block, current_tip),
    };

    let pass_start = Instant::now();

    ctx.refresh_savax_rate(current_tip).await;

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
        stats.consecutive_failures += 1;
        tracing::warn!(
            "Fetch failed for blocks {}–{} ({}/{}): {} — will retry same range",
            from_block,
            current_tip,
            stats.consecutive_failures,
            max_consecutive_failures,
            e
        );
        if stats.consecutive_failures >= max_consecutive_failures {
            anyhow::bail!(
                "giving up after {max_consecutive_failures} consecutive fetch/backtest failures"
            );
        }
        return Ok(false);
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
    let (raw_opps, pass_stats) = match ctx.run_blocks(&resolved, Some(&detect_progress)).await {
        Ok(o) => o,
        Err(e) => {
            stats.consecutive_failures += 1;
            tracing::warn!(
                "Backtest failed for blocks {}–{} ({}/{}): {} — will retry same range",
                from_block,
                current_tip,
                stats.consecutive_failures,
                max_consecutive_failures,
                e
            );
            if stats.consecutive_failures >= max_consecutive_failures {
                anyhow::bail!(
                    "giving up after {max_consecutive_failures} consecutive fetch/backtest failures"
                );
            }
            return Ok(false);
        }
    };
    let pass_elapsed = pass_start.elapsed();
    let opps = ctx.apply_gate(raw_opps, current_tip);

    for s in &pass_stats {
        stats.block_stats.push(s.clone());
        stats.total_txs_scanned += s.total_tx_count;
        stats.total_dex_txs += s.dex_tx_count;
        stats.total_pending_txs += s.pending_tx_count;
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
    let ledger = ctx.update_ledger(&opps).await?;
    progress.log(&render_ledger_summary_with_gate(
        &ledger,
        ctx.native_usd,
        Some(&ctx.gate_stats),
    ));
    progress.emit(ProgressEvent {
        stage: "complete".to_string(),
        done: None,
        total: None,
        run_id: Some(run_id.to_string()),
        ops: Some(opps.len() as u64),
        elapsed_ms: Some(pass_elapsed.as_millis() as u64),
    });
    ctx.runner.advance_to(current_tip);
    *last_block = current_tip;
    stats.blocks_processed += resolved.block_count;
    stats.total_opportunities += opps.len();
    stats.consecutive_failures = 0;
    Ok(true)
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
    let mut stats = LoopStats {
        blocks_processed: 0,
        total_txs_scanned: 0,
        total_dex_txs: 0,
        total_pending_txs: 0,
        total_opportunities: 0,
        block_stats: Vec::new(),
        consecutive_failures: 0,
    };

    loop {
        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;

        if should_stop(progress, deadline, max_blocks, &stats) {
            break;
        }

        let Some(current_tip) = poll_tip_with_retry(
            ctx,
            &mut stats.consecutive_failures,
            MAX_CONSECUTIVE_FAILURES,
        )
        .await?
        else {
            continue;
        };

        if current_tip <= last_block {
            continue;
        }

        process_pass(
            ctx,
            &mut last_block,
            &mut stats,
            current_tip,
            &run_id,
            MAX_CONSECUTIVE_FAILURES,
        )
        .await?;
    }

    progress.log("");
    progress.log("Session summary:");
    progress.log(&format!("  Blocks processed: {}", stats.blocks_processed));
    progress.log(&format!("  Txs scanned:      {}", stats.total_txs_scanned));
    progress.log(&format!("    DEX txs:        {}", stats.total_dex_txs));
    if stats.total_pending_txs > 0 {
        progress.log(&format!("    Pending txs:    {}", stats.total_pending_txs));
    }
    progress.log(&format!(
        "  Opportunities:    {}",
        stats.total_opportunities
    ));

    // Recomputed over the full accumulated list. `update_ledger` already ran
    // after every pass, including passes that found nothing; this last apply
    // is the row written at session end.
    let ledger = ctx.update_ledger(&[]).await?;
    progress.log(&render_ledger_summary_with_gate(
        &ledger,
        ctx.native_usd,
        Some(&ctx.gate_stats),
    ));
    let session_id = match ctx.persist_ledger_session(&ledger) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("paper-session persist failed: {e}");
            ctx.session_id.clone()
        }
    };
    progress.log(&format!("  Ledger session:   {session_id}"));
    progress.log("");

    Ok(LiveLoopOutcome {
        block_stats: stats.block_stats,
        session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanned_span_does_not_include_a_tip_that_was_only_observed() {
        // Setup saw an earlier tip. One-shot then scanned only the block that
        // was current when fetch started. The gap in between was not scanned,
        // so it must not show up as the session range.
        let span = widen_span(None, 96_596_041, 96_596_041);
        assert_eq!(span, (96_596_041, 96_596_041));
        let span = widen_span(Some(span), 96_596_042, 96_596_045);
        assert_eq!(span, (96_596_041, 96_596_045));
    }

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

    /// Every runnable strategy is native-eligible today, so `apply` cannot
    /// produce `NotNativeUnit` on its own. Build the skip directly to keep the
    /// rendering pinned for when a non-native strategy returns.
    #[test]
    fn summary_labels_not_native_unit_skips() {
        use crate::paper::PaperSkip;
        let ledger = LedgerResult {
            starting_gas_wei: 1_000_000,
            ending_gas_wei: 1_000_000,
            reserve_wei: 0,
            max_drawdown_wei: 0,
            fills: Vec::new(),
            skips: vec![PaperSkip {
                block_number: 10,
                tx_index: Some(0),
                canonical_id: None,
                strategy: "legacy".into(),
                reason: FillSkipReason::NotNativeUnit,
            }],
            gross_wei: 0,
            gas_wei: 0,
            net_profit_wei: 0,
            best_net_wei: 0,
            start_block: Some(10),
            end_block: Some(10),
            sim_mode: crate::paper::SimMode::Assumed,
            replay_blocks: 0,
            log_only_blocks: 0,
            seed_budget: 0,
            seeds_recorded: 0,
            seeds_capped: 0,
            exec_mode: "none".to_string(),
            exec_ok: 0,
            exec_revert: 0,
            revert_gas_wei: 0,
            exec_loss_wei: 0,
            setup_gas_wei: 0,
            recon_pass: None,
            recon_fail: None,
            recon_unverifiable: None,
            funding_fee_wei: 0,
            revert_gas_cost_wei: 0,
            basefee_wei: None,
            priority_fee_wei: None,
            sim_block: None,
            native_usd: None,
            manipulation_risk: false,
        };
        let text = render_ledger_summary(&ledger, None);
        assert!(
            text.contains("not_native_unit"),
            "expected an explicit note, got: {text}"
        );
    }

    #[test]
    fn summary_prints_state_vs_exec_recon() {
        let policy = LedgerPolicy {
            starting_gas_wei: 1_000_000,
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let mut ledger = policy.apply(&[]);
        ledger.exec_mode = "revm".into();
        ledger.recon_pass = Some(2);
        ledger.recon_fail = Some(1);
        ledger.recon_unverifiable = Some(3);
        let text = render_ledger_summary(&ledger, None);
        assert!(
            text.contains("recon: state vs exec 2 pass / 1 fail / 3 unverifiable"),
            "text was: {text}"
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
            strat_opp(12, 0, Strategy::MultiHopArb, 10u128.pow(17), 10u128.pow(17)),
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
            Strategy::MultiHopArb,
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
            strat_opp(11, 0, Strategy::TwoHopArb, 90 * 10u128.pow(17), 0),
        ]);
        let text = render_by_strategy(&ledger.fills, None);
        let arb = text.find("arb").expect("arb row");
        let jit = text.find("jit").expect("jit row");
        assert!(arb < jit, "largest |net| must come first:\n{text}");
    }

    #[test]
    fn by_strategy_is_empty_without_fills() {
        assert_eq!(render_by_strategy(&[], None), "");
    }

    // ── WP4 live gate ────────────────────────────────────────────────────────

    fn gate_opp(tx: usize, timestamp: u64) -> MevOpportunity {
        let mut o = MevOpportunity::new(
            10,
            tx,
            crate::types::Strategy::TwoHopArb,
            Address::repeat_byte((tx + 1) as u8),
            timestamp,
        );
        o.expected_profit = U256::from(100u64);
        o.gas_cost_wei = 10;
        o
    }

    /// An expired quote must never reach `persist_results` / `update_ledger`:
    /// only the fresh candidate survives, with an explicit reject reason.
    #[test]
    fn live_gate_rejects_expired_drift_and_risky_before_ledger() {
        let gate = LiveGateConfig {
            max_quote_age_ms: Some(1_000),
            require_risk_clear: true,
            ..Default::default()
        };
        let now_ms = 5_000;
        let mut expired = gate_opp(0, 5);
        expired.quote_expires_at = Some(now_ms - 1);
        let mut drift = gate_opp(1, 5);
        drift.risk_flags.push("quote_drift".into());
        let mut risky = gate_opp(2, 5);
        risky.risk_flags.push("funding_reentrancy".into());
        let old = gate_opp(3, 1); // 4000 ms old
        let mut fresh = gate_opp(4, 5);
        fresh.quote_expires_at = Some(now_ms + 1_000);

        let (kept, gated) =
            split_by_quote_freshness(vec![expired, drift, risky, old, fresh], now_ms, &gate);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].tx_index, 4);
        let reasons: Vec<_> = gated.iter().map(|(_, reason)| (*reason).as_str()).collect();
        assert_eq!(
            reasons,
            vec![
                "quote_expired",
                "quote_drift",
                "risk_flag_blocked",
                "quote_drift"
            ]
        );
        // The reject carries the same quote context for `rejected_candidates`.
        let reject = crate::explorer::RejectedCandidate::from_opportunity(
            &gated[0].0,
            10,
            gated[0].1,
            None,
            now_ms / 1000,
        );
        assert_eq!(reject.reject_reason, "quote_expired");
        assert_eq!(reject.tx_index, Some(0));
    }

    #[test]
    fn resolve_gate_config_applies_cli_overrides() {
        let mut config = Config::default();
        config.live.max_quote_age_ms = Some(100);
        config.live.pending_backrun_min_wei = Some("500".into());
        let o = LiveOpts {
            max_quote_age_ms: Some(200),
            require_risk_clear: true,
            pending_backrun: true,
            ..Default::default()
        };
        let gate = resolve_gate_config(&config, &o).unwrap();
        assert_eq!(gate.max_quote_age_ms, Some(200));
        assert!(gate.require_risk_clear);
        assert!(gate.pending_backrun);
        assert_eq!(gate.pending_swap_min_wei, 500);
        assert!(gate.pending_quote_ttl_ms > 0);
    }

    #[test]
    fn session_metrics_use_quote_and_gas_bases() {
        let policy = LedgerPolicy {
            starting_gas_wei: 10_000,
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let mut ledger = policy.apply(&[]);
        ledger.gas_wei = 1_000;
        ledger.funding_fee_wei = 100;
        ledger.revert_gas_wei = 50;
        let gate = LiveGateStats {
            quote_gate_attempts: 10,
            expired: 2,
            drift: 1,
            risk_blocked: 1,
            competition_loss_wei: 75,
            funding_source: None,
        };
        let m = session_metrics(&ledger, &gate);
        assert_eq!(m.quote_gate_attempts, 10);
        assert_eq!(m.quote_drift_fails, 1);
        assert_eq!(m.quote_expiry_fails, 2);
        assert!((m.quote_drift_failure_rate - 0.1).abs() < 1e-9);
        assert!((m.quote_expiry_failure_rate - 0.2).abs() < 1e-9);
        assert!((m.same_block_competition_loss_rate - 0.075).abs() < 1e-9);
        assert!((m.funding_fee_share_of_gas - 0.1).abs() < 1e-9);
        assert!((m.revert_gas_share - 50.0 / 1_050.0).abs() < 1e-9);
    }

    /// M-09: exactly three new lines, without rebuilding the existing
    /// `exec:` / `recon:` / `caps:` / `blocks:` lines.
    #[test]
    fn ledger_summary_has_three_new_live_lines() {
        let policy = LedgerPolicy {
            starting_gas_wei: 1_000_000,
            reserve_wei: 0,
            max_fills_per_block: HARD_MAX_FILLS_PER_BLOCK,
        };
        let ledger = policy.apply(&[]);
        let gate = LiveGateStats {
            quote_gate_attempts: 10,
            expired: 2,
            drift: 1,
            risk_blocked: 3,
            competition_loss_wei: 77,
            funding_source: Some("aave_v3".into()),
        };
        let text = render_ledger_summary_with_gate(&ledger, Some(2.0), Some(&gate));
        assert!(text.contains("  funding: aave_v3 | fee 0 wei"), "{text}");
        assert!(
            text.contains("  quotes: 2 expired / 1 drift | 3 risk-blocked (not in ledger)"),
            "{text}"
        );
        assert!(text.contains("  competition: est. 77 wei"), "{text}");
        assert!(
            text.contains("  metrics: drift 10.00% | expiry 20.00%"),
            "{text}"
        );
        assert_eq!(text.matches("  exec:").count(), 0);
        assert_eq!(text.matches("  recon:").count(), 0);
        assert_eq!(text.matches("  caps:").count(), 0);
        assert_eq!(text.matches("  blocks:").count(), 0);
        // The legacy public entry point must stay unchanged for existing callers.
        assert!(!render_ledger_summary(&ledger, None).contains("  quotes:"));
    }
}
