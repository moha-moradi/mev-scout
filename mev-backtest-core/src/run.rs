use std::cell::RefCell;

use crate::cache::CacheStore;
use crate::mev::opportunity::MevOpportunity;
use crate::mev::jit::JitDetector;
use crate::mev::sandwich::SandwichDetector;
use crate::mev::multi_hop::MultiHopArbDetector;
use crate::mev::two_hop::TwoHopArbDetector;
use alloy::primitives::Address;
use crate::pool::registry::PoolRegistry;
use crate::pool::state::{PoolInfo, PoolManager, PoolState, UniswapV2PoolState};
use crate::replay::BlockReplayer;
use crate::resolver::ResolvedRange;
use crate::rpc::RpcClient;
use crate::types::GasConfig;

/// Orchestrates backtesting: replays blocks and detects MEV opportunities.
pub struct BacktestRunner {
    replayer: BlockReplayer,
    pool_manager: PoolManager,
    gas_config: GasConfig,
}

impl BacktestRunner {
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

    /// Initialize pool manager by loading registry + discovered pools and fetching on-chain reserves.
    pub async fn init_pools(
        pool_manager: &mut PoolManager,
        registry_path: Option<&str>,
        rpc: &RpcClient,
        block_num: u64,
        cache: Option<&CacheStore>,
    ) {
        // 1. Load pools from JSON registry
        let registry_pools = PoolRegistry::load_optional(registry_path);
        tracing::info!("Loaded {} pools from registry", registry_pools.len());

        // 2. Load discovered pools from sled cache (if available)
        let mut discovered_pools = Vec::new();
        if let Some(cache) = cache {
            match cache.list_discovered_pools() {
                Ok(pools) => discovered_pools = pools,
                Err(e) => tracing::warn!("Failed to list discovered pools: {}", e),
            }
        }
        tracing::info!("Loaded {} discovered pools from cache", discovered_pools.len());

        // 3. Merge: registry pools take precedence over discovered pools
        let mut seen = std::collections::HashSet::new();
        for info in &registry_pools {
            seen.insert(info.address);
            add_pool_to_manager(pool_manager, info.clone());
        }
        for info in &discovered_pools {
            if seen.insert(info.address) {
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

    /// Replay a single block, skipping EVM execution for txs that don't interact with tracked pools.
    /// Replays txs that touch pool addresses or tracked token addresses.
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
        // Take ownership of pool_manager so the closure can mutate it via RefCell
        // (the replayer's closure требует &mut self, so we std::mem::take + RefCell
        // to satisfy the borrow checker; pool_manager is restored after the block)
        let pool_manager = std::mem::take(&mut self.pool_manager);
        let pool_manager = RefCell::new(pool_manager);

        // Shared cell bridging TxData.from from filter closure to on_tx closure
        let current_tx_from: RefCell<Option<Address>> =
            RefCell::new(None);
        let mut jit_detector = JitDetector::new(block_num);
        let mut sandwich_detector = SandwichDetector::new(block_num);

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
                let jit_opps = jit_detector.detect(timestamp);
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
                let sandwich_opps = sandwich_detector.detect(timestamp, &pm);
                if !sandwich_opps.is_empty() {
                    tracing::info!(
                        "Block {} tx {}: {} sandwich opportunities",
                        block_num,
                        i,
                        sandwich_opps.len()
                    );
                }
                all_opportunities.extend(sandwich_opps);

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
