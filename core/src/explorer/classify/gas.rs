//! Single gas-cost formula for every classifier pass (Task B1).
use alloy::primitives::U256;

use super::TxInput;

/// Gas cost in wei for a tx: gas_used × effective gas price.
///
/// Rounds the f64 gwei price to the nearest wei before multiplying, so
/// fractional-gwei prices are not silently truncated (the pre-B1 `as u128`
/// understated every gas figure by up to ~1 gwei — ~1.7% at 30.5 gwei).
pub(super) fn tx_gas_cost_wei(tx: &TxInput) -> U256 {
    let price_wei = (tx.effective_gas_price_gwei * 1e9).round() as u128;
    U256::from(tx.gas_used).saturating_mul(U256::from(price_wei))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::test_fixtures::tx;

    #[test]
    fn rounds_fractional_gwei_instead_of_truncating() {
        let mut t = tx(0, alloy::primitives::Address::ZERO, true, vec![], vec![]);
        t.gas_used = 1_000;
        t.effective_gas_price_gwei = 30.5;
        // 30.5 gwei = 30_500_000_000 wei exactly; truncation would give 30 gwei.
        assert_eq!(
            tx_gas_cost_wei(&t),
            U256::from(1_000u64) * U256::from(30_500_000_000u128)
        );

        t.effective_gas_price_gwei = 30.4;
        // 30.4 rounds to 30 gwei at wei precision (30_400_000_000).
        assert_eq!(
            tx_gas_cost_wei(&t),
            U256::from(1_000u64) * U256::from(30_400_000_000u128)
        );
    }
}
