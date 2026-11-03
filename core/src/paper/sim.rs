//! State-exact paper simulation — re-quote fills against pool snapshots.
//!
//! Under `--sim assumed` this module is unused: [`LedgerPolicy::apply`] credits
//! detector claims directly. Under `--sim state`, [`LedgerPolicy::apply_simulated`]
//! walks candidate legs over a block overlay and enforces flash-loan repayment.
use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::mev::detectors::arb_common;
use crate::pool::state::{apply_post_state, quote_swap_post, PoolManager, PoolState};
use crate::types::{MevOpportunity, Strategy};

use super::types::{FillSkipReason, SimMode};

/// Knobs for state-sim (from `[paper]` + CLI).
#[derive(Debug, Clone, Copy)]
pub struct SimPolicy {
    pub mode: SimMode,
    /// When true, enforce `out ≥ borrowed + flash_loan_fee(borrowed)`.
    pub flash_loan: bool,
    /// Flash-loan fee in basis points (mirrors `FlashLoanProvider::fee_rate_bps`).
    pub fee_bps: u128,
    /// Seed budget per block in the runner (not the ledger fill cap).
    pub max_candidates_per_block: usize,
}

impl Default for SimPolicy {
    fn default() -> Self {
        Self {
            mode: SimMode::Assumed,
            flash_loan: true,
            fee_bps: 0,
            max_candidates_per_block: 32,
        }
    }
}

impl SimPolicy {
    pub fn flash_loan_fee(&self, borrowed: u128) -> u128 {
        if !self.flash_loan || self.fee_bps == 0 {
            return 0;
        }
        borrowed.saturating_mul(self.fee_bps).saturating_div(10_000)
    }

    pub fn is_state_sim(self) -> bool {
        self.mode == SimMode::StateSim
    }

    /// Strategies that state-sim will re-quote. JIT is explicitly excluded.
    pub fn is_simulatable(strategy: Strategy) -> bool {
        matches!(
            strategy,
            Strategy::TwoHopArb | Strategy::MultiHopArb | Strategy::Backrun
        )
    }
}

/// Pool states a candidate was quoted against at detection time.
#[derive(Debug, Clone)]
pub struct PoolSnapshot {
    pub block_number: u64,
    pub tx_index: usize,
    pub pools: Vec<(Address, PoolState)>,
}

impl PoolSnapshot {
    pub fn get(&self, addr: &Address) -> Option<&PoolState> {
        self.pools.iter().find(|(a, _)| a == addr).map(|(_, s)| s)
    }
}

/// `canonical_id` → snapshot captured at emit time.
pub type SimSeeds = HashMap<String, PoolSnapshot>;

/// Outcome of re-quoting one candidate against the block overlay.
#[derive(Debug, Clone)]
pub struct SimOutcome {
    pub sim_gross_wei: u128,
    pub sim_net_wei: i128,
    pub borrowed_wei: u128,
    pub repay_ok: bool,
    pub state_drift: bool,
    /// Post-swap pool states to commit into the overlay on ledger accept.
    pub post_pools: Vec<(Address, PoolState)>,
}

/// Why a candidate could not be simulated into a positive fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimFail {
    NotQuotable,
    PathUnavailable,
    PriceConsumed,
    RepayShortfall,
    StrategyUnsupported,
}

impl SimFail {
    pub fn to_skip(self) -> FillSkipReason {
        match self {
            Self::NotQuotable => FillSkipReason::SimNotQuotable,
            Self::PathUnavailable => FillSkipReason::SimPathUnavailable,
            Self::PriceConsumed => FillSkipReason::SimPriceConsumed,
            Self::RepayShortfall => FillSkipReason::SimRepayShortfall,
            Self::StrategyUnsupported => FillSkipReason::SimStrategyUnsupported,
        }
    }
}

/// Pool addresses a candidate touches (path, else pool_a/pool_b).
pub fn pools_of_opp(opp: &MevOpportunity) -> Vec<Address> {
    let mut pools = Vec::new();
    if let Some(path) = &opp.path {
        for p in path {
            if !p.is_zero() && !pools.contains(p) {
                pools.push(*p);
            }
        }
    }
    if pools.is_empty() {
        if !opp.pool_a.is_zero() {
            pools.push(opp.pool_a);
        }
        if !opp.pool_b.is_zero() && !pools.contains(&opp.pool_b) {
            pools.push(opp.pool_b);
        }
    }
    pools
}

/// Capture a snapshot of `opp`'s pools from `pm` (call after canonical_id is set).
pub fn capture_snapshot(pm: &PoolManager, opp: &MevOpportunity) -> Option<(String, PoolSnapshot)> {
    let cid = opp.canonical_id.as_ref()?.clone();
    let addrs = pools_of_opp(opp);
    if addrs.is_empty() {
        return None;
    }
    let mut pools = Vec::with_capacity(addrs.len());
    for addr in &addrs {
        let state = pm.get(addr)?.clone();
        pools.push((*addr, state));
    }
    Some((
        cid,
        PoolSnapshot {
            block_number: opp.block_number,
            tx_index: opp.tx_index,
            pools,
        },
    ))
}

/// Record seeds for simulatable opps into `seeds`, capped per block.
pub fn record_seeds_for_block(
    seeds: &mut SimSeeds,
    pm: &PoolManager,
    opps: &[MevOpportunity],
    max_per_block: usize,
) {
    let mut per_block: HashMap<u64, usize> = HashMap::new();
    for opp in opps {
        if !SimPolicy::is_simulatable(opp.strategy) {
            continue;
        }
        let count = per_block.entry(opp.block_number).or_insert(0);
        if *count >= max_per_block {
            continue;
        }
        if let Some((cid, snap)) = capture_snapshot(pm, opp) {
            if seeds.insert(cid, snap).is_none() {
                *count += 1;
            }
        }
    }
}

fn next_token(pool: &PoolState, token_in: Address) -> Option<Address> {
    match pool {
        PoolState::Curve(c) => c.token_index.keys().filter(|k| **k != token_in).min().copied(),
        PoolState::Balancer(b) => b
            .token_index
            .keys()
            .filter(|k| **k != token_in)
            .min()
            .copied(),
        _ => {
            let (t0, t1) = pool.token_pair();
            if t0 == token_in {
                Some(t1)
            } else if t1 == token_in {
                Some(t0)
            } else {
                None
            }
        }
    }
}

/// Walk `path` mutating `overlay` via [`quote_swap_post`].
///
/// Returns final output amount, or `None` if a leg is missing / unquotable.
pub fn walk_quote_post(
    overlay: &mut HashMap<Address, PoolState>,
    path: &[Address],
    token_in: Address,
    amount: u128,
) -> Option<u128> {
    let mut current = amount;
    let mut current_token = token_in;
    for addr in path {
        let pool = overlay.get_mut(addr)?;
        let token_out = next_token(pool, current_token)?;
        let (out, post) = quote_swap_post(pool, current_token, current, token_out)?;
        apply_post_state(pool, &post);
        current = out;
        current_token = token_out;
    }
    Some(current)
}

fn ensure_pools_seeded(
    overlay: &mut HashMap<Address, PoolState>,
    overlay_tx: &HashMap<Address, usize>,
    snapshot: &PoolSnapshot,
    path: &[Address],
) -> Result<bool, SimFail> {
    let mut drifted = false;
    for addr in path {
        if overlay.contains_key(addr) {
            if let Some(written_tx) = overlay_tx.get(addr) {
                if *written_tx != snapshot.tx_index {
                    drifted = true;
                }
            }
            continue;
        }
        let state = snapshot
            .get(addr)
            .ok_or(SimFail::PathUnavailable)?
            .clone();
        overlay.insert(*addr, state);
    }
    Ok(drifted)
}

/// Re-quote one candidate against the block overlay.
///
/// Does **not** commit post-swap state into `overlay` — the ledger writes
/// [`SimOutcome::post_pools`] only when the fill is accepted.
pub fn simulate_candidate(
    opp: &MevOpportunity,
    seeds: &SimSeeds,
    overlay: &mut HashMap<Address, PoolState>,
    overlay_tx: &HashMap<Address, usize>,
    policy: &SimPolicy,
) -> Result<SimOutcome, SimFail> {
    if !SimPolicy::is_simulatable(opp.strategy) {
        return Err(SimFail::StrategyUnsupported);
    }
    let cid = opp.canonical_id.as_ref().ok_or(SimFail::NotQuotable)?;
    let snapshot = seeds.get(cid).ok_or(SimFail::NotQuotable)?;
    let path = pools_of_opp(opp);
    if path.is_empty() {
        return Err(SimFail::PathUnavailable);
    }

    let state_drift = ensure_pools_seeded(overlay, overlay_tx, snapshot, &path)?;

    // Work on a scratch copy so a failed candidate does not mutate the overlay.
    let mut scratch: HashMap<Address, PoolState> = path
        .iter()
        .filter_map(|a| overlay.get(a).map(|s| (*a, s.clone())))
        .collect();
    if scratch.len() != path.len() {
        return Err(SimFail::PathUnavailable);
    }

    let borrowed = if opp.input_amount > U256::from(u128::MAX) {
        u128::MAX
    } else {
        opp.input_amount.to::<u128>()
    };
    if borrowed == 0 {
        return Err(SimFail::PathUnavailable);
    }

    let out = walk_quote_post(&mut scratch, &path, opp.token_in, borrowed)
        .ok_or(SimFail::PathUnavailable)?;

    let fee = policy.flash_loan_fee(borrowed);
    let repay_ok = out >= borrowed.saturating_add(fee);
    if policy.flash_loan && !repay_ok {
        return Err(SimFail::RepayShortfall);
    }

    let token_gross = out.saturating_sub(borrowed).saturating_sub(fee);
    if token_gross == 0 {
        return Err(SimFail::PriceConsumed);
    }

    let mut pm = PoolManager::new();
    for state in scratch.values() {
        pm.add_pool(state.clone());
    }
    let (native_gross, _) =
        arb_common::normalize_profit(&pm, opp.token_in, opp.token_out, token_gross, borrowed);
    let sim_gross = if native_gross > U256::from(u128::MAX) {
        u128::MAX
    } else {
        native_gross.to::<u128>()
    };
    let sim_net = sim_gross as i128 - opp.gas_cost_wei as i128;
    if sim_net <= 0 {
        return Err(SimFail::PriceConsumed);
    }

    let post_pools: Vec<(Address, PoolState)> =
        path.iter().filter_map(|a| scratch.remove(a).map(|s| (*a, s))).collect();

    Ok(SimOutcome {
        sim_gross_wei: sim_gross,
        sim_net_wei: sim_net,
        borrowed_wei: borrowed,
        repay_ok,
        state_drift,
        post_pools,
    })
}

/// Commit an accepted fill's post-states into the block overlay.
pub fn commit_outcome(
    overlay: &mut HashMap<Address, PoolState>,
    overlay_tx: &mut HashMap<Address, usize>,
    outcome: &SimOutcome,
    tx_index: usize,
) {
    for (addr, state) in &outcome.post_pools {
        overlay.insert(*addr, state.clone());
        overlay_tx.insert(*addr, tx_index);
    }
}

/// Merge `incoming` into `session` by canonical_id (first write wins).
pub fn merge_seeds(session: &mut SimSeeds, incoming: SimSeeds) {
    for (k, v) in incoming {
        session.entry(k).or_insert(v);
    }
}
