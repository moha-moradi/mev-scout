use std::collections::{HashMap, VecDeque};
use alloy::primitives::Address;
use crate::types::MevOpportunity;
use crate::pool::state::{PoolManager, PoolState};
use crate::types::{GasConfig, Strategy};

/// Minimum relative price gap to consider an arbitrage opportunity significant.
const MIN_ARB_GAP: f64 = 0.005; // 0.5%

/// Detects MEV opportunities that span multiple consecutive blocks:
///
/// - **Persistent arbitrage**: A price gap between two pools sharing a token
///   that exists across multiple blocks without being captured. Confidence
///   increases with persistence.
///
/// - **Time-bandit**: A block's execution creates a more profitable pool state
///   than existed before, suggesting the sequencer could have reordered txs
///   to capture additional MEV.
pub struct CrossBlockDetector {
    window_size: usize,
    snapshots: VecDeque<BlockSnapshot>,
}

/// Lightweight pool price snapshot at a given block boundary.
#[allow(dead_code)]
struct BlockSnapshot {
    block_number: u64,
    /// pool_address -> price ratio (reserve1/reserve0 for V2,
    /// derived from sqrtPriceX96 for V3)
    prices: HashMap<Address, f64>,
}

impl CrossBlockDetector {
    /// Create a new detector with the given sliding window size.
    /// `window_size` is the number of consecutive blocks to retain for
    /// comparison (default 3).
    pub fn new(window_size: usize) -> Self {
        Self {
            window_size: window_size.max(2),
            snapshots: VecDeque::with_capacity(window_size.max(2)),
        }
    }

    /// Number of snapshots currently stored in the sliding window.
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// Record pool state after processing a block.
    /// This captures lightweight price data for all tracked pools.
    pub fn record_block(&mut self, block_number: u64, pm: &PoolManager) {
        let prices = Self::snapshot_prices(pm);
        if self.snapshots.len() >= self.window_size {
            self.snapshots.pop_front();
        }
        self.snapshots.push_back(BlockSnapshot { block_number, prices });
    }

    /// Snapshot current prices for all pools in the manager.
    fn snapshot_prices(pm: &PoolManager) -> HashMap<Address, f64> {
        let mut prices = HashMap::new();
        for addr in pm.pool_addresses() {
            if let Some(pool) = pm.get(&addr) {
                match pool {
                    PoolState::UniswapV2(v2) => {
                        if v2.reserve0 > 0 {
                            let price = v2.reserve1 as f64 / v2.reserve0 as f64;
                            prices.insert(addr, price);
                        }
                    }
                    PoolState::UniswapV3(v3) => {
                        // sqrt_price_x96 is a U256 — convert to f64 for comparison
                        let sqrt = v3.sqrt_price_x96.to::<u128>() as f64;
                        if sqrt > 0.0 {
                            // price = (sqrtPriceX96 / 2^96)^2 = price of token1/token0
                            let price = (sqrt * sqrt) / (2.0f64.powi(192));
                            prices.insert(addr, price);
                        }
                    }
                    _ => {}
                }
            }
        }
        prices
    }

    /// Run detection on the current sliding window.
    ///
    /// Returns opportunities for:
    /// 1. Persistent arb: same price gap seen across multiple blocks
    /// 2. Time-bandit: a price gap that widened compared to the previous snapshot
    pub fn detect(
        &self,
        block_number: u64,
        timestamp: u64,
        _gas_config: GasConfig,
    ) -> Vec<MevOpportunity> {
        let mut results = Vec::new();

        if self.snapshots.len() < 2 {
            return results;
        }
        let pairs = self.collect_arb_pairs();

        for &(pool_a, pool_b, token_in, token_out, price_gap) in &pairs {
            if price_gap < MIN_ARB_GAP {
                continue;
            }

            // Count how many consecutive snapshots show this gap
            let persistence = self.count_persistence(pool_a, pool_b, token_in, token_out);

            // Persistent arbitrage: gap seen in 2+ consecutive blocks
            if persistence >= 2 {
                let confidence = (persistence as f64 / self.window_size as f64).min(1.0);
                let mut opp = MevOpportunity::new(
                    block_number,
                    0,
                    Strategy::CrossBlockArb,
                    pool_a,
                    timestamp,
                );
                opp.pool_b = pool_b;
                opp.token_in = token_in;
                opp.token_out = token_out;
                opp.confidence = Some(confidence);
                results.push(opp);
            }

            // Time-bandit: gap widened vs previous snapshot
            if self.snapshots.len() >= 2 {
                let prev = &self.snapshots[self.snapshots.len() - 2];
                let prev_gap = self.compute_gap(prev, pool_a, pool_b, token_in, token_out);
                if let Some(prev_gap) = prev_gap {
                    if price_gap > prev_gap * 1.1 {
                        // Gap widened by >=10%
                        let confidence = 0.4; // inherently speculative
                        let mut opp = MevOpportunity::new(
                            block_number,
                            0,
                            Strategy::TimeBandit,
                            pool_a,
                            timestamp,
                        );
                        opp.pool_b = pool_b;
                        opp.token_in = token_in;
                        opp.token_out = token_out;
                        opp.confidence = Some(confidence);
                        results.push(opp);
                    }
                }
            }
        }

        results
    }

    /// Collect all pool pairs (from the current snapshot) that share a token,
    /// and compute the price gap between them.
    fn collect_arb_pairs(&self) -> Vec<(Address, Address, Address, Address, f64)> {
        let current = match self.snapshots.back() {
            Some(s) => s,
            None => return Vec::new(),
        };

        let mut pairs = Vec::new();
        let pool_addrs: Vec<Address> = current.prices.keys().copied().collect();

        for i in 0..pool_addrs.len() {
            for j in (i + 1)..pool_addrs.len() {
                let a = pool_addrs[i];
                let b = pool_addrs[j];
                let pa = current.prices[&a];
                let pb = current.prices[&b];

                // Compute gap between the two pools' prices
                let gap = if pa > 0.0 && pb > 0.0 {
                    (pa / pb).ln().abs()
                } else {
                    0.0
                };

                if gap > MIN_ARB_GAP {
                    // We don't know exact token_in/token_out without PoolManager,
                    // so compute direction from price ratio
                    let (token_in, token_out) = if pa > pb {
                        (b, a) // buy on B (cheaper), sell on A (more expensive)
                    } else {
                        (a, b)
                    };
                    pairs.push((a, b, token_in, token_out, gap));
                }
            }
        }

        pairs
    }

    /// Compute the price gap between two pools in a given snapshot.
    fn compute_gap(
        &self,
        snapshot: &BlockSnapshot,
        pool_a: Address,
        pool_b: Address,
        _token_in: Address,
        _token_out: Address,
    ) -> Option<f64> {
        let pa = snapshot.prices.get(&pool_a)?;
        let pb = snapshot.prices.get(&pool_b)?;
        if *pa > 0.0 && *pb > 0.0 {
            Some((pa / pb).ln().abs())
        } else {
            None
        }
    }

    /// Count how many consecutive snapshots (including current) show this gap.
    fn count_persistence(
        &self,
        pool_a: Address,
        pool_b: Address,
        token_in: Address,
        token_out: Address,
    ) -> usize {
        let mut count = 0;
        for snapshot in self.snapshots.iter().rev() {
            let gap = self.compute_gap(snapshot, pool_a, pool_b, token_in, token_out);
            match gap {
                Some(g) if g >= MIN_ARB_GAP => count += 1,
                _ => break,
            }
        }
        count
    }
}

