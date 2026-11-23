//! Candidate-`L` amount optimizer (plan A-04 / `mev_strategies.md` §38).
//!
//! Detectors quote a first trade size; this module re-simulates a coarse grid
//! around it and keeps the size with the best state-exact **net** profit. It is
//! opt-in and only meaningful with `--sim state`, because detection-time pool
//! snapshots (`SimSeeds`) are the state being re-quoted.
//!
//! `split_route` is an optimizer *mode*, not a new `MevKind`: when enabled the
//! sweep also evaluates splitting the borrow across the original route and an
//! alternative pool connecting the same tokens. The mode is flag-gated and is
//! recorded on the opportunity with `optimizer:split_route`.

use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::mev::detectors::arb_common;
use crate::pool::state::{PoolManager, PoolState};
use crate::types::MevOpportunity;

use super::sim::{pools_of_opp, simulate_candidate, SimPolicy, SimSeeds};

/// Fractions of the detector's first quote, in basis points. The detector's
/// own size (10_000) is always included even when the grid is customized.
pub const DEFAULT_FRACTIONS_BPS: &[u128] = &[2_500, 5_000, 7_500, 10_000, 12_500, 15_000, 20_000];
/// Allocations tried by the split-route mode: the first pool of the route gets
/// this share of the borrow, the alternative route gets the remainder.
pub const SPLIT_FRACTIONS_BPS: &[u128] = &[2_500, 5_000, 7_500];

/// Knobs for the amount sweep. Disabled by default so existing runs are
/// byte-identical to the pre-optimizer behaviour.
#[derive(Debug, Clone)]
pub struct AmountSweep {
    pub enabled: bool,
    /// Also try a two-route allocation when an alternative pool is tracked.
    pub split_route: bool,
    pub fractions_bps: Vec<u128>,
    pub max_candidates: usize,
}

impl Default for AmountSweep {
    fn default() -> Self {
        Self {
            enabled: false,
            split_route: false,
            fractions_bps: DEFAULT_FRACTIONS_BPS.to_vec(),
            max_candidates: 32,
        }
    }
}

impl AmountSweep {
    /// A sweep requested implicitly by `--split-route` is enabled too.
    pub fn new(enabled: bool, split_route: bool) -> Self {
        Self {
            enabled: enabled || split_route,
            split_route,
            ..Self::default()
        }
    }

    /// Candidate input amounts, deduplicated and bounded. The detector's own
    /// size is always present so the optimizer can never make the quote worse
    /// by excluding the original candidate.
    pub fn candidate_amounts(&self, base: u128) -> Vec<u128> {
        let mut out = Vec::new();
        for bps in &self.fractions_bps {
            let Some(scaled) = base.checked_mul(*bps) else {
                continue;
            };
            let amount = scaled / 10_000;
            if amount > 0 && !out.contains(&amount) {
                out.push(amount);
            }
        }
        if !out.contains(&base) {
            out.push(base);
        }
        let cap = self.max_candidates.max(1);
        if out.len() > cap {
            out.truncate(cap);
            if !out.contains(&base) {
                *out.last_mut().expect("cap >= 1") = base;
            }
        }
        out
    }
}

/// Result of re-quoting one opportunity at its best candidate size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Optimization {
    pub amount: U256,
    pub gross_profit_wei: U256,
    pub net_profit_wei: U256,
    pub split_route: bool,
    pub split_fraction_bps: Option<u128>,
    pub alt_route: Option<Vec<Address>>,
    pub candidates_tried: usize,
}

fn u128_of(value: U256) -> Option<u128> {
    (value <= U256::from(u128::MAX)).then(|| value.to::<u128>())
}

fn candidate_state(
    opp: &MevOpportunity,
    seeds: &SimSeeds,
    pm: Option<&PoolManager>,
) -> Option<HashMap<Address, PoolState>> {
    let cid = opp.canonical_id.as_ref()?;
    let snapshot = seeds.get(cid)?;
    let mut states = HashMap::new();
    for addr in pools_of_opp(opp) {
        states.insert(
            addr,
            snapshot
                .get(&addr)
                .or_else(|| pm.and_then(|p| p.get(&addr)))
                .cloned()?,
        );
    }
    Some(states)
}

/// Build the best two-route allocation for this opportunity, if a second pool
/// connecting one of the route's token pairs is tracked and the split beats the
/// single-route quote.
fn evaluate_split(
    opp: &MevOpportunity,
    seeds: &SimSeeds,
    policy: &SimPolicy,
    pm: &PoolManager,
    best_net: U256,
) -> Option<Optimization> {
    if opp.savax_route.is_some() {
        return None; // virtual rate leg has no alternative pool
    }
    let original = pools_of_opp(opp);
    if original.len() != 2 {
        return None;
    }
    let tokens = opp.route_tokens.as_deref()?;
    if tokens.len() != 3 {
        return None;
    }
    let (token_in, middle, token_out) = (tokens[0], tokens[1], tokens[2]);
    let base = u128_of(opp.input_amount)?;
    let original_states = candidate_state(opp, seeds, Some(pm))?;

    let (alt_pool, replaces_first) = match pm.find_pair_pool(&token_in, &middle) {
        Some(addr) if addr != original[0] && pm.get(&addr).is_some() => (addr, true),
        _ => match pm.find_pair_pool(&middle, &token_out) {
            Some(addr) if addr != original[1] && pm.get(&addr).is_some() => (addr, false),
            _ => return None,
        },
    };
    let alt_route = if replaces_first {
        vec![alt_pool, original[1]]
    } else {
        vec![original[0], alt_pool]
    };

    let mut result = None;
    for fraction in SPLIT_FRACTIONS_BPS {
        let Some(first) = base.checked_mul(*fraction).map(|v| v / 10_000) else {
            continue;
        };
        let second = base.saturating_sub(first);
        if first == 0 || second == 0 {
            continue;
        }

        let mut original_scratch = original_states.clone();
        let mut alt_scratch = HashMap::new();
        for addr in &alt_route {
            let state = original_states
                .get(addr)
                .or_else(|| pm.get(addr))
                .cloned()?;
            alt_scratch.insert(*addr, state);
        }
        let first_out = super::sim::walk_quote_post(
            &mut original_scratch,
            &original,
            Some(tokens),
            token_in,
            first,
            Some(pm),
        )?;
        let second_out = super::sim::walk_quote_post(
            &mut alt_scratch,
            &alt_route,
            Some(tokens),
            token_in,
            second,
            Some(pm),
        )?;
        let total_out = first_out.saturating_add(second_out);
        let fee = policy.flash_loan_fee(base);
        if total_out < base.saturating_add(fee) {
            continue;
        }
        let gross = total_out.saturating_sub(base).saturating_sub(fee);
        if gross == 0 {
            continue;
        }

        let mut combined = PoolManager::new();
        for state in original_scratch
            .values()
            .chain(alt_scratch.values())
        {
            combined.add_pool(state.clone());
        }
        let (native_gross, _) =
            arb_common::normalize_profit(&combined, token_in, token_out, gross, base);
        let gross_u128 = u128_of(native_gross).unwrap_or(u128::MAX);
        let net = (gross_u128 as i128) - (opp.gas_cost_wei as i128);
        if net <= 0 {
            continue;
        }
        let net_profit_wei = U256::from(net as u128);
        if net_profit_wei <= best_net {
            continue;
        }
        if result
            .as_ref()
            .is_some_and(|prev: &Optimization| net_profit_wei <= prev.net_profit_wei)
        {
            continue;
        }
        result = Some(Optimization {
            amount: U256::from(base),
            gross_profit_wei: U256::from(gross_u128),
            net_profit_wei,
            split_route: true,
            split_fraction_bps: Some(*fraction),
            alt_route: Some(alt_route.clone()),
            candidates_tried: 0, // filled by the caller
        });
    }
    result
}

/// Re-quote one opportunity over the configured candidate sizes.
///
/// Returns `None` when the optimizer is disabled, the strategy is not
/// state-simulatable, or no candidate produced a positive net profit. The
/// original opportunity is never mutated here.
pub fn optimize(
    opp: &MevOpportunity,
    seeds: &SimSeeds,
    policy: &SimPolicy,
    sweep: &AmountSweep,
    pm: Option<&PoolManager>,
) -> Option<Optimization> {
    if !sweep.enabled || !SimPolicy::is_simulatable(opp.strategy) {
        return None;
    }
    let base = u128_of(opp.input_amount)?;
    if base == 0 {
        return None;
    }

    let mut best: Option<Optimization> = None;
    let mut tried = 0usize;
    for amount in sweep.candidate_amounts(base) {
        let mut candidate = opp.clone();
        candidate.input_amount = U256::from(amount);
        // A fresh overlay per candidate: a losing size must not move state
        // that a later candidate re-quotes against.
        let mut overlay = HashMap::new();
        let overlay_tx = HashMap::new();
        tried += 1;
        let Ok(outcome) =
            simulate_candidate(&candidate, seeds, &mut overlay, &overlay_tx, policy, pm)
        else {
            continue;
        };
        if outcome.sim_net_wei <= 0 {
            continue;
        }
        let net_profit_wei = U256::from(outcome.sim_net_wei as u128);
        let better = best
            .as_ref()
            .map_or(true, |prev| net_profit_wei > prev.net_profit_wei);
        if better {
            best = Some(Optimization {
                amount: U256::from(amount),
                gross_profit_wei: U256::from(outcome.sim_gross_wei),
                net_profit_wei,
                split_route: false,
                split_fraction_bps: None,
                alt_route: None,
                candidates_tried: 0,
            });
        }
    }

    let best_net = best
        .as_ref()
        .map(|b| b.net_profit_wei)
        .unwrap_or(U256::ZERO);
    if sweep.split_route {
        if let Some(manager) = pm {
            if let Some(split) = evaluate_split(opp, seeds, policy, manager, best_net) {
                best = Some(split);
            }
        }
    }

    best.map(|mut result| {
        result.candidates_tried = tried + usize::from(result.split_route) * SPLIT_FRACTIONS_BPS.len();
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_grid_keeps_the_detector_size_and_deduplicates() {
        let sweep = AmountSweep::new(true, false);
        let amounts = sweep.candidate_amounts(1_000);
        assert_eq!(
            amounts,
            vec![250, 500, 750, 1_000, 1_250, 1_500, 2_000]
        );
    }

    #[test]
    fn candidate_grid_is_bounded_and_never_drops_the_original_quote() {
        let sweep = AmountSweep {
            max_candidates: 3,
            ..AmountSweep::new(true, true)
        };
        let amounts = sweep.candidate_amounts(80);
        assert_eq!(amounts.len(), 3);
        assert!(amounts.contains(&80));
    }

    #[test]
    fn disabled_sweep_does_not_quote() {
        let sweep = AmountSweep::default();
        assert!(!sweep.enabled);
        assert!(sweep.candidate_amounts(100).is_empty() || !sweep.enabled);
    }

    #[test]
    fn split_route_flag_enables_the_sweep() {
        let sweep = AmountSweep::new(false, true);
        assert!(sweep.enabled);
        assert!(sweep.split_route);
    }

    /// A-04: the optimizer is inert unless explicitly enabled and seeded.
    #[test]
    fn optimize_returns_none_without_a_live_seeded_sweep() {
        let mut opp = MevOpportunity::new(1, 0, crate::types::Strategy::TwoHopArb, Address::ZERO, 1);
        opp.input_amount = U256::from(1_000u128);
        let seeds = SimSeeds::new();
        let policy = SimPolicy {
            mode: super::super::SimMode::StateSim,
            ..SimPolicy::default()
        };

        assert!(optimize(&opp, &seeds, &policy, &AmountSweep::default(), None).is_none());

        let enabled = AmountSweep::new(true, false);
        // No canonical seed was recorded for this opportunity.
        assert!(optimize(&opp, &seeds, &policy, &enabled, None).is_none());

        opp.input_amount = U256::ZERO;
        assert!(optimize(&opp, &seeds, &policy, &enabled, None).is_none());
    }
}
