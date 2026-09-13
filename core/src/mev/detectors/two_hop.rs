//! Two-hop arbitrage detection — finds cyclic arbitrage across two connected pools (V2↔V2, V2↔V3, V3↔V3).

use super::arb_common;
use alloy::primitives::Address;
use std::cmp;

use crate::pool::math::balancer as balancer_math;
use crate::pool::math::curve as curve_math;
use crate::pool::math::{
    optimal_two_hop_arb, optimal_two_hop_arb_generic, optimal_two_hop_arb_segmented,
    quote_exact_in, PoolQuote, TwoHopArbResult,
};
use crate::pool::state::{
    calldata_gas_estimate, BalancerPoolState, CurvePoolState, PoolManager, PoolState, QuoteKind,
    ScanScope,
};
use crate::types::MevOpportunity;
use crate::types::{GasConfig, Strategy};

/// Maximum tick-band breakpoints enumerated per V3 pool when segmenting the
/// profit landscape for deterministic optimization.
const MAX_V3_BREAKPOINTS_PER_POOL: usize = 24;
/// Maximum inverted breakpoints taken from the second pool (inversion requires
/// binary-search probes through the first pool's quote, so this is kept lower).
const MAX_INVERTED_BREAKPOINTS: usize = 12;

/// Detects two-hop arbitrage opportunities across V2, V3, and mixed pools.
///
/// Uses analytical closed-form solutions for V2 pairs and a step-by-step quote
/// engine for V3 pools. Maintains a per-block dedup set so the same persistent
/// arb gap is not re-reported across multiple transactions in the same block.
/// If pool reserves change by >0.1% within the same block, the dedup is cleared
/// for that pair so the changed opportunity can be re-detected (H2).
pub struct TwoHopArbDetector {
    block_number: u64,
    seen: std::collections::HashMap<(Address, Address, Address, Address), (u128, u128)>,
}

impl TwoHopArbDetector {
    /// Create a new detector for the given block.
    /// The `seen` set is fresh each block, so opportunities can be re-detected
    /// on the next block if the price gap persists.
    pub fn new(block_number: u64) -> Self {
        Self {
            block_number,
            seen: std::collections::HashMap::new(),
        }
    }

    /// Check all arbitrage pool-pair directions and emit profitable two-hop opportunities.
    /// Deduplicates per block: each unique (pool_a, pool_b, token_in, token_out) is emitted
    /// at most once per block *unless* pool reserves change by >0.1%, in which case the
    /// dedup is cleared and the opportunity is re-evaluated (H2).
    ///
    /// `scope` restricts the scan: pass [`ScanScope::Full`] for the first detection
    /// pass of a block, then [`ScanScope::Dirty`] with the set of pools touched by
    /// earlier transactions — untouched pairs cannot produce new opportunities.
    pub fn detect(
        &mut self,
        pool_manager: &PoolManager,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
        scope: &ScanScope,
    ) -> Vec<MevOpportunity> {
        let mut opportunities = Vec::new();
        let pairs = pool_manager.arbitrage_pairs();

        for pair in pairs.iter() {
            if !scope.contains_pair(&pair.pool_a, &pair.pool_b) {
                continue;
            }
            for (buy_pool, sell_pool) in [(pair.pool_a, pair.pool_b), (pair.pool_b, pair.pool_a)] {
                if let Some(opp) = Self::check_direction(
                    pool_manager,
                    buy_pool,
                    sell_pool,
                    pair.shared_token,
                    self.block_number,
                    tx_index,
                    timestamp,
                    base_fee_per_gas,
                    gas_config,
                ) {
                    if arb_common::dedup_arb(&mut self.seen, pool_manager, &opp) {
                        opportunities.push(opp);
                    }
                }
            }
        }

        opportunities
    }

    #[expect(clippy::too_many_arguments)]
    fn check_direction(
        pm: &PoolManager,
        buy_pool: Address,
        sell_pool: Address,
        shared_token: Address,
        block_number: u64,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
    ) -> Option<MevOpportunity> {
        let pool_a = pm.get(&buy_pool)?;
        let pool_b = pm.get(&sell_pool)?;

        // Spot-price pre-filter: when the marginal prices are aligned (the gross
        // cycle rate cannot cover both pools' fees plus a safety margin), no
        // trade size can be profitable — skip the expensive optimizer entirely.
        // Eliminates ~90%+ of optimizer work at zero accuracy loss.
        if !arb_common::passes_spot_prefilter(pool_a, pool_b, shared_token) {
            return None;
        }

        let (token_in, token_out) = arb_tokens(pool_a, pool_b, shared_token)?;

        // Fee-on-transfer filter: the simulation assumes the full quote is
        // received, but sell-tax tokens take a cut on transfer — producing
        // phantom opportunities. Exclude known and dynamically-learned FOT tokens.
        if arb_common::is_fot_pair(pm, token_in, token_out) {
            return None;
        }

        let result = quote_path(pool_a, pool_b, shared_token)?;

        if result.profit == 0 {
            return None;
        }

        let gas_limit = estimate_gas_for_two_hop(
            pool_a,
            pool_b,
            shared_token,
            gas_config.flash_loan_provider.gas_overhead(),
            &gas_config.calibration,
        );
        let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);

        // Subtract flash loan fee from gross profit
        let flash_fee = gas_config.flash_loan_fee(result.input_amount);
        let profit_after_fl = result.profit.saturating_sub(flash_fee);

        // Normalize profit to wrapped native token when token_in != token_out
        let (expected_profit, raw_profit) = arb_common::normalize_profit(
            pm,
            token_in,
            token_out,
            profit_after_fl,
            result.input_amount,
        );

        let slippage = compute_slippage_profits(
            pm,
            pool_a,
            pool_b,
            shared_token,
            token_in,
            token_out,
            result.input_amount,
        );

        Some(arb_common::build_arb_opportunity(
            arb_common::ArbOpportunityInput {
                strategy: Strategy::TwoHopArb,
                block_number,
                tx_index,
                timestamp,
                pool_a: buy_pool,
                pool_b: sell_pool,
                token_in,
                token_out,
                input_amount: result.input_amount,
                expected_profit,
                raw_profit,
                slippage,
                gas_cost_wei,
                path: None,
            },
        ))
    }
}

/// Compute the optimal two-hop arbitrage result between any two pools that share a token.
///
/// Supports all pool type combinations — quoting and the piecewise/closed-form
/// path selection are dispatched per pool through the [`PoolState`] quoting
/// registry (see `pool/state/quoting.rs`), so a new DEX needs one math file
/// plus one arm in that registry instead of a combination match here.
///
/// Returns `None` if the pool types are not supported or no profitable path exists.
pub fn quote_path(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
) -> Option<TwoHopArbResult> {
    let (token_in, token_out) = arb_tokens(pool_a, pool_b, shared_token)?;

    match (pool_a.quote_kind(), pool_b.quote_kind()) {
        // Unsupported pool types (LB/Pendle/Metric/Fluid for two-hop) — no path.
        (QuoteKind::Unsupported, _) | (_, QuoteKind::Unsupported) => None,
        // Two constant-product legs — analytic closed form.
        (QuoteKind::ConstantProduct, QuoteKind::ConstantProduct) => {
            let (a_in, a_out) = pool_a.reserve_pair(shared_token, true)?;
            let (b_in, b_out) = pool_b.reserve_pair(shared_token, false)?;
            optimal_two_hop_arb(
                PoolQuote {
                    reserve_in: a_in,
                    reserve_out: a_out,
                    fee: pool_a.info().fee_tier(),
                },
                PoolQuote {
                    reserve_in: b_in,
                    reserve_out: b_out,
                    fee: pool_b.info().fee_tier(),
                },
            )
        }
        // At least one concentrated-liquidity leg — segment the input domain
        // at tick-band crossings before maximizing.
        (QuoteKind::Piecewise, _) | (_, QuoteKind::Piecewise) => {
            let max_input = match (pool_a.quote_kind(), pool_b.quote_kind()) {
                (QuoteKind::Piecewise, QuoteKind::Piecewise) => {
                    cmp::max(pool_a.max_input(token_in)?, pool_b.max_input(shared_token)?)
                }
                (QuoteKind::Piecewise, QuoteKind::ConstantProduct) => {
                    pool_b.sell_reserve(token_out)?
                }
                _ => pool_a.max_input(token_in)?,
            };
            let quote_a = |x: u128| pool_a.quote_dir(x, token_in, shared_token);
            let quote_b = |x: u128| pool_b.quote_dir(x, shared_token, token_out);
            let a_bp = pool_a.seg_breakpoints(max_input, token_in, MAX_V3_BREAKPOINTS_PER_POOL);
            let mid_max = quote_a(max_input).unwrap_or(u128::MAX);
            let b_bp = pool_b.seg_breakpoints(mid_max, shared_token, MAX_V3_BREAKPOINTS_PER_POOL);
            let breakpoints = compose_path_breakpoints(a_bp, b_bp, &quote_a, max_input);
            optimal_two_hop_arb_segmented(max_input, &breakpoints, &quote_a, &quote_b)
        }
        // Concave invariant pools (Curve/Balancer) with any non-piecewise leg —
        // golden-section maximization.
        _ => {
            let max_input = pool_a.max_input(token_in)?;
            let quote_a = |x: u128| pool_a.quote_dir(x, token_in, shared_token);
            let quote_b = |x: u128| pool_b.quote_dir(x, shared_token, token_out);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
    }
}

/// Compute profit at ±1%/±2% slippage levels around the optimal input,
/// normalized to native with the same C5 datapoint semantics as the profit
/// itself (a slippage probe that cannot be priced is an absent datapoint).
fn compute_slippage_profits(
    pm: &PoolManager,
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
    token_in: Address,
    token_out: Address,
    optimal_input: u128,
) -> arb_common::SlippageProfits {
    arb_common::slippage_profits(optimal_input, |input| {
        two_hop_profit_at(pool_a, pool_b, shared_token, input)
            .filter(|p| *p > 0)
            .and_then(|p| {
                arb_common::normalize_profit_native(pm, token_in, token_out, p, optimal_input)
            })
    })
}

/// Compute the profit for a two-hop arbitrage at a fixed input amount.
/// Returns the profit (output - input) or 0 if unprofitable.
fn two_hop_profit_at(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
    input_amount: u128,
) -> Option<u128> {
    let (token_in, token_out) = arb_tokens(pool_a, pool_b, shared_token)?;

    let intermediate = pool_a.quote_dir(input_amount, token_in, shared_token)?;
    let output = pool_b.quote_dir(intermediate, shared_token, token_out)?;

    (output > input_amount).then(|| output - input_amount)
}

/// Extract the token_in (spent) and token_out (received) for a two-hop arb
/// given two pools that share a common token.
///
/// For multi-token pools (Curve 3pool, Balancer weighted pools with 3+ tokens),
/// evaluates all candidate non-shared token pairs and picks the most profitable
/// one using a quick test estimate. Falls back to deterministic address-order
/// selection if all candidates are unprofitable (C3 fix).
fn arb_tokens(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
) -> Option<(Address, Address)> {
    let info_a = pool_a.info();
    let info_b = pool_b.info();

    let token_in_fast = if info_a.token0 == shared_token {
        Some(info_a.token1)
    } else if info_a.token1 == shared_token {
        Some(info_a.token0)
    } else {
        None
    };

    let token_out_fast = if info_b.token0 == shared_token {
        Some(info_b.token1)
    } else if info_b.token1 == shared_token {
        Some(info_b.token0)
    } else {
        None
    };

    // Fast path: both pools are 2-token — no multi-token ambiguity
    if let (Some(ti), Some(to)) = (token_in_fast, token_out_fast) {
        return Some((ti, to));
    }

    // Multi-token path: gather all candidate non-shared tokens from each pool
    let candidates_a: Vec<Address> = match pool_a {
        PoolState::Curve(c) => c
            .token_index
            .keys()
            .filter(|k| **k != shared_token && !k.is_zero())
            .copied()
            .collect(),
        PoolState::Balancer(b) => b
            .token_index
            .keys()
            .filter(|k| **k != shared_token && !k.is_zero())
            .copied()
            .collect(),
        _ => token_in_fast.into_iter().collect(),
    };

    let candidates_b: Vec<Address> = match pool_b {
        PoolState::Curve(c) => c
            .token_index
            .keys()
            .filter(|k| **k != shared_token && !k.is_zero())
            .copied()
            .collect(),
        PoolState::Balancer(b) => b
            .token_index
            .keys()
            .filter(|k| **k != shared_token && !k.is_zero())
            .copied()
            .collect(),
        _ => token_out_fast.into_iter().collect(),
    };

    if candidates_a.is_empty() || candidates_b.is_empty() {
        return None;
    }

    // Evaluate each candidate pair and pick the most profitable (C3)
    let mut best: Option<(Address, Address)> = None;
    let mut best_profit: u128 = 0;

    for &ti in &candidates_a {
        for &to in &candidates_b {
            if let Some(profit) = estimate_arb_pair_profit(pool_a, pool_b, shared_token, ti, to) {
                if profit > best_profit {
                    best_profit = profit;
                    best = Some((ti, to));
                }
            }
        }
    }

    // Fallback: deterministic address-order selection if no pair is profitable
    best.or_else(|| {
        let ti = candidates_a.into_iter().min()?;
        let to = candidates_b.into_iter().min()?;
        Some((ti, to))
    })
}

/// Quick profit estimate for a candidate (token_in, token_out) pair, using a
/// small test input (0.1% of pool A's reserve for token_in). Used by `arb_tokens`
/// to select the most profitable pair in multi-token pools (C3).
fn estimate_arb_pair_profit(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
    token_in: Address,
    token_out: Address,
) -> Option<u128> {
    let max_input = pool_a.max_input(token_in)?;
    let test_input = (max_input / 1000).max(1);

    let intermediate = quote_exact_in(pool_a, token_in, shared_token, test_input)?;
    let output = quote_exact_in(pool_b, shared_token, token_out, intermediate)?;

    (output > test_input).then(|| output - test_input)
}

/// Curve output amount dispatcher — forwards to `curve_math::curve_output_amount`.
pub fn curve_output_amount(
    amount_in: u128,
    pool: &CurvePoolState,
    token_in: Address,
    token_out: Address,
) -> Option<u128> {
    curve_math::curve_output_amount(amount_in, pool, token_in, token_out)
}

/// Balancer quote dispatcher — forwards to `balancer_math::balancer_quote_exact_in`.
pub fn balancer_quote_exact_in(
    amount_in: u128,
    pool: &BalancerPoolState,
    token_in: Address,
    token_out: Address,
) -> Option<u128> {
    balancer_math::balancer_quote_exact_in(amount_in, pool, token_in, token_out)
}

/// Combine first-pool breakpoints (already in input-x space) with second-pool
/// thresholds inverted through the monotone first-pool quote into x-space.
fn compose_path_breakpoints(
    direct: Vec<u128>,
    second_thresholds: Vec<u128>,
    quote_first: &impl Fn(u128) -> Option<u128>,
    max_input: u128,
) -> Vec<u128> {
    let mut out = direct;
    let mid_max = quote_first(max_input).unwrap_or(0);
    let mut inverted = 0usize;
    for m in second_thresholds {
        if inverted >= MAX_INVERTED_BREAKPOINTS {
            break;
        }
        if m == 0 || m > mid_max {
            continue;
        }
        if let Some(x) = arb_common::invert_monotone_quote(quote_first, m, max_input) {
            out.push(x);
            inverted += 1;
        }
    }
    out
}

/// Estimate the gas limit for a two-hop arbitrage opportunity based on the
/// actual pool types involved and the swap direction (H7).
///
/// For V3 pools, uses direction-aware tick crossing estimation. For V2/Curve/Balancer,
/// uses per-type empirical benchmarks. Includes base overhead and calldata cost.
///
/// When the observed-gas calibration has enough samples for this shape,
/// the structural estimate is replaced by the calibrated observation (clamped).
fn estimate_gas_for_two_hop(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
    flash_loan_gas: u64,
    calibration: &crate::types::gas::GasCalibrationSnapshot,
) -> u64 {
    let base_overhead = 40_000u64;
    let calldata = calldata_gas_estimate(2);

    let a_gas = {
        let (t0, t1) = pool_a.token_pair();
        let token_in = if t0 == shared_token { t1 } else { t0 };
        pool_a.swap_gas_estimate(token_in)
    };
    let b_gas = pool_b.swap_gas_estimate(shared_token);

    let analytic = base_overhead + calldata + a_gas + b_gas + flash_loan_gas;

    // #7: dominant DEX type buckets the observation; hop count is always 2 here.
    let mut dex_counts: std::collections::HashMap<crate::dex_type::DexType, usize> =
        std::collections::HashMap::new();
    *dex_counts.entry(pool_a.info().dex_type).or_default() += 1;
    *dex_counts.entry(pool_b.info().dex_type).or_default() += 1;

    arb_common::blend_gas_limit(calibration, &dex_counts, 2, analytic)
}
