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
use crate::types::{GasConfig, GasModel, PriceOracleMode, Strategy};

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
    pub chain_display_name: String,
    pub price_oracle_mode: PriceOracleMode,
    pub token_prices: HashMap<Address, f64>,
    pub chain_defaults: crate::config::ChainConfig,
    pub rpc_url: String,
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
    total_mempool_txs_seen: u64,
    best_trade_profit: U256,
    best_trade_desc: Option<String>,
    settled_strategy_counts: HashMap<Strategy, u64>,
    mempool_strategy_counts: HashMap<Strategy, u64>,
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
            total_mempool_txs_seen: 0,
            best_trade_profit: U256::ZERO,
            best_trade_desc: None,
            settled_strategy_counts: HashMap::new(),
            mempool_strategy_counts: HashMap::new(),
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
                .map_err(|e| anyhow::anyhow!("Failed to read replay file: {}", e))?;
            let pending_txs: Vec<crate::data::TxData> = serde_json::from_str(&file_content)
                .map_err(|e| anyhow::anyhow!("Failed to parse replay file: {}", e))?;
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
                    self.execute_virtual_trade(opp).await;
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

            // Fetch live gas prices for this cycle
            let (live_base_fee, live_priority_fee) = self.fetch_live_gas_prices().await;
            let mempool_gas_config = if let (Some(_base_fee), Some(pf_gwei)) = (live_base_fee, live_priority_fee) {
                let mut gc = self.config.gas_config;
                gc.priority_fee_gwei = pf_gwei;
                gc
            } else {
                self.config.gas_config
            };

            let pending = self.capture_pending_block().await;
            if let Some(ref pending) = pending {
                self.total_mempool_txs_seen += pending.tx_count as u64;

                if !pending.txs.is_empty() {
                    let opps = self.run_mempool_detection(pending, mempool_gas_config, live_base_fee).await;
                    if !opps.is_empty() {
                        tracing::info!(
                            "Mempool: {} arb opportunities from {} pending txs",
                            opps.len(),
                            pending.tx_count,
                        );
                        for opp in opps {
                            self.execute_virtual_trade(opp).await;
                        }
                    }
                }
            }

            // ── Bankruptcy check ───────────────────────────────────────
            let min_gas_cost = self.config.gas_config.compute_gas_cost_with_limit(
                self.config.gas_config.gas_limit,
                0, // base_fee = 0 to get pure priority-fee cost floor
            );
            let min_gas = U256::from(min_gas_cost).max(U256::from(1_000_000_000_000u128)); // at least 1e12 wei
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
        self.cache.put_block_data(block_num, &block, &txs, &receipts, None, None)?;
        Ok(())
    }

    /// Update wallet state and strategy counts from settled opportunities.
    fn process_settled_opportunities(&mut self, opportunities: Vec<MevOpportunity>) {
        for opp in &opportunities {
            if opp.expected_profit > U256::ZERO {
                self.wallet.total_profit_wei =
                    self.wallet.total_profit_wei.saturating_add(opp.expected_profit);
            }
            *self.settled_strategy_counts.entry(opp.strategy).or_insert(0) += 1;
        }
    }

    /// Handle chain reorg by reinitializing pool state at the new latest block.
    async fn handle_reorg(&mut self, latest_block: u64) {
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
    async fn run_mempool_detection(
        &mut self,
        pending: &mempool::PendingBlockCapture,
        mut gas_config: GasConfig,
        _live_base_fee: Option<u128>,
    ) -> Vec<MevOpportunity> {
        let base_fee = pending.base_fee_per_gas;
        let timestamp = pending.timestamp;
        let block_number = pending.block_number;

        if base_fee > 0 {
            gas_config.gas_model = GasModel::Live;
        }

        let mut speculative_state = self.pool_manager.clone();

        for tx in &pending.txs {
            let effects = mempool::simulate_pending_tx_pool_impact(
                tx,
                &speculative_state,
                &self.rpc,
                self.chain_id,
                block_number,
            )
            .await;
            if effects.is_empty() {
                let fallback = mempool::estimate_pending_tx_pool_impact(tx, &speculative_state);
                if !fallback.is_empty() {
                    self.apply_pool_effects(&mut speculative_state, &fallback);
                }
            } else {
                self.apply_pool_effects(&mut speculative_state, &effects);
            }
        }

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
                        let _ = state;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Fetch live gas prices from the chain for the current cycle.
    /// Returns (base_fee_gwei, priority_fee_gwei) or (None, None) on failure.
    async fn fetch_live_gas_prices(&self) -> (Option<u128>, Option<f64>) {
        let base_fee = self.rpc.get_gas_price().await.ok();
        let priority_fee = self.rpc.get_max_priority_fee().await.ok();
        if let (Some(bf), Some(pf)) = (base_fee, priority_fee) {
            let pf_gwei = pf as f64 / 1_000_000_000.0;
            (Some(bf), Some(pf_gwei))
        } else {
            (base_fee, priority_fee.map(|pf| pf as f64 / 1_000_000_000.0))
        }
    }

    /// Helper: transfer native balance to wrapped-native token balance.
    fn wrap_native(&mut self, amount: U256) {
        let actual = amount.min(self.wallet.native_balance_wei);
        if actual == U256::ZERO { return; }
        self.wallet.native_balance_wei = self.wallet.native_balance_wei.saturating_sub(actual);
        let wrapped = self.pool_manager.wrapped_native();
        if let Some(wn) = wrapped {
            let balance = self.wallet.token_balances.entry(wn).or_insert(U256::ZERO);
            *balance = balance.saturating_add(actual);
        }
    }

    /// Helper: transfer wrapped-native token balance back to native balance.
    fn unwrap_native(&mut self, amount: U256) {
        let wrapped = self.pool_manager.wrapped_native();
        if let Some(wn) = wrapped {
            let current = self.wallet.token_balances.get(&wn).copied().unwrap_or(U256::ZERO);
            let actual = amount.min(current);
            if actual == U256::ZERO { return; }
            let balance = self.wallet.token_balances.entry(wn).or_insert(U256::ZERO);
            *balance = balance.saturating_sub(actual);
            self.wallet.native_balance_wei = self.wallet.native_balance_wei.saturating_add(actual);
        }
    }

    /// Execute a virtual trade using wallet state (simulation only, no on-chain tx).
    async fn execute_virtual_trade(&mut self, opp: MevOpportunity) {
        let profit = opp.expected_profit;
        let gas_cost = U256::from(opp.gas_cost_wei);

        if profit < self.config.min_profit_threshold_wei {
            return;
        }
        if self.wallet.native_balance_wei < gas_cost {
            return;
        }

        self.wallet.native_balance_wei = self.wallet.native_balance_wei.saturating_sub(gas_cost);
        self.wallet.total_gas_spent_wei = self.wallet.total_gas_spent_wei.saturating_add(opp.gas_cost_wei);

        let output_amount = opp.input_amount.saturating_add(profit);

        if opp.token_in != Address::ZERO {
            let balance = self.wallet.token_balances.entry(opp.token_in).or_insert(U256::ZERO);
            *balance = balance.saturating_sub(opp.input_amount);
        }
        if opp.token_out != Address::ZERO {
            let balance = self.wallet.token_balances.entry(opp.token_out).or_insert(U256::ZERO);
            *balance = balance.saturating_add(output_amount);
        }

        self.wallet.native_balance_wei = self.wallet.native_balance_wei.saturating_add(profit);
        self.wallet.total_profit_wei = self.wallet.total_profit_wei.saturating_add(profit);

        if profit > self.best_trade_profit {
            self.best_trade_profit = profit;
            let native_unit = U256::from(1_000_000_000_000_000_000u128);
            let profit_whole = profit / native_unit;
            let profit_frac = (profit % native_unit) / U256::from(10_000_000_000_000_000u128);
            self.best_trade_desc = Some(format!(
                "+{}.{:04} native ({} @ {})",
                profit_whole,
                profit_frac,
                opp.strategy,
                opp.block_number,
            ));
        }

        *self.mempool_strategy_counts.entry(opp.strategy).or_insert(0) += 1;

        self.wallet.successful_executions += 1;
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
            success: true,
            opportunity: opp,
        };

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

    /// Format strategy counts map into a compact string like "two_hop:3 multi:1"
    fn fmt_strategy_counts(counts: &HashMap<Strategy, u64>) -> String {
        if counts.is_empty() {
            return "none".to_string();
        }
        let mut parts: Vec<String> = counts
            .iter()
            .filter(|(_, &c)| c > 0)
            .map(|(s, c)| format!("{}:{}", s, c))
            .collect();
        parts.sort();
        parts.join(" ")
    }

    /// Print the live dashboard.
    fn print_dashboard(&self, settled_blocks: u64, mempool_scans: u64) {
        let (native_whole, native_frac) = self.format_native_balance(self.wallet.native_balance_wei);
        let pnl = self.format_pnl(self.wallet.native_balance_wei, self.wallet.initial_native_balance_wei);

        println!("\n=== MEV Scout - Live Mode ===");
        println!("Chain: {} | Block: {} | Pending: {} txs",
            self.config.chain_display_name,
            self.last_processed_block,
            self.total_mempool_txs_seen,
        );
        println!("Settled blks:   {} | Mempool scans: {}", settled_blocks, mempool_scans);
        println!("----------------------------------------");
        println!("Native Balance: {}.{:04} native", native_whole, native_frac);
        println!("Tokens Held:    {} tokens", self.wallet.token_balances.len());
        if self.wallet.total_executions > 0 {
            println!("Executions:     {} ({} ok, {} fail)",
                self.wallet.total_executions,
                self.wallet.successful_executions,
                self.wallet.failed_executions,
            );
            let settled_strats = Self::fmt_strategy_counts(&self.settled_strategy_counts);
            let mempool_strats = Self::fmt_strategy_counts(&self.mempool_strategy_counts);
            println!("  Settled:      {} | {}", settled_blocks, settled_strats);
            println!("  Mempool:      {} | {}", mempool_scans, mempool_strats);
        }
        println!("Total Profit:   {}", pnl);
        let (gas_whole, gas_frac) = self.format_native_balance(U256::from(self.wallet.total_gas_spent_wei));
        println!("Total Gas:      {}.{:04} native", gas_whole, gas_frac);
        println!("Net P&L:        {}", pnl);
        if let Some(ref best) = self.best_trade_desc {
            println!("Best Trade:     {}", best);
        }
    }

    fn format_native_balance(&self, wei: U256) -> (U256, U256) {
        let native_unit = U256::from(1_000_000_000_000_000_000u128);
        let whole = wei / native_unit;
        let frac = (wei % native_unit) / U256::from(10_000_000_000_000_000u128);
        (whole, frac)
    }

    fn format_pnl(&self, current: U256, initial: U256) -> String {
        let native_unit = U256::from(1_000_000_000_000_000_000u128);
        if initial > U256::ZERO {
            let raw = current.saturating_sub(initial);
            let pct = raw * U256::from(10000) / initial;
            format!(
                "{:+}.{:04} native ({:+}.{:02}%)",
                raw / native_unit,
                (raw % native_unit) / U256::from(10_000_000_000_000u128),
                pct / U256::from(100),
                pct % U256::from(100),
            )
        } else {
            "0.0000 native".to_string()
        }
    }

    /// Print final summary on shutdown.
    fn print_summary(&self, start_time: Instant, settled_blocks: u64, mempool_scans: u64) {
        let elapsed = start_time.elapsed();
        let secs = elapsed.as_secs();
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        let secs_rem = secs % 60;

        let net_pnl = self.format_pnl(self.wallet.native_balance_wei, self.wallet.initial_native_balance_wei);

        let settled_strats = Self::fmt_strategy_counts(&self.settled_strategy_counts);
        let mempool_strats = Self::fmt_strategy_counts(&self.mempool_strategy_counts);

        let settled_with_opps: u64 = self.settled_strategy_counts.values().copied().sum();

        let (init_whole, init_frac) = self.format_native_balance(self.wallet.initial_native_balance_wei);
        let (final_whole, final_frac) = self.format_native_balance(self.wallet.native_balance_wei);
        let (gas_whole, gas_frac) = self.format_native_balance(U256::from(self.wallet.total_gas_spent_wei));

        println!("\n=== MEV Scout - Live Mode (shutdown) ===");
        println!("Runtime:          {}h {}m {}s", hours, mins, secs_rem);
        println!("Settled blocks:   {} ({} with opps)", settled_blocks, settled_with_opps);
        println!("Mempool scans:    {}", mempool_scans);
        println!("Total txs seen:   {}", self.total_mempool_txs_seen);
        println!("----------------------------------------");
        println!("Initial Balance:  {}.{:04} native", init_whole, init_frac);
        println!("Final Balance:    {}.{:04} native", final_whole, final_frac);
        println!("Net P&L:          {}", net_pnl);
        println!("Total Executions: {} ({} ok, {} fail)",
            self.wallet.total_executions,
            self.wallet.successful_executions,
            self.wallet.failed_executions,
        );
        if self.wallet.total_executions > 0 {
            let avg_profit = self.wallet.total_profit_wei / U256::from(self.wallet.total_executions);
            let (avg_whole, avg_frac) = self.format_native_balance(avg_profit);
            println!("  Settled:        {} ({})", settled_with_opps, settled_strats);
            println!("  Mempool:        {} ({})", self.wallet.total_executions - settled_with_opps, mempool_strats);
            println!("Avg Profit/Trade: {}.{:04} native", avg_whole, avg_frac);
            if let Some(ref best) = self.best_trade_desc {
                println!("Best Trade:      {}", best);
            }
        }
        println!("Total Gas Spent:  {}.{:04} native", gas_whole, gas_frac);
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
