//! Shared detection context passed into arb/backrun/JIT detectors.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::pool::state::{PoolManager, ScanScope};
use crate::types::GasConfig;

/// Block/tx plumbing shared by two-hop, multi-hop, backrun, and JIT detectors.
#[derive(Clone, Copy)]
pub struct DetectCtx<'a> {
    pub pool_manager: &'a PoolManager,
    pub tx_index: usize,
    pub timestamp: u64,
    pub base_fee_per_gas: u128,
    pub gas_config: GasConfig,
    pub scope: &'a ScanScope<'a>,
}

impl<'a> DetectCtx<'a> {
    pub fn new(
        pool_manager: &'a PoolManager,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
        scope: &'a ScanScope<'a>,
    ) -> Self {
        Self {
            pool_manager,
            tx_index,
            timestamp,
            base_fee_per_gas,
            gas_config,
            scope,
        }
    }
}
