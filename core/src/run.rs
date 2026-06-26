//! Backtest orchestration — replays blocks through revm and runs all MEV detection strategies.

use std::cell::RefCell;

use crate::cache::SqliteStore;
use crate::mev::opportunity::MevOpportunity;
use crate::mev::cross_block::CrossBlockDetector;
use crate::mev::mempool;
use crate::mev::mempool::detect_pending_opportunities;
use crate::pool::discovery::discover_pools_in_range;
use crate::mev::jit::JitDetector;
use crate::mev::liquidation::{AaveReserveCache, LiquidationDetector};
use crate::mev::sandwich::SandwichDetector;
use crate::mev::jit_arb::JitArbDetector;
use crate::mev::multi_hop::MultiHopArbDetector;
use crate::mev::two_hop::TwoHopArbDetector;
use crate::mev::pga::{self, PgaConfig};
use alloy::primitives::{Address, U256};
use crate::pool::state::{PoolInfo, PoolManager, PoolState, UniswapV2PoolState};
use crate::replay::BlockReplayer;
use crate::fact_check::BlockReplayStats;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;
use crate::gas_distribution::GasPriceDistribution;
use crate::types::GasConfig;

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
    cross_block_window: usize,
    cross_block_detector: Option<CrossBlockDetector>,
}

impl BacktestRunner {
    /// Create a new backtest runner with the given replayer, pool manager, and
    /// gas configuration.
    ///
    /// This is typically called after pool initialization is complete and the
    /// block replayer has been constructed with the cache and RPC client.
    pub fn new(
        replayer: BlockReplayer,
        pool_manager: PoolManager,
        gas_config: GasConfig,
    ) -> Self {
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
            cross_block_window: 3,
            cross_block_detector: None,
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

    /// Enable cross-block MEV detection with the given sliding window size.
    /// When enabled (> 1), the runner tracks pool price snapshots across
    /// consecutive blocks and emits persistent arb and time-bandit opportunities.
    pub fn with_cross_block(mut self, window: usize) -> Self {
        self.cross_block_window = window.max(2);
        self.cross_block_detector = Some(CrossBlockDetector::new(self.cross_block_window));
        self
    }

    /// Returns true if cross-block detection is enabled.
    pub fn cross_block_enabled(&self) -> bool {
        self.cross_block_detector.is_some()
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
    pub async fn prefetch_aave_reserves(
        &mut self,
        aave_pool: Address,
        block: u64,
    ) {
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
        self.aave_reserve_cache.prefetch(
            self.replayer.rpc(),
            aave_pool,
            &tokens,
            block,
        ).await;
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

        // Layer 2: verify remaining pools exist at target block via eth_getCode
        if !loaded_pools.is_empty() {
            let existing = PoolManager::filter_existing_pools(rpc, &loaded_pools, block_num, 20).await;
            let removed = loaded_pools.len() - existing.len();
            if removed > 0 {
                tracing::info!("Removed {} pools that don't exist at block {}", removed, block_num);
            }
            for info in &existing {
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
        pool_manager.init_from_rpc(rpc, block_num).await;

        let initialized = pool_manager.initialized_count();
        tracing::info!(
            "{}/{} pools initialized",
            initialized,
            pool_manager.pool_count()
        );
    }

    /// Discover pools from factory events in a block range and add them to the pool manager.
    /// Runs discovery for each chunk, adds newly discovered pools, and initializes their
    /// on-chain state at the chunk's start block for accurate reserves.
    pub async fn discover_and_add_pools(
        &mut self,
        rpc: &RpcClient,
        v2_factories: &[Address],
        v3_factories: &[Address],
        from_block: u64,
        to_block: u64,
        v2_factory_fees: &[Option<u32>],
        batch_size: u64,
    ) {
        tracing::info!(
            "Discovering pools from {} factory addresses in blocks {}..{}",
            v2_factories.len() + v3_factories.len(),
            from_block,
            to_block,
        );

        let discovered = discover_pools_in_range(
            rpc,
            v2_factories,
            v3_factories,
            from_block,
            to_block,
            v2_factory_fees,
            batch_size,
        )
        .await;

        if discovered.is_empty() {
            tracing::info!("No new pools discovered in range {}..{}", from_block, to_block);
            return;
        }

        tracing::info!("Discovered {} new pools, adding to pool manager", discovered.len());

        for pool in &discovered {
            let info: PoolInfo = pool.clone().into();
            add_pool_to_manager(&mut self.pool_manager, info);
        }

        // Initialize reserves for newly added pools at the start of the range
        let init_block = from_block.saturating_sub(1);
        self.pool_manager.init_from_rpc(rpc, init_block).await;

        tracing::info!(
            "Discovered and initialized {} pools for range {}..{}",
            discovered.len(),
            from_block,
            to_block,
        );
    }

    /// Run backtest over the resolved block range with live pool discovery.
    ///
    /// Splits the range into chunks of `chunk_size` blocks. For each chunk:
    /// 1. Scans factory events for newly created pools in that range
    /// 2. Adds discovered pools to the pool manager and initializes reserves
    /// 3. Processes blocks in the chunk via the standard replay pipeline
    ///
    /// PGA adjustment is applied globally across all chunks at the end.
    pub async fn run_range_with_live_discovery(
        &mut self,
        rpc: &RpcClient,
        resolved: &ResolvedRange,
        pga_config: Option<PgaConfig>,
        v2_factories: &[Address],
        v3_factories: &[Address],
        v2_factory_fees: &[Option<u32>],
        batch_size: u64,
        chunk_size: u64,
    ) -> anyhow::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>)> {
        let mut all_opps = Vec::new();
        let mut all_stats = Vec::new();
        let mut chunk_start = resolved.start_block;

        while chunk_start <= resolved.end_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(resolved.end_block);
            tracing::info!("Live discovery chunk: blocks {}..{}", chunk_start, chunk_end);

            // Discover pools created in this chunk's range and add to pool manager
            self.discover_and_add_pools(rpc, v2_factories, v3_factories, chunk_start, chunk_end, v2_factory_fees, batch_size)
                .await;

            // Process this chunk's blocks via the standard pipeline (no PGA per-chunk)
            let chunk_resolved = ResolvedRange {
                start_block: chunk_start,
                end_block: chunk_end,
                block_count: chunk_end - chunk_start + 1,
                mode: resolved.mode,
            };
            let (opps, stats) = self.run_range_with_pga(&chunk_resolved, None)?;
            all_opps.extend(opps);
            all_stats.extend(stats);

            if chunk_end == resolved.end_block {
                break;
            }
            chunk_start = chunk_end + 1;
        }

        // Apply PGA globally
        let all_opps = if let Some(cfg) = pga_config {
            tracing::info!("Applying PGA adjustment to {} opportunities", all_opps.len());
            pga::adjust_opportunities(all_opps, &cfg)
        } else {
            all_opps
        };

        Ok((all_opps, all_stats))
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
    pub fn run_block(&mut self, block_num: u64) -> anyhow::Result<(Vec<MevOpportunity>, BlockReplayStats, Vec<u128>)> {
        let (block_data, txs) = self.replayer.load_block_data(block_num)?;
        let total_tx_count = txs.len();
        if txs.is_empty() {
            return Ok((Vec::new(), BlockReplayStats { block_number: block_num, total_tx_count: 0, dex_tx_count: 0, pending_tx_count: 0, mempool_opp_count: 0 }, Vec::new()));
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
        let mut jit_arb_detector = JitArbDetector::new(block_num).with_proximity_window(self.proximity_window);
        let mut liquidation_detector = LiquidationDetector::new(block_num)
            .with_reserve_cache(self.aave_reserve_cache.clone());

        // Take ownership of pool_manager so the closure can mutate it via RefCell
        let pool_manager = std::mem::take(&mut self.pool_manager);
        let pool_manager = RefCell::new(pool_manager);

        // Shared cell bridging TxData.from from filter closure to on_tx closure
        let current_tx_from: RefCell<Option<Address>> =
            RefCell::new(None);
        let dex_tx_count: RefCell<usize> = RefCell::new(0);
        // Collect effective gas prices for gas price distribution (H10)
        let gas_prices: RefCell<Vec<u128>> = RefCell::new(Vec::new());

        self.replayer.replay_each_filtered(
            block_num,
            |tx, receipt_logs| {
                *current_tx_from.borrow_mut() = Some(tx.from);
                let matched = tx.to.is_some_and(|to| {
                    pool_addrs.contains(&to) || token_addrs.contains(&to)
                })
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
                let opps = two_hop_detector.detect(
                    &pm,
                    i,
                    timestamp,
                    base_fee_per_gas,
                    self.gas_config,
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
                let jit_opps = jit_detector.detect(timestamp, base_fee_per_gas, &self.gas_config, &pm);
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
                let sandwich_opps = sandwich_detector.detect(timestamp, &pm, base_fee_per_gas, &self.gas_config);
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
                let jit_arb_opps = jit_arb_detector.detect(timestamp, &pm, base_fee_per_gas, &self.gas_config);
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
                let liq_opps = liquidation_detector.detect(
                    &pm, timestamp, base_fee_per_gas, self.gas_config,
                );
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

                // Apply this tx's log updates to pool state AFTER detection
                pm.update_from_logs(&tx.logs);

                Ok(())
            },
        )?;

        // Filter: drop opportunities where expected profit doesn't cover gas
        all_opportunities.retain(|opp| opp.expected_profit > U256::from(opp.gas_cost_wei));

        // Assign canonical dedup IDs (L9) to all opportunities
        for opp in &mut all_opportunities {
            opp.canonical_id = Some(crate::mev::opportunity::compute_canonical_id(
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
        Ok((all_opportunities, BlockReplayStats {
            block_number: block_num,
            total_tx_count,
            dex_tx_count: dex_tx_count.into_inner(),
            pending_tx_count: 0, // populated at range level by run_range_with_pga
            mempool_opp_count: 0, // populated at range level by run_range_with_pga
        }, gas_prices.into_inner()))
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
    pub fn run_range(
        &mut self,
        resolved: &ResolvedRange,
    ) -> anyhow::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>)> {
        self.run_range_with_pga(resolved, None)
    }

    /// Run backtest over the resolved block range, optionally applying PGA
    /// profit adjustment to each opportunity after detection.
    ///
    /// H10: Maintains a `GasPriceDistribution` across blocks, feeding it
    /// per-tx effective gas prices and using the N-th percentile as the
    /// effective gas price for P90 / Distribution gas models.
    pub fn run_range_with_pga(
        &mut self,
        resolved: &ResolvedRange,
        pga_config: Option<PgaConfig>,
    ) -> anyhow::Result<(Vec<MevOpportunity>, Vec<BlockReplayStats>)> {
        let mut all = Vec::new();
        let mut all_stats = Vec::new();
        // H10: Gas price distribution across recent blocks (sliding window of 50)
        let mut gas_dist = GasPriceDistribution::new(50);
        for block_num in resolved.start_block..=resolved.end_block {
            // H10: Set the percentile gas price from historical distribution
            // before each block so detectors use it for gas cost computation.
            if let Some(p) = self.gas_config.gas_model.target_percentile() {
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
                    let block_timestamp = match self.replayer.load_block_data(block_num) {
                        Ok((block, _)) => {
                            let base_fee = block.base_fee_per_gas.unwrap_or(0);
                            gas_dist.record_block(base_fee, block.gas_used, block.gas_limit);
                            block.timestamp
                        }
                        Err(_) => {
                            gas_dist.record_block(0, 0, 30_000_000);
                            0
                        }
                    };
                    gas_dist.finalize_block();

                    all.extend(opps);
                    all_stats.push(stats);

                    // L2: Record pool state snapshot for cross-block detection
                    if let Some(ref mut detector) = self.cross_block_detector {
                        detector.record_block(block_num, &self.pool_manager);
                        if detector.snapshot_count() >= 2 {
                            let cross_opps = detector.detect(
                                block_num,
                                block_timestamp,
                                self.gas_config,
                            );
                            if !cross_opps.is_empty() {
                                tracing::info!(
                                    "Block {}: {} cross-block opportunities detected",
                                    block_num,
                                    cross_opps.len(),
                                );
                                all.extend(cross_opps);
                            }
                        }
                    }
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

        let all = if let Some(cfg) = pga_config {
            tracing::info!("Applying PGA adjustment to {} opportunities", all.len());
            pga::adjust_opportunities(all, &cfg)
        } else {
            all
        };
        Ok((all, all_stats))
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
        crate::pool::dex_type::DexType::UniswapV2 => {
            pool_manager.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
                info,
                reserve0: 0,
                reserve1: 0,
            }));
        }
        crate::pool::dex_type::DexType::UniswapV3 => {
            pool_manager.add_pool(PoolState::UniswapV3(
                crate::pool::state::UniswapV3PoolState::new(info),
            ));
        }
        crate::pool::dex_type::DexType::Curve => {
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
        crate::pool::dex_type::DexType::Balancer => {
            pool_manager.add_pool(PoolState::Balancer(
                crate::pool::state::BalancerPoolState {
                    info,
                    balances: vec![],
                    token_index: std::collections::HashMap::new(),
                    pool_id: None,
                    weights: vec![],
                    pool_variant: crate::pool::state::BalancerPoolVariant::Weighted,
                    amplification: None,
                    scaling_factors: vec![],
                    bpt_index: None,
                },
            ));
        }
    }
}
