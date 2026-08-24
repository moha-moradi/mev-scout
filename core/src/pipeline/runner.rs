//! Backtest orchestration — replays blocks through revm and runs all MEV detection strategies.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::cache::SqliteStore;
use crate::data::ExecutedLog;
use crate::dex_type::DexType;
use crate::error;
use crate::mev::detectors::JitArbDetector;
use crate::mev::detectors::JitDetector;
use crate::mev::detectors::MultiHopArbDetector;
use crate::mev::detectors::SandwichDetector;
use crate::mev::detectors::TwoHopArbDetector;
use crate::mev::detectors::{detect_pending_opportunities, mempool};
use crate::mev::detectors::{AaveReserveCache, LiquidationDetector};
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
/// Confidence floor for long-persisting opportunities (#10).
const PERSISTENCE_MIN_CONFIDENCE: f64 = 0.05;
/// Per-step decay applied to confidence for each consecutive block an
/// opportunity persists (#10): fresh gaps score 1.0, stale gaps decay.
const PERSISTENCE_DECAY: f64 = 0.75;

/// Map key tracking one opportunity's cross-block persistence (#10).
type PersistenceKey = (Strategy, Address, Address, Address, Address);

#[derive(Debug, Clone, Copy)]
struct PersistInfo {
    last_block: u64,
    blocks_seen: u32,
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
    pub pool_manager: PoolManager,
    gas_config: GasConfig,
    proximity_window: usize,
    aave_reserve_cache: AaveReserveCache,
    capture_pending: bool,
    /// Minimum profit in wei to keep an opportunity (filters dust). 0 = disabled.
    min_profit_wei: u64,
    /// Maximum candidates to keep per transaction (top by profit). 0 = unlimited.
    max_candidates_per_tx: usize,
    /// Cross-block opportunity persistence used as a competitiveness proxy (#10):
    /// opportunities persisting many consecutive blocks get decaying confidence.
    opp_persistence: HashMap<PersistenceKey, PersistInfo>,
    /// Whether persistence-based confidence scoring is applied (#10).
    persistence_scoring: bool,
    /// Observed-gasUsed calibration buckets (#7), recorded during replay and
    /// snapshotted into `gas_config.calibration` before each block.
    gas_calibration: GasCalibration,
    pub last_processed_block: u64,
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
            proximity_window: 3,
            aave_reserve_cache: AaveReserveCache::default(),
            capture_pending: false,
            min_profit_wei: 0,
            max_candidates_per_tx: 0,
            opp_persistence: HashMap::new(),
            persistence_scoring: true,
            gas_calibration: GasCalibration::default(),
            last_processed_block: 0,
        }
    }

    /// Set the JitArb proximity window (tx index gap for related swaps).
    pub fn with_proximity_window(mut self, window: usize) -> Self {
        self.proximity_window = window;
        self
    }

    /// Attach pre-fetched Aave V3 reserve data for per-asset liquidation parameters.
    /// When set, `LiquidationDetector` uses real on-chain thresholds and bonuses.
    pub fn with_aave_reserve_cache(mut self, cache: AaveReserveCache) -> Self {
        self.aave_reserve_cache = cache;
        self
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

    /// Enable or disable persistence-based confidence scoring (#10).
    /// Enabled by default: opportunities persisting across consecutive blocks
    /// receive decaying `confidence` values as a competitiveness proxy.
    pub fn with_persistence_scoring(mut self, enabled: bool) -> Self {
        self.persistence_scoring = enabled;
        self
    }

    /// Expose a reference to the Aave reserve cache for inspection.
    pub fn aave_reserve_cache(&self) -> &AaveReserveCache {
        &self.aave_reserve_cache
    }

    /// Pre-fetch Aave V3 reserve data for all known token addresses.
    /// This populates the reserve cache so `LiquidationDetector` can use
    /// real per-asset liquidation thresholds and bonuses during replay.
    ///
    /// Should be called once before `run_block()` / `run_range()`.
    pub async fn prefetch_aave_reserves(&mut self, aave_pool: Address, block: u64) {
        let tokens: Vec<Address> = self.pool_manager.token_addresses();
        if tokens.is_empty() {
            tracing::warn!("No tokens in pool manager, skipping Aave reserve pre-fetch");
            return;
        }
        tracing::info!(
            "Pre-fetching Aave V3 reserve data for {} tokens at block {}",
            tokens.len(),
            block,
        );
        self.aave_reserve_cache
            .prefetch(self.replayer.rpc(), aave_pool, &tokens, block)
            .await;
        tracing::info!(
            "Aave reserve cache: {}/{} tokens resolved",
            self.aave_reserve_cache.len(),
            tokens.len(),
        );
    }

    /// Initialize the pool manager by loading pool definitions and fetching
    /// on-chain reserve state at a reference block.
    ///
    /// Loads pool definitions from the local cache (on-chain discovery from
    /// prior runs). Pools whose `creation_block` is after the target block are
    /// skipped without an RPC call. Remaining pools are verified via
    /// concurrent `eth_getCode` checks to filter any that don't exist at the
    /// target block. Then fetches current reserves for each pool via
    /// `eth_call getReserves()` (V2) or `slot0()/liquidity()` (V3).
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
    /// `PoolManager` via `update_from_logs()`. All detectors operate on the
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
    /// 4. Sandwich attacks (frontrun/victim/backrun triple)
    /// 5. JIT+Arb hybrid (Mint + cross-pool swap by same sender)
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
        // Create stateful detectors (H2: persistent per-block dedup across transactions)
        let mut two_hop_detector = TwoHopArbDetector::new(block_num);
        let mut multi_hop_detector = MultiHopArbDetector::new(block_num);
        // Seed JIT detector tick cache BEFORE taking pool_manager
        let mut jit_detector = JitDetector::new(block_num);
        jit_detector.seed_pool_tick_cache(&self.pool_manager);
        let mut sandwich_detector = SandwichDetector::new(block_num);
        let mut jit_arb_detector =
            JitArbDetector::new(block_num).with_proximity_window(self.proximity_window);
        let mut liquidation_detector =
            LiquidationDetector::new(block_num).with_reserve_cache(self.aave_reserve_cache.clone());

        // Take ownership of pool_manager so the closure can mutate it via RefCell
        let pool_manager = std::mem::take(&mut self.pool_manager);
        let pool_manager = RefCell::new(pool_manager);

        // Shared cell bridging TxData.from from filter closure to on_tx closure
        let current_tx_from: RefCell<Option<Address>> = RefCell::new(None);
        let dex_tx_count: RefCell<usize> = RefCell::new(0);
        // Collect effective gas prices for gas price distribution (H10)
        let gas_prices: RefCell<Vec<u128>> = RefCell::new(Vec::new());
        // Dirty pools updated by earlier transactions of this block. The first
        // detection pass scans everything; subsequent passes only re-check
        // pairs containing a dirty pool (untouched states yield no new ops).
        let dirty_pools: RefCell<Option<std::collections::HashSet<Address>>> = RefCell::new(None);
        // #7: refresh the detector-visible calibration snapshot and collect this
        // block's observations through a cell (the on_tx closure needs access).
        self.gas_config.calibration = self.gas_calibration.snapshot();
        let gas_calibration = RefCell::new(std::mem::take(&mut self.gas_calibration));

        self.replayer.replay_each_filtered(
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

                // Detect FIRST against pre-tx pool state, THEN apply log updates.
                // C6: Running detection before update_from_logs means we see the
                // opportunity that existed *before* the current tx consumed it,
                // rather than only the residual post-tx leftovers.
                // H2: Detectors maintain a per-block seen set so the same persistent
                // arb gap is not re-reported across multiple transactions.
                let dirty_guard = dirty_pools.borrow();
                let scope = match dirty_guard.as_ref() {
                    Some(set) => ScanScope::Dirty(set),
                    None => ScanScope::Full,
                };
                let opps = two_hop_detector.detect(
                    &pm,
                    i,
                    timestamp,
                    base_fee_per_gas,
                    self.gas_config,
                    &scope,
                );
                if !opps.is_empty() {
                    tracing::info!(
                        "Block {} tx {}: {} arb opportunities",
                        block_num,
                        i,
                        opps.len()
                    );
                }
                all_opportunities.extend(opps);

                let multi_opps = multi_hop_detector.detect(
                    &pm,
                    i,
                    timestamp,
                    base_fee_per_gas,
                    self.gas_config,
                    &scope,
                );
                if !multi_opps.is_empty() {
                    tracing::info!(
                        "Block {} tx {}: {} multi-hop arb opportunities",
                        block_num,
                        i,
                        multi_opps.len()
                    );
                }
                all_opportunities.extend(multi_opps);

                // JIT detector
                let sender = *current_tx_from.borrow();
                jit_detector.process_tx(i, &tx.logs, sender, &pm);
                let jit_opps =
                    jit_detector.detect(timestamp, base_fee_per_gas, &self.gas_config, &pm);
                if !jit_opps.is_empty() {
                    tracing::info!(
                        "Block {} tx {}: {} JIT opportunities",
                        block_num,
                        i,
                        jit_opps.len()
                    );
                }
                all_opportunities.extend(jit_opps);

                // Sandwich detector
                sandwich_detector.process_tx(i, &tx.logs, sender, &pm);
                let sandwich_opps =
                    sandwich_detector.detect(timestamp, &pm, base_fee_per_gas, &self.gas_config);
                if !sandwich_opps.is_empty() {
                    tracing::info!(
                        "Block {} tx {}: {} sandwich opportunities",
                        block_num,
                        i,
                        sandwich_opps.len()
                    );
                }
                all_opportunities.extend(sandwich_opps);

                // JitArb detector — use &pm (auto-derefs to &PoolManager)
                jit_arb_detector.process_tx(i, &tx.logs, sender, &pm);
                let jit_arb_opps =
                    jit_arb_detector.detect(timestamp, &pm, base_fee_per_gas, &self.gas_config);
                if !jit_arb_opps.is_empty() {
                    tracing::info!(
                        "Block {} tx {}: {} JitArb opportunities",
                        block_num,
                        i,
                        jit_arb_opps.len()
                    );
                }
                all_opportunities.extend(jit_arb_opps);

                // Liquidation detector — catches Aave V3 LiquidationCall events
                liquidation_detector.process_tx(i, &tx.logs);
                let liq_opps =
                    liquidation_detector.detect(&pm, timestamp, base_fee_per_gas, self.gas_config);
                if !liq_opps.is_empty() {
                    tracing::info!(
                        "Block {} tx {}: {} liquidation opportunities",
                        block_num,
                        i,
                        liq_opps.len()
                    );
                }
                all_opportunities.extend(liq_opps);

                // Collect effective gas price for H10 distribution modeling
                gas_prices.borrow_mut().push(tx.gas_effective);

                // #7: observe actual tx gas per (dex type, pools touched) bucket
                if tx.status {
                    let mut per_dex: HashMap<DexType, std::collections::HashSet<Address>> =
                        HashMap::new();
                    for log in &tx.logs {
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
                        gas_calibration
                            .borrow_mut()
                            .record(dex, pools.len(), tx.gas_used);
                    }
                }

                // Learn taxed tokens from this tx (#9), then apply its log
                // updates to pool state — both AFTER detection.
                pm.learn_taxes_from_tx(&tx.logs);
                pm.update_from_logs(&tx.logs);

                // Accumulate dirtied pools for the next incremental scan
                let newly_dirty = pm.take_dirty_pools();
                if !newly_dirty.is_empty() {
                    dirty_pools
                        .borrow_mut()
                        .get_or_insert_with(Default::default)
                        .extend(newly_dirty);
                }

                Ok(())
            },
        )?;

        // Filter: drop opportunities where expected profit doesn't cover gas
        all_opportunities.retain(|opp| opp.expected_profit > U256::from(opp.gas_cost_wei));

        // Filter: drop dust opportunities below minimum profit threshold
        if self.min_profit_wei > 0 {
            all_opportunities.retain(|opp| opp.expected_profit > U256::from(self.min_profit_wei));
        }

        // Filter: cap candidates per transaction, keeping only top-profit ones
        if self.max_candidates_per_tx > 0 && all_opportunities.len() > self.max_candidates_per_tx {
            all_opportunities.sort_by(|a, b| b.expected_profit.cmp(&a.expected_profit));
            all_opportunities.truncate(self.max_candidates_per_tx);
        }

        // Assign canonical dedup IDs (L9) to all opportunities
        for opp in &mut all_opportunities {
            opp.canonical_id = Some(crate::types::compute_canonical_id(
                opp.strategy,
                opp.block_number,
                opp.pool_a,
                opp.pool_b,
                opp.token_in,
                opp.token_out,
                opp.victim_tx_index,
                opp.backrun_tx_index,
            ));
        }

        self.pool_manager = pool_manager.into_inner();
        self.gas_calibration = gas_calibration.into_inner();
        self.last_processed_block = block_num;

        // #10: persistence-based confidence scoring across blocks
        if self.persistence_scoring {
            Self::update_persistence(&mut self.opp_persistence, &mut all_opportunities, block_num);
        }

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
    /// Unlike `run_block()`, this does NOT execute transactions through revm.
    /// It reads the cached block header + receipts, synthesizes the log stream,
    /// and applies Swap/Sync/Mint/Burn events directly to `pool_manager` via
    /// `update_from_logs()`. This keeps pool state authoritative near the tip
    /// using only regular full-node RPC calls — no `eth_getProof`, so no archive
    /// node is required.
    ///
    /// Two-hop and multi-hop arb detection still run against the updated state,
    /// but EVM-context strategies (JIT, sandwich, liquidation) are skipped
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
        let mut dex_tx_count = 0usize;
        // Dirty pools touched by earlier transactions (incremental scanning)
        let mut dirty_pools: Option<std::collections::HashSet<Address>> = None;
        // #7: refresh detector-visible calibration before scanning the block
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
            let opps = two_hop_detector.detect(
                &self.pool_manager,
                i,
                timestamp,
                base_fee_per_gas,
                self.gas_config,
                &scope,
            );
            all_opportunities.extend(opps);

            let multi_opps = multi_hop_detector.detect(
                &self.pool_manager,
                i,
                timestamp,
                base_fee_per_gas,
                self.gas_config,
                &scope,
            );
            all_opportunities.extend(multi_opps);

            // #9: learn taxed tokens, then apply state updates
            self.pool_manager.learn_taxes_from_tx(&logs);
            self.pool_manager.update_from_logs(&logs);
            let newly_dirty = self.pool_manager.take_dirty_pools();
            if !newly_dirty.is_empty() {
                dirty_pools
                    .get_or_insert_with(Default::default)
                    .extend(newly_dirty);
            }

            // #7: observe actual tx gas per (dex type, pools touched) bucket
            if tx_status && tx_gas_used > 0 {
                let mut per_dex: HashMap<DexType, std::collections::HashSet<Address>> =
                    HashMap::new();
                for log in &logs {
                    if pool_addrs.contains(&log.address) {
                        if let Some(p) = self.pool_manager.get(&log.address) {
                            per_dex
                                .entry(p.info().dex_type)
                                .or_default()
                                .insert(log.address);
                        }
                    }
                }
                for (dex, pools) in per_dex {
                    self.gas_calibration.record(dex, pools.len(), tx_gas_used);
                }
            }
        }

        // Filter: drop opportunities where expected profit doesn't cover gas
        all_opportunities.retain(|opp| opp.expected_profit > U256::from(opp.gas_cost_wei));

        // Filter: drop dust opportunities below minimum profit threshold
        if self.min_profit_wei > 0 {
            all_opportunities.retain(|opp| opp.expected_profit > U256::from(self.min_profit_wei));
        }

        // Filter: cap candidates per transaction, keeping only top-profit ones
        if self.max_candidates_per_tx > 0 && all_opportunities.len() > self.max_candidates_per_tx {
            all_opportunities.sort_by(|a, b| b.expected_profit.cmp(&a.expected_profit));
            all_opportunities.truncate(self.max_candidates_per_tx);
        }

        for opp in &mut all_opportunities {
            opp.canonical_id = Some(crate::types::compute_canonical_id(
                opp.strategy,
                opp.block_number,
                opp.pool_a,
                opp.pool_b,
                opp.token_in,
                opp.token_out,
                opp.victim_tx_index,
                opp.backrun_tx_index,
            ));
        }

        // #10: persistence-based confidence scoring across blocks
        if self.persistence_scoring {
            Self::update_persistence(&mut self.opp_persistence, &mut all_opportunities, block_num);
        }

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

    /// Update cross-block persistence state and stamp confidence scores (#10).
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
            let key = (
                opp.strategy,
                opp.pool_a,
                opp.pool_b,
                opp.token_in,
                opp.token_out,
            );
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
    /// Each block is processed sequentially via `run_block()`. Failed blocks
    /// are logged as errors but do not halt the scan — the runner continues
    /// to the next block in the range.
    ///
    /// The returned vector contains opportunities from all successful blocks,
    /// sorted by block number and transaction index (as produced by
    /// `run_block()`).
    ///
    /// H10: Maintains a `GasPriceDistribution` across blocks, feeding it
    /// per-tx effective gas prices and using the N-th percentile as the
    /// effective gas price for P90 / Distribution gas models.
    pub fn run_range(
        &mut self,
        resolved: &ResolvedRange,
    ) -> error::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>)> {
        let mut all = Vec::new();
        let mut all_stats = Vec::new();
        // H10: Gas price distribution across recent blocks (sliding window of 50)
        let mut gas_dist = GasPriceDistribution::new(50);
        for block_num in resolved.start_block..=resolved.end_block {
            // H10: Set the percentile gas price from historical distribution
            // before each block so detectors use it for gas cost computation.
            // `HistoricalExact` also gets a P90 fallback so blocks with a
            // missing base fee (pre-EIP-1559 chains, failed fetches) still
            // pay a realistic gas estimate instead of zero.
            let percentile = match self.gas_config.gas_model.target_percentile() {
                Some(p) => Some(p),
                None if self.gas_config.gas_model == GasModel::HistoricalExact => Some(90),
                None => None,
            };
            if let Some(p) = percentile {
                self.gas_config.percentile_gas_price = gas_dist.percentile(p);
            }

            // H5: Checkpoint pool state before running the block.
            // On failure, the pool_manager inside run_block is consumed/lost,
            // so we restore from this checkpoint to prevent state divergence.
            let checkpoint = self.pool_manager.clone();
            match self.run_block(block_num) {
                Ok((opps, stats, block_prices)) => {
                    tracing::info!(
                        "Block {} done: {} opportunities ({} txs)",
                        block_num,
                        opps.len(),
                        block_prices.len(),
                    );
                    // Feed gas prices into the distribution (H10)
                    for price in &block_prices {
                        gas_dist.add_tx_gas_price(*price);
                    }
                    // Record block-level data for EIP-1559 forecasting
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

                    all.extend(opps);
                    all_stats.push(stats);
                }
                Err(e) => {
                    // Restore pool state to the pre-block checkpoint so
                    // subsequent blocks use correct, non-diverged state.
                    self.pool_manager = checkpoint;
                    tracing::error!("Block {} failed: {:?}", block_num, e);
                }
            }
        }
        // H8 Phase 1+3: capture pending block and run mempool detection
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
                // H8 Phase 3: run pool-state-based arb detection on pending state
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
    ) -> error::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>, Vec<BlockMode>)> {
        let mut all = Vec::new();
        let mut all_stats = Vec::new();
        let mut all_modes = Vec::new();
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
            let use_full = block_num >= state_horizon;
            let mode = if use_full {
                BlockMode::FullReplay
            } else {
                BlockMode::LogOnly
            };

            let percentile = match self.gas_config.gas_model.target_percentile() {
                Some(p) => Some(p),
                None if self.gas_config.gas_model == GasModel::HistoricalExact => Some(90),
                None => None,
            };
            if let Some(p) = percentile {
                self.gas_config.percentile_gas_price = gas_dist.percentile(p);
            }

            let checkpoint = self.pool_manager.clone();

            if use_full {
                match self.run_block(block_num) {
                    Ok((opps, stats, block_prices)) => {
                        tracing::info!(
                            "Block {} done (full-replay): {} opportunities ({} txs)",
                            block_num,
                            opps.len(),
                            block_prices.len(),
                        );
                        for price in &block_prices {
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
                        all.extend(opps);
                        all_stats.push(stats);
                        all_modes.push(mode);
                    }
                    Err(e) => {
                        self.pool_manager = checkpoint.clone();
                        tracing::warn!(
                            "Block {} full-replay failed ({}), falling back to log-only: {:?}",
                            block_num,
                            block_num,
                            e,
                        );
                        match self.sync_block_from_logs(block_num) {
                            Ok((opps, stats, _)) => {
                                tracing::info!(
                                    "Block {} done (log-only fallback): {} opportunities",
                                    block_num,
                                    opps.len(),
                                );
                                all.extend(opps);
                                all_stats.push(stats);
                                all_modes.push(BlockMode::LogOnly);
                            }
                            Err(e2) => {
                                self.pool_manager = checkpoint.clone();
                                tracing::error!(
                                    "Block {} log-only also failed: {:?}",
                                    block_num,
                                    e2
                                );
                            }
                        }
                    }
                }
            } else {
                match self.sync_block_from_logs(block_num) {
                    Ok((opps, stats, _)) => {
                        tracing::info!(
                            "Block {} done (log-only): {} opportunities",
                            block_num,
                            opps.len(),
                        );
                        all.extend(opps);
                        all_stats.push(stats);
                        all_modes.push(mode);
                    }
                    Err(e) => {
                        self.pool_manager = checkpoint;
                        tracing::error!("Block {} log-only failed: {:?}", block_num, e);
                    }
                }
            }
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
/// enabling `arbitrage_pairs()` to find shared-token pairs in O(n²) over
/// tokens rather than pools.
///
/// Adding a pool invalidates the cached arbitrage pairs (regenerated on
/// next call to `arbitrage_pairs()`).
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
