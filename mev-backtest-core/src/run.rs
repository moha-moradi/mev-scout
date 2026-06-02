use std::cell::RefCell;

use crate::mev::opportunity::MevOpportunity;
use crate::mev::two_hop::TwoHopArbDetector;
use crate::pool::registry::PoolRegistry;
use crate::pool::state::{PoolManager, PoolState, UniswapV2PoolState};
use crate::replay::BlockReplayer;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;

/// Orchestrates backtesting: replays blocks and detects MEV opportunities.
pub struct BacktestRunner {
    replayer: BlockReplayer,
    pool_manager: PoolManager,
    detector: TwoHopArbDetector,
    priority_fee_gwei: f64,
    fast_mode: bool,
}

impl BacktestRunner {
    pub fn new(
        replayer: BlockReplayer,
        pool_manager: PoolManager,
        min_profit_usd: f64,
    ) -> Self {
        BacktestRunner {
            replayer,
            pool_manager,
            detector: TwoHopArbDetector::new(min_profit_usd),
            priority_fee_gwei: 1.0,
            fast_mode: false,
        }
    }

    pub fn with_priority_fee(mut self, priority_fee_gwei: f64) -> Self {
        self.priority_fee_gwei = priority_fee_gwei;
        self
    }

    /// Enable fast mode: skip token address widening in tx filter.
    /// Only pools addresses are matched, not their tokens.
    pub fn with_fast_mode(mut self, fast: bool) -> Self {
        self.fast_mode = fast;
        self
    }

    /// Set a custom gas limit for arb transaction cost estimation.
    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.detector = TwoHopArbDetector::new(self.detector.min_profit_usd)
            .with_gas_limit(gas_limit);
        self
    }

    /// Initialize pool manager by loading registry and fetching on-chain reserves.
    pub async fn init_pools(
        pool_manager: &mut PoolManager,
        registry_path: Option<&str>,
        rpc: &RpcClient,
        block_num: u64,
    ) {
        let pool_infos = PoolRegistry::load_optional(registry_path);
        if pool_infos.is_empty() {
            tracing::warn!("No pools loaded from registry, skipping TwoHopArb detection");
            return;
        }

        tracing::info!("Loading {} pools from registry", pool_infos.len());

        for info in &pool_infos {
            match info.dex_type {
                crate::pool::dex_type::DexType::UniswapV2 => {
                    pool_manager.add_pool(PoolState::UniswapV2(UniswapV2PoolState {
                        info: info.clone(),
                        reserve0: 0,
                        reserve1: 0,
                    }));
                }
                crate::pool::dex_type::DexType::UniswapV3 => {
                    pool_manager.add_pool(PoolState::UniswapV3(
                        crate::pool::state::UniswapV3PoolState::new(info.clone()),
                    ));
                }
                crate::pool::dex_type::DexType::Curve => {
                    pool_manager.add_pool(PoolState::Curve(crate::pool::state::CurvePoolState {
                        info: info.clone(),
                        balances: vec![],
                        token_index: std::collections::HashMap::new(),
                    }));
                }
                crate::pool::dex_type::DexType::Balancer => {
                    pool_manager.add_pool(PoolState::Balancer(
                        crate::pool::state::BalancerPoolState {
                            info: info.clone(),
                            balances: vec![],
                            token_index: std::collections::HashMap::new(),
                            pool_id: None,
                        },
                    ));
                }
            }
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

    /// Replay a single block, skipping EVM execution for txs that don't interact with tracked pools.
    /// When `fast_mode` is false, also replays txs that touch any token address tracked by the pools.
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
        let pool_manager = std::mem::take(&mut self.pool_manager);
        let pool_manager = RefCell::new(pool_manager);
        let detector = &self.detector;
        let priority_fee_gwei = self.priority_fee_gwei;
        let fast_mode = self.fast_mode;

        self.replayer.replay_each_filtered(
            block_num,
            |tx, receipt_logs| {
                tx.to.map_or(false, |to| {
                    pool_addrs.contains(&to)
                        || (!fast_mode && token_addrs.contains(&to))
                })
                    || receipt_logs.iter().any(|l| {
                        pool_addrs.contains(&l.address)
                            || (!fast_mode && token_addrs.contains(&l.address))
                    })
            },
            |i, tx, _db| {
                let mut pm = pool_manager.borrow_mut();
                pm.update_from_logs(&tx.logs);

                let opps = detector.detect(
                    &*pm,
                    block_num,
                    i,
                    timestamp,
                    base_fee_per_gas,
                    priority_fee_gwei,
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

                Ok(())
            },
        )?;

        self.pool_manager = pool_manager.into_inner();
        Ok(all_opportunities)
    }

    /// Run backtest over a resolved block range.
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
