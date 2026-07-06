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
