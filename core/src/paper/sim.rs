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
/// `route_tokens` (when present) is the detector's exact hop sequence. Without
/// it, multi-token Curve/Balancer pools fall back to `next_token`, which can
/// only guess.
///
/// `pm` supplies the measured transfer-tax map and the `fot_quote` gate; when
/// present and enabled, each leg's pool input and received output are taxed the
/// same way the detector quoted them, and the reserves move by the pool-side
/// amount only.
///
/// Returns final output amount, or `None` if a leg is missing / unquotable.
pub fn walk_quote_post(
    overlay: &mut HashMap<Address, PoolState>,
    path: &[Address],
    route_tokens: Option<&[Address]>,
    token_in: Address,
    amount: u128,
    pm: Option<&PoolManager>,
) -> Option<u128> {
    let mut current = amount;
    let mut current_token = token_in;
    let tax_on = pm.is_some_and(|p| p.tax_quoting());
    for (i, addr) in path.iter().enumerate() {
        let pool = overlay.get_mut(addr)?;
        let token_out = route_tokens
            .and_then(|tokens| tokens.get(i + 1).copied())
            .or_else(|| next_token(pool, current_token))?;
        let (pool_in, tax_out_bps) = if tax_on {
            let pm = pm?;
            let in_bps = pm
                .tax_bps(&current_token)
                .map(|b| (b as u128).min(9_999))
                .unwrap_or(0);
            let out_bps = pm
                .tax_bps(&token_out)
                .map(|b| (b as u128).min(9_999))
                .unwrap_or(0);
            (
                current.saturating_sub(current.saturating_mul(in_bps) / 10_000),
                out_bps,
            )
        } else {
            (current, 0)
        };
        if pool_in == 0 {
            return None;
        }
        let (gross, post) = quote_swap_post(pool, current_token, pool_in, token_out)?;
        apply_post_state(pool, &post);
        current = gross.saturating_sub(gross.saturating_mul(tax_out_bps) / 10_000);
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
    pm: Option<&PoolManager>,
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
    if let Some(route) = opp.route_tokens.as_deref() {
        let excluded = |t: &Address| {
            crate::pool::state::is_fee_on_transfer_token(t)
                || pm.is_some_and(|p| !p.tax_quoting() && p.tax_bps(t).is_some())
        };
        if route.iter().any(excluded) {
            return Err(SimFail::PathUnavailable);
        }
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

    let out = if let Some(route) = &opp.savax_route {
        let rate = if route.rate_wad > U256::from(u128::MAX) {
            return Err(SimFail::PathUnavailable);
        } else {
            route.rate_wad.to::<u128>()
        };
        if rate == 0 {
            return Err(SimFail::PathUnavailable);
        }
        let one = 10u128.pow(18);
        match route.direction {
            crate::types::SavaxDirection::Redeem => {
                let shares = walk_quote_post(
                    &mut scratch,
                    &path,
                    opp.route_tokens.as_deref(),
                    opp.token_in,
                    borrowed,
                    pm,
                )
                .ok_or(SimFail::PathUnavailable)?;
                (U256::from(shares).saturating_mul(route.rate_wad) / U256::from(one)).to::<u128>()
            }
            crate::types::SavaxDirection::Mint => {
                let shares = borrowed.saturating_mul(one) / rate;
                if shares == 0 {
                    return Err(SimFail::PathUnavailable);
                }
                let tokens = opp
                    .route_tokens
                    .as_deref()
                    .ok_or(SimFail::PathUnavailable)?;
                let savax = *tokens.first().ok_or(SimFail::PathUnavailable)?;
                walk_quote_post(&mut scratch, &path, Some(tokens), savax, shares, pm)
                    .ok_or(SimFail::PathUnavailable)?
            }
        }
    } else {
        walk_quote_post(
            &mut scratch,
            &path,
            opp.route_tokens.as_deref(),
            opp.token_in,
            borrowed,
            pm,
        )
        .ok_or(SimFail::PathUnavailable)?
    };

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

    #[test]
    fn walk_quote_post_applies_learned_tax_to_pool_side_amounts() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pa = address!("0x00000000000000000000000000000000000000aa");
        let pb = address!("0x00000000000000000000000000000000000000bb");
        let mut overlay = HashMap::new();
        overlay.insert(pa, v2(pa, t0, t1, 1_000_000, 1_000_000));
        overlay.insert(pb, v2(pb, t0, t1, 1_000_000, 1_000_000));
        let route = [t0, t1, t0];
        let amount = 10_000u128;

        let untaxed = walk_quote_post(
            &mut overlay.clone(),
            &[pa, pb],
            Some(&route),
            t0,
            amount,
            None,
        )
        .expect("untaxed walk");

        let mut pm = PoolManager::new();
        pm.set_tax_bps(t0, 200);
        pm.set_tax_bps(t1, 100);
        let mut taxed_overlay = overlay.clone();
        let taxed = walk_quote_post(
            &mut taxed_overlay,
            &[pa, pb],
            Some(&route),
            t0,
            amount,
            Some(&pm),
        )
        .expect("taxed walk");
        assert!(taxed < untaxed, "taxed out {taxed} < untaxed {untaxed}");

        // The pool only ever sees the post-tax input (10_000 − 2% = 9_800).
        match taxed_overlay.get(&pa) {
            Some(PoolState::UniswapV2(state)) => {
                let (t0_reserve, t1_reserve) = if state.info.token0 == t0 {
                    (state.reserve0, state.reserve1)
                } else {
                    (state.reserve1, state.reserve0)
                };
                assert_eq!(t0_reserve, 1_000_000 + 9_800);
                assert!(t1_reserve < 1_000_000);
            }
            other => panic!("expected V2 pool, got {other:?}"),
        }

        // Quoting disabled: no tax, and learned-tax routes are excluded by sim.
        let mut off = PoolManager::new();
        off.set_tax_bps(t0, 200);
        off.set_tax_quoting(false);
        let plain = walk_quote_post(
            &mut overlay.clone(),
            &[pa, pb],
            Some(&route),
            t0,
            amount,
            Some(&off),
        )
        .expect("quote gate off walks untaxed");
        assert_eq!(plain, untaxed);
    }

    fn savax_pool(
        addr: Address,
        wrapped: Address,
        savax: Address,
        rw: u128,
        rs: u128,
    ) -> PoolState {
        v2(addr, wrapped, savax, rw, rs)
    }

    fn savax_opp(
        pool: Address,
        wrapped: Address,
        savax: Address,
        direction: crate::types::SavaxDirection,
        rate_wad: U256,
        amount: u128,
    ) -> MevOpportunity {
        let mut opp = MevOpportunity::new(5, 0, Strategy::MultiHopArb, pool, 0);
        opp.token_in = wrapped;
        opp.token_out = wrapped;
        opp.path = Some(vec![pool]);
        opp.route_tokens = Some(match direction {
            crate::types::SavaxDirection::Redeem => vec![wrapped, savax],
            crate::types::SavaxDirection::Mint => vec![savax, wrapped],
        });
        opp.savax_route = Some(crate::types::SavaxRoute {
            pool,
            direction,
            rate_wad,
            shares: U256::ZERO,
            min_out_wad: U256::ZERO,
        });
        opp.canonical_id = Some(crate::types::compute_savax_canonical_id(pool, direction));
        opp.input_amount = U256::from(amount);
        opp.expected_profit = U256::from(1u64);
        opp.gas_cost_wei = 0;
        opp
    }

    #[test]
    fn savax_redeem_simulates_virtual_rate_leg() {
        let wrapped = address!("b31f66aa3c1e785363f0875a1b74e27b85fd66c7");
        let savax = address!("2b2c81e08f1af8835a78bb2a90ae924ace0ea4be");
        let pool = address!("0x00000000000000000000000000000000000000aa");
        let state = savax_pool(pool, wrapped, savax, 1_000_000, 1_000_000);
        let opp = savax_opp(
            pool,
            wrapped,
            savax,
            crate::types::SavaxDirection::Redeem,
            U256::from(1_100_000_000_000_000_000u64),
            10_000,
        );
        let mut seeds = SimSeeds::new();
        seeds.insert(
            opp.canonical_id.clone().unwrap(),
            PoolSnapshot {
                block_number: 5,
                tx_index: 0,
                pools: vec![(pool, state)],
            },
        );
        let policy = SimPolicy {
            mode: SimMode::StateSim,
            ..Default::default()
        };
        let out = simulate_candidate(
            &opp,
            &seeds,
            &mut HashMap::new(),
            &HashMap::new(),
            &policy,
            None,
        )
        .expect("redeem sim");
        assert!(out.sim_gross_wei > 0);
        assert!(out.repay_ok);
    }

    #[test]
    fn savax_mint_simulates_virtual_rate_leg() {
        let wrapped = address!("b31f66aa3c1e785363f0875a1b74e27b85fd66c7");
        let savax = address!("2b2c81e08f1af8835a78bb2a90ae924ace0ea4be");
        let pool = address!("0x00000000000000000000000000000000000000ab");
        // Pool prices sAVAX above the mint rate (2 vs 1.1).
        let state = savax_pool(pool, wrapped, savax, 2_000_000, 1_000_000);
        let opp = savax_opp(
            pool,
            wrapped,
            savax,
            crate::types::SavaxDirection::Mint,
            U256::from(1_100_000_000_000_000_000u64),
            10_000,
        );
        let mut seeds = SimSeeds::new();
        seeds.insert(
            opp.canonical_id.clone().unwrap(),
            PoolSnapshot {
                block_number: 5,
                tx_index: 0,
                pools: vec![(pool, state)],
            },
        );
        let policy = SimPolicy {
            mode: SimMode::StateSim,
            ..Default::default()
        };
        let out = simulate_candidate(
            &opp,
            &seeds,
            &mut HashMap::new(),
            &HashMap::new(),
            &policy,
            None,
        )
        .expect("mint sim");
        assert!(out.sim_gross_wei > 0);
        assert!(out.repay_ok);
    }

    #[test]
    fn unsupported_venue_is_a_skip_never_a_fill() {
        use crate::pool::state::MetricPoolState;
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let pool = address!("0x00000000000000000000000000000000000000ac");
        let state = PoolState::Metric(MetricPoolState::new(crate::pool::state::PoolInfo {
            address: pool,
            token0: t0,
            token1: t1,
            fee: 30,
            dex_type: crate::dex_type::DexType::Metric,
            ..Default::default()
        }));
        let mut opp = MevOpportunity::new(6, 0, Strategy::TwoHopArb, pool, 0);
        opp.token_in = t0;
        opp.token_out = t1;
        opp.path = Some(vec![pool]);
        opp.route_tokens = Some(vec![t0, t1]);
        opp.canonical_id = Some("metric-row".into());
        opp.input_amount = U256::from(1_000u64);
        let mut seeds = SimSeeds::new();
        seeds.insert(
            "metric-row".into(),
            PoolSnapshot {
                block_number: 6,
                tx_index: 0,
                pools: vec![(pool, state)],
            },
        );
        let policy = SimPolicy {
            mode: SimMode::StateSim,
            ..Default::default()
        };
        let err = simulate_candidate(
            &opp,
            &seeds,
            &mut HashMap::new(),
            &HashMap::new(),
            &policy,
            None,
        )
        .unwrap_err();
        assert_eq!(err, SimFail::PathUnavailable);
        assert_eq!(
            err.to_skip(),
            crate::paper::types::FillSkipReason::SimPathUnavailable
        );
    }
}
