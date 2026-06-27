//! PGA (Priority Gas Auction) simulator.
//!
//! Models competition among searchers for MEV opportunities.
//! In a first-price sealed-bid auction for priority fee, the winning
//! searcher pays their bid × gas_used. This module estimates the
//! expected profit after accounting for the winning bid.

use alloy::primitives::U256;
use crate::types::MevOpportunity;

/// Parameters controlling the PGA simulation.
#[derive(Debug, Clone)]
pub struct PgaConfig {
    /// Mean number of competing searchers (Poisson λ). Default: 3.
    pub mean_competitors: f64,
    /// Competition intensity — fraction of gross profit that is
    /// dissipated in the auction. Higher values mean more aggressive
    /// bidding. Range [0, 1]. Default: 0.5.
    pub intensity: f64,
}

impl Default for PgaConfig {
    fn default() -> Self {
        Self {
            mean_competitors: 3.0,
            intensity: 0.5,
        }
    }
}

impl PgaConfig {
    /// Create a new config with the given mean competitors and intensity.
    pub fn new(mean_competitors: f64, intensity: f64) -> Self {
        Self { mean_competitors, intensity }
    }
}

/// Simulate a PGA and return the expected profit after the winning bid.
///
/// Model: N ∼ Poisson(λ) competitors each with private valuation
/// uniformly distributed on [0, V] where V = gross_surplus / gas_used.
/// In equilibrium of a first-price auction, each bids b(v) = (N-1)/N · v.
/// The expected surplus for the winner = V / (N+1).
///
/// When `gross_surplus <= gas_cost_wei`, returns 0 (no searcher would compete).
pub fn simulate_pga(gross_surplus: U256, gas_cost_wei: u128, config: &PgaConfig) -> U256 {
    let gross = gross_surplus.to::<u128>();
    if gross <= gas_cost_wei {
        return U256::ZERO;
    }
    let net_surplus = gross - gas_cost_wei;

    // Expected number of competitors (round to nearest integer for the formula)
    let n = config.mean_competitors.round() as u128;
    let n = n.max(1); // at least 1 competitor (ourselves)

    // Expected surplus after auction = net_surplus / (n + 1) × intensity factor
    // With intensity=0: full surplus captured (no competition)
    // With intensity=1: full dissipation model
    let dissipated = net_surplus / (n + 1);
    let kept = net_surplus.saturating_sub(
        (dissipated as f64 * config.intensity) as u128,
    );

    U256::from(kept)
}

/// Apply PGA adjustment to a single opportunity.
/// Sets `pga_adjusted_profit` on the opportunity.
pub fn adjust_opportunity(mut opp: MevOpportunity, config: &PgaConfig) -> MevOpportunity {
    let adjusted = simulate_pga(opp.expected_profit, opp.gas_cost_wei, config);
    opp.pga_adjusted_profit = Some(adjusted);
    opp
}

/// Apply PGA adjustment to a batch of opportunities.
pub fn adjust_opportunities(opps: Vec<MevOpportunity>, config: &PgaConfig) -> Vec<MevOpportunity> {
    opps.into_iter()
        .map(|opp| adjust_opportunity(opp, config))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    #[test]
    fn test_simulate_pga_high_profit() {
        let profit = U256::from(1_000_000u128);
        let gas = 100_000;
        let config = PgaConfig::default();
        let result = simulate_pga(profit, gas, &config);
        // Net surplus = 900_000. With n=3, dissipated = 900_000/4 = 225_000
        // kept = 900_000 - 225_000 * 0.5 = 900_000 - 112_500 = 787_500
        assert!(result > U256::ZERO);
        assert!(result < profit);
    }

    #[test]
    fn test_simulate_pga_no_profit() {
        let result = simulate_pga(U256::from(1_000), 100_000u128, &PgaConfig::default());
        assert_eq!(result, U256::ZERO);
    }

    #[test]
    fn test_simulate_pga_zero_gas() {
        let config = PgaConfig::new(2.0, 1.0);
        let result = simulate_pga(U256::from(100_000), 0, &config);
        // net = 100_000, n = 2, dissipated = 100_000/3 ≈ 33_333, kept = 100_000 - 33_333 = 66_667
        assert_eq!(result, U256::from(66_667u128));
    }

    #[test]
    fn test_adjust_opportunity() {
        let opp = MevOpportunity {
            expected_profit: U256::from(10_000_000),
            gas_cost_wei: 1_000_000,
            ..MevOpportunity::new(1, 0, crate::types::Strategy::TwoHopArb, Default::default(), 0)
        };
        let config = PgaConfig::default();
        let adjusted = adjust_opportunity(opp, &config);
        assert!(adjusted.pga_adjusted_profit.is_some());
        let pga = adjusted.pga_adjusted_profit.unwrap();
        assert!(pga > U256::ZERO);
        assert!(pga < U256::from(10_000_000));
    }

    #[test]
    fn test_adjust_opportunities_batch() {
        let opps = vec![
            MevOpportunity::new(1, 0, crate::types::Strategy::TwoHopArb, Default::default(), 0),
            MevOpportunity::new(1, 1, crate::types::Strategy::MultiHopArb, Default::default(), 0),
        ];
        let adjusted = adjust_opportunities(opps, &PgaConfig::default());
        assert_eq!(adjusted.len(), 2);
        for opp in &adjusted {
            assert!(opp.pga_adjusted_profit.is_some());
        }
    }
}
