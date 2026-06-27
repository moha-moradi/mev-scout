use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, U256};

use crate::cache::SqliteStore;
use crate::mev::detectors::mempool;
use crate::mev::detectors::MultiHopArbDetector;
use crate::mev::detectors::TwoHopArbDetector;
use crate::types::MevOpportunity;
use crate::pool::{PoolManager, PoolState};
use crate::replay::BlockReplayer;
use crate::rpc::RpcClient;
use crate::pipeline::BacktestRunner;
use crate::types::{GasConfig, Strategy};

/// Runtime configuration for live mode operation.
#[derive(Debug, Clone)]
pub struct LiveConfig {
    pub initial_balance_wei: U256,
    pub min_profit_threshold_wei: U256,
    pub poll_interval_ms: u64,
    pub max_executions: Option<u64>,
    pub strategies: Vec<Strategy>,
    pub gas_config: GasConfig,
    pub resync_interval: u64,
    pub export_path: String,
    pub replay_file: Option<String>,
}

/// Record of a single virtual execution (settled or mempool-originated).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionRecord {
    pub opportunity: MevOpportunity,
    pub simulated_profit_wei: U256,
    pub simulated_gas_cost_wei: u128,
    pub token_in: Address,
    pub token_out: Address,
    pub input_amount: U256,
    pub output_amount: U256,
    pub block_number: u64,
    pub timestamp: u64,
    pub success: bool,
}

/// Virtual wallet state tracking native and token balances.
#[derive(Debug, Clone)]
pub struct LiveRunnerState {
    pub native_balance_wei: U256,
    pub token_balances: HashMap<Address, U256>,
    pub initial_native_balance_wei: U256,
    pub total_executions: u64,
    pub successful_executions: u64,
    pub failed_executions: u64,
    pub total_profit_wei: U256,
    pub total_gas_spent_wei: u128,
    pub execution_history: Vec<ExecutionRecord>,
}

impl LiveRunnerState {
    pub fn new(initial_balance_wei: U256) -> Self {
        LiveRunnerState {
            native_balance_wei: initial_balance_wei,
            token_balances: HashMap::new(),
            initial_native_balance_wei: initial_balance_wei,
            total_executions: 0,
            successful_executions: 0,
            failed_executions: 0,
            total_profit_wei: U256::ZERO,
            total_gas_spent_wei: 0,
            execution_history: Vec::new(),
        }
    }

    /// Check if the wallet has gone bankrupt (no native balance for gas and
    /// no token holdings that can be swapped back to native).
    pub fn is_bankrupt(&self, min_gas_cost_wei: U256) -> bool {
        if self.native_balance_wei >= min_gas_cost_wei {
            return false;
        }
        // No token holdings to swap for gas
        self.token_balances.values().all(|b| *b == U256::ZERO)
    }
}

/// Live MEV runner that simulates a virtual live MEV bot.
///
/// Connects to the live chain, processes settled blocks for authoritative
/// state sync and full MEV detection, and scans the mempool for arb
/// opportunities. Tracks a virtual wallet with P&L — no real transactions.
#[allow(dead_code)]
pub struct LiveRunner {
    config: LiveConfig,
    rpc: RpcClient,
    cache: SqliteStore,
    pool_manager: PoolManager,
    backtest_runner: BacktestRunner,
    block_replayer: BlockReplayer,
    wallet: LiveRunnerState,
    last_processed_block: u64,
    last_resync_block: u64,
    chain_id: u64,
}

impl LiveRunner {
    /// Create a new LiveRunner. The pool manager should already be initialized
    /// and the backtest runner configured before calling this.
    pub async fn new(
        config: LiveConfig,
        rpc: RpcClient,
        cache: SqliteStore,
        pool_manager: PoolManager,
        backtest_runner: BacktestRunner,
        block_replayer: BlockReplayer,
        chain_id: u64,
    ) -> Self {
        // Determine starting block from the latest finalized block
        let latest_block = rpc.get_block_number().await.unwrap_or(0);
        tracing::info!("LiveRunner initialized at block {}", latest_block);

        let initial_balance = config.initial_balance_wei;
        LiveRunner {
            config,
            rpc,
            cache,
            pool_manager,
            backtest_runner,
            block_replayer,
            wallet: LiveRunnerState::new(initial_balance),
            last_processed_block: latest_block,
            last_resync_block: latest_block,
            chain_id,
        }
    }

    /// Main run loop. Processes settled blocks and scans the mempool
    /// in each cycle until stopped (Ctrl+C, max_executions, or bankruptcy).
    ///
    /// When `config.replay_file` is set, the runner loads a pre-recorded
    /// pending-tx JSON file and processes it once (offline replay) instead
    /// of live RPC polling.
    pub async fn run(&mut self, cancel: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<()> {
        let start_time = Instant::now();
        let mut settled_blocks_processed: u64 = 0;
        let mut mempool_scans: u64 = 0;

        // ── Replay-file mode ───────────────────────────────────────────
        if let Some(ref replay_path) = self.config.replay_file {
            tracing::info!("Replay-file mode: loading txs from {}", replay_path);
            let file_content = std::fs::read_to_string(replay_path)
                .map_err(|e| anyhow::anyhow!("Failed to read replay file: {}", e)))?;
            let pending_txs: Vec<crate::data::TxData> = serde_json::from_str(&file_content)
                .map_err(|e| anyhow::anyhow!("Failed to parse replay file: {}", e)))?;
            tracing::info!("Loaded {} pending txs from replay file", pending_txs.len());

            // Skip settled block processing in replay mode — just use current pool state
            mempool_scans = 1;
            let gas_config = self.config.gas_config;
            let block_number = self.last_processed_block;
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            // Clone pool manager for speculative state
            let mut speculative_state = self.pool_manager.clone();

            // Apply effects
            for tx in &pending_txs {
                let effects = mempool::estimate_pending_tx_pool_impact(tx, &speculative_state);
                self.apply_pool_effects(&mut speculative_state, &effects);
            }

            // Run arb-only detection
            let mut opps = Vec::new();
            if self.config.strategies.contains(&Strategy::TwoHopArb) {
                let mut two_hop = TwoHopArbDetector::new(block_number);
                let detected = two_hop.detect(
                    &speculative_state, 0, timestamp, 0, gas_config,
                );
                opps.extend(detected);
            }
            if self.config.strategies.contains(&Strategy::MultiHopArb) {
                let mut multi_hop = MultiHopArbDetector::new(block_number);
                let detected = multi_hop.detect(
                    &speculative_state, 0, timestamp, 0, gas_config,
                );
                opps.extend(detected);
            }
            for opp in &mut opps {
                opp.mempool_only = true;
            }

            if !opps.is_empty() {
                tracing::info!("Replay: {} arb opportunities detected", opps.len());
                for opp in opps {
                    self.execute_mempool_opportunity(opp, &mempool::PendingBlockCapture {
                        block_number,
                        txs: pending_txs.clone(),
                        tx_count: pending_txs.len(),
                        base_fee_per_gas: 0,
                        timestamp,
                    });
                }
            }

            self.print_dashboard(settled_blocks_processed, mempool_scans);
            self.print_summary(start_time, settled_blocks_processed, mempool_scans);
            self.save_execution_history()?;
            return Ok(());
        }

        // ── Live mode loop ────────────────────────────────────────────
        loop {
            if *cancel.borrow() {
                tracing::info!("Live mode shutdown signal received");
                break;
            }

            // ── Phase A: Settled block processing ──────────────────────
            let latest_block = match self.rpc.get_block_number().await {
                Ok(n) => n,
                Err(e) => {
                    tracing::error!("Failed to get latest block number: {}", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            // Reorg detection
            if latest_block < self.last_processed_block {
                tracing::warn!(
                    "Chain reorg detected: last={} latest={}, reinitializing pool state",
                    self.last_processed_block,
                    latest_block
                );
                self.handle_reorg(latest_block).await;
                continue;
            }

            // Process new settled blocks
            while self.last_processed_block < latest_block {
                let block_num = self.last_processed_block + 1;

                // Read-through cache: fetch block if not cached
                self.fetch_block_if_uncached(block_num).await?;

                // Replay block and run detection
                match self.backtest_runner.run_block(block_num) {
                    Ok((opportunities, stats, _)) => {
                        tracing::debug!(
                            "Block {}: {} txs, {} DEX txs, {} opportunities",
                            block_num,
                            stats.total_tx_count,
                            stats.dex_tx_count,
                            opportunities.len(),
                        );
                        if !opportunities.is_empty() {
                            self.process_settled_opportunities(opportunities);
                        }
                        settled_blocks_processed += 1;
                    }
                    Err(e) => {
                        tracing::error!("Block {} replay failed: {:?}", block_num, e);
                    }
                }

                self.last_processed_block = block_num;
            }

            // Cross-block detection on accumulated window
            if self.backtest_runner.cross_block_enabled() {
                let cross_opps = self.backtest_runner.detect_cross_block();
                if !cross_opps.is_empty() {
                    tracing::info!("Cross-block: {} opportunities detected", cross_opps.len());
                    self.process_settled_opportunities(cross_opps);
                }
            }

            // ── Phase B: Mempool scanning ─────────────────────────────
            mempool_scans += 1;

            // Periodic full resync
            if mempool_scans % self.config.resync_interval == 0 {
                self.resync_pool_state().await;
            }

            let pending = self.capture_pending_block().await;
            if let Some(ref pending) = pending {
                if !pending.txs.is_empty() {
                    let opps = self.run_mempool_detection(pending);
                    if !opps.is_empty() {
                        tracing::info!(
                            "Mempool: {} arb opportunities from {} pending txs",
                            opps.len(),
                            pending.tx_count,
                        );
                        // Execute mempool opportunities against wallet
                        for opp in opps {
                            self.execute_mempool_opportunity(opp, pending);
                        }
                    }
                }
            }

            // ── Bankruptcy check ───────────────────────────────────────
            let min_gas = U256::from(self.config.gas_config.gas_limit as u128 * 10_000_000_000u128);
            if self.wallet.is_bankrupt(min_gas) {
                tracing::warn!(
                    "Wallet bankrupt (native={} wei), stopping live mode",
                    self.wallet.native_balance_wei,
                );
                break;
            }

            // ── Max executions check ────────────────────────────────────
            if let Some(max) = self.config.max_executions {
                if self.wallet.total_executions >= max {
                    tracing::info!("Reached max_executions={}, stopping", max);
                    break;
                }
            }

            // ── Phase C: Output & sleep ────────────────────────────────
            self.print_dashboard(settled_blocks_processed, mempool_scans);
            tokio::time::sleep(Duration::from_millis(self.config.poll_interval_ms)).await;
        }

        self.print_summary(start_time, settled_blocks_processed, mempool_scans);
        self.save_execution_history()?;
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Fetch a single block's data and receipts if not already cached.
    async fn fetch_block_if_uncached(&self, block_num: u64) -> anyhow::Result<()> {
        if self.cache.has_block(block_num)? {
            return Ok(());
        }
        tracing::debug!("Fetching block {} from RPC (read-through cache)", block_num);
        let (block, txs, receipts) = self.rpc.get_block_and_receipts_batch(block_num).await?;
        self.cache.put_block_data(block_num, &block, &txs, &receipts)?;
        Ok(())
    }

    /// Update wallet state from settled opportunities.
    fn process_settled_opportunities(&mut self, opportunities: Vec<MevOpportunity>) {
        // In settled block mode, opportunities are informational (pool state
        // already reflects execution). We record them to track what was visible.
        for opp in &opportunities {
            if opp.expected_profit > U256::ZERO {
                self.wallet.total_profit_wei =
                    self.wallet.total_profit_wei.saturating_add(opp.expected_profit);
            }
        }
    }

    /// Handle chain reorg by reinitializing pool state at the new latest block.
    async fn handle_reorg(&mut self, latest_block: u64) {
        // Reinitialize pool manager at the reorg block
        BacktestRunner::init_pools(
            &mut self.pool_manager,
            &self.rpc,
            latest_block,
            Some(&self.cache),
        )
        .await;
        self.last_processed_block = latest_block;
        self.last_resync_block = latest_block;
    }

    /// Capture the current pending block from the mempool.
    async fn capture_pending_block(&self) -> Option<mempool::PendingBlockCapture> {
        mempool::capture_pending_block(&self.rpc).await
    }

    /// Run arbitrage detection on the mempool state (forked from settled state).
    fn run_mempool_detection(&mut self, pending: &mempool::PendingBlockCapture) -> Vec<MevOpportunity> {
        let base_fee = pending.base_fee_per_gas;
        let timestamp = pending.timestamp;
        let block_number = pending.block_number;

        // Create a gas config with live gas pricing
        let gas_config = self.config.gas_config;

        // Clone pool manager for speculative state
        let mut speculative_state = self.pool_manager.clone();

        // Apply pending tx effects to the speculative pool state
        for tx in &pending.txs {
            let effects = mempool::estimate_pending_tx_pool_impact(tx, &speculative_state);
            self.apply_pool_effects(&mut speculative_state, &effects);
        }

        // Run arb-only detection
        let mut results = Vec::new();

        if self.config.strategies.contains(&Strategy::TwoHopArb) {
            let mut two_hop = TwoHopArbDetector::new(block_number);
            let opps = two_hop.detect(&speculative_state, 0, timestamp, base_fee, gas_config);
            results.extend(opps);
        }

        if self.config.strategies.contains(&Strategy::MultiHopArb) {
            let mut multi_hop = MultiHopArbDetector::new(block_number);
            let opps = multi_hop.detect(&speculative_state, 0, timestamp, base_fee, gas_config);
            results.extend(opps);
        }

        for opp in &mut results {
            opp.mempool_only = true;
        }

        results
    }

    /// Apply `PendingPoolEffect`s to a `PoolManager` (used for speculative state).
    fn apply_pool_effects(&self, pool_manager: &mut PoolManager, effects: &[mempool::PendingPoolEffect]) {
        for effect in effects {
            if let Some(pool) = pool_manager.get_mut(&effect.pool_address) {
                match pool {
                    PoolState::UniswapV2(state) => {
                        if state.info.token0 == effect.token_address {
                            if effect.reserve_delta >= 0 {
                                state.reserve0 = state.reserve0.saturating_add(effect.reserve_delta as u128);
                            } else {
                                state.reserve0 = state.reserve0.saturating_sub((-effect.reserve_delta) as u128);
                            }
                        } else if state.info.token1 == effect.token_address {
                            if effect.reserve_delta >= 0 {
                                state.reserve1 = state.reserve1.saturating_add(effect.reserve_delta as u128);
                            } else {
                                state.reserve1 = state.reserve1.saturating_sub((-effect.reserve_delta) as u128);
                            }
                        }
                    }
                    PoolState::UniswapV3(state) => {
                        // V3 price impact estimation is complex — skip per-token reserve
                        // updates for V3 pools. The user relies on settled block replay
                        // (Phase A) for authoritative V3 state.
                        let _ = state;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Execute a mempool opportunity against the virtual wallet.
    fn execute_mempool_opportunity(&mut self, opp: MevOpportunity, _pending: &mempool::PendingBlockCapture) {
        let profit = opp.expected_profit;
        let gas_cost = U256::from(opp.gas_cost_wei);

        // Check wallet constraints
        if profit < self.config.min_profit_threshold_wei {
            return;
        }
        if self.wallet.native_balance_wei < gas_cost {
            return;
        }

        // Deduct gas cost from native balance
        self.wallet.native_balance_wei = self.wallet.native_balance_wei.saturating_sub(gas_cost);
        self.wallet.total_gas_spent_wei = self.wallet.total_gas_spent_wei.saturating_add(opp.gas_cost_wei);

        // For arb opportunities, output = input + profit (gross)
        let output_amount = opp.input_amount.saturating_add(profit);

        // Update token balances
        if opp.token_in != Address::ZERO {
            let balance = self.wallet.token_balances.entry(opp.token_in).or_insert(U256::ZERO);
            *balance = balance.saturating_sub(opp.input_amount);
        }
        if opp.token_out != Address::ZERO {
            let balance = self.wallet.token_balances.entry(opp.token_out).or_insert(U256::ZERO);
            *balance = balance.saturating_add(output_amount);
        }

        // Update native balance from profit (assumes profit is in native or WETH-equivalent)
        self.wallet.native_balance_wei = self.wallet.native_balance_wei.saturating_add(profit);
        self.wallet.total_profit_wei = self.wallet.total_profit_wei.saturating_add(profit);

        let success = true;
        if success {
            self.wallet.successful_executions += 1;
        } else {
            self.wallet.failed_executions += 1;
        }
        self.wallet.total_executions += 1;

        let record = ExecutionRecord {
            simulated_profit_wei: profit,
            simulated_gas_cost_wei: opp.gas_cost_wei,
            token_in: opp.token_in,
            token_out: opp.token_out,
            input_amount: opp.input_amount,
            output_amount,
            block_number: opp.block_number,
            timestamp: opp.timestamp,
            success,
            opportunity: opp,
        };

        // Cap history at 10k records
        if self.wallet.execution_history.len() >= 10_000 {
            self.wallet.execution_history.remove(0);
        }
        self.wallet.execution_history.push(record);
    }

    /// Full resync of pool state from chain.
    async fn resync_pool_state(&mut self) {
        let block_num = self.last_processed_block.saturating_sub(1);
        tracing::info!("Resyncing pool state at block {}", block_num);
        BacktestRunner::init_pools(
            &mut self.pool_manager,
            &self.rpc,
            block_num,
            Some(&self.cache),
        )
        .await;
        self.last_resync_block = self.last_processed_block;
    }

    /// Print the live dashboard.
    fn print_dashboard(&self, settled_blocks: u64, mempool_scans: u64) {
        let native_whole = self.wallet.native_balance_wei / U256::from(1_000_000_000_000_000_000u128);
        let native_frac = (self.wallet.native_balance_wei % U256::from(1_000_000_000_000_000_000u128))
            / U256::from(10_000_000_000_000_000u128);

        let pnl = if self.wallet.initial_native_balance_wei > U256::ZERO {
            let raw = self.wallet.native_balance_wei.saturating_sub(self.wallet.initial_native_balance_wei);
            let pct = raw * U256::from(10000) / self.wallet.initial_native_balance_wei;
            format!(
                "{:+}.{:04} native ({:+}.{:02}%)",
                raw / U256::from(1_000_000_000_000_000_000u128),
                (raw % U256::from(1_000_000_000_000_000_000u128)) / U256::from(10_000_000_000_000u128),
                pct / U256::from(100),
                pct % U256::from(100),
            )
        } else {
            "0.0000 native".to_string()
        };

        println!("\n=== MEV Scout - Live Mode ===");
        println!("Native Balance: {}.{:04} native", native_whole, native_frac);
        println!("Tokens Held:    {} tokens", self.wallet.token_balances.len());
        if self.wallet.total_executions > 0 {
            println!("Executions:     {} ({} ok, {} fail)",
                self.wallet.total_executions,
                self.wallet.successful_executions,
                self.wallet.failed_executions,
            );
        }
        println!("Settled blks:   {} | Mempool scans: {}", settled_blocks, mempool_scans);
        println!("Total Profit:   {} native", pnl);
        println!("Total Gas:      {}.{:04} native",
            U256::from(self.wallet.total_gas_spent_wei) / U256::from(1_000_000_000_000_000_000u128),
            (U256::from(self.wallet.total_gas_spent_wei) % U256::from(1_000_000_000_000_000_000u128))
                / U256::from(10_000_000_000_000_000u128),
        );
        println!("Net P&L:        {}", pnl);
    }

    /// Print final summary on shutdown.
    fn print_summary(&self, start_time: Instant, settled_blocks: u64, mempool_scans: u64) {
        let elapsed = start_time.elapsed();
        let secs = elapsed.as_secs();
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        let secs_rem = secs % 60;

        let native_unit = U256::from(1_000_000_000_000_000_000u128);
        let gas_native = U256::from(self.wallet.total_gas_spent_wei);
        let net_pnl = if self.wallet.initial_native_balance_wei > U256::ZERO {
            let raw = self.wallet.native_balance_wei.saturating_sub(self.wallet.initial_native_balance_wei);
            let pct = raw * U256::from(10000) / self.wallet.initial_native_balance_wei;
            format!(
                "{:+}.{:04} native ({:+}.{:02}%)",
                raw / native_unit,
                (raw % native_unit) / U256::from(10_000_000_000_000_000u128),
                pct / U256::from(100),
                pct % U256::from(100),
            )
        } else {
            "0.0000 native".to_string()
        };

        println!("\n=== MEV Scout - Live Mode (shutdown) ===");
        println!("Runtime:          {}h {}m {}s", hours, mins, secs_rem);
        println!("Settled blocks:   {} | Mempool scans: {}", settled_blocks, mempool_scans);
        println!("Total txs seen:   ~{}", mempool_scans * 100);
        println!("----------------------------------------");
        println!("Initial Balance:  {}.{:04} native",
            self.wallet.initial_native_balance_wei / native_unit,
            (self.wallet.initial_native_balance_wei % native_unit) / U256::from(10_000_000_000_000_000u128),
        );
        println!("Final Balance:    {}.{:04} native",
            self.wallet.native_balance_wei / native_unit,
            (self.wallet.native_balance_wei % native_unit) / U256::from(10_000_000_000_000_000u128),
        );
        println!("Net P&L:          {}", net_pnl);
        println!("Total Executions: {} ({} ok, {} fail)",
            self.wallet.total_executions,
            self.wallet.successful_executions,
            self.wallet.failed_executions,
        );
        if self.wallet.total_executions > 0 {
            let avg_profit = self.wallet.total_profit_wei / U256::from(self.wallet.total_executions);
            println!("Avg Profit/Trade: {}.{:04} native",
                avg_profit / native_unit,
                (avg_profit % native_unit) / U256::from(10_000_000_000_000_000u128),
            );
        }
        println!("Total Gas Spent:  {}.{:04} native",
            gas_native / native_unit,
            (gas_native % native_unit) / U256::from(10_000_000_000_000_000u128),
        );
        // Show token holdings
        if !self.wallet.token_balances.is_empty() {
            println!("Token Holdings:   {} tokens held", self.wallet.token_balances.len());
        }
    }

    /// Save execution history to JSON.
    fn save_execution_history(&self) -> anyhow::Result<()> {
        if self.wallet.execution_history.is_empty() {
            return Ok(());
        }
        let run_id = format!(
            "live_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("System clock went backwards")
                .as_secs()
        );
        let dir = std::path::Path::new(&self.config.export_path);
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", run_id));
        let json = serde_json::to_string_pretty(&self.wallet.execution_history)?;
        std::fs::write(&path, json)?;
        tracing::info!("Execution history saved to {}", path.display());
        Ok(())
    }
}
