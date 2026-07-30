pub mod aggregate;
pub mod gas;
pub mod runner;
pub mod scanner;
pub use aggregate::{aggregate, aggregate_with_prices, AggregationResult, DexMeta, DexMetrics, StrategyMetrics, SummaryMetrics};
pub use gas::GasPriceDistribution;
pub use runner::{add_pool_to_manager, BacktestRunner};
pub use scanner::{topics, ActivityScanner};

use serde::{Deserialize, Serialize};

/// Per-block stats collected during a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockReplayStats {
    pub block_number: u64,
    pub total_tx_count: usize,
    pub dex_tx_count: usize,
    pub pending_tx_count: usize,
    pub mempool_opp_count: usize,
}
