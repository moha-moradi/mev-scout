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

    /// Estimate the next block's base fee using EIP-1559 dynamics.
    ///
    /// When `gas_used > gas_limit / 2`, base fee increases by up to 12.5%.
    /// When `gas_used < gas_limit / 2`, base fee decreases by up to 12.5%.
    /// The adjustment scales linearly with how far usage is from the target.
    pub fn forecast_base_fee(&self, current_base_fee: u128) -> u128 {
        let (_, last_ratio) = match self.base_fees.last() {
            Some(v) => *v,
            None => return current_base_fee,
        };
        let target = 0.5;
        if last_ratio > target {
            let excess = ((last_ratio - target) / target).min(1.0);
            let bump = (current_base_fee as f64 * excess * 0.125) as u128;
            current_base_fee.saturating_add(bump.max(1))
        } else if last_ratio < target {
            let deficit = ((target - last_ratio) / target).min(1.0);
            let drop = (current_base_fee as f64 * deficit * 0.125) as u128;
            current_base_fee.saturating_sub(drop)
        } else {
            current_base_fee
        }
    }

    /// Clear all tracked data.
    pub fn clear(&mut self) {
        self.prices.clear();
        self.base_fees.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_distribution() {
        let dist = GasPriceDistribution::new(50);
        assert!(dist.percentile(50).is_none());
        assert_eq!(dist.forecast_base_fee(100), 100);
    }

    #[test]
    fn test_single_price() {
        let mut dist = GasPriceDistribution::new(50);
        dist.add_tx_gas_price(100);
        dist.finalize_block();
        assert_eq!(dist.percentile(50), Some(100));
        assert_eq!(dist.percentile(0), Some(100));
        assert_eq!(dist.percentile(100), Some(100));
    }

    #[test]
    fn test_multiple_prices_percentile() {
        let mut dist = GasPriceDistribution::new(50);
        for i in [10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100] {
            dist.add_tx_gas_price(i);
        }
        dist.finalize_block();
        // With 10 prices: p0=10, p50 ≈ 55, p90 ≈ 91, p100=100
        let p50 = dist.percentile(50).unwrap();
        assert!(p50 >= 50 && p50 <= 60, "p50={p50} should be ~55");
        let p90 = dist.percentile(90).unwrap();
        assert!(p90 >= 90, "p90={p90} should be >=90");
        assert_eq!(dist.percentile(100), Some(100));
    }

    #[test]
    fn test_block_recording() {
        let mut dist = GasPriceDistribution::new(10);
        dist.record_block(50, 15_000_000, 30_000_000);
        dist.record_block(55, 20_000_000, 30_000_000);
        assert_eq!(dist.base_fees.len(), 2);
    }

    #[test]
    fn test_forecast_base_fee_increase() {
        let mut dist = GasPriceDistribution::new(10);
        // gas_used 25M / gas_limit 30M = 0.83 → above 0.5 target → base fee increases
        dist.record_block(100, 25_000_000, 30_000_000);
        let forecast = dist.forecast_base_fee(100);
        assert!(forecast > 100, "base fee should increase, got {forecast}");
        assert!(forecast <= 113, "max 12.5% increase, got {forecast}");
    }

    #[test]
    fn test_forecast_base_fee_decrease() {
        let mut dist = GasPriceDistribution::new(10);
        // gas_used 10M / gas_limit 30M = 0.33 → below 0.5 target → base fee decreases
        dist.record_block(100, 10_000_000, 30_000_000);
        let forecast = dist.forecast_base_fee(100);
        assert!(forecast < 100, "base fee should decrease, got {forecast}");
    }

    #[test]
    fn test_finalize_block_trims() {
        let mut dist = GasPriceDistribution::new(1);
        // Push enough to trigger trim (max_blocks rounds up to 10, cap = 2000)
        for i in 0..3000 {
            dist.add_tx_gas_price(i);
        }
        dist.finalize_block();
        assert!(dist.prices.len() <= 2000, "got {} expected <=2000", dist.prices.len());
    }
}
