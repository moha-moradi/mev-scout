pub mod extraction;
pub mod profiler;
pub mod calibrator;
pub mod report;

pub use extraction::*;
pub use profiler::*;
pub use calibrator::*;
pub use report::*;

use alloy::primitives::Address;
use crate::data::{BlockData, ExecutedLog};
use crate::pool::state::PoolManager;
use crate::types::MevOpportunity;

/// Analyzes on-chain transactions during block replay to identify
/// competitor MEV extraction events.
///
/// Usage:
/// 1. Create with `CompetitionAnalyzer::new()`
/// 2. Call `process_tx()` for each transaction in the block
/// 3. Call `finalize_block()` after all txs to get `BlockCompetition`
pub struct CompetitionAnalyzer {
    txs: Vec<(usize, Address, u64, u128, Vec<ExecutedLog>)>,
}

impl CompetitionAnalyzer {
    pub fn new() -> Self {
        CompetitionAnalyzer {
            txs: Vec::new(),
        }
    }

    /// Record a transaction's data for later analysis.
    pub fn process_tx(
        &mut self,
        tx_index: usize,
        sender: Address,
        gas_used: u64,
        gas_effective: u128,
        logs: &[ExecutedLog],
        _pool_manager: &PoolManager,
    ) {
        self.txs.push((
            tx_index,
            sender,
            gas_used,
            gas_effective,
            logs.to_vec(),
        ));
    }

    /// Finalize the block: run extraction identification against all recorded txs.
    /// Clears the internal tx buffer after analysis.
    pub fn finalize_block(
        &mut self,
        block_number: u64,
        block_data: &BlockData,
        pool_manager: &PoolManager,
        opportunities: &[MevOpportunity],
    ) -> BlockCompetition {
        let result = extraction::analyze_block(
            block_number,
            &self.txs,
            pool_manager,
            opportunities,
            block_data.base_fee_per_gas.unwrap_or(0),
            block_data.coinbase,
        );
        self.txs.clear();
        result
    }
}
