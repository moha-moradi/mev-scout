//! Backtest orchestration — replays blocks through revm and runs all MEV detection strategies.
use std::cell::RefCell;
use std::collections::HashMap;

use crate::cache::SqliteStore;
use crate::data::{ExecutedLog, ExecutedTx, TxData};
use crate::dex_type::DexType;
use crate::error;
use crate::mev::detectors::BackrunDetector;
use crate::mev::detectors::DetectCtx;
use crate::mev::detectors::JitDetector;
use crate::mev::detectors::MultiHopArbDetector;
use crate::mev::detectors::TwoHopArbDetector;
use crate::mev::detectors::{detect_pending_opportunities, mempool, DetectionPath};
use crate::pipeline::{BlockMode, BlockReplayStats, GasPriceDistribution};
use crate::pool::state::{PoolInfo, PoolManager, PoolState, ScanScope, UniswapV2PoolState};
use crate::replay::BlockReplayer;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;
use crate::types::gas::GasCalibration;
use crate::types::MevOpportunity;
use crate::types::{GasConfig, GasModel, Strategy};
use alloy::primitives::{Address, U256};

/// Grace window after which a persistence entry is pruned when its
/// opportunity stops appearing.
const PERSISTENCE_GRACE_BLOCKS: u64 = 5;
/// Confidence floor for long-persisting opportunities.
const PERSISTENCE_MIN_CONFIDENCE: f64 = 0.05;
/// Per-step decay applied to confidence for each consecutive block an
/// opportunity persists: fresh gaps score 1.0, stale gaps decay.
const PERSISTENCE_DECAY: f64 = 0.75;

/// Map key tracking one opportunity's cross-block persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PersistenceKey {
    strategy: Strategy,
    pool_a: Address,
    pool_b: Address,
    token_in: Address,
    token_out: Address,
}

#[derive(Debug, Clone, Copy)]
struct PersistInfo {
    last_block: u64,
    blocks_seen: u32,
}

struct DetectTxArgs<'a> {
    block_num: u64,
    tx_index: usize,
    tx: &'a ExecutedTx,
    all_txs: &'a [TxData],
    pm: &'a mut PoolManager,
    dirty_pools: &'a RefCell<Option<std::collections::HashSet<Address>>>,
    pool_addrs: &'a std::collections::HashSet<Address>,
    timestamp: u64,
    base_fee_per_gas: u128,
    gas_config: GasConfig,
    current_tx_from: Option<Address>,
    gas_prices: &'a RefCell<Vec<u128>>,
    gas_calibration: &'a RefCell<GasCalibration>,
    all_opportunities: &'a mut Vec<MevOpportunity>,
}

/// Per-block MEV detectors (dedup state spans transactions in one block).
struct BlockDetectors {
    two_hop: TwoHopArbDetector,
    multi_hop: MultiHopArbDetector,
    jit: JitDetector,
    backrun: BackrunDetector,
}

impl BlockDetectors {
    fn new(block_num: u64, pool_manager: &PoolManager) -> Self {
        let mut jit = JitDetector::new(block_num);
        jit.seed_pool_tick_cache(pool_manager);
        Self {
            two_hop: TwoHopArbDetector::new(block_num),
            multi_hop: MultiHopArbDetector::new(block_num),
            jit,
            backrun: BackrunDetector::new(block_num),
        }
    }

    fn detect_tx(&mut self, args: DetectTxArgs<'_>) {
        let DetectTxArgs {
            block_num,
            tx_index,
            tx,
            all_txs,
            pm,
            dirty_pools,
            pool_addrs,
            timestamp,
            base_fee_per_gas,
            gas_config,
            current_tx_from,
            gas_prices,
            gas_calibration,
            all_opportunities,
        } = args;
        // Snapshot dirty set (not a held `Ref`): `ScanScope::Dirty` borrows it for
        // the whole detect block; folding newly-dirty pools below uses `borrow_mut`.
        let dirty_snapshot = dirty_pools.borrow().clone();
        let scope = match dirty_snapshot.as_ref() {
            Some(set) => ScanScope::Dirty(set),
            None => ScanScope::Full,
        };
        let ctx = DetectCtx::new(
            pm,
            tx_index,
            timestamp,
            base_fee_per_gas,
            gas_config,
            &scope,
        );
        let opps = self.two_hop.detect(ctx);
        if !opps.is_empty() {
            tracing::info!(
                "Block {} tx {}: {} arb opportunities",
                block_num,
                tx_index,
                opps.len()
            );
        }
        all_opportunities.extend(opps);

        let multi_opps = self.multi_hop.detect(ctx);
        if !multi_opps.is_empty() {
            tracing::info!(
                "Block {} tx {}: {} multi-hop arb opportunities",
                block_num,
                tx_index,
                multi_opps.len()
            );
        }
        all_opportunities.extend(multi_opps);

        self.jit.process_tx(tx_index, &tx.logs, current_tx_from, pm);
        let jit_opps = self.jit.detect(ctx);
        if !jit_opps.is_empty() {
            tracing::info!(
                "Block {} tx {}: {} JIT opportunities",
                block_num,
                tx_index,
                jit_opps.len()
            );
        }
        all_opportunities.extend(jit_opps);

        gas_prices.borrow_mut().push(tx.gas_effective);

        if tx.status {
            record_gas_observation(
                pm,
                pool_addrs,
                &tx.logs,
                tx.gas_used,
                &mut gas_calibration.borrow_mut(),
            );
        }

        let will_touch: std::collections::HashSet<_> = tx
            .logs
            .iter()
            .map(|l| l.address)
            .filter(|a| pool_addrs.contains(a))
            .collect();
        if !will_touch.is_empty() {
            let scope_pre = ScanScope::Dirty(&will_touch);
            let ctx_pre = DetectCtx::new(
                pm,
                tx_index,
                timestamp,
                base_fee_per_gas,
                gas_config,
                &scope_pre,
            );
            self.backrun.pre_detect(ctx_pre);
        }

        pm.learn_taxes_from_tx(&tx.logs);
        pm.update_from_logs(&tx.logs);

        let newly_dirty = pm.take_dirty_pools();
        if !newly_dirty.is_empty() {
            let scope_post = ScanScope::Dirty(&newly_dirty);
            let ctx_post = DetectCtx::new(
                pm,
                tx_index,
                timestamp,
                base_fee_per_gas,
                gas_config,
                &scope_post,
            );
            let backrun_opps = self.backrun.post_detect(ctx_post, &tx.logs, all_txs);
            if !backrun_opps.is_empty() {
                all_opportunities.extend(backrun_opps);
            }
        }

        if !newly_dirty.is_empty() {
            dirty_pools
                .borrow_mut()
                .get_or_insert_with(Default::default)
                .extend(newly_dirty);
        }
    }
}

/// Record observed gasUsed into calibration buckets by dex type and pools touched.
fn record_gas_observation(
    pm: &PoolManager,
    pool_addrs: &std::collections::HashSet<Address>,
    logs: &[ExecutedLog],
    gas_used: u64,
    calibration: &mut GasCalibration,
) {
    let mut per_dex: HashMap<DexType, std::collections::HashSet<Address>> = HashMap::new();
    for log in logs {
        if pool_addrs.contains(&log.address) {
            if let Some(p) = pm.get(&log.address) {
                per_dex
                    .entry(p.info().dex_type)
                    .or_default()
                    .insert(log.address);
            }
        }
    }
    for (dex, pools) in per_dex {
        calibration.record(dex, pools.len(), gas_used);
    }
}

/// Orchestrates MEV backtest execution by replaying blocks through revm and
/// running detection strategies against updated pool state.
///
/// This is the central sync workhorse of the engine. For each block in the
/// resolved range, it loads cached block data, replays transactions through
/// a filtered EVM pipeline, and invokes all active MEV detectors against
/// the updated `PoolManager` state.
///
/// The runner is intentionally stateless between blocks — pool state is
/// carried forward via `PoolManager` which accumulates reserve updates from
/// Swap/Sync events emitted during replay.
pub struct BacktestRunner {
    replayer: BlockReplayer,
    pool_manager: PoolManager,
    gas_config: GasConfig,
    capture_pending: bool,
    /// Minimum profit in wei to keep an opportunity (filters dust). 0 = disabled.
    min_profit_wei: u64,
    /// Maximum candidates to keep per transaction (top by profit). 0 = unlimited.
    max_candidates_per_tx: usize,
    /// Cross-block opportunity persistence used as a competitiveness proxy:
    /// opportunities persisting many consecutive blocks get decaying confidence.
    opp_persistence: HashMap<PersistenceKey, PersistInfo>,
    /// Whether persistence-based confidence scoring is applied.
    persistence_scoring: bool,
    /// Observed-gasUsed calibration buckets, recorded during replay and
    /// snapshotted into `gas_config.calibration` before each block.
    gas_calibration: GasCalibration,
    /// Whether rejected candidates are buffered for the explorer's
    /// `rejected_candidates` table. Opt-in via `with_record_rejections`.
    record_rejections: bool,
    /// Rejected-candidate buffer drained by the CLI via `take_rejections`.
    pending_rejections: Vec<crate::explorer::RejectedCandidate>,
    last_processed_block: u64,
}

impl BacktestRunner {
    /// Create a new backtest runner with the given replayer, pool manager, and
    /// gas configuration.
    ///
    /// This is typically called after pool initialization is complete and the
    /// block replayer has been constructed with the cache and RPC client.
    pub fn new(replayer: BlockReplayer, pool_manager: PoolManager, gas_config: GasConfig) -> Self {
        if gas_config.priority_fee_gwei == 0.0 {
            tracing::warn!(
                "priority_fee_gwei is 0 — profit estimates will overestimate \
                 real-world returns. Set --priority-fee to a realistic value \
                 (e.g. 1-5 gwei) for accurate estimates."
            );
        }
        BacktestRunner {
            replayer,
            pool_manager,
            gas_config,
            capture_pending: false,
            min_profit_wei: 0,
            max_candidates_per_tx: 0,
            opp_persistence: HashMap::new(),
            persistence_scoring: true,
            gas_calibration: GasCalibration::default(),
            record_rejections: false,
            pending_rejections: Vec::new(),
            last_processed_block: 0,
        }
    }

    /// Enable or disable pending transaction capture from the mempool.
    /// When enabled, the runner fetches the pending block after processing
    /// each block range and logs the pending tx count into per-block stats.
    pub fn with_capture_pending(mut self, enabled: bool) -> Self {
        self.capture_pending = enabled;
        self
    }

    /// Set minimum profit threshold (in wei) below which opportunities are filtered.
    /// Set to 0 to disable dust filtering (default).
    pub fn with_min_profit_wei(mut self, min_profit: u64) -> Self {
        self.min_profit_wei = min_profit;
        self
    }

    /// Set maximum candidates to keep per transaction (top by profit).
    /// Set to 0 for unlimited (default).
    pub fn with_max_candidates_per_tx(mut self, max: usize) -> Self {
        self.max_candidates_per_tx = max;
        self
    }

    /// Buffer rejected candidates with their filter reason. Opt-in —
    /// off by default to bound write volume; recommended ON for windows later
    /// fed to `explorer validate`.
    pub fn with_record_rejections(mut self, enabled: bool) -> Self {
        self.record_rejections = enabled;
        self
    }

    /// Drain buffered rejected candidates (insert into the explorer store).
    pub fn take_rejections(&mut self) -> Vec<crate::explorer::RejectedCandidate> {
        std::mem::take(&mut self.pending_rejections)
    }

    /// Current pool-state snapshot, used by the CLI to render result tables.
    pub fn pool_manager(&self) -> &PoolManager {
        &self.pool_manager
    }

    /// Last block number processed by the runner.
    pub fn last_processed_block(&self) -> u64 {
        self.last_processed_block
    }

    /// Advance the runner's progress marker without re-processing blocks —
    /// used by live mode to fast-forward past an already-indexed tip.
    pub fn advance_to(&mut self, block: u64) {
        self.last_processed_block = block;
    }

    /// Gas/min-profit filters with optional rejection recording.
    /// Replaces bare `retain` calls so the scanner's negative space is
    /// observable — a true coverage gap stays distinguishable from a
    /// filtered candidate.
    fn retain_with_rejections(&mut self, opps: &mut Vec<MevOpportunity>, block_num: u64) {
        let mut kept = Vec::with_capacity(opps.len());
        for opp in opps.drain(..) {
            if opp.expected_profit.is_zero() {
                self.record_rejection(
                    &opp,
                    block_num,
                    crate::explorer::RejectReason::QuoteNonpositive,
                    None,
                );
                continue;
            }
            if opp.expected_profit <= U256::from(opp.gas_cost_wei) {
                self.record_rejection(
                    &opp,
                    block_num,
                    crate::explorer::RejectReason::GasDominates,
                    Some(format!(
                        "profit={} wei <= gas={} wei",
                        opp.expected_profit, opp.gas_cost_wei
                    )),
                );
                continue;
            }
            if self.min_profit_wei > 0 && opp.expected_profit <= U256::from(self.min_profit_wei) {
                self.record_rejection(
                    &opp,
                    block_num,
                    crate::explorer::RejectReason::BelowMinProfit,
                    Some(format!(
                        "profit={} wei <= min_profit={} wei",
                        opp.expected_profit, self.min_profit_wei
                    )),
                );
                continue;
            }
            kept.push(opp);
        }
        *opps = kept;
    }

    fn record_rejections_batch(
        &mut self,
        opps: &[MevOpportunity],
        block_num: u64,
        reason: crate::explorer::RejectReason,
        detail: &str,
    ) {
        for opp in opps {
            self.record_rejection(opp, block_num, reason, Some(detail.to_string()));
        }
    }

    fn record_rejection(
        &mut self,
        opp: &MevOpportunity,
        block_num: u64,
        reason: crate::explorer::RejectReason,
        detail: Option<String>,
    ) {
        if !self.record_rejections {
            return;
        }
        let path = opp.path.as_ref().map(|p| {
            p.iter()
                .map(|a| format!("{a:#x}"))
                .collect::<Vec<_>>()
                .join(",")
        });
        self.pending_rejections
            .push(crate::explorer::RejectedCandidate {
                block_number: block_num,
                tx_index: Some(opp.tx_index as u64),
                strategy: opp.strategy.to_string(),
                pool_a: Some(format!("{:#x}", opp.pool_a)),
                pool_b: (opp.pool_b != Address::ZERO).then(|| format!("{:#x}", opp.pool_b)),
                path,
                token_in: (!opp.token_in.is_zero()).then(|| format!("{:#x}", opp.token_in)),
                token_out: (!opp.token_out.is_zero()).then(|| format!("{:#x}", opp.token_out)),
                input_amount: Some(opp.input_amount.to_string()),
                expected_profit: Some(opp.expected_profit.to_string()),
                expected_profit_usd: None,
                gas_cost_wei: Some(opp.gas_cost_wei.to_string()),
                reject_reason: reason.as_str().to_string(),
                detail,
                created_at: crate::utils::epoch_secs(),
            });
    }

    fn finalize_block_opportunities(
        &mut self,
        opps: &mut Vec<MevOpportunity>,
        txs: &[TxData],
        block_num: u64,
        path: DetectionPath,
    ) {
        crate::mev::detectors::backrun::suppress_superseded_arbs(opps);
        self.retain_with_rejections(opps, block_num);

        if self.max_candidates_per_tx > 0 && opps.len() > self.max_candidates_per_tx {
            opps.sort_by_key(|o| std::cmp::Reverse(o.expected_profit));
            let dropped = opps.split_off(self.max_candidates_per_tx);
            self.record_rejections_batch(
                &dropped,
                block_num,
                crate::explorer::RejectReason::MaxCandidates,
                "per-tx top-N cap",
            );
        }

        for opp in opps.iter_mut() {
            if opp.strategy != Strategy::Backrun {
                opp.canonical_id = Some(crate::types::compute_canonical_id(
                    crate::types::CanonicalIdParts {
                        strategy: opp.strategy,
                        pool_a: opp.pool_a,
                        pool_b: opp.pool_b,
                        token_in: opp.token_in,
                        token_out: opp.token_out,
                    },
                ));
            }
            opp.detection_path = Some(path);
            if opp.sender.is_none() {
                opp.sender = txs.get(opp.tx_index).map(|t| t.from);
            }
            if opp.tx_hash.is_none() {
                opp.tx_hash = txs.get(opp.tx_index).map(|t| t.hash);
            }
        }

        if self.persistence_scoring {
            Self::update_persistence(&mut self.opp_persistence, opps, block_num);
        }
    }

    fn gas_model_target_percentile(&self) -> Option<u8> {
        match self.gas_config.gas_model.target_percentile() {
            Some(p) => Some(p),
            None if self.gas_config.gas_model == GasModel::HistoricalExact => Some(90),
            None => None,
        }
    }

    fn apply_percentile_gas_price(&mut self, gas_dist: &GasPriceDistribution) {
        if let Some(p) = self.gas_model_target_percentile() {
            self.gas_config.percentile_gas_price = gas_dist.percentile(p);
        }
    }

    fn feed_gas_distribution(
        &self,
        gas_dist: &mut GasPriceDistribution,
        block_num: u64,
        block_prices: &[u128],
    ) {
        for price in block_prices {
            gas_dist.add_tx_gas_price(*price);
        }
        match self.replayer.load_block_data(block_num) {
            Ok((block, _)) => {
                let base_fee = block.base_fee_per_gas.unwrap_or(0);
                gas_dist.record_block(base_fee, block.gas_used, block.gas_limit);
            }
            Err(_) => {
                gas_dist.record_block(0, 0, 30_000_000);
            }
        }
        gas_dist.finalize_block();
    }

    fn hybrid_skip_uncached_block(&self, block_num: u64) -> bool {
        match self.replayer.has_cached_block(block_num) {
            Ok(false) => true,
            Ok(true) => false,
            Err(e) => {
                tracing::warn!("cache probe failed for block {block_num}: {e}");
                true
            }
        }
    }

    fn hybrid_commit_success(
        opps: &mut Vec<MevOpportunity>,
        stats: &mut Vec<BlockReplayStats>,
        modes: &mut Vec<BlockMode>,
        block_opps: Vec<MevOpportunity>,
        block_stats: BlockReplayStats,
        mode: BlockMode,
    ) {
        opps.extend(block_opps);
        stats.push(block_stats);
        modes.push(mode);
    }

    fn hybrid_try_full_replay_block(
        &mut self,
        block_num: u64,
        gas_dist: &mut GasPriceDistribution,
        full_replay_mode: BlockMode,
    ) -> Option<(Vec<MevOpportunity>, BlockReplayStats, BlockMode)> {
        match self.run_block(block_num) {
            Ok((opps, stats, block_prices)) => {
                self.pool_manager.end_undo();
                tracing::info!(
                    "Block {} done (full-replay): {} opportunities ({} txs)",
                    block_num,
                    opps.len(),
                    block_prices.len(),
                );
                self.feed_gas_distribution(gas_dist, block_num, &block_prices);
                Some((opps, stats, full_replay_mode))
            }
            Err(e) => {
                self.pool_manager.undo();
                tracing::warn!(
                    "Block {} full-replay failed ({}), falling back to log-only: {:?}",
                    block_num,
                    block_num,
                    e,
                );
                match self.sync_block_from_logs(block_num) {
                    Ok((opps, stats, _)) => {
                        self.pool_manager.end_undo();
                        tracing::info!(
                            "Block {} done (log-only fallback): {} opportunities",
                            block_num,
                            opps.len(),
                        );
                        Some((opps, stats, BlockMode::LogOnly))
                    }
                    Err(e2) => {
                        self.pool_manager.undo();
                        tracing::error!("Block {} log-only also failed: {:?}", block_num, e2);
                        None
                    }
                }
            }
        }
    }

    fn hybrid_try_log_only_block(
        &mut self,
        block_num: u64,
    ) -> Option<(Vec<MevOpportunity>, BlockReplayStats)> {
        match self.sync_block_from_logs(block_num) {
            Ok((opps, stats, _)) => {
                self.pool_manager.end_undo();
                tracing::info!(
                    "Block {} done (log-only): {} opportunities",
                    block_num,
                    opps.len(),
                );
                Some((opps, stats))
            }
            Err(e) => {
                self.pool_manager.undo();
                tracing::error!("Block {} log-only failed: {:?}", block_num, e);
                None
            }
        }
    }

    /// Initialize the pool manager by loading pool definitions and fetching
    /// on-chain reserve state at a reference block.
    ///
    /// Loads pool definitions from the local cache (on-chain discovery from
    /// prior runs). Pools whose `creation_block` is after the target block are
    /// skipped without an RPC call. Remaining pools are verified via
    /// concurrent `eth_getCode` checks to filter any that don't exist at the
    /// target block. Then fetches current reserves for each pool via
    /// `eth_call getReserves` (V2) or `slot0/liquidity` (V3).
    ///
    /// Pools that fail to initialize (e.g., the contract no longer exists at
    /// that block) are logged as warnings but do not halt execution.
    pub async fn init_pools(
        pool_manager: &mut PoolManager,
        rpc: &RpcClient,
        block_num: u64,
        cache: Option<&SqliteStore>,
    ) {
        let mut loaded_pools: Vec<PoolInfo> = Vec::new();

        // Load discovered pools from local cache (if available)
        if let Some(cache) = cache {
            match cache.list_discovered_pools() {
                Ok(pools) => {
                    let mut skipped_creation = 0usize;
                    for info in &pools {
                        // Layer 1: free check — skip if pool was created after target block
                        if info.creation_block > 0 && info.creation_block > block_num {
                            skipped_creation += 1;
                            continue;
                        }
                        loaded_pools.push(info.clone());
                    }
                    tracing::info!(
                        "Loaded {} pools from discovery cache (skipped {} by creation block)",
                        loaded_pools.len(),
                        skipped_creation
                    );
                }
                Err(e) => tracing::warn!("Failed to list discovered pools: {}", e),
            }
        }

        // Layer 2: add pools to manager (init_from_rpc will handle non-existent contracts)
        if !loaded_pools.is_empty() {
            for info in &loaded_pools {
                // Dedup: skip if already added from registry
                if pool_manager.get(&info.address).is_some() {
                    tracing::debug!("Skipping duplicate pool {} (already loaded)", info.address);
                    continue;
                }
                add_pool_to_manager(pool_manager, info.clone());
            }
        }

        if pool_manager.pool_count() == 0 {
            tracing::warn!("No pools loaded, skipping TwoHopArb detection");
            return;
        }

        tracing::info!(
            "Initializing {} pool reserves at block {}",
            pool_manager.pool_count(),
            block_num
        );
        pool_manager.init_from_rpc(rpc, block_num, cache).await;

        let initialized = pool_manager.initialized_count();
        tracing::info!(
            "{}/{} pools initialized",
            initialized,
            pool_manager.pool_count()
        );
    }

    /// Replay a single block and run all active MEV detection strategies.
    ///
    /// # Filtered replay
    /// Transactions are filtered before EVM execution: only transactions whose
    /// `to` address or log emitter matches a tracked pool or token address
    /// are fully replayed through revm. All others take the **fast path** —
    /// their `ExecutedTx` is synthesized directly from cached receipt data
    /// with no EVM execution. This is the primary performance optimization
    /// for large backtests.
    ///
    /// # Pool state management
    /// After each transaction, Swap/Sync events are decoded and applied to
    /// `PoolManager` via `update_from_logs`. All detectors operate on the
    /// updated pool state, so opportunities are detected against the
    /// post-transaction reserves (not the pre-transaction state).
    ///
    /// # Borrow checker workaround
    /// This method takes ownership of `pool_manager` via `mem::take` +
    /// `RefCell` because the replayer's `on_tx` callback requires `&mut self`
    /// on the runner but we need to mutate pool state inside the closure.
    /// `pool_manager` is restored to `self.pool_manager` after the block.
    ///
    /// # Detection order per transaction
    /// 1. Two-hop arbitrage (all pool pairs, both directions)
    /// 2. Multi-hop arbitrage (BFS paths up to depth 4)
    /// 3. JIT liquidity (Mint→Swap→Burn pattern)
    pub fn run_block(
        &mut self,
        block_num: u64,
    ) -> error::Result<(Vec<MevOpportunity>, BlockReplayStats, Vec<u128>)> {
        let (block_data, txs) = self.replayer.load_block_data(block_num)?;
        let total_tx_count = txs.len();
        if txs.is_empty() {
            return Ok((
                Vec::new(),
                BlockReplayStats {
                    block_number: block_num,
                    total_tx_count: 0,
                    dex_tx_count: 0,
                    pending_tx_count: 0,
                    mempool_opp_count: 0,
                },
                Vec::new(),
            ));
        }

        let timestamp = block_data.timestamp;
        let base_fee_per_gas = block_data.base_fee_per_gas.unwrap_or(0);

        let pool_addrs: std::collections::HashSet<_> =
            self.pool_manager.pool_addresses().into_iter().collect();
        let token_addrs: std::collections::HashSet<_> =
            self.pool_manager.token_addresses().into_iter().collect();

        let mut all_opportunities = Vec::new();
        let mut block_detectors = BlockDetectors::new(block_num, &self.pool_manager);

        // Take ownership of pool_manager so the closure can mutate it via RefCell
        let mut pool_manager = std::mem::take(&mut self.pool_manager);
        // record the pre-state of pools this block touches so a failure
        // can be rolled back with `undo` — replaces the per-block full clone.
        pool_manager.begin_undo();
        let pool_manager = RefCell::new(pool_manager);

        // Shared cell bridging TxData.from from filter closure to on_tx closure
        let current_tx_from: RefCell<Option<Address>> = RefCell::new(None);
        let dex_tx_count: RefCell<usize> = RefCell::new(0);
        // Collect effective gas prices for gas price distribution
        let gas_prices: RefCell<Vec<u128>> = RefCell::new(Vec::new());
        // Dirty pools updated by earlier transactions of this block. The first
        // detection pass scans everything; subsequent passes only re-check
        // pairs containing a dirty pool (untouched states yield no new ops).
        let dirty_pools: RefCell<Option<std::collections::HashSet<Address>>> = RefCell::new(None);
        // refresh the detector-visible calibration snapshot and collect this
        // block's observations through a cell (the on_tx closure needs access).
        self.gas_config.calibration = self.gas_calibration.snapshot();
        let gas_calibration = RefCell::new(std::mem::take(&mut self.gas_calibration));

        let replay_result = self.replayer.replay_each_filtered(
            block_num,
            |tx, receipt_logs| {
                *current_tx_from.borrow_mut() = Some(tx.from);
                let matched = tx
                    .to
                    .is_some_and(|to| pool_addrs.contains(&to) || token_addrs.contains(&to))
                    || receipt_logs.iter().any(|l| {
                        pool_addrs.contains(&l.address) || token_addrs.contains(&l.address)
                    });
                if matched {
                    *dex_tx_count.borrow_mut() += 1;
                }
                matched
            },
            |i, tx, _db| {
                let mut pm = pool_manager.borrow_mut();
                let sender = *current_tx_from.borrow();
                block_detectors.detect_tx(DetectTxArgs {
                    block_num,
                    tx_index: i,
                    tx,
                    all_txs: &txs,
                    pm: &mut pm,
                    dirty_pools: &dirty_pools,
                    pool_addrs: &pool_addrs,
                    timestamp,
                    base_fee_per_gas,
                    gas_config: self.gas_config,
                    current_tx_from: sender,
                    gas_prices: &gas_prices,
                    gas_calibration: &gas_calibration,
                    all_opportunities: &mut all_opportunities,
                });
                Ok(())
            },
        );

        // always hand the taken pool_manager and gas_calibration back to
        // self (even on error) so the caller can roll the block back with
        // PoolManager::undo; recording started in run_block stays active.
        self.pool_manager = pool_manager.into_inner();
        self.gas_calibration = gas_calibration.into_inner();
        replay_result?;

        self.finalize_block_opportunities(
            &mut all_opportunities,
            &txs,
            block_num,
            DetectionPath::Replay,
        );
        self.last_processed_block = block_num;

        Ok((
            all_opportunities,
            BlockReplayStats {
                block_number: block_num,
                total_tx_count,
                dex_tx_count: dex_tx_count.into_inner(),
                pending_tx_count: 0, // populated at range level by run_range_with_pga
                mempool_opp_count: 0, // populated at range level by run_range_with_pga
            },
            gas_prices.into_inner(),
        ))
    }

    /// Lightweight, archive-free pool-state sync used by live mode.
    ///
    /// Unlike `run_block`, this does NOT execute transactions through revm.
    /// It reads the cached block header + receipts, synthesizes the log stream,
    /// and applies Swap/Sync/Mint/Burn events directly to `pool_manager` via
    /// `update_from_logs`. This keeps pool state authoritative near the tip
    /// using only regular full-node RPC calls — no `eth_getProof`, so no archive
    /// node is required.
    ///
    /// Two-hop and multi-hop arb detection still run against the updated state,
    /// but EVM-context strategies (JIT) are skipped
    /// because they require full transaction execution.
    pub fn sync_block_from_logs(
        &mut self,
        block_num: u64,
    ) -> error::Result<(Vec<MevOpportunity>, BlockReplayStats, Vec<u128>)> {
        let (block_data, txs) = self.replayer.load_block_data(block_num)?;
        let receipts = self.replayer.load_receipts(block_num)?;
        let total_tx_count = txs.len();
        if txs.is_empty() {
            return Ok((
                Vec::new(),
                BlockReplayStats {
                    block_number: block_num,
                    total_tx_count: 0,
                    dex_tx_count: 0,
                    pending_tx_count: 0,
                    mempool_opp_count: 0,
                },
                Vec::new(),
            ));
        }

        let timestamp = block_data.timestamp;
        let base_fee_per_gas = block_data.base_fee_per_gas.unwrap_or(0);

        let pool_addrs: std::collections::HashSet<_> =
            self.pool_manager.pool_addresses().into_iter().collect();
        let token_addrs: std::collections::HashSet<_> =
            self.pool_manager.token_addresses().into_iter().collect();

        let mut all_opportunities = Vec::new();
        let mut two_hop_detector = TwoHopArbDetector::new(block_num);
        let mut multi_hop_detector = MultiHopArbDetector::new(block_num);
        let mut backrun_detector = BackrunDetector::new(block_num);
        let mut dex_tx_count = 0usize;
        // Dirty pools touched by earlier transactions (incremental scanning)
        let mut dirty_pools: Option<std::collections::HashSet<Address>> = None;
        // record pre-state of pools this sync mutates so a failure can
        // be rolled back by the caller with `PoolManager::undo`.
        self.pool_manager.begin_undo();
        // refresh detector-visible calibration before scanning the block
        self.gas_config.calibration = self.gas_calibration.snapshot();

        for (i, tx) in txs.iter().enumerate() {
            let logs: Vec<ExecutedLog> = receipts
                .get(i)
                .map(|r| {
                    r.logs
                        .iter()
                        .map(|l| ExecutedLog {
                            address: l.address,
                            topics: l.topics.clone(),
                            data: l.data.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();

            let matched = tx
                .to
                .is_some_and(|to| pool_addrs.contains(&to) || token_addrs.contains(&to))
                || logs
                    .iter()
                    .any(|l| pool_addrs.contains(&l.address) || token_addrs.contains(&l.address));
            if matched {
                dex_tx_count += 1;
            }
            let (tx_status, tx_gas_used) = match receipts.get(i) {
                Some(r) => (r.status, r.gas_used),
                None => (false, 0),
            };

            let scope = match dirty_pools.as_ref() {
                Some(set) => ScanScope::Dirty(set),
                None => ScanScope::Full,
            };

            // Detect against pre-tx pool state, THEN apply log updates
            // (mirrors run_block's detect-before-apply ordering).
            let ctx = DetectCtx::new(
                &self.pool_manager,
                i,
                timestamp,
                base_fee_per_gas,
                self.gas_config,
                &scope,
            );
            let opps = two_hop_detector.detect(ctx);
            all_opportunities.extend(opps);

            let multi_opps = multi_hop_detector.detect(ctx);
            all_opportunities.extend(multi_opps);

            // Backrun pre-image: opportunities on the pre-tx state, captured
            // before the log updates below (mirrors run_block A).
            let will_touch: std::collections::HashSet<_> = logs
                .iter()
                .map(|l| l.address)
                .filter(|a| pool_addrs.contains(a))
                .collect();
            if !will_touch.is_empty() {
                let scope_pre = ScanScope::Dirty(&will_touch);
                let ctx_pre = DetectCtx::new(
                    &self.pool_manager,
                    i,
                    timestamp,
                    base_fee_per_gas,
                    self.gas_config,
                    &scope_pre,
                );
                backrun_detector.pre_detect(ctx_pre);
            }

            // learn taxed tokens, then apply state updates
            self.pool_manager.learn_taxes_from_tx(&logs);
            self.pool_manager.update_from_logs(&logs);
            let newly_dirty = self.pool_manager.take_dirty_pools();

            // Backrun post-image + differential (mirrors run_block D).
            // Before the `dirty_pools` extend below, which consumes the set.
            if !newly_dirty.is_empty() {
                let scope_post = ScanScope::Dirty(&newly_dirty);
                let ctx_post = DetectCtx::new(
                    &self.pool_manager,
                    i,
                    timestamp,
                    base_fee_per_gas,
                    self.gas_config,
                    &scope_post,
                );
                let backrun_opps = backrun_detector.post_detect(ctx_post, &logs, &txs);
                if !backrun_opps.is_empty() {
                    all_opportunities.extend(backrun_opps);
                }
            }

            if !newly_dirty.is_empty() {
                dirty_pools
                    .get_or_insert_with(Default::default)
                    .extend(newly_dirty);
            }

            if tx_status && tx_gas_used > 0 {
                record_gas_observation(
                    &self.pool_manager,
                    &pool_addrs,
                    &logs,
                    tx_gas_used,
                    &mut self.gas_calibration,
                );
            }
        }

        self.finalize_block_opportunities(
            &mut all_opportunities,
            &txs,
            block_num,
            DetectionPath::LogOnly,
        );
        self.last_processed_block = block_num;

        Ok((
            all_opportunities,
            BlockReplayStats {
                block_number: block_num,
                total_tx_count,
                dex_tx_count,
                pending_tx_count: 0,
                mempool_opp_count: 0,
            },
            Vec::new(),
        ))
    }

    /// Update cross-block persistence state and stamp confidence scores.
    ///
    /// An opportunity seen in the immediately preceding block extends its
    /// streak; anything else starts a fresh one. Confidence decays geometrically
    /// with the streak length: a gap persisting for many blocks is either
    /// phantom or uncontested, and either way less actionable than a fresh one
    /// (the `tx_index` exclusive-insertion assumption only holds briefly).
    fn update_persistence(
        map: &mut HashMap<PersistenceKey, PersistInfo>,
        opps: &mut [MevOpportunity],
        block_num: u64,
    ) {
        // Prune entries that have not been refreshed within the grace window.
        map.retain(|_, info| block_num <= info.last_block + PERSISTENCE_GRACE_BLOCKS);

        for opp in opps.iter_mut() {
            let key = PersistenceKey {
                strategy: opp.strategy,
                pool_a: opp.pool_a,
                pool_b: opp.pool_b,
                token_in: opp.token_in,
                token_out: opp.token_out,
            };
            let blocks_seen = match map.get_mut(&key) {
                Some(info) if info.last_block == block_num => {
                    // Same-block duplicate (e.g. both scan directions): keep streak.
                    info.blocks_seen
                }
                Some(info) if info.last_block + 1 == block_num => {
                    info.last_block = block_num;
                    info.blocks_seen += 1;
                    info.blocks_seen
                }
                _ => {
                    map.insert(
                        key,
                        PersistInfo {
                            last_block: block_num,
                            blocks_seen: 1,
                        },
                    );
                    1
                }
            };
            let decayed = PERSISTENCE_DECAY.powi((blocks_seen - 1) as i32);
            opp.confidence = Some(decayed.max(PERSISTENCE_MIN_CONFIDENCE));
        }
    }

    /// Run backtest over a resolved block range, collecting all detected
    /// opportunities across every block.
    ///
    /// Each block is processed sequentially via `run_block`. Failed blocks
    /// are logged as errors but do not halt the scan — the runner continues
    /// to the next block in the range.
    ///
    /// The returned vector contains opportunities from all successful blocks,
    /// sorted by block number and transaction index (as produced by
    /// `run_block`).
    ///
    /// Maintains a `GasPriceDistribution` across blocks, feeding it
    /// per-tx effective gas prices and using the N-th percentile as the
    /// effective gas price for P90 / Distribution gas models.
    pub fn run_range(
        &mut self,
        resolved: &ResolvedRange,
        progress: Option<&dyn Fn(u64, u64) -> bool>,
    ) -> error::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>)> {
        let mut all = Vec::new();
        let mut all_stats = Vec::new();
        let mut processed: u64 = 0;
        // Gas price distribution across recent blocks (sliding window of 50)
        let mut gas_dist = GasPriceDistribution::new(50);
        for block_num in resolved.start_block..=resolved.end_block {
            // Set the percentile gas price from historical distribution
            // before each block so detectors use it for gas cost computation.
            // `HistoricalExact` also gets a P90 fallback so blocks with a
            // missing base fee (pre-EIP-1559 chains, failed fetches) still
            // pay a realistic gas estimate instead of zero.
            self.apply_percentile_gas_price(&gas_dist);

            // `run_block` begins an undo log on the pool manager;
            // success ends it, failure rolls the block back in place (no
            // full-manager checkpoint clone per block anymore).
            match self.run_block(block_num) {
                Ok((opps, stats, block_prices)) => {
                    self.pool_manager.end_undo();
                    tracing::info!(
                        "Block {} done: {} opportunities ({} txs)",
                        block_num,
                        opps.len(),
                        block_prices.len(),
                    );
                    self.feed_gas_distribution(&mut gas_dist, block_num, &block_prices);

                    all.extend(opps);
                    all_stats.push(stats);
                }
                Err(e) => {
                    // Roll pool state back to the pre-block checkpoint so
                    // subsequent blocks use correct, non-diverged state.
                    self.pool_manager.undo();
                    tracing::error!("Block {} failed: {:?}", block_num, e);
                }
            }
            // Progress callback (block done, regardless of outcome).
            processed += 1;
            if let Some(cb) = progress {
                if !cb(processed, resolved.block_count) {
                    return Err(error::Error::Cancelled);
                }
            }
        }
        // Capture pending block and run mempool detection
        if self.capture_pending {
            let rpc = self.replayer.rpc().clone();
            if let Some(capture) = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(mempool::capture_pending_block(&rpc))
            }) {
                tracing::info!(
                    "Pending block captured: {} transactions in mempool (block #{})",
                    capture.tx_count,
                    capture.block_number,
                );
                if let Some(last) = all_stats.last_mut() {
                    last.pending_tx_count = capture.tx_count;
                }
                // Run pool-state-based arb detection on pending state
                let pending_opps = detect_pending_opportunities(
                    &self.pool_manager,
                    self.gas_config,
                    capture.base_fee_per_gas,
                    capture.timestamp,
                    capture.block_number,
                );
                if !pending_opps.is_empty() {
                    tracing::info!(
                        "Mempool detection: {} opportunities visible in mempool (block #{})",
                        pending_opps.len(),
                        capture.block_number,
                    );
                    if let Some(last) = all_stats.last_mut() {
                        last.mempool_opp_count = pending_opps.len();
                    }
                    all.extend(pending_opps);
                }
            } else {
                tracing::warn!("Failed to capture pending block — mempool may be unavailable");
            }
        }

        Ok((all, all_stats))
    }

    /// Run backtest over a resolved block range, auto-detecting per block whether
    /// full EVM replay is possible based on state availability.
    ///
    /// Blocks at or above `state_horizon` are processed via [`run_block`] (full
    /// EVM replay, all strategies). Blocks below the horizon are processed via
    /// [`sync_block_from_logs`] (log-based, arb strategies only). This allows
    /// backtesting over ranges that extend beyond the full node's state
    /// retention window without requiring an archive node.
    ///
    /// The returned [`BlockMode`] vector indicates which mode was used per block,
    /// aligned 1:1 with `block_stats`.
    pub fn run_range_hybrid(
        &mut self,
        resolved: &ResolvedRange,
        state_horizon: u64,
        progress: Option<&dyn Fn(u64, u64) -> bool>,
    ) -> error::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>, Vec<BlockMode>)> {
        let mut all = Vec::new();
        let mut all_stats = Vec::new();
        let mut all_modes = Vec::new();
        let mut processed: u64 = 0;
        let mut not_fetched: u64 = 0;
        let mut gas_dist = GasPriceDistribution::new(50);

        let log_only_count =
            resolved.start_block..resolved.end_block.min(state_horizon.saturating_sub(1));
        let full_count = state_horizon.max(resolved.start_block)..=resolved.end_block;
        let log_only_n = if log_only_count.start <= log_only_count.end {
            log_only_count.end - log_only_count.start + 1
        } else {
            0
        };
        let full_n = if *full_count.start() <= *full_count.end() {
            *full_count.end() - *full_count.start() + 1
        } else {
            0
        };

        tracing::info!(
            "Hybrid range: blocks {}–{} ({} blocks) — {} log-only, {} full-replay (horizon={})",
            resolved.start_block,
            resolved.end_block,
            resolved.block_count,
            log_only_n,
            full_n,
            state_horizon,
        );

        for block_num in resolved.start_block..=resolved.end_block {
            if self.hybrid_skip_uncached_block(block_num) {
                not_fetched += 1;
                continue;
            }

            let use_full = block_num >= state_horizon;
            let mode = if use_full {
                BlockMode::FullReplay
            } else {
                BlockMode::LogOnly
            };

            self.apply_percentile_gas_price(&gas_dist);

            if use_full {
                if let Some((opps, stats, committed_mode)) =
                    self.hybrid_try_full_replay_block(block_num, &mut gas_dist, mode)
                {
                    Self::hybrid_commit_success(
                        &mut all,
                        &mut all_stats,
                        &mut all_modes,
                        opps,
                        stats,
                        committed_mode,
                    );
                }
            } else if let Some((opps, stats)) = self.hybrid_try_log_only_block(block_num) {
                Self::hybrid_commit_success(
                    &mut all,
                    &mut all_stats,
                    &mut all_modes,
                    opps,
                    stats,
                    mode,
                );
            }
            // Progress callback (block done, regardless of outcome).
            processed += 1;
            if let Some(cb) = progress {
                if !cb(processed, resolved.block_count) {
                    return Err(error::Error::Cancelled);
                }
            }
        }

        if not_fetched > 0 {
            tracing::info!(
                "{not_fetched} block(s) in {}–{} were not fetched (no tracked-pool \
                 activity) and contributed 0 opportunities",
                resolved.start_block,
                resolved.end_block,
            );
        }

        if self.capture_pending {
            let rpc = self.replayer.rpc().clone();
            if let Some(capture) = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(mempool::capture_pending_block(&rpc))
            }) {
                tracing::info!(
                    "Pending block captured: {} transactions in mempool (block #{})",
                    capture.tx_count,
                    capture.block_number,
                );
                if let Some(last) = all_stats.last_mut() {
                    last.pending_tx_count = capture.tx_count;
                }
                let pending_opps = detect_pending_opportunities(
                    &self.pool_manager,
                    self.gas_config,
                    capture.base_fee_per_gas,
                    capture.timestamp,
                    capture.block_number,
                );
                if !pending_opps.is_empty() {
                    tracing::info!(
                        "Mempool detection: {} opportunities visible in mempool (block #{})",
                        pending_opps.len(),
                        capture.block_number,
                    );
                    if let Some(last) = all_stats.last_mut() {
                        last.mempool_opp_count = pending_opps.len();
                    }
                    all.extend(pending_opps);
                }
            } else {
                tracing::warn!("Failed to capture pending block — mempool may be unavailable");
            }
        }

        Ok((all, all_stats, all_modes))
    }
}

/// Add a pool to the manager, registering it in the token index for fast
/// arbitrage pair enumeration.
///
/// The token index maps each token address to all pools that trade it,
/// enabling `arbitrage_pairs` to find shared-token pairs in O(n²) over
/// tokens rather than pools.
///
/// Adding a pool invalidates the cached arbitrage pairs (regenerated on
/// next call to `arbitrage_pairs`).
pub fn add_pool_to_manager(pool_manager: &mut PoolManager, info: PoolInfo) {
    match info.dex_type {
        crate::dex_type::DexType::UniswapV2 => {
            pool_manager.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
                info,
                reserve0: 0,
                reserve1: 0,
            }));
        }
        crate::dex_type::DexType::UniswapV3 => {
            pool_manager.add_pool(PoolState::UniswapV3(
                crate::pool::state::UniswapV3PoolState::new(info),
            ));
        }
        crate::dex_type::DexType::UniswapV4 => {
            pool_manager.add_pool(PoolState::UniswapV4(
                crate::pool::state::UniswapV4PoolState::new(info),
            ));
        }
        crate::dex_type::DexType::PancakeInfinity => {
            pool_manager.add_pool(PoolState::PancakeInfinity(
                crate::pool::state::PancakeInfinityPoolState::new(info),
            ));
        }
        crate::dex_type::DexType::Curve => {
            pool_manager.add_pool(PoolState::Curve(crate::pool::state::CurvePoolState {
                info,
                balances: vec![],
                token_index: std::collections::HashMap::new(),
                a_coeff: 100,
                pool_variant: crate::pool::state::CurvePoolVariant::Plain,
                gamma: None,
                price_scale: vec![],
                base_pool: None,
            }));
        }
        crate::dex_type::DexType::Balancer => {
            pool_manager.add_pool(PoolState::Balancer(crate::pool::state::BalancerPoolState {
                info,
                balances: vec![],
                token_index: std::collections::HashMap::new(),
                pool_id: None,
                weights: vec![],
                pool_variant: crate::pool::state::BalancerPoolVariant::Weighted,
                amplification: None,
                scaling_factors: vec![],
                bpt_index: None,
                rate_providers: vec![],
            }));
        }
        crate::dex_type::DexType::TraderJoeLB => {
            let bin_step = info.bin_step.unwrap_or(0);
            pool_manager.add_pool(PoolState::TraderJoeLB(
                crate::pool::state::pool_types::TraderJoeLBPoolState::new(info, 0, bin_step),
            ));
        }
        crate::dex_type::DexType::Pendle => {
            pool_manager.add_pool(PoolState::Pendle(
                crate::pool::state::pool_types::PendlePoolState::new(info),
            ));
        }
        crate::dex_type::DexType::Solidly | crate::dex_type::DexType::Camelot => {
            if info.is_stable == Some(true) {
                // Solidly/Camelot stable pools use StableSwap invariant (A=200 for Solidly)
                let a_coeff = if info.dex_type == crate::dex_type::DexType::Solidly {
                    200
                } else {
                    100
                };
                pool_manager.add_pool(PoolState::Curve(crate::pool::state::CurvePoolState {
                    info: info.clone(),
                    balances: vec![0, 0],
                    token_index: {
                        let mut m = std::collections::HashMap::new();
                        m.insert(info.token0, 0);
                        m.insert(info.token1, 1);
                        m
                    },
                    a_coeff,
                    pool_variant: crate::pool::state::CurvePoolVariant::Plain,
                    gamma: None,
                    price_scale: vec![],
                    base_pool: None,
                }));
            } else {
                pool_manager.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
                    info,
                    reserve0: 0,
                    reserve1: 0,
                }));
            }
        }
        crate::dex_type::DexType::Metric => {
            pool_manager.add_pool(PoolState::Metric(
                crate::pool::state::pool_types::MetricPoolState::new(info),
            ));
        }
        crate::dex_type::DexType::Fluid => {
            pool_manager.add_pool(PoolState::Fluid(
                crate::pool::state::pool_types::FluidPoolState::new(info),
            ));
        }
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use alloy::primitives::address;

    fn opp(block: u64) -> MevOpportunity {
        let mut o = MevOpportunity::new(
            block,
            0,
            Strategy::TwoHopArb,
            address!("1111111111111111111111111111111111111111"),
            0,
        );
        o.pool_b = address!("2222222222222222222222222222222222222222");
        o.token_in = address!("3333333333333333333333333333333333333333");
        o.token_out = address!("4444444444444444444444444444444444444444");
        o
    }

    #[test]
    fn fresh_opportunity_full_confidence() {
        let mut map = HashMap::new();
        let mut opps = vec![opp(100)];
        BacktestRunner::update_persistence(&mut map, &mut opps, 100);
        assert_eq!(opps[0].confidence, Some(1.0));
    }

    #[test]
    fn streak_decays_confidence() {
        let mut map = HashMap::new();
        for block in 100..=103u64 {
            let mut opps = vec![opp(block)];
            BacktestRunner::update_persistence(&mut map, &mut opps, block);
            let expected = PERSISTENCE_DECAY.powi((block - 100) as i32);
            assert_eq!(opps[0].confidence, Some(expected), "block {block}");
        }
    }

    #[test]
    fn gap_resets_streak() {
        let mut map = HashMap::new();
        let mut first = vec![opp(100)];
        BacktestRunner::update_persistence(&mut map, &mut first, 100);
        // Skip blocks 101-102 (within grace): next sight starts a new streak.
        let mut later = vec![opp(103)];
        BacktestRunner::update_persistence(&mut map, &mut later, 103);
        assert_eq!(later[0].confidence, Some(1.0));
    }

    #[test]
    fn stale_entries_pruned_beyond_grace() {
        let mut map = HashMap::new();
        let mut first = vec![opp(100)];
        BacktestRunner::update_persistence(&mut map, &mut first, 100);
        assert_eq!(map.len(), 1);

        let mut other = opp(100 + PERSISTENCE_GRACE_BLOCKS + 1);
        other.token_in = address!("5555555555555555555555555555555555555555");
        let mut others = vec![other];
        BacktestRunner::update_persistence(
            &mut map,
            &mut others,
            100 + PERSISTENCE_GRACE_BLOCKS + 1,
        );
        // Old entry pruned; only the new one remains.
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn same_block_duplicates_do_not_extend_streak() {
        let mut map = HashMap::new();
        let mut first = vec![opp(100)];
        BacktestRunner::update_persistence(&mut map, &mut first, 100);
        let mut duplicate = vec![opp(100)];
        duplicate[0].pool_b = address!("6666666666666666666666666666666666666666");
        BacktestRunner::update_persistence(&mut map, &mut duplicate, 100);
        assert!(map
            .values()
            .all(|i| i.last_block == 100 && i.blocks_seen == 1));
    }
}
