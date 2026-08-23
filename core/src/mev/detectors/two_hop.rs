//! Two-hop arbitrage detection — finds cyclic arbitrage across two connected pools (V2↔V2, V2↔V3, V3↔V3).

use alloy::primitives::{Address, U256, U512};

/// Percentage multiplier for +1% adjustment (101/100).
const PCT_101: u128 = 101;
/// Percentage multiplier for -1% adjustment (99/100).
const PCT_99: u128 = 99;
/// Percentage multiplier for +2% adjustment (102/100).
const PCT_102: u128 = 102;
/// Percentage multiplier for -2% adjustment (98/100).
const PCT_98: u128 = 98;
use std::cmp;

use crate::pool::math::balancer as balancer_math;
use crate::pool::math::curve as curve_math;
use crate::pool::math::v3::{estimate_v3_swap_gas, max_v3_tradeable_amount, quote_v3_exact_in};
use crate::pool::math::{
    constant_product_output_amount, optimal_two_hop_arb, optimal_two_hop_arb_generic,
    optimal_two_hop_arb_segmented, quote_exact_in, v3_breakpoints, TwoHopArbResult,
};
use crate::pool::state::{
    calldata_gas_estimate, check_dedup_key, is_fee_on_transfer_token, BalancerPoolState,
    CurvePoolState, PoolManager, PoolState, ScanScope, UniswapV2PoolState,
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

        for (pool_a, pool_b, shared_token) in pairs.iter() {
            if !scope.contains_pair(pool_a, pool_b) {
                continue;
            }
            if let Some(opp) = Self::check_direction(
                pool_manager,
                *pool_a,
                *pool_b,
                *shared_token,
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
            if let Some(opp) = Self::check_direction(
                pool_manager,
                *pool_b,
                *pool_a,
                *shared_token,
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
        if !passes_spot_prefilter(pool_a, pool_b, shared_token) {
            return None;
        }

        let (token_in, token_out) = arb_tokens(pool_a, pool_b, shared_token)?;

        // Fee-on-transfer filter (#9): the simulation assumes the full quote is
        // received, but sell-tax tokens take a cut on transfer — producing
        // phantom opportunities. Exclude known FOT tokens entirely.
        if is_fee_on_transfer_token(&token_in) || is_fee_on_transfer_token(&token_out) {
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
        );
        let gas_cost_wei = gas_config.compute_gas_cost_with_limit(gas_limit, base_fee_per_gas);

        // Subtract flash loan fee from gross profit
        let flash_fee = gas_config.flash_loan_fee(result.input_amount);
        let profit_after_fl = result.profit.saturating_sub(flash_fee);

        // Normalize profit to wrapped native token when token_in != token_out
        let (expected_profit, raw_profit) = if token_in == token_out {
            (U256::from(profit_after_fl), None)
        } else {
            let raw = U256::from(profit_after_fl);
            // Convert output token profit to native using pool reserves.
            // Falls back to: total_output_native - total_input_native when
            // direct profit normalization is unavailable (C5).
            let native_profit = pm
                .normalize_to_native(token_out, profit_after_fl)
                .or_else(|| {
                    let total_input = result.input_amount;
                    let total_output = total_input.saturating_add(profit_after_fl);
                    let native_in = pm.normalize_to_native(token_in, total_input)?;
                    let native_out = pm.normalize_to_native(token_out, total_output)?;
                    native_out.checked_sub(native_in)
                })
                .unwrap_or(profit_after_fl);
            (U256::from(native_profit), Some(raw))
        };

        let (profit_slippage_p1, profit_slippage_m1, profit_slippage_p2, profit_slippage_m2) =
            compute_slippage_profits(pool_a, pool_b, shared_token, result.input_amount);

        Some(MevOpportunity {
            canonical_id: None,
            block_number,
            tx_index,
            strategy: Strategy::TwoHopArb,
            pool_a: buy_pool,
            pool_b: sell_pool,
            token_in,
            token_out,
            input_amount: U256::from(result.input_amount),
            expected_profit,
            raw_profit,
            profit_slippage_p1,
            profit_slippage_m1,
            profit_slippage_p2,
            profit_slippage_m2,
            gas_cost_wei,
            timestamp,
            path: None,
            tick_lower: None,
            tick_upper: None,
            liquidity_amount: None,
            victim_tx_index: None,
            backrun_tx_index: None,
            mempool_only: false,
            confidence: None,
        })
    }
}

/// Compute the optimal two-hop arbitrage result between any two pools that share a token.
///
/// Supports all pool type combinations:
/// - UniswapV2 ↔ UniswapV2
/// - UniswapV3 ↔ UniswapV3
/// - UniswapV2 ↔ UniswapV3 (both directions)
/// - Curve ↔ Curve
/// - Balancer ↔ Balancer
///
/// Returns `None` if the pool types are not supported or no profitable path exists.
pub fn quote_path(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
) -> Option<TwoHopArbResult> {
    let (token_in, token_out) = arb_tokens(pool_a, pool_b, shared_token)?;
    match (pool_a, pool_b) {
        (PoolState::UniswapV2(a), PoolState::UniswapV2(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            optimal_two_hop_arb(r_a_other, r_a_shared, fee_a, r_b_in, r_b_out, fee_b)
        }
        (PoolState::UniswapV3(a), PoolState::UniswapV3(b)) => {
            let zero_a = shared_token == a.info.token1;
            let zero_b = shared_token == b.info.token0;
            let max_input = cmp::max(
                max_v3_tradeable_amount(a, zero_a),
                max_v3_tradeable_amount(b, zero_b),
            );
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
            let mid_max = quote_a(max_input).unwrap_or(u128::MAX);
            let breakpoints = compose_path_breakpoints(
                v3_breakpoints(a, zero_a, max_input, MAX_V3_BREAKPOINTS_PER_POOL),
                v3_breakpoints(b, zero_b, mid_max, MAX_V3_BREAKPOINTS_PER_POOL),
                &quote_a,
                max_input,
            );
            optimal_two_hop_arb_segmented(max_input, &breakpoints, &quote_a, &quote_b)
        }
        (PoolState::UniswapV2(a), PoolState::UniswapV3(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let zero_b = shared_token == b.info.token0;
            let max_input = r_a_other;
            let quote_a = |x: u128| constant_product_output_amount(x, r_a_other, r_a_shared, fee_a);
            let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
            let mid_max = quote_a(max_input).unwrap_or(u128::MAX);
            let breakpoints = compose_path_breakpoints(
                Vec::new(),
                v3_breakpoints(b, zero_b, mid_max, MAX_V3_BREAKPOINTS_PER_POOL),
                &quote_a,
                max_input,
            );
            optimal_two_hop_arb_segmented(max_input, &breakpoints, &quote_a, &quote_b)
        }
        (PoolState::UniswapV3(a), PoolState::UniswapV2(b)) => {
            let zero_a = shared_token == a.info.token1;
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            let max_input = r_b_out;
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| constant_product_output_amount(x, r_b_in, r_b_out, fee_b);
            let breakpoints = v3_breakpoints(a, zero_a, max_input, MAX_V3_BREAKPOINTS_PER_POOL);
            optimal_two_hop_arb_segmented(max_input, &breakpoints, &quote_a, &quote_b)
        }
        (PoolState::Curve(a), PoolState::Curve(b)) => {
            let max_input = a.balances[*a.token_index.get(&token_in)?];
            let quote_a = |x: u128| curve_output_amount(x, a, token_in, shared_token);
            let quote_b = |x: u128| curve_output_amount(x, b, shared_token, token_out);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::Balancer(b)) => {
            let max_input = *a.balances.get(*a.token_index.get(&token_in)?)?;
            let quote_a = |x: u128| balancer_quote_exact_in(x, a, token_in, shared_token);
            let quote_b = |x: u128| balancer_quote_exact_in(x, b, shared_token, token_out);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::Curve(a), PoolState::UniswapV2(b)) => {
            let max_input = a.balances[*a.token_index.get(&token_in)?];
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            let quote_a = |x: u128| curve_output_amount(x, a, token_in, shared_token);
            let quote_b = |x: u128| constant_product_output_amount(x, r_b_in, r_b_out, fee_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::UniswapV2(a), PoolState::Curve(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let quote_a = |x: u128| constant_product_output_amount(x, r_a_other, r_a_shared, fee_a);
            let quote_b = |x: u128| curve_output_amount(x, b, shared_token, token_out);
            optimal_two_hop_arb_generic(r_a_other, &quote_a, &quote_b)
        }
        (PoolState::Curve(a), PoolState::UniswapV3(b)) => {
            let max_input = a.balances[*a.token_index.get(&token_in)?];
            let zero_b = shared_token == b.info.token0;
            let quote_a = |x: u128| curve_output_amount(x, a, token_in, shared_token);
            let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
            let mid_max = quote_a(max_input).unwrap_or(u128::MAX);
            let breakpoints = compose_path_breakpoints(
                Vec::new(),
                v3_breakpoints(b, zero_b, mid_max, MAX_V3_BREAKPOINTS_PER_POOL),
                &quote_a,
                max_input,
            );
            optimal_two_hop_arb_segmented(max_input, &breakpoints, &quote_a, &quote_b)
        }
        (PoolState::UniswapV3(a), PoolState::Curve(b)) => {
            let zero_a = shared_token == a.info.token1;
            let max_input = max_v3_tradeable_amount(a, zero_a);
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| curve_output_amount(x, b, shared_token, token_out);
            let breakpoints = v3_breakpoints(a, zero_a, max_input, MAX_V3_BREAKPOINTS_PER_POOL);
            optimal_two_hop_arb_segmented(max_input, &breakpoints, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::UniswapV2(b)) => {
            let max_input = *a.balances.get(*a.token_index.get(&token_in)?)?;
            let (r_b_in, r_b_out, fee_b) = v2_reserves(b, shared_token, false)?;
            let quote_a = |x: u128| balancer_quote_exact_in(x, a, token_in, shared_token);
            let quote_b = |x: u128| constant_product_output_amount(x, r_b_in, r_b_out, fee_b);
            optimal_two_hop_arb_generic(max_input, &quote_a, &quote_b)
        }
        (PoolState::UniswapV2(a), PoolState::Balancer(b)) => {
            let (r_a_other, r_a_shared, fee_a) = v2_reserves(a, shared_token, true)?;
            let quote_a = |x: u128| constant_product_output_amount(x, r_a_other, r_a_shared, fee_a);
            let quote_b = |x: u128| balancer_quote_exact_in(x, b, shared_token, token_out);
            optimal_two_hop_arb_generic(r_a_other, &quote_a, &quote_b)
        }
        (PoolState::Balancer(a), PoolState::UniswapV3(b)) => {
            let max_input = *a.balances.get(*a.token_index.get(&token_in)?)?;
            let zero_b = shared_token == b.info.token0;
            let quote_a = |x: u128| balancer_quote_exact_in(x, a, token_in, shared_token);
            let quote_b = |x: u128| quote_v3_exact_in(b, x, zero_b);
            let mid_max = quote_a(max_input).unwrap_or(u128::MAX);
            let breakpoints = compose_path_breakpoints(
                Vec::new(),
                v3_breakpoints(b, zero_b, mid_max, MAX_V3_BREAKPOINTS_PER_POOL),
                &quote_a,
                max_input,
            );
            optimal_two_hop_arb_segmented(max_input, &breakpoints, &quote_a, &quote_b)
        }
        (PoolState::UniswapV3(a), PoolState::Balancer(b)) => {
            let zero_a = shared_token == a.info.token1;
            let max_input = max_v3_tradeable_amount(a, zero_a);
            let quote_a = |x: u128| quote_v3_exact_in(a, x, zero_a);
            let quote_b = |x: u128| balancer_quote_exact_in(x, b, shared_token, token_out);
            let breakpoints = v3_breakpoints(a, zero_a, max_input, MAX_V3_BREAKPOINTS_PER_POOL);
            optimal_two_hop_arb_segmented(max_input, &breakpoints, &quote_a, &quote_b)
        }
        // Unsupported type combinations
        _ => None,
    }
}

/// Compute profit at ±1% and ±2% slippage levels around the optimal input.
fn compute_slippage_profits(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
    optimal_input: u128,
) -> (Option<U256>, Option<U256>, Option<U256>, Option<U256>) {
    if optimal_input == 0 {
        return (None, None, None, None);
    }
    let eval = |input: u128| match two_hop_profit_at(pool_a, pool_b, shared_token, input) {
        Some(p) if p > 0 => Some(U256::from(p)),
        _ => None,
    };
    (
        eval(optimal_input.saturating_mul(PCT_101) / 100),
        eval(optimal_input.saturating_mul(PCT_99) / 100),
        eval(optimal_input.saturating_mul(PCT_102) / 100),
        eval(optimal_input.saturating_mul(PCT_98) / 100),
    )
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

    let intermediate = match pool_a {
        PoolState::UniswapV2(a) => {
            let (r_a_other, r_a_shared, fee) = v2_reserves(a, shared_token, true)?;
            constant_product_output_amount(input_amount, r_a_other, r_a_shared, fee)?
        }
        PoolState::UniswapV3(a) => {
            let zero_a = shared_token == a.info.token1;
            quote_v3_exact_in(a, input_amount, zero_a)?
        }
        PoolState::UniswapV4(a) => {
            let zero_a = shared_token == a.info.token1;
            quote_v3_exact_in(a, input_amount, zero_a)?
        }
        PoolState::Curve(a) => curve_output_amount(input_amount, a, token_in, shared_token)?,
        PoolState::Balancer(a) => balancer_quote_exact_in(input_amount, a, token_in, shared_token)?,
        PoolState::TraderJoeLB(a) => {
            let (r_a_other, r_a_shared) = if a.info.token0 == shared_token {
                (a.reserve_y, a.reserve_x)
            } else if a.info.token1 == shared_token {
                (a.reserve_x, a.reserve_y)
            } else {
                return None;
            };
            constant_product_output_amount(input_amount, r_a_other, r_a_shared, a.info.fee)?
        }
        PoolState::Pendle(a) => {
            let (r_a_other, r_a_shared) = if a.info.token0 == shared_token {
                (a.total_sy, a.total_pt)
            } else if a.info.token1 == shared_token {
                (a.total_pt, a.total_sy)
            } else {
                return None;
            };
            constant_product_output_amount(input_amount, r_a_other, r_a_shared, 0)?
        }
    };

    let output = match pool_b {
        PoolState::UniswapV2(b) => {
            let (r_b_in, r_b_out, fee) = v2_reserves(b, shared_token, false)?;
            constant_product_output_amount(intermediate, r_b_in, r_b_out, fee)?
        }
        PoolState::UniswapV3(b) => {
            let zero_b = shared_token == b.info.token0;
            quote_v3_exact_in(b, intermediate, zero_b)?
        }
        PoolState::UniswapV4(b) => {
            let zero_b = shared_token == b.info.token0;
            quote_v3_exact_in(b, intermediate, zero_b)?
        }
        PoolState::Curve(b) => curve_output_amount(intermediate, b, shared_token, token_out)?,
        PoolState::Balancer(b) => {
            balancer_quote_exact_in(intermediate, b, shared_token, token_out)?
        }
        PoolState::TraderJoeLB(b) => {
            let (r_b_in, r_b_out) = if b.info.token0 == shared_token {
                (b.reserve_x, b.reserve_y)
            } else if b.info.token1 == shared_token {
                (b.reserve_y, b.reserve_x)
            } else {
                return None;
            };
            constant_product_output_amount(intermediate, r_b_in, r_b_out, b.info.fee)?
        }
        PoolState::Pendle(b) => {
            let (r_b_in, r_b_out) = if b.info.token0 == shared_token {
                (b.total_pt, b.total_sy)
            } else if b.info.token1 == shared_token {
                (b.total_sy, b.total_pt)
            } else {
                return None;
            };
            constant_product_output_amount(intermediate, r_b_in, r_b_out, 0)?
        }
    };

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
    let max_input = match pool_a {
        PoolState::Curve(c) => c.balances[*c.token_index.get(&token_in)?],
        PoolState::Balancer(b) => b.balances[*b.token_index.get(&token_in)?],
        PoolState::UniswapV2(v2) => {
            if v2.info.token0 == token_in {
                v2.reserve0
            } else {
                v2.reserve1
            }
        }
        PoolState::UniswapV3(v3) => max_v3_tradeable_amount(v3, v3.info.token0 == token_in),
        PoolState::UniswapV4(v4) => max_v3_tradeable_amount(v4, v4.info.token0 == token_in),
        PoolState::TraderJoeLB(lb) => {
            if lb.info.token0 == token_in {
                lb.reserve_x
            } else {
                lb.reserve_y
            }
        }
        PoolState::Pendle(p) => {
            if p.info.token0 == token_in {
                p.total_pt
            } else {
                p.total_sy
            }
        }
    };
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

/// Balancer weighted pool output — forwards to `balancer_math::balancer_output_amount`.
pub fn balancer_output_amount(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    weight_in: u128,
    weight_out: u128,
    fee: u32,
) -> Option<u128> {
    balancer_math::balancer_output_amount(
        amount_in,
        reserve_in,
        reserve_out,
        weight_in,
        weight_out,
        fee,
    )
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

/// Extract V2 pool reserves for a given direction relative to `shared_token`.
/// `buy_side = true`  → returns (reserve_other, reserve_shared, fee) where
///                        reserve_shared is what we receive (the bridge token).
/// `buy_side = false` → returns (reserve_shared, reserve_other, fee) where
///                        reserve_shared is what we give (the bridge token).
fn v2_reserves(
    pool: &UniswapV2PoolState,
    shared_token: Address,
    buy_side: bool,
) -> Option<(u128, u128, u32)> {
    let fee = pool.info.fee;
    if buy_side {
        // We give the other token, receive shared_token
        if pool.info.token0 == shared_token {
            Some((pool.reserve1, pool.reserve0, fee))
        } else if pool.info.token1 == shared_token {
            Some((pool.reserve0, pool.reserve1, fee))
        } else {
            None
        }
    } else {
        // We give shared_token, receive the other token
        if pool.info.token0 == shared_token {
            Some((pool.reserve0, pool.reserve1, fee))
        } else if pool.info.token1 == shared_token {
            Some((pool.reserve1, pool.reserve0, fee))
        } else {
            None
        }
    }
}

/// Safety margin applied on top of the combined-fee break-even in the spot
/// pre-filter: +2 bps. Absorbs integer truncation so only provably aligned
/// prices are skipped.
const PREFILTER_SAFETY_NUM: u64 = 100_002;
const PREFILTER_SAFETY_DEN: u64 = 100_000;

/// Spot-price pre-filter: returns `false` only when the marginal cycle rate is
/// *provably* below the fee break-even (plus safety margin), meaning no input
/// size can be profitable and the numeric optimizer can be skipped entirely.
///
/// Compares the marginal spot price of `shared_token` quoted in each pool's
/// other token: V2-style reserve ratios and V3 `sqrtRatioX96²`, in exact
/// integer arithmetic. Returns `true` when either price is unavailable.
fn passes_spot_prefilter(pool_a: &PoolState, pool_b: &PoolState, shared_token: Address) -> bool {
    let (Some((cost_n, cost_d)), Some((yield_n, yield_d))) = (
        marginal_price_fraction(pool_a, shared_token),
        marginal_price_fraction(pool_b, shared_token),
    ) else {
        return true;
    };
    let (Some((fa_n, fa_d)), Some((fb_n, fb_d))) = (fee_fraction(pool_a), fee_fraction(pool_b))
    else {
        return true;
    };

    // Gross cycle rate = (yield_n·cost_d) / (yield_d·cost_n), scaled by the
    // fee factors ((fa_d−fa_n)/fa_d)·((fb_d−fb_n)/fb_d). Require it to beat
    // the safety ratio SAFETY_NUM/SAFETY_DEN:
    //   yield_n·cost_d·(fa_d−fa_n)·(fb_d−fb_n)·SAFETY_DEN
    //     > yield_d·cost_n·fa_d·fb_d·SAFETY_NUM
    let lhs = U512::from(yield_n)
        * U512::from(cost_d)
        * U512::from(fa_d - fa_n.min(fa_d))
        * U512::from(fb_d - fb_n.min(fb_d))
        * U512::from(PREFILTER_SAFETY_DEN);
    let rhs = U512::from(yield_d)
        * U512::from(cost_n)
        * U512::from(fa_d)
        * U512::from(fb_d)
        * U512::from(PREFILTER_SAFETY_NUM);
    lhs > rhs
}

/// Marginal spot price of `shared_token` expressed in the pool's other token,
/// as an exact fraction `(numerator, denominator)`:
/// - V2-family: reserve ratio of the other token over the shared-token reserve
/// - V3/V4: `sqrtPriceX96² / 2^192` (token1 per token0), pre-shifted by 2^64
///   on both sides to keep cross-multiplications compact
///
/// Returns `None` for pool types without a local closed-form spot price; the
/// caller then skips filtering for that pair.
fn marginal_price_fraction(pool: &PoolState, shared_token: Address) -> Option<(U256, U256)> {
    match pool {
        PoolState::UniswapV2(v2) => {
            if v2.reserve0 == 0 || v2.reserve1 == 0 {
                return None;
            }
            if v2.info.token0 == shared_token {
                Some((U256::from(v2.reserve1), U256::from(v2.reserve0)))
            } else if v2.info.token1 == shared_token {
                Some((U256::from(v2.reserve0), U256::from(v2.reserve1)))
            } else {
                None
            }
        }
        PoolState::TraderJoeLB(lb) => {
            if lb.reserve_x == 0 || lb.reserve_y == 0 {
                return None;
            }
            if lb.info.token0 == shared_token {
                Some((U256::from(lb.reserve_y), U256::from(lb.reserve_x)))
            } else if lb.info.token1 == shared_token {
                Some((U256::from(lb.reserve_x), U256::from(lb.reserve_y)))
            } else {
                None
            }
        }
        PoolState::Pendle(p) => {
            if p.total_pt == 0 || p.total_sy == 0 {
                return None;
            }
            if p.info.token0 == shared_token {
                Some((U256::from(p.total_sy), U256::from(p.total_pt)))
            } else if p.info.token1 == shared_token {
                Some((U256::from(p.total_pt), U256::from(p.total_sy)))
            } else {
                None
            }
        }
        PoolState::UniswapV3(_) | PoolState::UniswapV4(_) => {
            let (sqrt_price_x96, token0, token1) = match pool {
                PoolState::UniswapV3(v3) => (v3.sqrt_price_x96, v3.info.token0, v3.info.token1),
                PoolState::UniswapV4(v4) => (v4.sqrt_price_x96, v4.info.token0, v4.info.token1),
                _ => unreachable!(),
            };
            // Guard against absurd prices (≥ 2^128 would overflow the compact
            // fraction below); skip filtering instead.
            if sqrt_price_x96.is_zero() || sqrt_price_x96 >= (U256::from(1u8) << 128usize) {
                return None;
            }
            // p01 = sqrtP²/2^192, rescaled: numerator = sqrtP² >> 128 (< 2^128),
            // denominator = 2^64 — relative truncation error ≤ 2^-64, ample for a filter.
            let s = U512::from(sqrt_price_x96);
            let sq = ((s * s) >> 128usize).to::<u128>();
            if sq == 0 {
                return None;
            }
            if token0 == shared_token {
                Some((U256::from(sq), U256::from(1u128 << 64)))
            } else if token1 == shared_token {
                Some((U256::from(1u128 << 64), U256::from(sq)))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Pool fee as an exact fraction per DEX convention:
/// basis points for constant-product pools, parts-per-million for
/// concentrated-liquidity pools. Pendle is simulated fee-free upstream.
fn fee_fraction(pool: &PoolState) -> Option<(u64, u64)> {
    match pool {
        PoolState::UniswapV2(p) => Some((p.info.fee as u64, BPS_FEE_DENOM)),
        PoolState::TraderJoeLB(p) => Some((p.info.fee as u64, BPS_FEE_DENOM)),
        PoolState::UniswapV3(p) | PoolState::UniswapV4(p) => {
            Some((p.info.fee as u64, PPM_FEE_DENOM))
        }
        PoolState::Pendle(_) => Some((0, BPS_FEE_DENOM)),
        _ => None,
    }
}
const BPS_FEE_DENOM: u64 = 10_000;
const PPM_FEE_DENOM: u64 = 1_000_000;

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
        if let Some(x) = invert_monotone_quote(quote_first, m, max_input) {
            out.push(x);
            inverted += 1;
        }
    }
    out
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

/// Estimate the gas limit for a two-hop arbitrage opportunity based on the
/// actual pool types involved and the swap direction (H7).
///
/// For V3 pools, uses direction-aware tick crossing estimation. For V2/Curve/Balancer,
/// uses per-type empirical benchmarks. Includes base overhead and calldata cost.
fn estimate_gas_for_two_hop(
    pool_a: &PoolState,
    pool_b: &PoolState,
    shared_token: Address,
    flash_loan_gas: u64,
) -> u64 {
    let base_overhead = 40_000u64;
    let calldata = calldata_gas_estimate(2);

    let a_gas = match pool_a {
        PoolState::UniswapV3(v3) => {
            let zero_for_one = shared_token == v3.info.token1;
            estimate_v3_swap_gas(v3, zero_for_one)
        }
        other => other.gas_estimate(),
    };
    let b_gas = match pool_b {
        PoolState::UniswapV3(v3) => {
            let zero_for_one = shared_token == v3.info.token0;
            estimate_v3_swap_gas(v3, zero_for_one)
        }
        other => other.gas_estimate(),
    };

    base_overhead + calldata + a_gas + b_gas + flash_loan_gas
}
