//! Backtest orchestration — replays blocks through revm and runs all MEV detection strategies.

use std::cell::RefCell;

use crate::cache::CacheStore;
use crate::mev::opportunity::MevOpportunity;
use crate::mev::jit::JitDetector;
use crate::mev::sandwich::SandwichDetector;
use crate::mev::jit_arb::JitArbDetector;
use crate::mev::multi_hop::MultiHopArbDetector;
use crate::mev::two_hop::TwoHopArbDetector;
use alloy::primitives::{Address, U256};
use crate::pool::state::{PoolInfo, PoolManager, PoolState, UniswapV2PoolState};
use crate::replay::BlockReplayer;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;
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
    pool_manager: PoolManager,
    gas_config: GasConfig,
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
        BacktestRunner {
            replayer,
            pool_manager,
            gas_config,
        }
    }

    /// Initialize the pool manager by loading pool definitions and fetching
    /// on-chain reserve state at a reference block.
    ///
    /// Loads pool definitions from the sled cache (on-chain discovery from
    /// prior runs or auto-discovery at backtest start). Then fetches current
    /// reserves for each pool via `eth_call getReserves()` (V2) or
    /// `slot0()/liquidity()` (V3) with up to 20 concurrent RPC calls.
    ///
    /// Pools that fail to initialize (e.g., the contract no longer exists at
    /// that block) are logged as warnings but do not halt execution.
    pub async fn init_pools(
        pool_manager: &mut PoolManager,
        rpc: &RpcClient,
        block_num: u64,
        cache: Option<&CacheStore>,
    ) {
        // Load discovered pools from sled cache (if available)
        if let Some(cache) = cache {
            match cache.list_discovered_pools() {
                Ok(pools) => {
                    for info in &pools {
                        add_pool_to_manager(pool_manager, info.clone());
                    }
                    tracing::info!("Loaded {} pools from discovery cache", pools.len());
                }
                Err(e) => tracing::warn!("Failed to list discovered pools: {}", e),
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
    pub fn run_block(&mut self, block_num: u64) -> anyhow::Result<Vec<MevOpportunity>> {
        let (block_data, txs) = self.replayer.load_block_data(block_num)?;
        if txs.is_empty() {
            return Ok(Vec::new());
        }

        let timestamp = block_data.timestamp;
        let base_fee_per_gas = block_data.base_fee_per_gas.unwrap_or(0);

        let pool_addrs: std::collections::HashSet<_> =
            self.pool_manager.pool_addresses().into_iter().collect();
        let token_addrs: std::collections::HashSet<_> =
            self.pool_manager.token_addresses().into_iter().collect();

        let mut all_opportunities = Vec::new();
        // Seed JIT detector tick cache BEFORE taking pool_manager
        let mut jit_detector = JitDetector::new(block_num);
        jit_detector.seed_pool_tick_cache(&self.pool_manager);
        let mut sandwich_detector = SandwichDetector::new(block_num);
        let mut jit_arb_detector = JitArbDetector::new(block_num);

        // Take ownership of pool_manager so the closure can mutate it via RefCell
        let pool_manager = std::mem::take(&mut self.pool_manager);
        let pool_manager = RefCell::new(pool_manager);

        // Shared cell bridging TxData.from from filter closure to on_tx closure
        let current_tx_from: RefCell<Option<Address>> =
            RefCell::new(None);

        self.replayer.replay_each_filtered(
            block_num,
            |tx, receipt_logs| {
                *current_tx_from.borrow_mut() = Some(tx.from);
                tx.to.is_some_and(|to| {
                    pool_addrs.contains(&to) || token_addrs.contains(&to)
                })
                    || receipt_logs.iter().any(|l| {
                        pool_addrs.contains(&l.address) || token_addrs.contains(&l.address)
                    })
            },
            |i, tx, _db| {
                let mut pm = pool_manager.borrow_mut();
                pm.update_from_logs(&tx.logs);

                let opps = TwoHopArbDetector::detect(
                    &pm,
                    block_num,
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

                let multi_opps = MultiHopArbDetector::detect(
                    &pm,
                    block_num,
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
                jit_detector.process_tx(i, &tx.logs, sender);
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
                sandwich_detector.process_tx(i, &tx.logs, sender);
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

                drop(pm);

                // JitArb detector
                jit_arb_detector.process_tx(i, &tx.logs, sender);
                let jit_arb_opps = jit_arb_detector.detect(timestamp, &pool_manager.borrow(), base_fee_per_gas, &self.gas_config);
                if !jit_arb_opps.is_empty() {
                    tracing::info!(
                        "Block {} tx {}: {} JitArb opportunities",
                        block_num,
                        i,
                        jit_arb_opps.len()
                    );
                }
                all_opportunities.extend(jit_arb_opps);

                Ok(())
            },
        )?;

        // Filter: drop opportunities where expected profit doesn't cover gas
        all_opportunities.retain(|opp| opp.expected_profit > U256::from(opp.gas_cost_wei));

        self.pool_manager = pool_manager.into_inner();
        Ok(all_opportunities)
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
    ) -> anyhow::Result<Vec<MevOpportunity>> {
        let mut all = Vec::new();
        for block_num in resolved.start_block..=resolved.end_block {
            match self.run_block(block_num) {
                Ok(opps) => {
                    tracing::info!(
                        "Block {} done: {} opportunities",
                        block_num,
                        opps.len()
                    );
                    all.extend(opps);
                }
                Err(e) => {
                    tracing::error!("Block {} failed: {:?}", block_num, e);
                }
            }
        }
        Ok(all)
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
fn add_pool_to_manager(pool_manager: &mut PoolManager, info: PoolInfo) {
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
            }));
        }
        crate::pool::dex_type::DexType::Balancer => {
            pool_manager.add_pool(PoolState::Balancer(
                crate::pool::state::BalancerPoolState {
                    info,
                    balances: vec![],
                    token_index: std::collections::HashMap::new(),
                    pool_id: None,
                },
            ));
        }
    }
}
