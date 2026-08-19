use crate::pool::state::calldata_gas_estimate;

pub const BASE_TX_GAS: u64 = 40_000;
pub const DEFAULT_POOL_GAS: u64 = 80_000;
pub const JIT_OVERHEAD: u64 = 150_000;
pub const LIQUIDATION_GAS_LIMIT: u64 = 180_000;
/// Default flash loan gas overhead (Balancer V2, cheapest provider).
/// Covers flashLoanSimple() call, provider accounting, token transfer,
/// callback dispatch, and repayment. Real-world range: 150k-250k.
pub const FLASH_LOAN_OVERHEAD_GAS: u64 = 150_000;

pub fn estimate_base_gas(swap_count: usize) -> u64 {
    BASE_TX_GAS + calldata_gas_estimate(swap_count)
}

pub fn estimate_multi_swap_gas(swap_count: usize, pool_gases: &[u64]) -> u64 {
    pool_gases
        .iter()
        .fold(estimate_base_gas(swap_count), |acc, g| acc.saturating_add(*g))
}
