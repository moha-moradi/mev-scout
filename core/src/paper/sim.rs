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

/// Match key for detect-time seeds before `canonical_id` is stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeedMatchKey {
    pub strategy: Strategy,
    pub pool_a: Address,
    pub pool_b: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub block_number: u64,
    pub tx_index: usize,
}

impl SeedMatchKey {
    pub fn from_opp(opp: &MevOpportunity) -> Self {
        Self {
            strategy: opp.strategy,
            pool_a: opp.pool_a,
            pool_b: opp.pool_b,
            token_in: opp.token_in,
            token_out: opp.token_out,
            block_number: opp.block_number,
            tx_index: opp.tx_index,
        }
    }
}

/// Snapshot captured at detect time (before later txs mutate the manager).
#[derive(Debug, Clone)]
pub struct PendingSeed {
    pub key: SeedMatchKey,
    pub snap: PoolSnapshot,
}

fn snapshot_pools(pm: &PoolManager, opp: &MevOpportunity) -> Option<PoolSnapshot> {
    let addrs = pools_of_opp(opp);
    if addrs.is_empty() {
        return None;
    }
    let mut pools = Vec::with_capacity(addrs.len());
    for addr in &addrs {
        let state = pm.get(addr)?.clone();
        pools.push((*addr, state));
    }
    Some(PoolSnapshot {
        block_number: opp.block_number,
        tx_index: opp.tx_index,
        pools,
    })
}

/// Capture a snapshot of `opp`'s pools from `pm` (call after canonical_id is set).
pub fn capture_snapshot(pm: &PoolManager, opp: &MevOpportunity) -> Option<(String, PoolSnapshot)> {
    let cid = opp.canonical_id.as_ref()?.clone();
    let snap = snapshot_pools(pm, opp)?;
    Some((cid, snap))
}

/// Push detect-time snapshots (no `canonical_id` required). Call **before**
/// applying the tx's logs so the seed matches the quoted state.
pub fn push_pending_seeds(
    pending: &mut Vec<PendingSeed>,
    pm: &PoolManager,
    opps: &[MevOpportunity],
) {
    for opp in opps {
        if !SimPolicy::is_simulatable(opp.strategy) {
            continue;
        }
        if let Some(snap) = snapshot_pools(pm, opp) {
            pending.push(PendingSeed {
                key: SeedMatchKey::from_opp(opp),
                snap,
            });
        }
    }
}

/// Promote detect-time pending seeds into `seeds` keyed by `canonical_id`.
///
/// Returns `(recorded, capped)` where `capped` is how many simulatable opps
/// were skipped because the per-block seed budget was already full.
pub fn promote_pending_seeds(
    seeds: &mut SimSeeds,
    pending: &mut Vec<PendingSeed>,
    opps: &[MevOpportunity],
    max_per_block: usize,
) -> (usize, usize) {
    let mut per_block: HashMap<u64, usize> = HashMap::new();
    for snap in seeds.values() {
        *per_block.entry(snap.block_number).or_insert(0) += 1;
    }
    let mut index: HashMap<SeedMatchKey, PoolSnapshot> = HashMap::new();
    for p in pending.drain(..) {
        index.entry(p.key).or_insert(p.snap);
    }
    let mut recorded = 0usize;
    let mut capped = 0usize;
    for opp in opps {
        if !SimPolicy::is_simulatable(opp.strategy) {
            continue;
        }
        let Some(cid) = opp.canonical_id.as_ref() else {
            continue;
        };
        if seeds.contains_key(cid) {
            continue;
        }
        let count = per_block.entry(opp.block_number).or_insert(0);
        if *count >= max_per_block {
            capped += 1;
            continue;
        }
        let key = SeedMatchKey::from_opp(opp);
        if let Some(snap) = index.remove(&key) {
            seeds.insert(cid.clone(), snap);
            *count += 1;
            recorded += 1;
        }
    }
    (recorded, capped)
}

/// Record seeds for simulatable opps into `seeds`, capped per block.
///
/// Fallback for paths that already have `canonical_id` and quote against the
/// current manager (e.g. mempool). Prefer [`push_pending_seeds`] +
/// [`promote_pending_seeds`] for block replay/log-only.
pub fn record_seeds_for_block(
    seeds: &mut SimSeeds,
    pm: &PoolManager,
    opps: &[MevOpportunity],
    max_per_block: usize,
) -> (usize, usize) {
    let mut per_block: HashMap<u64, usize> = HashMap::new();
    for snap in seeds.values() {
        *per_block.entry(snap.block_number).or_insert(0) += 1;
    }
    let mut recorded = 0usize;
    let mut capped = 0usize;
    for opp in opps {
        if !SimPolicy::is_simulatable(opp.strategy) {
            continue;
        }
        let count = per_block.entry(opp.block_number).or_insert(0);
        if *count >= max_per_block {
            capped += 1;
            continue;
        }
        if let Some((cid, snap)) = capture_snapshot(pm, opp) {
            if seeds.insert(cid, snap).is_none() {
                *count += 1;
                recorded += 1;
            }
        }
    }
    (recorded, capped)
}

fn next_token(pool: &PoolState, token_in: Address) -> Option<Address> {
    match pool {
        PoolState::Curve(c) => c
            .token_index
            .keys()
            .filter(|k| **k != token_in)
            .min()
            .copied(),
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
        let state = snapshot.get(addr).ok_or(SimFail::PathUnavailable)?.clone();
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

    let post_pools: Vec<(Address, PoolState)> = path
        .iter()
        .filter_map(|a| scratch.remove(a).map(|s| (*a, s)))
        .collect();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex_type::DexType;
    use crate::pool::state::{PoolInfo, UniswapV2PoolState};
    use alloy::primitives::address;

    fn v2(addr: Address, t0: Address, t1: Address, r0: u128, r1: u128) -> PoolState {
        PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: addr,
                token0: t0,
                token1: t1,
                fee: 30,
                dex_type: DexType::UniswapV2,
                ..Default::default()
            },
            reserve0: r0,
            reserve1: r1,
        })
    }

    #[test]
    fn promote_pending_seeds_requires_detect_time_capture() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let pb = address!("0x00000000000000000000000000000000000000bb");
        let mut pm = PoolManager::new();
        pm.add_pool(v2(pa, t0, t1, 1_000_000, 1_000_000));
        pm.add_pool(v2(pb, t0, t1, 1_000_000, 1_000_000));

        let mut opp = MevOpportunity::new(7, 3, Strategy::TwoHopArb, pa, 0);
        opp.pool_b = pb;
        opp.token_in = t0;
        opp.token_out = t0;
        opp.path = Some(vec![pa, pb]);

        // Without pending push, promote finds nothing.
        let mut seeds = SimSeeds::new();
        let mut pending = Vec::new();
        opp.canonical_id = Some("cid".into());
        let (rec, _cap) = promote_pending_seeds(&mut seeds, &mut pending, &[opp.clone()], 32);
        assert_eq!(rec, 0);
        assert!(seeds.is_empty());

        // Detect-time push then promote.
        opp.canonical_id = None;
        push_pending_seeds(&mut pending, &pm, &[opp.clone()]);
        assert_eq!(pending.len(), 1);
        opp.canonical_id = Some("cid".into());
        let (rec, capped) = promote_pending_seeds(&mut seeds, &mut pending, &[opp], 32);
        assert_eq!(rec, 1);
        assert_eq!(capped, 0);
        assert!(seeds.contains_key("cid"));
        assert_eq!(seeds["cid"].tx_index, 3);
    }

    #[test]
    fn assumed_path_keeps_pending_empty_when_disabled() {
        // Documented contract: with sim disabled the runner clears pending and
        // never records — unit-level: record with empty pm opps is a no-op.
        let mut seeds = SimSeeds::new();
        let pm = PoolManager::new();
        let (rec, capped) = record_seeds_for_block(&mut seeds, &pm, &[], 32);
        assert_eq!((rec, capped), (0, 0));
        assert!(seeds.is_empty());
    }
}
