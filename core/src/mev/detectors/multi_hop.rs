//! Multi-hop arbitrage detection — finds profitable swap cycles across connected pools.
//!
//! Candidate cycles are discovered with Bellman–Ford negative-cycle detection on a
//! `-log(exchange_rate)` token graph (#5) instead of enumerating all BFS paths up to
//! depth 4: work is bounded by O(V·E · rounds) rather than branching-factor^depth.
//! Each discovered cycle is then priced through the standard V2/V3 AMM quote engine
//! with a deterministic segment-wise optimizer (no stochastic restarts).

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::dex_type::DexType;
use crate::pool::math::v3::{max_v3_tradeable_amount, v3_breakpoints};
use crate::pool::math::{constant_product_output_amount, optimal_on_segments, quote_exact_in};
use crate::pool::state::{
    calldata_gas_estimate, check_dedup_key, PoolManager, PoolState, ScanScope, UniswapV2PoolState,
};
use crate::types::gas::GasCalibrationSnapshot;
use crate::types::MevOpportunity;
use crate::types::{GasConfig, Strategy};

/// Maximum tick-band breakpoints enumerated per V3 pool when segmenting the
/// profit landscape for deterministic optimization.
const MAX_V3_BREAKPOINTS_PER_POOL: usize = 24;
/// Total inverted breakpoints taken across all V3 pools of an N-hop path
/// (inversion requires binary-search probes through the prefix quote, so the
/// total count is capped independently of the per-pool cap).
const MAX_N_HOP_BREAKPOINTS: usize = 16;
/// Cap on distinct negative cycles emitted per detection pass (#5). Bounds the
/// ban-and-retry loop on adversarially dense graphs.
const MAX_NEGATIVE_CYCLES: usize = 16;
/// Hard cap on ban-and-retry attempts even when cycles keep being found.
const MAX_CYCLE_ATTEMPTS: usize = 64;
/// Relative margin required on the spot-rate cycle product before a candidate
/// cycle is handed to the numeric optimizer: product must exceed 1 by more than
/// this (absorbs f64 rounding; genuinely aligned cycles are never emitted).
const CYCLE_RATE_MARGIN: f64 = 1e-9;
/// Strict-improvement epsilon for Bellman–Ford relaxation (suppresses churn
/// from floating-point noise on perfectly aligned price cycles).
const RELAX_EPS: f64 = 1e-12;

/// Detects multi-hop arbitrage opportunities across V2/V3/Curve/Balancer pool paths.
///
/// Cycles are found by Bellman–Ford relaxation over the token graph (#5), then
/// validated and priced numerically. Maintains a per-block dedup set so the same
/// persistent path is not re-reported across multiple transactions.
pub struct MultiHopArbDetector {
    block_number: u64,
    seen: std::collections::HashMap<(Address, Address, Address, Address), (u128, u128)>,
}

impl MultiHopArbDetector {
    /// Create a new detector for the given block.
    pub fn new(block_number: u64) -> Self {
        Self {
            block_number,
            seen: std::collections::HashMap::new(),
        }
    }

    /// Scan all pool paths and emit profitable multi-hop arbitrage opportunities.
    /// Deduplicates per block: each unique (pool_a, pool_b, token_in, token_out) is emitted
    /// at most once per block *unless* pool reserves change by >0.1%, in which case the
    /// dedup is cleared and the opportunity is re-evaluated (H2).
    ///
    /// `scope` restricts emitted cycles: pass [`ScanScope::Full`] for the first
    /// detection pass of a block, then [`ScanScope::Dirty`] with the set of pools
    /// touched by earlier transactions — a cycle must contain at least one dirty pool.
    pub fn detect(
        &mut self,
        pool_manager: &PoolManager,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
        scope: &ScanScope,
    ) -> Vec<MevOpportunity> {
        let max_depth = 4usize;
        let mut opportunities = Vec::new();

        let paths = Self::find_negative_cycle_paths(pool_manager, max_depth, scope);

        for path in &paths {
            if let Some(opp) = Self::check_path(
                pool_manager,
                path,
                self.block_number,
                tx_index,
                timestamp,
                base_fee_per_gas,
                gas_config,
            ) {
                let key = (opp.pool_a, opp.pool_b, opp.token_in, opp.token_out);
                if check_dedup_key(&mut self.seen, &key, pool_manager, opp.pool_a, opp.pool_b) {
                    opportunities.push(opp);
                }
            }
        }

        opportunities
    }

    // ------------------------------------------------------------------
    // #5: Negative-cycle discovery on a -log(exchange_rate) token graph
    // ------------------------------------------------------------------

    /// Find profitable candidate cycles via Bellman–Ford (#5).
    ///
    /// Nodes are tokens; each pool contributes two directed edges weighted by
    /// `-ln(marginal_exchange_rate)`. A cycle can only be profitable if its edge
    /// weight sum is negative (product of marginal rates > 1). Relaxation runs
    /// for at most `max_depth` rounds, so only cycles up to `max_depth` hops are
    /// surfaced — matching the execution model. Discovered cycles have their
    /// pools banned and the search repeats until no further cycles emerge,
    /// bounded by [`MAX_NEGATIVE_CYCLES`] emissions / [`MAX_CYCLE_ATTEMPTS`] tries.
    ///
    /// When `scope` is [`ScanScope::Dirty`], only cycles containing at least one
    /// dirty pool are returned (clean pools still participate as intermediates).
    pub fn find_negative_cycle_paths(
        pm: &PoolManager,
        max_depth: usize,
        scope: &ScanScope,
    ) -> Vec<Vec<Address>> {
        let graph = TokenGraph::build(pm);
        if graph.is_empty() {
            return Vec::new();
        }
        let rounds = max_depth.min(8);
        let mut banned: HashSet<Address> = HashSet::new();
        let mut emitted: HashSet<Vec<Address>> = HashSet::new();
        let mut out: Vec<Vec<Address>> = Vec::new();

        for _ in 0..MAX_CYCLE_ATTEMPTS {
            if out.len() >= MAX_NEGATIVE_CYCLES {
                break;
            }
            let Some(candidate) = graph.find_negative_cycle(rounds, max_depth, &banned) else {
                break; // converged: no further bounded negative cycles
            };

            // Ban every pool of the candidate so each attempt makes progress,
            // regardless of whether the candidate survives validation below.
            let pools: Vec<Address> = candidate.pools();
            for &p in &pools {
                banned.insert(p);
            }

            if !candidate.is_valid(pm, &graph, max_depth) {
                continue;
            }
            if !match scope {
                ScanScope::Full => true,
                ScanScope::Dirty(dirty) => pools.iter().any(|p| dirty.contains(p)),
            } {
                continue;
            }

            // Rotation-normalize so the same cycle found from a different start
            // node dedups to one entry. The opposite trade direction remains a
            // separate (valid) opportunity, mirroring two-hop behavior.
            let mut path = pools;
            normalize_rotation(&mut path);
            if emitted.insert(path.clone()) {
                out.push(path);
            }
        }

        out
    }

    /// BFS-limited enumeration of pool paths through the token graph.
    /// Each path is [buy_pool, ..., sell_pool] where adjacent pools share a token.
    ///
    /// Retained for compatibility and testing; production detection uses
    /// [`Self::find_negative_cycle_paths`].
    pub fn find_paths(pm: &PoolManager, max_depth: usize) -> Vec<Vec<Address>> {
        Self::find_paths_scoped(pm, max_depth, &ScanScope::Full)
    }

    /// Scope-aware variant of [`Self::find_paths`]: when `scope` is
    /// [`ScanScope::Dirty`], only seed pairs containing at least one dirty
    /// pool are enumerated — untouched pairs cannot produce new opportunities.
    pub fn find_paths_scoped(
        pm: &PoolManager,
        max_depth: usize,
        scope: &ScanScope,
    ) -> Vec<Vec<Address>> {
        let mut all_paths = Vec::new();

        // Seed 2-pool paths from existing arbitrage pairs (both directions)
        let pairs = pm.arbitrage_pairs();
        for &(pool_a, pool_b, _shared) in pairs.iter() {
            if !scope.contains_pair(&pool_a, &pool_b) {
                continue;
            }
            let seed = vec![pool_a, pool_b];
            all_paths.push(seed.clone());
            Self::extend_path(pm, seed, &mut all_paths, max_depth);
            let rev = vec![pool_b, pool_a];
            all_paths.push(rev.clone());
            Self::extend_path(pm, rev, &mut all_paths, max_depth);
        }

        all_paths
    }

    fn extend_path(
        pm: &PoolManager,
        path: Vec<Address>,
        all_paths: &mut Vec<Vec<Address>>,
        max_depth: usize,
    ) {
        if path.len() >= max_depth {
            return;
        }

        let last_pool = match pm.get(&path[path.len() - 1]) {
            Some(p) => p,
            None => return,
        };
        let prev_pool = match pm.get(&path[path.len() - 2]) {
            Some(p) => p,
            None => return,
        };

        // Determine the "forward token" — the token NOT shared with the previous pool
        let forward_token = Self::non_shared_token(last_pool, prev_pool);

        for &next_addr in pm.pools_for_token(&forward_token).into_iter().flatten() {
            if path.contains(&next_addr) {
                continue;
            }
            let mut new_path = path.clone();
            new_path.push(next_addr);
            all_paths.push(new_path.clone());
            Self::extend_path(pm, new_path, all_paths, max_depth);
        }
    }

    /// Given a pool and the previous pool in the path, determine which token
    /// of `pool` is the "forward" side (not shared with `prev`).
    fn non_shared_token(pool: &PoolState, prev: &PoolState) -> Address {
        let info = pool.info();
        let prev_info = prev.info();
        if info.token0 == prev_info.token0 || info.token0 == prev_info.token1 {
            info.token1
        } else {
            info.token0
        }
    }

    fn check_path(
        pm: &PoolManager,
        path: &[Address],
        block_number: u64,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
    ) -> Option<MevOpportunity> {
        if path.len() < 2 {
            return None;
        }

        let pool_a = pm.get(&path[0])?;
        let pool_b = pm.get(&path[path.len() - 1])?;

        // token_in = non-shared side of first pool
        let next = pm.get(&path[1])?;
        let info_a = pool_a.info();
        let info_next = next.info();
        let first_shared = if info_a.token0 == info_next.token0 || info_a.token0 == info_next.token1
        {
            info_a.token0
        } else {
            info_a.token1
        };
        let token_in = if info_a.token0 == first_shared {
            info_a.token1
        } else {
            info_a.token0
        };

        // token_out = non-shared side of last pool
        let prev = pm.get(&path[path.len() - 2])?;
        let info_b = pool_b.info();
        let last_shared =
            if info_b.token0 == prev.info().token0 || info_b.token0 == prev.info().token1 {
                info_b.token0
            } else {
                info_b.token1
            };
        let token_out = if info_b.token0 == last_shared {
            info_b.token1
        } else {
            info_b.token0
        };

        // Fee-on-transfer filter (#9): quotes assume full output received; sell-tax
        // tokens produce phantom opportunities. Exclude known and dynamically
        // learned FOT tokens.
        if pm.is_taxed_token(&token_in) || pm.is_taxed_token(&token_out) {
            return None;
        }

        let max_input = Self::pool_max_input(pool_a);

        let quote_fn = |x: u128| -> Option<u128> {
            let mut current = x;
            let mut current_token = token_in;
            for &addr in path {
                let pool = pm.get(&addr)?;
                current = Self::quote_single_pool(pool, current_token, current)?;
                let info = pool.info();
                current_token = if info.token0 == current_token {
                    info.token1
                } else {
                    info.token0
                };
            }
            Some(current)
        };

        // Deterministic segment-wise optimization (#6 generalization): brackets
        // the input domain at V3 tick-band crossings inverted into input space
        // through the prefix quotes, golden-section-searching each concave
        // bracket. Replaces grid + random restarts with a deterministic pass.
        let breakpoints = Self::compose_n_hop_breakpoints(pm, path, token_in, max_input);
        let (input_amount, output_amount) =
            optimal_on_segments(max_input, &breakpoints, &quote_fn)?;

        if output_amount <= input_amount {
            return None;
        }

        let gas_limit = estimate_gas_for_multi_hop(
            path,
            pm,
            gas_config.flash_loan_provider.gas_overhead(),
            &gas_config.calibration,
        );
        let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);

        let gross_profit = output_amount.saturating_sub(input_amount);
        // Subtract flash loan fee from gross profit
        let flash_fee = gas_config.flash_loan_fee(input_amount);
        let net_profit = gross_profit.saturating_sub(flash_fee);

        // Normalize profit to native when token_in != token_out (H6).
        let (expected_profit, raw_profit) = if token_in == token_out {
            (U256::from(net_profit), None)
        } else {
            let raw = U256::from(net_profit);
            let native_profit = pm
                .normalize_to_native(token_out, net_profit)
                .or_else(|| {
                    let total_input = input_amount;
                    let total_output = total_input.saturating_add(net_profit);
                    let native_in = pm.normalize_to_native(token_in, total_input)?;
                    let native_out = pm.normalize_to_native(token_out, total_output)?;
                    native_out.checked_sub(native_in)
                })
                .unwrap_or(net_profit);
            (U256::from(native_profit), Some(raw))
        };

        // Compute slippage-adjusted profits
        let eval_raw = |x: u128| -> Option<u128> {
            let mut cur = x;
            let mut cur_token = token_in;
            for &addr in path {
                let pool = pm.get(&addr)?;
                cur = Self::quote_single_pool(pool, cur_token, cur)?;
                let info = pool.info();
                cur_token = if info.token0 == cur_token {
                    info.token1
                } else {
                    info.token0
                };
            }
            (cur > x).then(|| cur - x)
        };
        let normalize_slippage = |p: u128| -> Option<U256> {
            if token_in == token_out {
                Some(U256::from(p))
            } else {
                pm.normalize_to_native(token_out, p)
                    .or_else(|| {
                        let native_in = pm.normalize_to_native(token_in, input_amount)?;
                        let native_out = pm.normalize_to_native(token_out, input_amount + p)?;
                        native_out.checked_sub(native_in)
                    })
                    .map(U256::from)
            }
        };
        let p1 = if input_amount > 0 {
            eval_raw(input_amount.saturating_mul(101) / 100).and_then(normalize_slippage)
        } else {
            None
        };
        let m1 = if input_amount > 0 {
            eval_raw(input_amount.saturating_mul(99) / 100).and_then(normalize_slippage)
        } else {
            None
        };
        let p2 = if input_amount > 0 {
            eval_raw(input_amount.saturating_mul(102) / 100).and_then(normalize_slippage)
        } else {
            None
        };
        let m2 = if input_amount > 0 {
            eval_raw(input_amount.saturating_mul(98) / 100).and_then(normalize_slippage)
        } else {
            None
        };

        Some(MevOpportunity {
            canonical_id: None,
            block_number,
            tx_index,
            strategy: Strategy::MultiHopArb,
            pool_a: path[0],
            pool_b: path[path.len() - 1],
            token_in,
            token_out,
            input_amount: U256::from(input_amount),
            expected_profit,
            raw_profit,
            profit_slippage_p1: p1,
            profit_slippage_m1: m1,
            profit_slippage_p2: p2,
            profit_slippage_m2: m2,
            gas_cost_wei,
            timestamp,
            path: Some(path.to_vec()),
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
            mempool_only: false,
            confidence: None,
        })
    }

    /// Compose deterministic input-domain breakpoints for an N-hop path.
    ///
    /// Each V3-family pool along the path contributes tick-band thresholds in
    /// its own intermediate-token domain; those thresholds are inverted into
    /// input space through the monotone composed prefix quote. Paths containing
    /// no breakpoint-capable pools (pure constant-product chains) return an
    /// empty vector, for which [`optimal_on_segments`] degenerates to a single
    /// golden-section search — exact, since composite constant-product profit
    /// is globally concave.
    fn compose_n_hop_breakpoints(
        pm: &PoolManager,
        path: &[Address],
        token_in: Address,
        max_input: u128,
    ) -> Vec<u128> {
        let mut out: Vec<u128> = Vec::new();
        let mut walk_token = token_in;

        for (i, &addr) in path.iter().enumerate() {
            let Some(pool) = pm.get(&addr) else { break };

            if i > 0 {
                if let PoolState::UniswapV3(v3) = pool {
                    let zero_for_one = walk_token == pool.info().token0;
                    let prefix = |x: u128| -> Option<u128> {
                        let mut cur = x;
                        let mut prefix_walk_token = token_in;
                        for &prev_addr in &path[..i] {
                            let prev_pool = pm.get(&prev_addr)?;
                            cur = Self::quote_single_pool(prev_pool, prefix_walk_token, cur)?;
                            let info = prev_pool.info();
                            prefix_walk_token = if info.token0 == prefix_walk_token {
                                info.token1
                            } else {
                                info.token0
                            };
                        }
                        Some(cur)
                    };
                    let mid_max = prefix(max_input).unwrap_or(0);
                    let thresholds =
                        v3_breakpoints(v3, zero_for_one, mid_max, MAX_V3_BREAKPOINTS_PER_POOL);
                    for t in thresholds {
                        if out.len() >= MAX_N_HOP_BREAKPOINTS {
                            break;
                        }
                        if t == 0 || t > mid_max {
                            continue;
                        }
                        if let Some(x) = invert_monotone_quote(&prefix, t, max_input) {
                            out.push(x);
                        }
                    }
                }
            }

            let info = pool.info();
            walk_token = if info.token0 == walk_token {
                info.token1
            } else {
                info.token0
            };
        }

        out.sort_unstable();
        out.dedup();
        out
    }

    fn pool_max_input(pool: &PoolState) -> u128 {
        match pool {
            PoolState::UniswapV2(v2) => std::cmp::min(v2.reserve0, v2.reserve1),
            PoolState::UniswapV3(v3) => {
                max_v3_tradeable_amount(v3, true).max(max_v3_tradeable_amount(v3, false))
            }
            PoolState::UniswapV4(v4) => {
                max_v3_tradeable_amount(v4, true).max(max_v3_tradeable_amount(v4, false))
            }
            PoolState::Curve(c) => c.balances.iter().fold(0u128, |a, &b| a.max(b)),
            PoolState::Balancer(b) => b.balances.iter().fold(0u128, |a, &b| a.max(b)),
            PoolState::TraderJoeLB(lb) => std::cmp::min(lb.reserve_x, lb.reserve_y),
            PoolState::Pendle(p) => std::cmp::min(p.total_pt, p.total_sy),
        }
    }

    fn quote_single_pool(pool: &PoolState, token_in: Address, amount_in: u128) -> Option<u128> {
        match pool {
            PoolState::UniswapV2(v2) => {
                let (reserve_in, reserve_out) = if v2.info.token0 == token_in {
                    (v2.reserve0, v2.reserve1)
                } else if v2.info.token1 == token_in {
                    (v2.reserve1, v2.reserve0)
                } else {
                    return None;
                };
                constant_product_output_amount(amount_in, reserve_in, reserve_out, v2.info.fee)
            }
            PoolState::Curve(curve) => {
                let token_out = curve.token_index.keys().filter(|k| **k != token_in).min()?;
                quote_exact_in(pool, token_in, *token_out, amount_in)
            }
            PoolState::Balancer(bal) => {
                let token_out = *bal.token_index.keys().filter(|k| **k != token_in).min()?;
                quote_exact_in(pool, token_in, token_out, amount_in)
            }
            _ => {
                // For V3 and future pool types, use the unified dispatcher
                // which determines token_out from the pool's second token
                let token_out = if pool.info().token0 == token_in {
                    pool.info().token1
                } else if pool.info().token1 == token_in {
                    pool.info().token0
                } else {
                    return None;
                };
                quote_exact_in(pool, token_in, token_out, amount_in)
            }
        }
    }
}

/// Smallest x in [1, max_input] whose monotone-increasing quote reaches `target`.
fn invert_monotone_quote(
    quote: &impl Fn(u128) -> Option<u128>,
    target: u128,
    max_input: u128,
) -> Option<u128> {
    if target == 0 {
        return None;
    }
    if quote(max_input)? < target {
        return None;
    }
    let mut lo = 1u128;
    let mut hi = max_input;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if quote(mid).unwrap_or(0) >= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(lo)
}

/// Rotate a cyclic pool path so it starts at the lexicographically smallest pool.
fn normalize_rotation(path: &mut [Address]) {
    if path.len() < 2 {
        return;
    }
    let min_idx = path
        .iter()
        .enumerate()
        .min_by_key(|(_, a)| **a)
        .map(|(i, _)| i)
        .unwrap_or(0);
    path.rotate_left(min_idx);
}

// ----------------------------------------------------------------------
// #5 helpers: marginal rates, token graph, Bellman–Ford
// ----------------------------------------------------------------------

/// Optimistic marginal exchange rate (output per input at zero trade size).
///
/// Used exclusively by the negative-cycle filter (#5): the marginal rate is the
/// BEST achievable rate for every pool type (larger trades only move prices
/// against the trader), so using it as the edge weight can only produce false
/// positives — never missed cycles. Curve/Balancer use balance-ratio spot
/// approximations chosen with the same optimistic bias.
pub fn marginal_exchange_rate(
    pool: &PoolState,
    token_in: Address,
    token_out: Address,
) -> Option<f64> {
    const BPS: f64 = 10_000.0;
    const PPM: f64 = 1_000_000.0;

    let ratio = |reserve_in: u128, reserve_out: u128| -> Option<f64> {
        if reserve_in == 0 || reserve_out == 0 {
            return None;
        }
        Some(reserve_out as f64 / reserve_in as f64)
    };

    match pool {
        PoolState::UniswapV2(v2) => {
            let (ri, ro) = v2_directional_reserves(v2, token_in, token_out)?;
            Some((1.0 - v2.info.fee as f64 / BPS) * ratio(ri, ro)?)
        }
        PoolState::TraderJoeLB(lb) => {
            let (ri, ro) = if lb.info.token0 == token_in && lb.info.token1 == token_out {
                (lb.reserve_x, lb.reserve_y)
            } else if lb.info.token1 == token_in && lb.info.token0 == token_out {
                (lb.reserve_y, lb.reserve_x)
            } else {
                return None;
            };
            Some((1.0 - lb.info.fee as f64 / BPS) * ratio(ri, ro)?)
        }
        PoolState::Pendle(p) => {
            let (ti, to) = if p.info.token0 == token_in && p.info.token1 == token_out {
                (p.total_pt, p.total_sy)
            } else if p.info.token1 == token_in && p.info.token0 == token_out {
                (p.total_sy, p.total_pt)
            } else {
                return None;
            };
            ratio(ti, to) // Pendle is simulated fee-free upstream
        }
        PoolState::UniswapV3(_) | PoolState::UniswapV4(_) => {
            let (sqrt_price_x96, t0, t1, fee) = match pool {
                PoolState::UniswapV3(v3) => (
                    v3.sqrt_price_x96,
                    v3.info.token0,
                    v3.info.token1,
                    v3.info.fee,
                ),
                PoolState::UniswapV4(v4) => (
                    v4.sqrt_price_x96,
                    v4.info.token0,
                    v4.info.token1,
                    v4.info.fee,
                ),
                _ => unreachable!(),
            };
            // Guard absurd prices before converting to f64.
            if sqrt_price_x96.is_zero() || sqrt_price_x96 >= (U256::from(1u8) << 200usize) {
                return None;
            }
            let price01 = sqrt_price_ratio_f64(sqrt_price_x96)?;
            let fee_factor = 1.0 - fee as f64 / PPM;
            if t0 == token_in && t1 == token_out {
                Some(fee_factor * price01)
            } else if t1 == token_in && t0 == token_out {
                let inv = 1.0 / price01;
                (inv.is_finite() && inv > 0.0).then_some(fee_factor * inv)
            } else {
                None
            }
        }
        PoolState::Curve(c) => {
            let i = *c.token_index.get(&token_in)?;
            let j = *c.token_index.get(&token_out)?;
            let bi = *c.balances.get(i)?;
            let bj = *c.balances.get(j)?;
            ratio(bi, bj)
        }
        PoolState::Balancer(b) => {
            let i = *b.token_index.get(&token_in)?;
            let j = *b.token_index.get(&token_out)?;
            let bi = *b.balances.get(i)?;
            let bj = *b.balances.get(j)?;
            let wi = *b.weights.get(i)?;
            let wj = *b.weights.get(j)?;
            if bi == 0 || bj == 0 || wi == 0 || wj == 0 {
                return None;
            }
            // Weighted-pool spot = (bal_out/bal_in)·(w_in/w_out). Weights are
            // mandatory: omitting them would be pessimistic whenever w_in>w_out.
            let r = (bj as f64 / bi as f64) * (wi as f64 / wj as f64);
            (r.is_finite() && r > 0.0).then_some(r)
        }
    }
}

fn v2_directional_reserves(
    v2: &UniswapV2PoolState,
    token_in: Address,
    token_out: Address,
) -> Option<(u128, u128)> {
    if v2.info.token0 == token_in && v2.info.token1 == token_out {
        Some((v2.reserve0, v2.reserve1))
    } else if v2.info.token1 == token_in && v2.info.token0 == token_out {
        Some((v2.reserve1, v2.reserve0))
    } else {
        None
    }
}

/// token1-per-token0 price implied by `sqrtPriceX96`, as positive finite f64.
fn sqrt_price_ratio_f64(sqrt_price_x96: U256) -> Option<f64> {
    const TWO_64: f64 = 18_446_744_073_709_551_616.0;
    const TWO_96: f64 = 79_228_162_514_264_337_593_543_950_336.0;
    const TWO_128: f64 = 340_282_366_920_938_463_463_374_607_431_768_211_456.0;
    const TWO_192: f64 =
        6_277_101_735_386_680_763_835_789_423_207_666_416_102_355_444_464_034_512_896.0;
    let &[l0, l1, l2, l3] = sqrt_price_x96.as_limbs();
    // Little-endian limbs: value = l0 + l1·2^64 + l2·2^128 + l3·2^192.
    // Relative error ≤ a few ulps — ample for a pruning filter.
    let s = l0 as f64 + (l1 as f64) * TWO_64 + (l2 as f64) * TWO_128 + (l3 as f64) * TWO_192;
    if !s.is_finite() || s <= 0.0 {
        return None;
    }
    let scaled = s / TWO_96;
    let price = scaled * scaled;
    (price.is_finite() && price > 0.0).then_some(price)
}

#[derive(Clone, Copy)]
struct Edge {
    to: usize,
    pool: Address,
    weight: f64,
}

struct TokenGraph {
    tokens: Vec<Address>,
    /// Per from-token adjacency: (to-token, pool, -ln(marginal_rate)).
    adj: Vec<Vec<Edge>>,
}

/// One directed edge of an extracted candidate cycle (from-token → to-token).
#[derive(Clone, Copy)]
struct CycleEdge {
    pool: Address,
    from: usize,
    to: usize,
}

struct CandidateCycle {
    edges: Vec<CycleEdge>,
}

impl CandidateCycle {
    fn pools(&self) -> Vec<Address> {
        self.edges.iter().map(|e| e.pool).collect()
    }

    /// Re-validate profitability with freshly computed exact marginal rates:
    /// the product around the cycle must exceed 1 (potentially profitable at
    /// some finite trade size).
    fn is_valid(&self, pm: &PoolManager, graph: &TokenGraph, max_depth: usize) -> bool {
        if self.edges.len() < 2 || self.edges.len() > max_depth {
            return false;
        }
        // Continuity: each edge must start where the previous one ended.
        for pair in self.edges.windows(2) {
            if pair[0].to != pair[1].from {
                return false;
            }
        }
        if self.edges.last().map(|e| e.to) != self.edges.first().map(|e| e.from) {
            return false;
        }
        let mut product = 1.0f64;
        for e in &self.edges {
            let (Some(&tin), Some(&tout)) = (graph.tokens.get(e.from), graph.tokens.get(e.to))
            else {
                return false;
            };
            let Some(pool) = pm.get(&e.pool) else {
                return false;
            };
            match marginal_exchange_rate(pool, tin, tout) {
                Some(r) if r.is_finite() && r > 0.0 => product *= r,
                _ => return false,
            }
        }
        product > 1.0 + CYCLE_RATE_MARGIN
    }
}

impl TokenGraph {
    fn is_empty(&self) -> bool {
        self.adj.is_empty()
    }

    fn build(pm: &PoolManager) -> Self {
        let mut token_ids: HashMap<Address, usize> = HashMap::new();
        let mut tokens: Vec<Address> = Vec::new();
        let mut adj: Vec<Vec<Edge>> = Vec::new();

        for pool in pm.all_pools() {
            let info = pool.info();
            if info.token0.is_zero() || info.token1.is_zero() || info.token0 == info.token1 {
                continue;
            }
            let u = intern_token(&mut token_ids, &mut tokens, &mut adj, info.token0);
            let v = intern_token(&mut token_ids, &mut tokens, &mut adj, info.token1);

            if let Some(rate) = marginal_exchange_rate(pool, info.token0, info.token1) {
                if let Some(w) = neg_log(rate) {
                    adj[u].push(Edge {
                        to: v,
                        pool: info.address,
                        weight: w,
                    });
                }
            }
            if let Some(rate) = marginal_exchange_rate(pool, info.token1, info.token0) {
                if let Some(w) = neg_log(rate) {
                    adj[v].push(Edge {
                        to: u,
                        pool: info.address,
                        weight: w,
                    });
                }
            }
        }

        TokenGraph { tokens, adj }
    }

    /// One series of Bellman–Ford rounds (≤ `rounds`, virtual-source init at 0)
    /// followed by predecessor-chain cycle extraction. Returns the first
    /// candidate cycle whose pools are not banned, or `None` when the graph
    /// holds no bounded negative cycle outside `banned`.
    fn find_negative_cycle(
        &self,
        rounds: usize,
        max_depth: usize,
        banned: &HashSet<Address>,
    ) -> Option<CandidateCycle> {
        let n = self.adj.len();
        let mut dist = vec![0.0f64; n];
        let mut pred: Vec<Option<(usize, Address)>> = vec![None; n];
        let mut last_relaxed: Vec<usize> = Vec::new();
        let mut any_improvement = false;

        for _ in 0..rounds {
            any_improvement = false;
            let mut relaxed_this_round: Vec<usize> = Vec::new();
            for u in 0..n {
                let du = dist[u];
                for edge in &self.adj[u] {
                    if banned.contains(&edge.pool) {
                        continue;
                    }
                    let nd = du + edge.weight;
                    if nd < dist[edge.to] - RELAX_EPS {
                        dist[edge.to] = nd;
                        pred[edge.to] = Some((u, edge.pool));
                        any_improvement = true;
                        relaxed_this_round.push(edge.to);
                    }
                }
            }
            last_relaxed = relaxed_this_round;
            if !any_improvement {
                break;
            }
        }

        if !any_improvement && last_relaxed.is_empty() {
            return None;
        }

        // Walk predecessor chains from nodes relaxed in the final round; the
        // first repeated node within the window delimits an embedded cycle.
        for &start in &last_relaxed {
            if let Some(cycle) = extract_cycle_from_pred(&pred, start, max_depth) {
                return Some(cycle);
            }
        }
        None
    }
}

fn intern_token(
    ids: &mut HashMap<Address, usize>,
    tokens: &mut Vec<Address>,
    adj: &mut Vec<Vec<Edge>>,
    t: Address,
) -> usize {
    *ids.entry(t).or_insert_with(|| {
        tokens.push(t);
        adj.push(Vec::new());
        adj.len() - 1
    })
}

/// Follow predecessors from `start` for at most `max_depth + 1` steps; when a
/// node repeats within the window, return the enclosed cycle as directed edges
/// (chain order preserved: edges[i].to == edges[i+1].from).
fn extract_cycle_from_pred(
    pred: &[Option<(usize, Address)>],
    start: usize,
    max_depth: usize,
) -> Option<CandidateCycle> {
    let mut seen: HashMap<usize, usize> = HashMap::new();
    let mut chain: Vec<CycleEdge> = Vec::new();
    let mut cur = start;

    for _ in 0..=(max_depth + 1) {
        if let Some(&pos) = seen.get(&cur) {
            // The chain was collected walking predecessors backwards, so the
            // enclosed slice must be reversed to restore hop order.
            let cycle_edges: Vec<CycleEdge> = chain[pos..].iter().rev().copied().collect();
            if cycle_edges.len() >= 2 && cycle_edges.len() <= max_depth {
                return Some(CandidateCycle { edges: cycle_edges });
            }
            return None;
        }
        seen.insert(cur, chain.len());
        let (prev_node, pool) = pred[cur]?;
        chain.push(CycleEdge {
            pool,
            from: prev_node,
            to: cur,
        });
        cur = prev_node;
    }
    None
}

fn neg_log(rate: f64) -> Option<f64> {
    if !rate.is_finite() || rate <= 0.0 {
        return None;
    }
    let w = -(rate.ln());
    w.is_finite().then_some(w)
}

fn estimate_gas_for_multi_hop(
    path: &[Address],
    pm: &PoolManager,
    flash_loan_gas: u64,
    calibration: &GasCalibrationSnapshot,
) -> u64 {
    let calldata = calldata_gas_estimate(path.len());
    let mut total = 40_000u64 + calldata + flash_loan_gas;
    let mut dex_counts: HashMap<DexType, usize> = HashMap::new();
    for addr in path {
        if let Some(pool) = pm.get(addr) {
            total = total.saturating_add(pool.gas_estimate());
            *dex_counts.entry(pool.info().dex_type).or_default() += 1;
        } else {
            total = total.saturating_add(80_000);
        }
    }

    // #7: when enough same-shape transactions have been observed, replace the
    // structural estimate with the calibrated observation clamped to ±100% of
    // the structural estimate (rejects outlier transactions).
    let dominant = dominant_dex_type(&dex_counts);
    calibration.blended_gas_limit(dominant, path.len(), total)
}

/// Most frequent DEX type among participating pools.
fn dominant_dex_type(counts: &HashMap<DexType, usize>) -> DexType {
    counts
        .iter()
        .max_by_key(|(_, &c)| c)
        .map(|(&d, _)| d)
        .unwrap_or(DexType::UniswapV2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn tok(b: u8) -> Address {
        Address::repeat_byte(b)
    }

    fn v2_pool(addr: Address, token0: Address, token1: Address, r0: u128, r1: u128) -> PoolState {
        PoolState::UniswapV2(UniswapV2PoolState {
            info: crate::pool::state::PoolInfo {
                address: addr,
                token0,
                token1,
                fee: 30,
                dex_type: DexType::UniswapV2,
                ..Default::default()
            },
            reserve0: r0,
            reserve1: r1,
        })
    }

    fn pm_with(pools: Vec<PoolState>) -> PoolManager {
        let mut pm = PoolManager::new();
        for p in pools {
            pm.add_pool(p);
        }
        pm
    }

    #[test]
    fn marginal_rate_v2_directional() {
        let pool = v2_pool(
            address!("aa00000000000000000000000000000000000001"),
            tok(1),
            tok(2),
            1_000_000,
            2_000_000,
        );
        let r12 = marginal_exchange_rate(&pool, tok(1), tok(2)).unwrap();
        let r21 = marginal_exchange_rate(&pool, tok(2), tok(1)).unwrap();
        assert!((r12 - 2.0 * 0.997).abs() < 1e-9);
        assert!((r21 - 0.5 * 0.997).abs() < 1e-9);
        assert!(marginal_exchange_rate(&pool, tok(3), tok(1)).is_none());
    }

    #[test]
    fn finds_two_pool_cycle_when_mispriced() {
        // Same pair, divergent prices: 1:4 vs 1:5 (token1 per token0).
        let pa = v2_pool(
            address!("aa00000000000000000000000000000000000002"),
            tok(1),
            tok(2),
            400_000,
            1_000_000,
        );
        let pb = v2_pool(
            address!("aa00000000000000000000000000000000000003"),
            tok(1),
            tok(2),
            500_000,
            1_000_000,
        );
        let pm = pm_with(vec![pa, pb]);

        let paths = MultiHopArbDetector::find_negative_cycle_paths(&pm, 4, &ScanScope::Full);
        assert_eq!(paths.len(), 1, "one 2-cycle expected, got {paths:?}");
        assert_eq!(paths[0].len(), 2);
    }

    #[test]
    fn finds_triangular_cycle() {
        // USDC(tok1) -> WMATIC(tok2) at 0.5, WMATIC -> USDT(tok3) at 2.0,
        // USDT -> USDC at 1.0: cycle product ~3.96.
        let pa = v2_pool(
            address!("bb00000000000000000000000000000000000001"),
            tok(1),
            tok(2),
            1_000_000,
            2_000_000,
        );
        let pb = v2_pool(
            address!("bb00000000000000000000000000000000000002"),
            tok(3),
            tok(2),
            1_000_000,
            500_000,
        );
        let pc = v2_pool(
            address!("bb00000000000000000000000000000000000003"),
            tok(1),
            tok(3),
            1_000_000,
            1_000_000,
        );
        let pm = pm_with(vec![pa, pb, pc]);

        let paths = MultiHopArbDetector::find_negative_cycle_paths(&pm, 4, &ScanScope::Full);
        assert!(!paths.is_empty(), "triangle must be found");
        assert!(paths.iter().any(|p| p.len() == 3), "expected a 3-pool path");
    }

    #[test]
    fn aligned_prices_yield_no_cycles() {
        // Identical prices everywhere: every cycle product = (1-f)^k < 1.
        let pa = v2_pool(
            address!("cc00000000000000000000000000000000000001"),
            tok(1),
            tok(2),
            1_000_000,
            1_000_000,
        );
        let pb = v2_pool(
            address!("cc00000000000000000000000000000000000002"),
            tok(1),
            tok(2),
            1_000_000,
            1_000_000,
        );
        let pc = v2_pool(
            address!("cc00000000000000000000000000000000000003"),
            tok(2),
            tok(3),
            1_000_000,
            1_000_000,
        );
        let pd = v2_pool(
            address!("cc00000000000000000000000000000000000004"),
            tok(3),
            tok(1),
            1_000_000,
            1_000_000,
        );
        let pm = pm_with(vec![pa, pb, pc, pd]);

        let paths = MultiHopArbDetector::find_negative_cycle_paths(&pm, 4, &ScanScope::Full);
        assert!(
            paths.is_empty(),
            "aligned prices must not emit cycles, got {paths:?}"
        );
    }

    #[test]
    fn dirty_scope_filters_clean_cycles() {
        // Profitable triangle; only one pool marked dirty.
        let pa = v2_pool(
            address!("dd00000000000000000000000000000000000001"),
            tok(1),
            tok(2),
            1_000_000,
            2_000_000,
        );
        let pb = v2_pool(
            address!("dd00000000000000000000000000000000000002"),
            tok(3),
            tok(2),
            1_000_000,
            500_000,
        );
        let pc = v2_pool(
            address!("dd00000000000000000000000000000000000003"),
            tok(1),
            tok(3),
            1_000_000,
            1_000_000,
        );
        let pm = pm_with(vec![pa, pb, pc]);

        let mut dirty = std::collections::HashSet::new();
        dirty.insert(address!("dd00000000000000000000000000000000000003"));
        let paths =
            MultiHopArbDetector::find_negative_cycle_paths(&pm, 4, &ScanScope::Dirty(&dirty));
        assert!(
            paths
                .iter()
                .all(|p| p.contains(&address!("dd00000000000000000000000000000000000003"))),
            "every emitted cycle must contain the dirty pool, got {paths:?}"
        );
        assert!(!paths.is_empty());
    }
}
