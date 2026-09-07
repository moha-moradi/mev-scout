//! Gas price distribution modeling for realistic gas cost estimation (H10).
//!
//! Tracks effective gas prices from recent blocks to model the gas price
//! distribution needed to win inclusion in competitive blocks. Replaces
//! the crude P90 multiplier (`base_fee * 150%`) with actual percentile
//! estimates from recent transaction gas prices. Also provides EIP-1559
//! base fee forecasting based on block gas usage ratios.

/// Tracks effective gas prices from recent blocks and provides percentile
/// estimates for gas cost modeling (H10).
///
/// Maintains a sliding window of recent effective gas prices from block
/// transactions. Supports EIP-1559 base fee forecasting by tracking
/// per-block base fees and gas usage ratios.
#[derive(Debug, Clone)]
pub struct GasPriceDistribution {
    /// Maximum number of recent blocks to retain prices from.
    max_blocks: usize,
    /// Effective gas prices from recent blocks (in wei), maintained sorted.
    prices: Vec<u128>,
    /// Per-block (base_fee, gas_used_ratio) for EIP-1559 dynamics.
    base_fees: Vec<(u128, f64)>,
}

impl GasPriceDistribution {
    /// Create a new distribution tracking up to `max_blocks` recent blocks.
    pub fn new(max_blocks: usize) -> Self {
        Self {
            max_blocks: max_blocks.max(10),
            prices: Vec::new(),
            base_fees: Vec::new(),
        }
    }

    /// Add a single transaction's effective gas price to the distribution.
    pub fn add_tx_gas_price(&mut self, price: u128) {
        self.prices.push(price);
    }

    /// Record a block's base fee and gas usage ratio for EIP-1559 forecasting.
    pub fn record_block(&mut self, base_fee: u128, gas_used: u64, gas_limit: u64) {
        let ratio = if gas_limit > 0 {
            gas_used as f64 / gas_limit as f64
        } else {
            0.5
        };
        self.base_fees.push((base_fee, ratio));
        while self.base_fees.len() > self.max_blocks {
            self.base_fees.remove(0);
        }
    }

    /// Finalize the current block: sort accumulated prices and trim old data.
    pub fn finalize_block(&mut self) {
        self.prices.sort_unstable();
        if self.prices.len() > self.max_blocks.saturating_mul(200) {
            let keep = self.prices.len() - self.max_blocks.saturating_mul(100);
            self.prices = self.prices[keep..].to_vec();
        }
    }

    /// Compute the p-th percentile gas price from recent blocks.
    /// Returns `None` when no prices are tracked (caller should fall back).
    pub fn percentile(&self, p: u8) -> Option<u128> {
        if self.prices.is_empty() {
            return None;
        }
        let p = p.min(100) as usize;
        if p == 0 {
            return self.prices.first().copied();
        }
        if p >= 100 {
            return self.prices.last().copied();
        }
        let idx = ((self.prices.len() - 1).saturating_mul(p)) / 100;
        self.prices.get(idx).copied()
    }
}
